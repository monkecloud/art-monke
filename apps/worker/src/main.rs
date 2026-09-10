//! Claims transcode jobs from Postgres, runs ffmpeg, writes the derivative to the bucket.
//!
//! One job at a time, deliberately: the unit of scale is the replica count, which keeps the
//! two fixed scratch paths below exactly that — fixed — with no per-slot bookkeeping and no
//! way for two concurrent jobs in one pod to land on the same file.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use monke_common::s3::M4A_CONTENT_TYPE;
use monke_common::targets::{
    derivative_key, progress_key, target_bitrate, target_column, PROGRESS_FAILED, PROGRESS_READY,
};
use monke_common::{pg_pool, redis_client, S3Store};
use sqlx::postgres::PgPool;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::signal::unix::{signal, SignalKind};

/// Two fixed paths under the pod's own writable /tmp — no PVC, nothing shared between
/// replicas. k8s/worker.yaml sizes its ephemeral-storage request around one source file here.
const SCRATCH_DIR: &str = "/tmp/scratch";

/// Polling, not LISTEN/NOTIFY: an empty queue is the normal state and two seconds of latency
/// on a job that takes tens of seconds is not worth a second connection that has to be held
/// open and reconnected.
const IDLE_SLEEP: Duration = Duration::from_secs(2);

/// How long a claim is good for, as a Postgres interval. One constant because the claim query
/// and the renewal below have to agree.
const LEASE_INTERVAL: &str = "5 minutes";

/// Re-stamped this often while a job runs. Comfortably inside LEASE_INTERVAL so a renewal can
/// be missed without the lease lapsing.
const LEASE_RENEW_EVERY: Duration = Duration::from_secs(60);

/// Counted per claim rather than per failure, so a worker that dies without writing anything
/// back still burns one and a poisonous job cannot be retried forever.
const MAX_ATTEMPTS: i32 = 8;

/// Short, and refreshed on every tick. A crashed worker's last percentage expires on its own
/// rather than needing anything to go and clean it up.
const PROGRESS_TTL_SECS: u64 = 120;

/// `last_error` is for a human reading the queue; ffmpeg can be chatty and the whole of it is
/// not worth storing per row.
const MAX_ERROR_CHARS: usize = 500;

#[derive(sqlx::FromRow)]
struct Job {
    id: i64,
    audio_file_id: i64,
    target: String,
    /// Already incremented by the claim, so this counts the attempt now running.
    attempts: i32,
}

/// Whether it is worth this job being claimed again.
enum Failure {
    /// Garage, the network, or Postgres. A retry plausibly outlasts it.
    Retryable(String),
    /// The source is gone, or ffmpeg will not accept it. A retry changes nothing and only
    /// delays the row reaching a state someone can look at.
    Terminal(String),
}

fn source_path() -> PathBuf {
    Path::new(SCRATCH_DIR).join("source")
}

/// `.m4a` rather than a bare name: ffmpeg picks its muxer from the output extension, and with
/// no extension it refuses the job outright instead of writing MP4.
fn output_path() -> PathBuf {
    Path::new(SCRATCH_DIR).join("output.m4a")
}

