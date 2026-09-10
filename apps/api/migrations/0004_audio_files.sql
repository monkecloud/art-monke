-- Tracks audio files in the S3 bucket through their lifecycle. The row exists before the
-- object is fully uploaded (status 'uploading') so a client can be given an id to upload
-- against, and outlives the object being deleted (status 'deleted') so history isn't lost.
CREATE TABLE IF NOT EXISTS audio_files (
    id         BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    user_id    BIGINT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    s3_key     TEXT NOT NULL UNIQUE,
    filename   TEXT NOT NULL,
    status     TEXT NOT NULL DEFAULT 'uploading'
               CHECK (status IN ('uploading', 'uploaded', 'deleted')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS audio_files_user_id_idx ON audio_files (user_id);

-- The reaper scans for stale 'uploading' rows by status; the transcode queue will too once
-- it exists.
CREATE INDEX IF NOT EXISTS audio_files_status_idx ON audio_files (status);
