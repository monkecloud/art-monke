use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use argon2::password_hash::rand_core::{OsRng, RngCore};
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier};
use axum::extract::{FromRef, FromRequestParts, Path, Query, Request, State};
use axum::http::request::Parts;
use axum::http::{header, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{routing::get, routing::post, Json, Router};
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use futures::{Stream, StreamExt};
use monke_common::targets::{progress_key, target_from_progress_key};
use monke_common::{redis_client, S3Store, TARGETS};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPool;
use tokio::signal::unix::{signal, SignalKind};

const SESSION_COOKIE: &str = "session";

// Matches the "max upload size of like a gig" call from the disk-sizing discussion.
const MAX_UPLOAD_BYTES: u64 = 1024 * 1024 * 1024;

#[derive(Clone)]
struct AppState {
    pool: PgPool,
    s3: S3Store,
    // Transcode progress only, and nothing here ever fails a request over it — see
    // read_progress. The cache is one pod on one node and is explicitly disposable.
    redis: redis::Client,
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

    let body = reqwest::Body::wrap_stream(request.into_body().into_data_stream());
    let put_result = app
        .s3
        .put(&key, content_length, &content_type, body)
        .await;

    if let Err(e) = put_result {
        eprintln!("s3 put failed: {e}");

        // Best-effort delete at the same key before giving up on the row. A failed PUT does
        // not prove nothing landed — a lost response looks identical from here — and S3
        // DELETE is idempotent, so this is the same reasoning delete_audio already relies
        // on. Doing it now closes the window where the object outlives any record of it.
        if let Err(e) = app.s3.delete(&key).await {
            eprintln!("s3 delete after failed put failed: {e}");
        }

        // Not a row-delete: dropping the row would throw away the s3_key, which is the only
        // handle on an object that may in fact be there. 'delete_pending' keeps it, guarded
        // on 'uploading' so a delete that landed meanwhile isn't overwritten. Finalizing
        // these rows is the reaper's job, which is deliberately not part of this change.
        if let Err(e) = sqlx::query(
            "UPDATE audio_files SET status = 'delete_pending'
             WHERE id = $1 AND status = 'uploading'",
        )
        .bind(file_id)
        .execute(&app.pool)
        .await
        {
            eprintln!("query failed: {e}");
        }
        return Err(AudioError::UploadFailed);
    }

    // The flip to 'uploaded' and the three queue rows go in together: a committed 'uploaded'
    // with no jobs is a file that silently never gets transcoded, and jobs against a row
    // that never became 'uploaded' are three guaranteed failures.
    let mut tx = app.pool.begin().await.map_err(|e| {
        eprintln!("begin failed: {e}");
        AudioError::Internal
    })?;

    // Guarded on the row still being 'uploading' so a delete that lands while this upload
    // was in flight isn't clobbered back to 'uploaded' by this update arriving after it.
    let flipped = sqlx::query(
        "UPDATE audio_files SET status = 'uploaded' WHERE id = $1 AND status = 'uploading'",
    )
    .bind(file_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    // Zero means exactly that lost race: the row is already 'deleted' (or 'delete_pending')
    // and queueing work against it would only produce three jobs with nothing to read.
    if flipped.rows_affected() == 1 {
        let targets: Vec<String> = TARGETS.iter().map(|t| t.to_string()).collect();
        // ON CONFLICT DO NOTHING against the UNIQUE (audio_file_id, target): harmless
        // belt-and-braces, since the only way rows could already exist is a retried upload
        // against a row that had somehow gone back to 'uploading'.
        sqlx::query(
            "INSERT INTO transcode_jobs (audio_file_id, target)
             SELECT $1, target FROM unnest($2::text[]) AS target
             ON CONFLICT (audio_file_id, target) DO NOTHING",
        )
        .bind(file_id)
        .bind(&targets)
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            eprintln!("query failed: {e}");
            AudioError::Internal
        })?;
    }

    tx.commit().await.map_err(|e| {
        eprintln!("commit failed: {e}");
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
    // Always all three, in TARGETS order, so a client can render a fixed set of rows rather
    // than discovering which tiers exist from the payload.
    transcodes: Vec<TranscodeState>,
}

// The SELECT behind list_audio. `flatten` is what lets the transcode half be the same type
// the SSE snapshot selects on its own.
#[derive(sqlx::FromRow)]
struct AudioFileRow {
    id: i64,
    filename: String,
    status: String,
    created_at: String,
    #[sqlx(flatten)]
    transcodes: TranscodeRow,
}

#[derive(Serialize)]
struct TranscodeState {
    target: &'static str,
    // The has_aac_* column: the derivative is in the bucket and can be served.
    ready: bool,
    // 0–100 while a worker is actually running this tier, null otherwise — including a
    // 'pending' job nobody has picked up yet, and a tier whose worker has not ticked since
    // the progress key's TTL lapsed.
    progress: Option<u8>,
}

// The flags-and-in-flight-targets shape that both list_audio and the SSE snapshot need,
// selected by TRANSCODE_COLUMNS. Kept in one place so the two cannot disagree about what
// "ready" means.
#[derive(sqlx::FromRow)]
struct TranscodeRow {
    has_aac_64: bool,
    has_aac_128: bool,
    has_aac_224: bool,
    in_progress: Vec<String>,
}

impl TranscodeRow {
    // Matched by name rather than indexed by position, so nothing here depends on TARGETS
    // happening to be in the same order as the columns.
    fn ready(&self, target: &str) -> bool {
        match target {
            "aac_64" => self.has_aac_64,
            "aac_128" => self.has_aac_128,
            "aac_224" => self.has_aac_224,
            _ => false,
        }
    }

    // Keys for exactly the tiers worth asking Redis about. A tier that is already ready, or
    // has no running job, has nothing to report.
    fn progress_keys(&self, audio_file_id: i64) -> Vec<String> {
        self.in_progress
            .iter()
            .map(|target| progress_key(audio_file_id, target))
            .collect()
    }

    fn states(&self, audio_file_id: i64, progress: &HashMap<String, u8>) -> Vec<TranscodeState> {
        TARGETS
            .iter()
            .map(|&target| TranscodeState {
                target,
                ready: self.ready(target),
                progress: self
                    .in_progress
                    .iter()
                    .any(|t| t == target)
                    .then(|| progress.get(&progress_key(audio_file_id, target)).copied())
                    .flatten(),
            })
            .collect()
    }
}

// `status = 'in_progress'` without a lease check on purpose: a lapsed lease still means the
// tier is queued rather than finished, and the absent progress key is what makes it read as
// "no percentage yet" instead of a stale one.
const TRANSCODE_COLUMNS: &str = r#"has_aac_64, has_aac_128, has_aac_224,
    ARRAY(SELECT j.target FROM transcode_jobs j
          WHERE j.audio_file_id = audio_files.id AND j.status = 'in_progress') AS in_progress"#;

// Progress is cosmetic, and k8s/redis.yaml is one pod on one node that is gone for good if
// that node is lost. So a Redis failure degrades to "no percentage reported" rather than
// failing a request that is otherwise answerable entirely from Postgres.
async fn read_progress(client: &redis::Client, keys: &[String]) -> HashMap<String, u8> {
    if keys.is_empty() {
        return HashMap::new();
    }

    // One MGET for every in-flight tier across every file, rather than a round trip each.
    let values: Result<Vec<Option<String>>, redis::RedisError> = async {
        let mut conn = client.get_multiplexed_async_connection().await?;
        redis::AsyncCommands::mget(&mut conn, keys).await
    }
    .await;

    match values {
        Ok(values) => keys
            .iter()
            .zip(values)
            .filter_map(|(key, value)| {
                // A worker writes a plain 0–100 integer; anything else is not ours to show.
                let percent = value?.parse::<u8>().ok()?;
                (percent <= 100).then(|| (key.clone(), percent))
            })
            .collect(),
        Err(e) => {
            eprintln!("redis mget failed: {e}");
            HashMap::new()
        }
    }
}

async fn list_audio(
    State(app): State<AppState>,
    user: AuthUser,
) -> Result<Json<Vec<AudioFile>>, AudioError> {
    // 'delete_pending' is excluded alongside 'deleted': it is the failed-upload state, which
    // used to be a row-delete and so has never been something a client sees.
    let rows: Vec<AudioFileRow> = sqlx::query_as(&format!(
        r#"SELECT id, filename, status,
                  to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS"Z"')
                      AS created_at,
                  {TRANSCODE_COLUMNS}
           FROM audio_files
           WHERE user_id = $1 AND status NOT IN ('deleted', 'delete_pending')
           -- Qualified, not bare: the to_char column above is aliased `created_at`, and an
           -- unqualified ORDER BY resolves to that output alias — which would sort the
           -- second-precision *text* and tie two uploads in the same second.
           ORDER BY audio_files.created_at DESC"#
    ))
    .bind(user.id)
    .fetch_all(&app.pool)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    // One MGET for every in-flight tier of every file, rather than one per file.
    let keys: Vec<String> = rows
        .iter()
        .flat_map(|row| row.transcodes.progress_keys(row.id))
        .collect();
    let progress = read_progress(&app.redis, &keys).await;

    Ok(Json(
        rows.into_iter()
            .map(|row| AudioFile {
                transcodes: row.transcodes.states(row.id, &progress),
                id: row.id,
                filename: row.filename,
                status: row.status,
                created_at: row.created_at,
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
//
// Forwards a client Range header straight through to Garage and mirrors back whatever
// Garage answers (206 + Content-Range, or a plain 200) rather than parsing ranges itself —
// that's what the audio player's seek bar relies on to fetch just the bytes it needs instead
// of the whole file. Content-Disposition: attachment is harmless here even for the player:
// browsers only act on it for a navigation/explicit download, not for a <audio>/<video> src
// fetch, which is the only other consumer of this route.
async fn download_audio(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    headers: axum::http::HeaderMap,
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

    // A Range that isn't valid UTF-8 is not a range Garage would honour either, so dropping
    // it here just means the client gets the whole object back, as it would have anyway.
    let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());

    let s3_response = app.s3.get(&key, range).await.map_err(|e| {
        eprintln!("s3 get failed: {e}");
        AudioError::DownloadFailed
    })?;

    let status = s3_response.status();

    // Whatever content-type the upload stored (or defaulted to) is what Garage hands back.
    let content_type = s3_response
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| axum::http::HeaderValue::from_static("application/octet-stream"));
    let content_length = s3_response.headers().get(header::CONTENT_LENGTH).cloned();
    let content_range = s3_response.headers().get(header::CONTENT_RANGE).cloned();

    let body = axum::body::Body::from_stream(s3_response.bytes_stream());

    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(
            header::CONTENT_DISPOSITION,
            format!(
                "attachment; filename=\"{}\"",
                sanitize_filename_header(&filename)
            ),
        );
    if let Some(len) = content_length {
        builder = builder.header(header::CONTENT_LENGTH, len);
    }
    if let Some(range) = content_range {
        builder = builder.header(header::CONTENT_RANGE, range);
    }

    builder.body(body).map_err(|e| {
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
    if let Err(e) = app.s3.delete(&key).await {
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

// Streams one file's transcode progress as it happens, so a client watching an upload does
// not have to poll list_audio for three tiers.
//
// Ownership is checked exactly the way download_audio checks it, and the same way: 404 for a
// row that isn't the caller's, so the endpoint never confirms that someone else's id exists.
async fn audio_progress(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, AudioError> {
    let row: Option<TranscodeRow> = sqlx::query_as(&format!(
        "SELECT {TRANSCODE_COLUMNS} FROM audio_files
         WHERE id = $1 AND user_id = $2 AND status = 'uploaded'"
    ))
    .bind(id)
    .bind(user.id)
    .fetch_optional(&app.pool)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    let transcodes = row.ok_or(AudioError::NotFound)?;

    // Subscribe before reading the snapshot would be tidier in theory, but the snapshot is
    // the whole state rather than a delta — a tick that lands between the two is a
    // percentage the next tick supersedes anyway.
    let progress = read_progress(&app.redis, &transcodes.progress_keys(id)).await;
    let snapshot = Event::default()
        .event("snapshot")
        .json_data(transcodes.states(id, &progress))
        .map_err(|e| {
            eprintln!("sse snapshot serialize failed: {e}");
            AudioError::Internal
        })?;

    // One PSUBSCRIBE rather than three SUBSCRIBEs: the pattern covers every tier of this
    // file, including the ones whose job has not been claimed yet.
    let messages = match subscribe_progress(&app.redis, &progress_key(id, "*")).await {
        Ok(stream) => stream.boxed(),
        // Snapshot-only rather than an error. The client still gets the current state, and
        // an EventSource reconnects on its own — which while the cache is down amounts to
        // polling, instead of a dead endpoint.
        Err(e) => {
            eprintln!("redis psubscribe failed: {e}");
            futures::stream::empty().boxed()
        }
    };

    // The snapshot goes out immediately so a client that connects mid-transcode renders the
    // real state instead of sitting blank until some worker happens to tick.
    let stream = futures::stream::once(async move { Ok(snapshot) }).chain(messages);

    // Traefik and the browser will both drop a connection that goes quiet, and a transcode
    // can legitimately be silent for a while — a periodic comment keeps it open.
    Ok(Sse::new(stream).keep_alive(KeepAlive::default()))
}

// Split out so the handler above can fall back to snapshot-only on any Redis failure: both
// the connect and the PSUBSCRIBE can fail, and neither is worth a 5xx.
async fn subscribe_progress(
    client: &redis::Client,
    pattern: &str,
) -> Result<impl Stream<Item = Result<Event, Infallible>>, redis::RedisError> {
    let mut pubsub = client.get_async_pubsub().await?;
    pubsub.psubscribe(pattern).await?;

    Ok(pubsub.into_on_message().filter_map(|msg| async move {
        // Anything that doesn't parse is silently dropped rather than breaking the stream:
        // the pattern could match a key written by something other than a worker.
        let target = target_from_progress_key(msg.get_channel_name())?;
        let percent = msg.get_payload::<String>().ok()?.parse::<u8>().ok()?;
        if percent > 100 {
            return None;
        }

        Event::default()
            .event("progress")
            .json_data(TranscodeState {
                target,
                // A tier being transcoded right now is by definition not yet servable; the
                // flip to ready is a Postgres write, which the next snapshot reports.
                ready: false,
                progress: Some(percent),
            })
            .inspect_err(|e| eprintln!("sse progress serialize failed: {e}"))
            .ok()
            .map(Ok)
    }))
}

// Deliberately does not touch Postgres. A probe that queries the database turns one blip
// into every replica failing readiness at once, which is an outage the blip did not cause.
async fn healthz() -> &'static str {
    "ok\n"
}

#[tokio::main]
async fn main() {
    let pool = monke_common::pg_pool(5);

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

    // Both panic on a missing variable, at startup rather than on the first request that
    // needs one. redis_client() does not connect here — it is opened per use.
    let app_state = AppState {
        pool,
        s3: S3Store::from_env(),
        redis: redis_client(),
    };

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/api/auth/register", post(register))
        .route("/api/auth/login", post(login))
        .route("/api/auth/logout", post(logout))
        .route("/api/auth/me", get(me))
        .route("/api/audio", post(upload_audio).get(list_audio))
        .route("/api/audio/{id}", get(download_audio).delete(delete_audio))
        .route("/api/audio/{id}/progress", get(audio_progress))
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
