-- The identifier a client is given for a file, replacing `id` on every route.
--
-- `id` is `GENERATED ALWAYS AS IDENTITY`, so it says out loud both how many files exist and in
-- what order they were uploaded. That was invisible while it only ever travelled in a fetch,
-- but a track is now linkable at /u/<username>/t/<id>, and a URL is the one identifier people
-- read, keep and send to each other. `/u/yarn/t/13` reads as "the 13th track", which is a
-- claim about the library that nobody asked to publish.
--
-- Unguessability is not the point and is not relied on anywhere: these libraries are public
-- and fully listable, so the id guards nothing that the list does not already hand out. Every
-- route still scopes its lookup by the owning account exactly as before -- the id changing
-- shape does not change what any of them will serve. What this buys is only that the
-- identifier stops carrying information it was never meant to carry.
--
-- 16 hex characters: 64 bits, far past any collision worth planning for at one row per
-- uploaded file, and the unique index below makes a collision a failed insert rather than two
-- files quietly sharing an address. Hex rather than something denser, because this gets read
-- aloud, retyped and pasted through chat clients — an alphabet with no case and no lookalike
-- characters is worth the extra few characters.
--
-- md5 over a random UUID rather than a slice of the UUID itself, which is the obvious way to
-- write this and is wrong: a v4 UUID is not uniformly random across its digits. It spends a
-- nibble on its version and two bits on its variant, so `left(uuid, 16)` would be 60 random
-- bits with a literal '4' sitting in the thirteenth character of every id in the table. The
-- md5 is a mixer, not a security measure — the randomness is `gen_random_uuid()`'s, and all
-- this does is spread it evenly so every digit of the prefix carries its share. Both functions
-- are core, so this needs no extension.
--
-- Added WITH the default rather than backfilled afterwards: the default is volatile, which
-- makes Postgres rewrite the table and evaluate it once per row, so every existing file comes
-- out of this one statement with its own distinct id.
ALTER TABLE audio_files
    ADD COLUMN IF NOT EXISTS public_id TEXT NOT NULL
    DEFAULT left(md5(gen_random_uuid()::text), 16);

-- Every lookup that used to be by primary key is now by this, so it is an index rather than
-- just a constraint. UNIQUE across the whole table, not per user: a public_id is the whole
-- address of a file on the owner's routes, where no username narrows it first.
CREATE UNIQUE INDEX IF NOT EXISTS audio_files_public_id_key ON audio_files (public_id);

-- Deliberately not applied to `transcode_jobs.audio_file_id`, the Redis progress keys, or
-- anything else internal. Those never face a client, `id` remains the primary key every
-- foreign key points at, and rewriting them would be churn with nothing on the other side of
-- it. This column exists for the wire and nowhere else.