#[tokio::main]
async fn main() {
    // Two connections: one for the claim and bookkeeping, one spare. The worker is not
    // concurrent, so a larger pool would only hold idle connections against a shared server.
    let pool = pg_pool(2);
    let s3 = S3Store::from_env();
    let redis = redis_client();

    // The pod name in the cluster, so a row's worker_id says which pod is holding it.
    let worker_id = format!(
        "{}-{}",
        std::env::var("HOSTNAME").unwrap_or_else(|_| "local".into()),
        std::process::id()
    );

    // SIGTERM means stop claiming, not stop working: finishing the job in hand is bounded by
    // k8s's termination grace period, whereas abandoning it makes every client wait out the
    // full lease before another worker picks it up.
    let draining = Arc::new(AtomicBool::new(false));
    tokio::spawn({
        let draining = Arc::clone(&draining);
        async move {
            let mut term = signal(SignalKind::terminate()).expect("install SIGTERM handler");
            term.recv().await;
            eprintln!("SIGTERM received; finishing the job in hand, then exiting");
            draining.store(true, Ordering::Relaxed);
        }
    });

    eprintln!("monke-worker {worker_id} started");

    while !draining.load(Ordering::Relaxed) {
        match claim_job(&pool, &worker_id).await {
            Ok(Some(job)) => {
                eprintln!(
                    "job {} ({} for file {}), attempt {}",
                    job.id, job.target, job.audio_file_id, job.attempts
                );
                if let Err(failure) = run_job(&pool, &s3, &redis, &worker_id, &job).await {
                    record_failure(&pool, &redis, &job, failure).await;
                }
            }
            Ok(None) => tokio::time::sleep(IDLE_SLEEP).await,
            // Postgres being briefly unreachable is not worth exiting over — the pool is
            // lazy and the next claim reconnects.
            Err(e) => {
                eprintln!("claim failed: {e}");
                tokio::time::sleep(IDLE_SLEEP).await;
            }
        }
    }

    eprintln!("monke-worker {worker_id} exiting");
}

/// Claims the oldest available job, with no filter on `target`: one generic pool of workers
/// rather than one deployment per bitrate, so a backlog of any one tier is spread over every
/// replica instead of queueing behind a single dedicated pod.
///
/// `FOR UPDATE SKIP LOCKED` is what makes this safe to run from every replica at once — a row
/// another worker is mid-claim on is skipped rather than waited for.
async fn claim_job(pool: &PgPool, worker_id: &str) -> Result<Option<Job>, sqlx::Error> {
    sqlx::query_as(&format!(
        "UPDATE transcode_jobs SET
             status = 'in_progress',
             worker_id = $1,
             lease_expires_at = now() + interval '{LEASE_INTERVAL}',
             attempts = attempts + 1
         WHERE id = (
             SELECT id FROM transcode_jobs
             WHERE (status = 'pending'
                    OR (status = 'in_progress' AND lease_expires_at < now()))
               AND attempts < $2
             ORDER BY created_at
             LIMIT 1
             FOR UPDATE SKIP LOCKED
         )
         RETURNING id, audio_file_id, target, attempts"
    ))
    .bind(worker_id)
    .bind(MAX_ATTEMPTS)
    .fetch_optional(pool)
    .await
}

