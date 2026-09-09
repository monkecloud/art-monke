use std::net::SocketAddr;
use std::time::Duration;

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::{FromRef, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, routing::post, Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use rusty_s3::actions::{DeleteObject, GetObject, PutObject, S3Action};
use rusty_s3::{Bucket, Credentials as S3Credentials, UrlStyle};
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use tokio::signal::unix::{signal, SignalKind};

const SESSION_COOKIE: &str = "session";

// Garage doesn't expose its region per bucket, so it isn't one of the env vars the admin
// hands out — it just has to match the s3_region set once in the cluster's garage.toml.
const S3_REGION: &str = "garage";

// Matches the "max upload size of like a gig" call from the disk-sizing discussion.
const MAX_UPLOAD_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    s3_bucket: Bucket,
    s3_credentials: S3Credentials,
    http: reqwest::Client,
}

impl FromRef<AppState> for PgPool {
    fn from_ref(state: &AppState) -> Self {
        state.pool.clone()
    }
}

#[derive(Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

// Differentiated from ApiError because a login failure is the caller's problem (401/409),
// not the api's, and the caller needs to be able to tell the two apart.
enum AuthError {
    InvalidCredentials,
    UsernameTaken,
    Unauthenticated,
    Internal,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            AuthError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, "invalid username or password\n")
            }
            AuthError::UsernameTaken => (StatusCode::CONFLICT, "username taken\n"),
            AuthError::Unauthenticated => (StatusCode::UNAUTHORIZED, "not signed in\n"),
            AuthError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n"),
        };
        (status, body).into_response()
    }
}

// 32 random bytes looked up in `sessions`, not a JWT or signed cookie: revoking one is a
// DELETE rather than needing a key-rotation or blocklist story.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn start_session(
    pool: &PgPool,
    jar: CookieJar,
    user_id: i64,
) -> Result<CookieJar, AuthError> {
    let token = random_token();
    sqlx::query("INSERT INTO sessions (token, user_id) VALUES ($1, $2)")
        .bind(&token)
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| {
            eprintln!("query failed: {e}");
            AuthError::Internal
        })?;

    // Lax, not Strict: web and api share one hostname under different path prefixes, so
    // every request is same-site regardless, and Lax is enough to keep the cookie off
    // cross-site requests.
    let cookie = Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .build();

    Ok(jar.add(cookie))
}

async fn register(
    State(pool): State<PgPool>,
    jar: CookieJar,
    Json(creds): Json<Credentials>,
) -> Result<(CookieJar, StatusCode), AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    let hash = Argon2::default()
        .hash_password(creds.password.as_bytes(), &salt)
        .map_err(|e| {
            eprintln!("hash failed: {e}");
            AuthError::Internal
        })?
        .to_string();

    let user_id: i64 = sqlx::query_scalar(
        "INSERT INTO users (username, password_hash) VALUES ($1, $2) RETURNING id",
    )
    .bind(&creds.username)
    .bind(&hash)
    .fetch_one(&pool)
    .await
    .map_err(|e| match e.as_database_error().and_then(|d| d.code()) {
        Some(code) if code == "23505" => AuthError::UsernameTaken,
        _ => {
            eprintln!("query failed: {e}");
            AuthError::Internal
        }
    })?;

    let jar = start_session(&pool, jar, user_id).await?;
    Ok((jar, StatusCode::CREATED))
}

async fn login(
    State(pool): State<PgPool>,
    jar: CookieJar,
    Json(creds): Json<Credentials>,
) -> Result<(CookieJar, StatusCode), AuthError> {
    let row: Option<(i64, String)> =
        sqlx::query_as("SELECT id, password_hash FROM users WHERE username = $1")
            .bind(&creds.username)
            .fetch_optional(&pool)
            .await
            .map_err(|e| {
                eprintln!("query failed: {e}");
                AuthError::Internal
            })?;

    let (user_id, hash) = row.ok_or(AuthError::InvalidCredentials)?;
    let parsed = PasswordHash::new(&hash).map_err(|e| {
        eprintln!("stored hash unparseable: {e}");
        AuthError::Internal
    })?;
    Argon2::default()
        .verify_password(creds.password.as_bytes(), &parsed)
        .map_err(|_| AuthError::InvalidCredentials)?;

    let jar = start_session(&pool, jar, user_id).await?;
    Ok((jar, StatusCode::NO_CONTENT))
}

