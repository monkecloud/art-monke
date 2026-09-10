//! Finishes what `delete_audio` starts.
//!
//! The api marks a file `delete_pending` and returns without touching Garage, because a
//! DELETE from there could never be atomic with what a worker is doing. This is what actually
//! converges the bucket: it removes the file's queue rows, deletes all four objects it can
//! have, and moves the row to `deleted`.
//!
//! Runs from the worker's claim loop on every replica at once. That is safe because every
//! step is idempotent -- S3 DELETE succeeds on a key that was never written, and the
//! finalizing UPDATE is guarded on the status it expects -- so two sweepers landing on the
//! same file duplicate work but cannot corrupt it. No `SKIP LOCKED` here for that reason:
//! taking a row lock would mean holding a transaction open across several calls to Garage,
//! which is a worse trade than occasionally deleting an absent key twice.
//!
//! See docs/delete-lifecycle.md for the full design and the failure cases it is built around.

use monke_common::targets::{derivative_key, TARGETS};
use monke_common::S3Store;
use sqlx::postgres::PgPool;

use crate::MAX_ATTEMPTS;

/// How long a lapsed lease has to stay lapsed before a job that nothing will ever re-claim is
/// treated as dead. See `clear_resolved_jobs` for why only unclaimable rows qualify.
///
/// Generous on purpose: this is a backstop on a path nobody is waiting on, so a short value
/// buys no speed, while a long one keeps the in-progress barrier meaningful. `LEASE_INTERVAL`
/// is 5 minutes and a healthy worker re-stamps every 60s, so an hour is many times any
/// realistic stall.
const STRANDED_AFTER: &str = "1 hour";

/// Files finalized per pass. A cap rather than the whole backlog so one pass cannot occupy a
/// worker for minutes on end -- the sweep shares the claim loop with transcoding.
const BATCH: i64 = 32;

/// One pass. Errors are logged and swallowed: Postgres or Garage being briefly unreachable is
/// not worth propagating when the next pass simply redoes the work.
pub async fn sweep_once(pool: &PgPool, s3: &S3Store) {
    if let Err(e) = clear_resolved_jobs(pool).await {
        eprintln!("sweep: clearing resolved jobs failed: {e}");
        return;
    }

    let files = match claim_free_files(pool).await {
        Ok(files) => files,
        Err(e) => {
            eprintln!("sweep: selecting delete_pending files failed: {e}");
            return;
        }
    };

    for (id, s3_key) in files {
        if let Err(e) = finalize(pool, s3, id, &s3_key).await {
            eprintln!("sweep: finalizing file {id} failed: {e}");
        }
    }
}

/// Removes every queue row for a deleting file that a worker cannot still be holding.
///
/// `in_progress` is the barrier the whole design rests on, so it is deliberately the one
/// status not cleared here: a worker holds its row at `in_progress` for the whole of
/// `run_job`, *including* the PUT, which makes row presence a truer signal than the lease --
/// a lease can lapse while the worker is alive and about to write. Clearing them wholesale
/// would make the next pass see an empty queue and wrongly conclude nothing is running.
///
/// `pending` rows have to go, or a worker would claim one and start work on a file that is
/// being deleted. `done` and `failed` are inert but keep the file off the barrier check.
///
/// The second arm is the one case `in_progress` never resolves on its own. Normally a lapsed
/// lease is enough: another worker re-claims the row, hits the `delete_pending` status check
/// before the download, fails Terminal, and the row lands on `failed` for the next pass. But
/// `claim_job` only considers rows under `MAX_ATTEMPTS`, so a row sitting at the cap whose
/// worker died is never re-claimed and never leaves `in_progress` -- the file would block
/// forever and its objects would never be cleaned. Scoped to exactly that predicate, so the
/// common path stays purely barrier-driven.
async fn clear_resolved_jobs(pool: &PgPool) -> Result<(), sqlx::Error> {
    let cleared = sqlx::query(&format!(
        "DELETE FROM transcode_jobs j
         USING audio_files f
         WHERE j.audio_file_id = f.id
           AND f.status = 'delete_pending'
           AND (j.status <> 'in_progress'
                OR (j.attempts >= $1
                    AND j.lease_expires_at < now() - interval '{STRANDED_AFTER}'))"
    ))
    .bind(MAX_ATTEMPTS)
    .execute(pool)
    .await?;

    if cleared.rows_affected() > 0 {
        eprintln!("sweep: cleared {} queue rows", cleared.rows_affected());
    }
    Ok(())
}

/// Deleting files that no worker can still be holding a job for.
///
/// `NOT EXISTS` over the whole queue rather than a test for `in_progress` specifically:
/// `clear_resolved_jobs` has just removed everything else, so any row left is one a worker may
/// still write against, and "the queue holds no reference to this file" is the plainer way to
/// say it. A file with a row left is simply skipped and picked up by a later pass.
async fn claim_free_files(pool: &PgPool) -> Result<Vec<(i64, String)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT f.id, f.s3_key
         FROM audio_files f
         WHERE f.status = 'delete_pending'
           AND NOT EXISTS (
               SELECT 1 FROM transcode_jobs j WHERE j.audio_file_id = f.id
           )
         ORDER BY f.id
         LIMIT $1",
    )
    .bind(BATCH)
    .fetch_all(pool)
    .await
}

/// Deletes the file's objects, then marks the row `deleted`.
///
/// All four keys unconditionally, whether they were ever written or not. The derivatives live
/// at keys derived from `s3_key` by `derivative_key` and are recorded nowhere, so there is
/// nothing to consult about how far a transcode got before it died -- which is exactly what
/// makes a worker dying at an arbitrary point safe rather than needing a case per point.
///
/// The status write happens last and only if every delete succeeded. Finalizing first would
/// leave a row nothing revisits if Garage were down for the deletes; leaving it
/// `delete_pending` means the next pass simply tries again.
async fn finalize(pool: &PgPool, s3: &S3Store, id: i64, s3_key: &str) -> Result<(), sqlx::Error> {
    let mut keys = vec![s3_key.to_string()];
    keys.extend(TARGETS.iter().map(|t| derivative_key(s3_key, t)));

    for key in &keys {
        if let Err(e) = s3.delete(key).await {
            eprintln!("sweep: file {id}: delete of {key} failed: {e}");
            return Ok(());
        }
    }

    // Guarded on the status it expects, so a second sweeper that raced through the same file
    // cannot rewrite a `deleted_at` this one already set.
    let finalized = sqlx::query(
        "UPDATE audio_files SET status = 'deleted', deleted_at = now()
         WHERE id = $1 AND status = 'delete_pending'",
    )
    .bind(id)
    .execute(pool)
    .await?;

    if finalized.rows_affected() > 0 {
        eprintln!("sweep: file {id} deleted ({} objects removed)", keys.len());
    }
    Ok(())
}
