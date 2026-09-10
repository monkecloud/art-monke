use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
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
use futures::{Stream, StreamExt, TryStreamExt};
use monke_common::targets::{
    derivative_key, progress_key, target_column, target_from_progress_key, PROGRESS_FAILED,
    PROGRESS_READY,
};
use monke_common::{redis_client, S3Store, TARGETS};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// 32 random bytes looked up in `sessions`, not a JWT or signed cookie: revoking one is a
// DELETE rather than needing a key-rotation or blocklist story.
fn random_token() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    to_hex(&bytes)
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
    // Carries the id of the file this upload duplicated, so the client can point the user at
    // what they already have instead of just refusing.
    Duplicate { existing_id: i64 },
    UploadFailed,
    DownloadFailed,
    Internal,
}

// The duplicate case is the only one a client needs to act on programmatically, so it is the
// only one with a structured body; the rest stay plain text as they were.
#[derive(Serialize)]
struct DuplicateBody {
    duplicate_of: i64,
}

impl IntoResponse for AudioError {
    fn into_response(self) -> Response {
        if let AudioError::Duplicate { existing_id } = self {
            return (
                StatusCode::CONFLICT,
                Json(DuplicateBody {
                    duplicate_of: existing_id,
                }),
            )
                .into_response();
        }

        let (status, body) = match self {
            AudioError::LengthRequired => {
                (StatusCode::LENGTH_REQUIRED, "content-length required\n")
            }
            AudioError::TooLarge => (StatusCode::PAYLOAD_TOO_LARGE, "file too large\n"),
            AudioError::NotFound => (StatusCode::NOT_FOUND, "not found\n"),
            // Handled above; matched here only because the compiler needs it to be.
            AudioError::Duplicate { .. } => (StatusCode::CONFLICT, "duplicate\n"),
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

fn is_unique_violation(e: &sqlx::Error) -> bool {
    e.as_database_error()
        .and_then(|d| d.code())
        .is_some_and(|code| code == "23505")
}

// Unwinds an upload that turned out to duplicate one this user already has: the object this
// request just wrote is removed, its row is retired, and the original's id is looked up so the
// response can name it.
//
// Returns the error to answer with rather than a Result, because every path through here still
// owes the client a response — the upload did not land, whatever else went wrong on the way.
async fn discard_duplicate(
    app: &AppState,
    file_id: i64,
    key: &str,
    user_id: i64,
    filename: &str,
    content_hash: &str,
) -> AudioError {
    // Safe to remove: this is the redundant second copy, written under its own random key.
    // The original row's object lives at a different key entirely and is untouched.
    let deleted = app.s3.delete(key).await;
    if let Err(e) = &deleted {
        eprintln!("s3 delete of duplicate object failed: {e}");
    }

    // 'deleted' only when the object is definitely gone; 'delete_pending' when it might not
    // be. Same distinction the failed-PUT path draws, so a reaper inherits exactly the rows
    // that still need looking at and none that don't.
    let status = if deleted.is_ok() {
        "deleted"
    } else {
        "delete_pending"
    };
    if let Err(e) =
        sqlx::query("UPDATE audio_files SET status = $2 WHERE id = $1 AND status = 'uploading'")
            .bind(file_id)
            .bind(status)
            .execute(&app.pool)
            .await
    {
        eprintln!("query failed: {e}");
    }

    let existing: Result<Option<(i64,)>, sqlx::Error> = sqlx::query_as(
        "SELECT id FROM audio_files
         WHERE user_id = $1 AND filename = $2 AND content_hash = $3 AND status = 'uploaded'",
    )
    .bind(user_id)
    .bind(filename)
    .bind(content_hash)
    .fetch_optional(&app.pool)
    .await;

    match existing {
        Ok(Some((existing_id,))) => AudioError::Duplicate { existing_id },
        // The row it collided with was deleted in the gap between the violation and this
        // lookup. Nothing was stored either way, so this cannot be reported as success.
        Ok(None) => {
            eprintln!("duplicate of a row that no longer exists");
            AudioError::Internal
        }
        Err(e) => {
            eprintln!("query failed: {e}");
            AudioError::Internal
        }
    }
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

    // Hashed as the bytes stream past on their way to Garage, so nothing is buffered purely
    // to be digested — a ~1GB upload would not fit in this pod's memory limit. The hasher is
    // shared with the stream adapter rather than returned by it, because wrap_stream takes
    // ownership of the stream and there is no way to get anything back out afterwards.
    let hasher = Arc::new(Mutex::new(Sha256::new()));
    let hashed_body = {
        let hasher = Arc::clone(&hasher);
        request
            .into_body()
            .into_data_stream()
            // map_ok, so a broken upload stream still propagates its error to the PUT rather
            // than being silently treated as the end of the body.
            .map_ok(move |chunk| {
                // The lock can never actually contend — one stream, polled one chunk at a
                // time — and is here only to make the hasher Send + Sync for wrap_stream.
                // Nothing holds it across an await, so it cannot be poisoned either.
                hasher.lock().expect("hasher mutex poisoned").update(&chunk);
                chunk
            })
    };

    let put_result = app
        .s3
        .put(
            &key,
            content_length,
            &content_type,
            reqwest::Body::wrap_stream(hashed_body),
        )
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

    // Only knowable now: the hash covers the whole body, so the duplicate check below cannot
    // happen before the bytes have been sent. That is the cost of hashing server-side instead
    // of trusting a client-supplied digest — a duplicate is detected after the transfer, not
    // before it.
    let content_hash = to_hex(
        &hasher
            .lock()
            .expect("hasher mutex poisoned")
            .clone()
            .finalize(),
    );

    // The flip to 'uploaded' and the three queue rows go in together: a committed 'uploaded'
    // with no jobs is a file that silently never gets transcoded, and jobs against a row
    // that never became 'uploaded' are three guaranteed failures.
    let mut tx = app.pool.begin().await.map_err(|e| {
        eprintln!("begin failed: {e}");
        AudioError::Internal
    })?;

    // Guarded on the row still being 'uploading' so a delete that lands while this upload
    // was in flight isn't clobbered back to 'uploaded' by this update arriving after it.
    let flip = sqlx::query(
        "UPDATE audio_files SET status = 'uploaded', content_hash = $2
         WHERE id = $1 AND status = 'uploading'",
    )
    .bind(file_id)
    .bind(&content_hash)
    .execute(&mut *tx)
    .await;

    let flipped = match flip {
        Ok(flipped) => flipped,
        // The partial UNIQUE index fired: this user already has a live file with this name
        // and these exact bytes. Letting the database decide is what makes two identical
        // uploads racing each other safe — a SELECT-then-write check would let both through.
        Err(e) if is_unique_violation(&e) => {
            // Already aborted by the violation, so nothing further can run inside it.
            if let Err(e) = tx.rollback().await {
                eprintln!("rollback failed: {e}");
            }
            return Err(
                discard_duplicate(&app, file_id, &key, user.id, &query.filename, &content_hash)
                    .await,
            );
        }
        Err(e) => {
            eprintln!("query failed: {e}");
            return Err(AudioError::Internal);
        }
    };

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
    // Absent until a worker has probed the source, and on files older than the column. The
    // client shows a length only when there is one.
    duration_seconds: Option<f64>,
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
    duration_seconds: Option<f64>,
    #[sqlx(flatten)]
    transcodes: TranscodeRow,
}

// One field rather than a set of booleans, so a client renders a single switch and cannot
// paint two states at once.
//
// "ready"   - the has_aac_* column: the derivative is in the bucket and can be served
// "running" - a worker holds the job right now
// "pending" - queued, not claimed by any worker yet
// "failed"  - gave up, either terminally or after exhausting its attempts
#[derive(Serialize)]
struct TranscodeState {
    target: &'static str,
    state: &'static str,
    // 0–100, and only while running — and even then absent until the first tick lands, or if
    // the progress key's TTL lapsed because the worker stopped reporting.
    progress: Option<u8>,
}

const STATE_READY: &str = "ready";
const STATE_RUNNING: &str = "running";
const STATE_PENDING: &str = "pending";
const STATE_FAILED: &str = "failed";

// The flags-and-in-flight-targets shape that both list_audio and the SSE snapshot need,
// selected by TRANSCODE_COLUMNS. Kept in one place so the two cannot disagree about what
// "ready" means.
#[derive(sqlx::FromRow)]
struct TranscodeRow {
    has_aac_64: bool,
    has_aac_128: bool,
    has_aac_224: bool,
    in_progress: Vec<String>,
    failed: Vec<String>,
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

    // Ready is checked first and wins outright: the flag means the object is in the bucket and
    // servable, which is true regardless of what any job row says about it afterwards.
    fn state(&self, target: &str) -> &'static str {
        if self.ready(target) {
            STATE_READY
        } else if self.failed.iter().any(|t| t == target) {
            STATE_FAILED
        } else if self.in_progress.iter().any(|t| t == target) {
            STATE_RUNNING
        } else {
            STATE_PENDING
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
                state: self.state(target),
                // Asked for only when a worker actually holds the job: a percentage against a
                // queued or finished tier would be stale at best.
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
//
// Two subqueries rather than one aggregate over statuses: both are index-only lookups on
// transcode_jobs_audio_file_id_idx, and keeping them separate means the Rust side receives two
// plain Vec<String> instead of having to unpack a JSON object per row.
const TRANSCODE_COLUMNS: &str = r#"has_aac_64, has_aac_128, has_aac_224,
    ARRAY(SELECT j.target FROM transcode_jobs j
          WHERE j.audio_file_id = audio_files.id AND j.status = 'in_progress') AS in_progress,
    ARRAY(SELECT j.target FROM transcode_jobs j
          WHERE j.audio_file_id = audio_files.id AND j.status = 'failed') AS failed"#;

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
                  duration_seconds,
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
                duration_seconds: row.duration_seconds,
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

#[derive(Deserialize)]
struct DownloadQuery {
    // Absent means the original, which is what the Download link serves. The player always
    // names a tier — it never plays the source.
    tier: Option<String>,
}

// A derivative is a different file from the original, so it should not claim the original's
// name if anyone saves it: song.flac at 224k saves as song.aac_224.m4a.
fn derivative_filename(filename: &str, target: &str) -> String {
    let stem = filename
        .rsplit_once('.')
        .map_or(filename, |(stem, _extension)| stem);
    format!("{stem}.{target}.m4a")
}

// Proxies the download for the same reason uploads are proxied: Garage has no public
// Ingress, so a presigned URL handed to the browser would point somewhere it can't reach.
// Streams the S3 response straight into the api's own response body rather than buffering.
//
// Serves either the original or one transcoded tier, chosen by `?tier=`. One handler rather
// than a second route because everything around the object is identical — the ownership
// check, the Range forwarding, the streaming — and only the key differs.
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
    Query(query): Query<DownloadQuery>,
    headers: axum::http::HeaderMap,
) -> Result<Response, AudioError> {
    let (key, filename) = match query.tier.as_deref() {
        None => {
            let row: Option<(String, String)> = sqlx::query_as(
                "SELECT s3_key, filename FROM audio_files
                 WHERE id = $1 AND user_id = $2 AND status = 'uploaded'",
            )
            .bind(id)
            .bind(user.id)
            .fetch_optional(&app.pool)
            .await
            .map_err(|e| {
                eprintln!("query failed: {e}");
                AudioError::Internal
            })?;

            row.ok_or(AudioError::NotFound)?
        }
        Some(target) => {
            // An unknown tier is indistinguishable from a file that does not exist, as far as
            // the caller is concerned.
            let column = target_column(target).ok_or(AudioError::NotFound)?;

            // The flag is part of the WHERE rather than checked afterwards, so one query
            // covers ownership, upload status *and* whether this tier has actually been
            // written — a tier that is not ready yet simply matches no row and 404s.
            // `column` is a &'static str from a fixed list, because an identifier cannot be a
            // bind parameter.
            let row: Option<(String, String)> = sqlx::query_as(&format!(
                "SELECT s3_key, filename FROM audio_files
                 WHERE id = $1 AND user_id = $2 AND status = 'uploaded' AND {column}"
            ))
            .bind(id)
            .bind(user.id)
            .fetch_optional(&app.pool)
            .await
            .map_err(|e| {
                eprintln!("query failed: {e}");
                AudioError::Internal
            })?;

            let (s3_key, filename) = row.ok_or(AudioError::NotFound)?;
            (
                derivative_key(&s3_key, target),
                derivative_filename(&filename, target),
            )
        }
    };

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

// Marks the file for deletion and returns. Nothing here touches S3: the objects, the queue
// rows and eventually this row itself are cleaned up by the worker's sweep.
//
// Deliberately not an inline delete. A DELETE to Garage from here cannot be made atomic with
// what a worker is doing, so it could never stop a transcode that is already running from
// writing its derivative afterwards -- and it made the button fail outright whenever Garage
// hiccuped, leaving the file visible to someone who had asked for it gone. Marking intent is
// one Postgres write that cannot half-fail, and the sweep is what actually converges the
// bucket. See docs/delete-lifecycle.md.
async fn delete_audio(
    State(app): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> Result<StatusCode, AudioError> {
    // Guarded on the two live statuses rather than `!= 'deleted'`, so a second click on a row
    // that is already mid-sweep is a no-op rather than something that re-enters the pipeline
    // or resets a deleted_at the retention pass is counting from.
    let marked = sqlx::query(
        "UPDATE audio_files SET status = 'delete_pending'
         WHERE id = $1 AND user_id = $2 AND status IN ('uploading', 'uploaded')",
    )
    .bind(id)
    .bind(user.id)
    .execute(&app.pool)
    .await
    .map_err(|e| {
        eprintln!("query failed: {e}");
        AudioError::Internal
    })?;

    // Same 404-for-someone-else's-id reasoning as download_audio: a row that is not the
    // caller's and a row that is already going away are not distinguished here.
    if marked.rows_affected() == 0 {
        return Err(AudioError::NotFound);
    }

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
        let payload = msg.get_payload::<String>().ok()?;

        // Three kinds of message on one channel. The two terminal markers exist because a tier
        // going ready or failed is a Postgres write — without them a live subscriber would sit
        // at 99% until it happened to refetch the list.
        let state = match payload.as_str() {
            PROGRESS_READY => TranscodeState {
                target,
                state: STATE_READY,
                progress: Some(100),
            },
            PROGRESS_FAILED => TranscodeState {
                target,
                state: STATE_FAILED,
                progress: None,
            },
            percent => TranscodeState {
                target,
                state: STATE_RUNNING,
                progress: Some(percent.parse::<u8>().ok().filter(|p| *p <= 100)?),
            },
        };

        Event::default()
            .event("progress")
            .json_data(state)
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