async fn logout(State(pool): State<PgPool>, jar: CookieJar) -> (CookieJar, StatusCode) {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        if let Err(e) = sqlx::query("DELETE FROM sessions WHERE token = $1")
            .bind(cookie.value())
            .execute(&pool)
            .await
        {
            eprintln!("query failed: {e}");
        }
    }
    let removal = Cookie::build(SESSION_COOKIE).path("/");
    (jar.remove(removal), StatusCode::NO_CONTENT)
}

#[derive(Serialize)]
struct Me {
    username: String,
}

async fn me(user: AuthUser) -> Json<Me> {
    Json(Me {
        username: user.username,
    })
}

// Reads the session cookie and resolves it against Postgres on every request rather than
// trusting a signed payload, so a revoked session (logout, or a DELETE run by hand) stops
// working immediately instead of only once a signed token would expire.
struct AuthUser {
    id: i64,
    username: String,
}

impl<S> FromRequestParts<S> for AuthUser
where
    PgPool: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AuthError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let pool = PgPool::from_ref(state);
        let jar = CookieJar::from_request_parts(parts, state)
            .await
            .expect("CookieJar extraction is infallible");
        let token = jar
            .get(SESSION_COOKIE)
            .ok_or(AuthError::Unauthenticated)?
            .value()
            .to_owned();

        let row: Option<(i64, String)> = sqlx::query_as(
            "SELECT users.id, users.username FROM sessions
             JOIN users ON users.id = sessions.user_id
             WHERE sessions.token = $1 AND sessions.expires_at > now()",
        )
        .bind(&token)
        .fetch_optional(&pool)
        .await
        .map_err(|e| {
            eprintln!("query failed: {e}");
            AuthError::Internal
        })?;

        row.map(|(id, username)| AuthUser { id, username })
            .ok_or(AuthError::Unauthenticated)
    }
}

// AuthUser's own rejection (AuthError) already handles the unauthenticated case directly,
// so this only needs to cover failures past that point.
enum AudioError {
    LengthRequired,
    TooLarge,
    NotFound,
    UploadFailed,
    DownloadFailed,
    Internal,
}

impl IntoResponse for AudioError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            AudioError::LengthRequired => {
                (StatusCode::LENGTH_REQUIRED, "content-length required\n")
            }
            AudioError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "file too large\n"),
            AudioError::NotFound => (StatusCode::NOT_FOUND, "not found\n"),
            AudioError::UploadFailed => (StatusCode::BAD_GATEWAY, "upload failed\n"),
            AudioError::DownloadFailed => (StatusCode::BAD_GATEWAY, "download failed\n"),
            AudioError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n"),
        };
        (status, body).into_response()
    }
}

#[derive(Serialize)]
struct UploadedFile {
    id: i64,
}

#[derive(Deserialize)]
struct UploadQuery {
    filename: String,
}

// Proxies the upload to Garage rather than handing the client a presigned URL: Garage has
// no public Ingress of its own (NodePort only, no TLS), so a browser outside the cluster
// can't reach it directly. Streams the request body straight into the outgoing S3 PUT
// instead of buffering it, so a ~1GB file doesn't sit fully in memory and the client's
// upload-progress event tracks real progress rather than just reaching the api.
async fn upload_audio(
    State(app): State<AppState>,
    user: AuthUser,
    Query(query): Query<UploadQuery>,
    request: Request,
) -> Result<(StatusCode, Json<UploadedFile>), AudioError> {
    let content_length = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or(AudioError::LengthRequired)?;

    if content_length == 0 || content_length > MAX_UPLOAD_BYTES {
        return Err(AudioError::TooLarge);
    }

    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();

    // Reuses the same "opaque random token" shape as session ids purely for convenience;
    // it has no relation to any session.
    let key = format!("{}/{}", user.id, random_token());

    let file_id: i64 = sqlx::query_scalar(
        "INSERT INTO audio_files (user_id, s3_key, filename) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(user.id)
    .bind(&key)
    .bind(&query.filename)
    .fetch_one(&app.pool)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    // Signed for the api to use immediately itself, not handed to the client, so a short
    // expiry is fine — it only needs to outlive this one request.
    let action = PutObject::new(&app.s3_bucket, Some(&app.s3_credentials), &key);
    let signed_url = action.sign(Duration::from_secs(60));

    let body = reqwest::Body::wrap_stream(request.into_body().into_data_stream());
    let put_result = app
        .http
        .put(signed_url)
        .header(header::CONTENT_LENGTH, content_length)
        .header(header::CONTENT_TYPE, content_type)
        .body(body)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status);

    if let Err(e) = put_result {
        eprintln!("s3 put failed: {e}");
        // The object never landed, so the row shouldn't outlive it either — nothing else
        // will ever move a permanently-'uploading' row forward.
        if let Err(e) = sqlx::query("DELETE FROM audio_files WHERE id = $1")
            .bind(file_id)
            .execute(&app.pool)
            .await
        {
            eprintln!("query failed: {e}");
        }
        return Err(AudioError::UploadFailed);
    }

    // Guarded on the row still being 'uploading' so a delete that lands while this upload
    // was in flight isn't clobbered back to 'uploaded' by this update arriving after it.
    sqlx::query(
        "UPDATE audio_files SET status = 'uploaded' WHERE id = $1 AND status = 'uploading'",
    )
    .bind(file_id)
    .execute(&app.pool)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    Ok((StatusCode::CREATED, Json(UploadedFile { id: file_id })))
}

