-- Duplicate detection. A duplicate is the same user uploading the same filename with the
-- same bytes; either one differing on its own is a distinct file, because a renamed copy is
-- a deliberate act and two different recordings can share a name.
--
-- Nullable, and no backfill: rows that predate this column have no hash and so cannot be
-- compared. Hashing them would mean re-reading every object out of the bucket for a
-- convenience feature, and the partial index below simply excludes them.
ALTER TABLE audio_files ADD COLUMN IF NOT EXISTS content_hash TEXT;

-- Hex SHA-256 of the object's bytes, computed by the api as it streams the upload through to
-- Garage — never supplied by the client, which could not be trusted about it and would have
-- to buffer the whole file to work it out anyway.
--
-- A UNIQUE index rather than a SELECT-then-insert check, because the check and the write
-- cannot be made atomic from the api: two identical uploads racing would both find nothing
-- and both commit. Here the loser gets a constraint violation, which upload_audio turns into
-- a 409 naming the file it duplicated.
--
-- Partial, on three counts: only 'uploaded' rows are live (a deleted file must not block
-- re-uploading it later), only rows that actually have a hash can participate, and an
-- 'uploading' row has no hash yet so it cannot collide mid-flight.
CREATE UNIQUE INDEX IF NOT EXISTS audio_files_user_filename_hash_key
    ON audio_files (user_id, filename, content_hash)
    WHERE status = 'uploaded' AND content_hash IS NOT NULL;
