-- Every uploaded file also gets transcoded into three AAC/.m4a bitrate tiers. The work is
-- done by apps/worker, out of band, so an upload still completes as soon as the original
-- object has landed.
--
-- AAC in a faststart MP4 container rather than MP3: the container carries a real
-- byte-offset index, which is what lets download_audio's Range proxying serve an accurate
-- seek to an arbitrary offset without shipping the whole file first.

-- One flag per tier rather than a single "transcoded" boolean, so a player can offer the
-- tiers that are ready instead of waiting for all three.
--
-- 64/128/224 rather than a longer ladder: 224k is measured to be the ceiling of ffmpeg's
-- native `aac` encoder, so a tier named for anything above it would not have delivered it.
ALTER TABLE audio_files
    ADD COLUMN IF NOT EXISTS has_aac_64  BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS has_aac_128 BOOLEAN NOT NULL DEFAULT false,
    ADD COLUMN IF NOT EXISTS has_aac_224 BOOLEAN NOT NULL DEFAULT false;

-- 'delete_pending' replaces the row-delete the api used to do when a PUT to Garage failed.
-- Dropping the row lost the s3_key, and with it the only handle on an object that may in
-- fact have landed (a lost response looks exactly like a failed write). The row now stays,
-- and finalizing it is a reaper's job.
--
-- The constraint name is Postgres' auto-generated one, confirmed against the live schema
-- rather than assumed; DROP ... IF EXISTS keeps this migration safe on a database where
-- it somehow differs.
ALTER TABLE audio_files DROP CONSTRAINT IF EXISTS audio_files_status_check;
ALTER TABLE audio_files ADD CONSTRAINT audio_files_status_check
    CHECK (status IN ('uploading', 'uploaded', 'delete_pending', 'deleted'));

-- The transcode queue. Postgres rather than Redis because this is durable work: the cache
-- is one pod on one node and is gone for good if that node is lost, and a queue that
-- evaporates would silently leave files with no derivatives and nothing to notice it.
--
-- One row per (file, target) so each tier succeeds, fails and retries on its own — a
-- single row for all three would redo finished work on every retry.
CREATE TABLE IF NOT EXISTS transcode_jobs (
    id               BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    audio_file_id    BIGINT NOT NULL REFERENCES audio_files(id) ON DELETE CASCADE,
    target           TEXT NOT NULL
                     CHECK (target IN ('aac_64', 'aac_128', 'aac_224')),
    status           TEXT NOT NULL DEFAULT 'pending'
                     CHECK (status IN ('pending', 'in_progress', 'done', 'failed')),
    -- Incremented as the job is claimed, not as it fails, so a worker that dies without
    -- ever writing back still burns an attempt instead of being retried forever.
    attempts         INT NOT NULL DEFAULT 0,
    worker_id        TEXT,
    -- A claim is a lease, not a lock: a worker that dies holding a job leaves the lease to
    -- expire, and the next claim picks it up. Renewed while the job runs.
    lease_expires_at TIMESTAMPTZ,
    last_error       TEXT,
    created_at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (audio_file_id, target)
);

-- Exactly the shape of the claim query's WHERE: pending rows, plus in_progress rows whose
-- lease has lapsed.
CREATE INDEX IF NOT EXISTS transcode_jobs_claim_idx
    ON transcode_jobs (status, lease_expires_at);

-- list_audio looks up every job for one file.
CREATE INDEX IF NOT EXISTS transcode_jobs_audio_file_id_idx
    ON transcode_jobs (audio_file_id);