#[derive(Serialize)]
struct AudioFile {
    id: i64,
    filename: String,
    status: String,
    created_at: String,
}

async fn list_audio(
    State(app): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<AudioFile>>, AudioError> {
    let rows: Vec<(i64, String, String, String)> = sqlx::query_as(
        r#"SELECT id, filename, status,
                  to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
           FROM audio_files
           WHERE user_id = $1 AND status != 'deleted'
           ORDER BY created_at DESC"#,
    )
    .bind(user.id)
    .fetch_all(&app.pool)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    Ok(Json(
        rows.into_iter()
            .map(|(id, filename, status, created_at)| AudioFile {
                id,
                filename,
                status,
                created_at,
            })
            .collect(),
    ))
}

// A Content-Disposition filename is a quoted header value, not free text: reject anything
// that could break out of the quotes or inject a header, and fall back to '_' rather than
// failing the whole download over a stray character in a name the user picked themselves.
fn sanitize_filename_header(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii() && !c.is_ascii_control() && c != '"' && c != '\\' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

// Proxies the download for the same reason uploads are proxied: Garage has no public
// Ingress, so a presigned URL handed to the browser would point somewhere it can't reach.
// Streams the S3 response straight into the api's own response body rather than buffering.
async fn download_audio(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> Result<Response, AudioError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT s3_key, filename FROM audio_files WHERE id = $1 AND user_id = $2 AND status = 'uploaded'",
    )
    .bind(id)
    .bind(user.id)
    .fetch_optional(&app.pool)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    let (key, filename) = row.ok_or(AudioError::NotFound)?;

    let action = GetObject::new(&app.s3_bucket, Some(&app.s3_credentials), &key);
    let signed_url = action.sign(Duration::from_secs(60));

    let s3_response = app
        .http
        .get(signed_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| {
            eprintln!("s3 get failed: {e}");
            AudioError::DownloadFailed
        })?;

    // Whatever content-type the upload stored (or defaulted to) is what Garage hands back.
    let content_type = s3_response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_owned();

    let body = axum::body::Body::from_stream(s3_response.bytes_stream());

    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                sanitize_filename_header(&filename)
            ),
        )
        .body(body)
        .map_err(|e| {
            eprintln!("response build failed: {e}");
            AudioError::Internal
        })
}