async fn run_job(
    pool: &PgPool,
    s3: &S3Store,
    redis: &redis::Client,
    worker_id: &str,
    job: &Job,
) -> Result<(), Failure> {
    // Both come from a CHECK-constrained column, so an unknown value here means the schema
    // and this binary disagree — which no retry fixes.
    let bitrate = target_bitrate(&job.target)
        .ok_or_else(|| Failure::Terminal(format!("unknown target '{}'", job.target)))?;
    let column = target_column(&job.target)
        .ok_or_else(|| Failure::Terminal(format!("unknown target '{}'", job.target)))?;

    // Before touching anything, so cleanup never depends on the *previous* job having exited
    // cleanly — the job that crashed is precisely the one that left a file behind.
    reset_scratch()
        .await
        .map_err(|e| Failure::Retryable(format!("could not reset {SCRATCH_DIR}: {e}")))?;

    let row: Option<(String, String)> =
        sqlx::query_as("SELECT s3_key, status FROM audio_files WHERE id = $1")
            .bind(job.audio_file_id)
            .fetch_optional(pool)
            .await
            .map_err(|e| Failure::Retryable(format!("source lookup failed: {e}")))?;

    // ON DELETE CASCADE means a missing row should have taken the job with it, so this is
    // only reachable mid-delete. Either way there is nothing left to transcode.
    let (s3_key, status) = row.ok_or_else(|| {
        Failure::Terminal(format!("audio_files row {} is gone", job.audio_file_id))
    })?;
    if status != "uploaded" {
        return Err(Failure::Terminal(format!(
            "source is '{status}', not 'uploaded'"
        )));
    }

    download_source(s3, &s3_key, &source_path()).await?;

    // The lease has been running since the claim, and a ~1GB download can eat a good part of
    // it before ffmpeg has started.
    renew_lease(pool, job, worker_id).await;

    let duration = probe_duration(&source_path()).await?;
    transcode(pool, redis, job, worker_id, &bitrate, duration).await?;

    // ffmpeg's exit code is necessary but not sufficient: a zero-byte output that exits 0 is
    // still nothing worth publishing as a playable derivative.
    let size = fs::metadata(output_path())
        .await
        .map_err(|e| Failure::Terminal(format!("ffmpeg wrote no output: {e}")))?
        .len();
    if size == 0 {
        return Err(Failure::Terminal("ffmpeg wrote an empty output".into()));
    }

    let key = derivative_key(&s3_key, &job.target);
    upload_derivative(s3, &key, size).await?;

    // Both writes together: a set flag with no 'done' job gets the work redone, and a 'done'
    // job with no flag leaves a derivative nothing will ever serve.
    let mut tx = pool
        .begin()
        .await
        .map_err(|e| Failure::Retryable(format!("begin failed: {e}")))?;

    // Guarded on 'uploaded' so a delete that landed while this ran doesn't get a flag
    // advertising a derivative for a row with no source. `column` is a &'static str from a
    // fixed list — see monke_common::targets::target_column — because an identifier cannot
    // be a bind parameter.
    let flagged = sqlx::query(&format!(
        "UPDATE audio_files SET {column} = true WHERE id = $1 AND status = 'uploaded'"
    ))
    .bind(job.audio_file_id)
    .execute(&mut *tx)
    .await
    .map_err(|e| Failure::Retryable(format!("flag update failed: {e}")))?;

    if flagged.rows_affected() == 0 {
        // The object is in the bucket with nothing pointing at it. Left for the deferred
        // deletion-cleanup sweep rather than deleted here: it is the same orphan that a
        // delete racing any in-flight job produces, and one sweep should own all of them.
        eprintln!(
            "job {}: file {} is no longer 'uploaded'; marking done without setting {column}",
            job.id, job.audio_file_id
        );
    }

    sqlx::query(
        "UPDATE transcode_jobs
         SET status = 'done', last_error = NULL, worker_id = NULL, lease_expires_at = NULL
         WHERE id = $1",
    )
    .bind(job.id)
    .execute(&mut *tx)
    .await
    .map_err(|e| Failure::Retryable(format!("job update failed: {e}")))?;

    tx.commit()
        .await
        .map_err(|e| Failure::Retryable(format!("commit failed: {e}")))?;

    eprintln!("job {} done ({} bytes at {})", job.id, size, key);

    // After the commit, never before: this announces a fact about Postgres, so publishing it
    // first would let a listener show a tier as ready while the flag could still roll back.
    publish_terminal(redis, job, PROGRESS_READY).await;
    Ok(())
}

/// Puts the queue row somewhere a retry — or a human — can act on.
async fn record_failure(pool: &PgPool, redis: &redis::Client, job: &Job, failure: Failure) {
    let (status, error) = match failure {
        // Back to 'pending' only while the claim query would still pick it up. Past the cap
        // it has to be 'failed', or it sits 'pending' forever as a row nothing will select
        // and nothing will ever explain.
        Failure::Retryable(error) if job.attempts < MAX_ATTEMPTS => ("pending", error),
        Failure::Retryable(error) => ("failed", error),
        Failure::Terminal(error) => ("failed", error),
    };

    let error = truncate(&error, MAX_ERROR_CHARS);
    eprintln!("job {} -> {status}: {error}", job.id);

    // Lease and worker_id cleared either way: nothing holds this row now, and a stale
    // worker_id on a 'failed' row only misleads whoever reads it next.
    if let Err(e) = sqlx::query(
        "UPDATE transcode_jobs
         SET status = $1, last_error = $2, worker_id = NULL, lease_expires_at = NULL
         WHERE id = $3",
    )
    .bind(status)
    .bind(&error)
    .bind(job.id)
    .execute(pool)
    .await
    {
        // Nothing left to do but log: the lease lapses and another worker re-claims it.
        eprintln!("job {}: could not record failure: {e}", job.id);
    }

    // Only for a final failure. A retryable one goes back to 'pending', which to anyone
    // watching is still "queued" — announcing it as failed would be wrong and would make a
    // tier flicker red on its way to succeeding.
    if status == "failed" {
        publish_terminal(redis, job, PROGRESS_FAILED).await;
    }
}