// Soft-delete: the S3 object is actually removed, but the row stays around at status
// 'deleted' rather than being dropped, so history isn't lost.
async fn delete_audio(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, AudioError> {
    let key: Option<(String,)> = sqlx::query_as(
        "SELECT s3_key FROM audio_files WHERE id = $1 AND user_id = $2 AND status != 'deleted'",
    )
    .bind(id)
    .bind(user.id)
    .fetch_optional(&app.pool)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    let (key,) = key.ok_or(AudioError::NotFound)?;

    // S3 DELETE is idempotent, so this is fine even for a row that's still 'uploading' and
    // never actually got an object written.
    let action = DeleteObject::new(&app.s3_bucket, Some(&app.s3_credentials), &key);
    let signed_url = action.sign(Duration::from_secs(60));
    if let Err(e) = app
        .http
        .delete(signed_url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
    {
        eprintln!("s3 delete failed: {e}");
        return Err(AudioError::UploadFailed);
    }

    sqlx::query("UPDATE audio_files SET status = 'deleted' WHERE id = $1")
        .bind(id)
        .execute(&app.pool)
        .await
        .map_err(|e| {
            eprintln!("query failed: {e}");
            AudioError::Internal
        })?;

    Ok(StatusCode::NO_CONTENT)
}

// Deliberately does not touch Postgres. A probe that queries the database turns one blip
// into every replica failing readiness at once, which is an outage the blip did not cause.
async fn healthz() -> &'static str {
    "ok\n"
}

#[tokio::main]
async fn main() {
    // Two ways in. Locally, one DATABASE_URL as .env.example shows. In the cluster, the
    // libpq variables: host/port/username/password come from the owner's Postgres Secret
    // unchanged, and PGDATABASE names this app's own database, which is the only part that
    // differs between environments. Keeping them separate means no password is ever
    // substituted into a URL, so a character like @ or / in it cannot corrupt the string.
    let options = if let Ok(url) = std::env::var("DATABASE_URL") {
        url.parse::<PgConnectOptions>()
            .expect("DATABASE_URL is not a valid Postgres connection string")
    } else if std::env::var_os("PGDATABASE").is_some() {
        PgConnectOptions::new()
    } else {
        // Not defaulted: PgConnectOptions would otherwise fall back to a database named
        // after the connecting user, which is the shared one this app just moved off.
        panic!("set DATABASE_URL, or the PG* variables including PGDATABASE");
    };

    // Lazy: the pod comes up and answers probes even if Postgres is briefly unreachable,
    // instead of crashlooping on a transient failure. Connections are opened on first use.
    let pool = PgPoolOptions::new()
        .max_connections(5)
        .acquire_timeout(Duration::from_secs(5))
        .connect_lazy_with(options);

    // Migrations take a Postgres advisory lock, so both replicas starting together is safe:
    // one applies, the other waits and finds nothing to do. Retried because a pod can start
    // while Postgres is still coming back, and a blocked egress hangs rather than erroring.
    let mut attempt = 0;
    loop {
        attempt += 1;
        match sqlx::migrate!("./migrations").run(&pool).await {
            Ok(()) => break,
            Err(e) if attempt < 5 => {
                eprintln!("migration attempt {attempt} failed: {e}; retrying");
                tokio::time::sleep(Duration::from_secs(3)).await;
            }
            // Give up loudly. With maxUnavailable: 0 the old pods keep serving, so a
            // crashloop stalls the rollout rather than taking the api down.
            Err(e) => panic!("migrations failed after {attempt} attempts: {e}"),
        }
    }

    let s3_endpoint = std::env::var("S3_ENDPOINT")
        .expect("set S3_ENDPOINT")
        .parse::<url::Url>()
        .expect("S3_ENDPOINT is not a valid URL");
    let s3_bucket_name = std::env::var("S3_BUCKET").expect("set S3_BUCKET");
    let s3_access_key = std::env::var("S3_ACCESS_KEY").expect("set S3_ACCESS_KEY");
    let s3_secret_key = std::env::var("S3_SECRET_KEY").expect("set S3_SECRET_KEY");

    // Path style, not virtual-host: Garage is reached by IP/NodePort here, not a hostname
    // that a bucket subdomain could be carved out of.
    let s3_bucket = Bucket::new(s3_endpoint, UrlStyle::Path, s3_bucket_name, S3_REGION)
        .expect("S3_ENDPOINT/S3_BUCKET did not form a valid bucket url");
    let s3_credentials = S3Credentials::new(s3_access_key, s3_secret_key);

    let app_state = AppState {
        pool,
        s3_bucket,
        s3_credentials,
        http: reqwest::Client::new(),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
        .route("/api/audio", post(upload_audio).get(list_audio))
        .route("/api/audio/{id}", get(download_audio).delete(delete_audio))
        .with_state(app_state);

    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8080);
    // 0.0.0.0, not loopback: a container bound to 127.0.0.1 is unreachable from the pod
    // network and every probe fails.
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    eprintln!("monke-api listening on {addr}");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .expect("server error");
}

// Kubernetes sends SIGTERM and waits before killing. Without this the process dies
// immediately and in-flight requests are dropped on every rolling update.
async fn shutdown() {
    let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
    term.recv().await;
    eprintln!("SIGTERM received, draining");
}