/// Announces a tier's final state to whoever is subscribed.
///
/// Opens its own connection rather than borrowing the ProgressReporter's: this fires once per
/// job, not once per tick, and the reporter's connection has already been dropped by the time a
/// job finishes. Best-effort like every other cache write — a listener that misses this still
/// gets the right answer from the next snapshot or list refresh.
async fn publish_terminal(client: &redis::Client, job: &Job, payload: &str) {
    let key = progress_key(job.audio_file_id, &job.target);
    let result = async {
        let mut conn = client.get_multiplexed_async_connection().await?;
        redis::AsyncCommands::publish::<_, _, ()>(&mut conn, &key, payload).await
    }
    .await;

    if let Err(e) = result {
        eprintln!("job {}: could not publish '{payload}': {e}", job.id);
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((byte_index, _)) => format!("{}…", &s[..byte_index]),
        None => s.to_owned(),
    }
}

/// Removes both scratch files if they are there, and makes sure the directory is.
async fn reset_scratch() -> std::io::Result<()> {
    fs::create_dir_all(SCRATCH_DIR).await?;
    for path in [source_path(), output_path()] {
        if let Err(e) = fs::remove_file(&path).await {
            if e.kind() != ErrorKind::NotFound {
                return Err(e);
            }
        }
    }
    Ok(())
}

/// A 4xx means the object this job exists to transcode is not there (or not ours), which no
/// retry changes. A 5xx, a timeout or a connection failure is Garage or the network.
fn classify_s3(e: reqwest::Error) -> Failure {
    match e.status() {
        Some(status) if status.is_client_error() => Failure::Terminal(format!("s3: {e}")),
        _ => Failure::Retryable(format!("s3: {e}")),
    }
}

/// Streams to disk rather than into memory: the source can be ~1GB and the pod's memory limit
/// is a fraction of that.
async fn download_source(s3: &S3Store, key: &str, path: &Path) -> Result<(), Failure> {
    let mut response = s3.get(key, None).await.map_err(classify_s3)?;

    let mut file = fs::File::create(path)
        .await
        .map_err(|e| Failure::Retryable(format!("could not create {}: {e}", path.display())))?;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| Failure::Retryable(format!("reading source: {e}")))?
    {
        file.write_all(&chunk)
            .await
            .map_err(|e| Failure::Retryable(format!("writing source: {e}")))?;
    }

    file.flush()
        .await
        .map_err(|e| Failure::Retryable(format!("flushing source: {e}")))?;
    Ok(())
}

/// Streamed off disk for the same reason the download is streamed onto it.
async fn upload_derivative(s3: &S3Store, key: &str, size: u64) -> Result<(), Failure> {
    let file = fs::File::open(output_path())
        .await
        .map_err(|e| Failure::Retryable(format!("could not reopen output: {e}")))?;
    let body = reqwest::Body::wrap_stream(tokio_util::io::ReaderStream::new(file));

    // Overwriting is safe and deliberate: the key is derived from the source's own s3_key, so
    // two workers that somehow raced this job write identical bytes to the same place.
    s3.put(key, size, M4A_CONTENT_TYPE, body)
        .await
        .map_err(classify_s3)?;
    Ok(())
}

/// The source's length in seconds, which is the denominator for every progress percentage.
///
/// `Ok(None)` means the file probed cleanly but reports no usable duration. That transcodes
/// fine; it just transcodes without a percentage, which beats failing the job over a
/// progress bar.
async fn probe_duration(path: &Path) -> Result<Option<f64>, Failure> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .await
        // A missing binary is a broken image rather than a bad file, so it stays retryable:
        // once the image is fixed, jobs with attempts left recover on their own.
        .map_err(|e| Failure::Retryable(format!("could not run ffprobe: {e}")))?;

    if !output.status.success() {
        return Err(Failure::Terminal(format!(
            "ffprobe exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|seconds| *seconds > 0.0))
}

async fn transcode(
    pool: &PgPool,
    redis: &redis::Client,
    job: &Job,
    worker_id: &str,
    bitrate: &str,
    duration: Option<f64>,
) -> Result<(), Failure> {
    // The native `aac` encoder, which Debian's packaged ffmpeg has and which is license-free;
    // libfdk_aac would mean building ffmpeg rather than installing it.
    //
    // This encoder has a hard output ceiling, and it clamps to it silently — no warning even
    // at -loglevel warning. Measured on the packaged ffmpeg 5.1.9 this image installs, stereo
    // pink noise: the cap scales with sample rate at about 5.06 kbps per kHz, so ~223kbps at
    // 44.1kHz and ~243kbps at 48kHz. Requesting more does not just fail to help, it destabilises
    // rate control — 288k and above measured *lower* at 48kHz (242k) than a 256k request (260k).
    //
    // Hence the top tier is aac_224, which is the highest value that still tracks its request at
    // both common sample rates (222k at 44.1kHz, 226k at 48kHz) and stays below the unstable
    // region. 64k and 128k land on their nominal rate exactly.
    //
    // `+faststart` moves the moov atom ahead of mdat, which is the whole point of the
    // container: a player can seek to an arbitrary offset without first reading the tail.
    let mut child = Command::new("ffmpeg")
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"])
        .args(["-progress", "pipe:1", "-nostats"])
        .arg("-i")
        .arg(source_path())
        .args(["-vn", "-c:a", "aac", "-b:a", bitrate])
        .args(["-movflags", "+faststart"])
        .arg(output_path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| Failure::Retryable(format!("could not run ffmpeg: {e}")))?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");

    // Drained concurrently, not after waiting: ffmpeg blocks once a pipe buffer fills, and a
    // worker waiting on a process that is waiting on us is a deadlock that only shows up on
    // the chattiest inputs.
    let stderr_task = tokio::spawn(async move {
        let mut buf = String::new();
        let _ = BufReader::new(stderr).read_to_string(&mut buf).await;
        buf
    });

    let mut reporter = ProgressReporter::connect(redis, job).await;
    let mut lines = BufReader::new(stdout).lines();
    let mut last_percent: Option<u8> = None;
    let mut last_renewal = Instant::now();

    loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                let Some(micros) = parse_out_time_micros(&line) else {
                    continue;
                };

                // Renewed off the progress pipe rather than a separate timer: this is already
                // proof the process is alive and making headway.
                if last_renewal.elapsed() >= LEASE_RENEW_EVERY {
                    renew_lease(pool, job, worker_id).await;
                    last_renewal = Instant::now();
                }

                let Some(duration) = duration else { continue };
                let percent =
                    ((micros as f64 / 1_000_000.0 / duration) * 100.0).clamp(0.0, 100.0) as u8;
                // Only on a change: ffmpeg ticks about twice a second, and most ticks land
                // inside the same whole percent.
                if last_percent != Some(percent) {
                    last_percent = Some(percent);
                    reporter.report(percent).await;
                }
            }
            Ok(None) => break,
            // The pipe is only a progress feed; losing it is not a reason to abandon a
            // transcode that may well be finishing fine. The exit status decides.
            Err(e) => {
                eprintln!("job {}: progress pipe read failed: {e}", job.id);
                break;
            }
        }
    }

    let status = child
        .wait()
        .await
        .map_err(|e| Failure::Retryable(format!("waiting on ffmpeg: {e}")))?;
    let stderr = stderr_task.await.unwrap_or_default();

    if !status.success() {
        // Terminal: ffmpeg rejecting this input will reject it identically next time.
        return Err(Failure::Terminal(format!(
            "ffmpeg exited {}: {}",
            status,
            stderr.trim()
        )));
    }

    Ok(())
}

/// ffmpeg's progress block carries both `out_time_us` and `out_time_ms`, and despite the name
/// **both are microseconds** — `out_time_ms` is a long-standing misnomer, verified against
/// ffmpeg n9, where a 30s input reports `out_time_ms=30000000`. Reading it as milliseconds
/// would put every percentage 1000x over.
///
/// Early ticks report `N/A`, which simply fails to parse and is skipped.
fn parse_out_time_micros(line: &str) -> Option<u64> {
    let value = line
        .strip_prefix("out_time_us=")
        .or_else(|| line.strip_prefix("out_time_ms="))?;
    value.trim().parse::<u64>().ok()
}

/// A claim is a lease, and a ~1GB source can legitimately outlive a five-minute one. Without
/// this another worker would claim the job out from under this one and redo it — idempotent,
/// because the derivative key is deterministic, but pure waste.
async fn renew_lease(pool: &PgPool, job: &Job, worker_id: &str) {
    // Guarded on still holding it: if the lease did lapse and someone else has the job, this
    // must not reach over and re-stamp theirs.
    if let Err(e) = sqlx::query(&format!(
        "UPDATE transcode_jobs SET lease_expires_at = now() + interval '{LEASE_INTERVAL}'
         WHERE id = $1 AND worker_id = $2 AND status = 'in_progress'"
    ))
    .bind(job.id)
    .bind(worker_id)
    .execute(pool)
    .await
    {
        // Not fatal: the job either finishes inside the lease it already has, or is re-claimed.
        eprintln!("job {}: lease renewal failed: {e}", job.id);
    }
}

/// Writes one tier's percentage to the cache, as both a key and a publish.
///
/// Holds one connection for the whole job rather than opening one per tick — a tick can fire
/// a hundred times. If the cache is unreachable the transcode still runs; it just reports
/// nothing, because a progress bar is never a reason to fail real work.
struct ProgressReporter {
    key: String,
    conn: Option<redis::aio::MultiplexedConnection>,
}

impl ProgressReporter {
    async fn connect(client: &redis::Client, job: &Job) -> Self {
        let key = progress_key(job.audio_file_id, &job.target);
        let conn = match client.get_multiplexed_async_connection().await {
            Ok(conn) => Some(conn),
            Err(e) => {
                eprintln!(
                    "job {}: cache unavailable, progress will not be reported: {e}",
                    job.id
                );
                None
            }
        };
        Self { key, conn }
    }

    async fn report(&mut self, percent: u8) {
        let Some(mut conn) = self.conn.take() else {
            return;
        };

        // The SET is what a polling read (list_audio, and the SSE snapshot) sees; the PUBLISH
        // is what the SSE stream forwards live. Both every tick: it is one extra round trip,
        // and it means neither side has to change if the delivery mechanism does. The TTL is
        // the cleanup story — a crashed worker's last percentage expires on its own.
        let result: Result<(), redis::RedisError> = async {
            redis::AsyncCommands::set_ex::<_, _, ()>(
                &mut conn,
                &self.key,
                percent,
                PROGRESS_TTL_SECS,
            )
            .await?;
            redis::AsyncCommands::publish::<_, _, ()>(&mut conn, &self.key, percent).await
        }
        .await;

        match result {
            Ok(()) => self.conn = Some(conn),
            // Dropped rather than retried for the rest of this job: the cache is one
            // disposable pod, and hammering it per tick while it is down helps nobody.
            Err(e) => eprintln!("progress report failed; not reporting further: {e}"),
        }
    }
}
