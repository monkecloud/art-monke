-- The 64k and 128k tiers are retired; TARGETS is just aac_224 now.
--
-- The player defaults to the highest ready tier and nothing selects a lower one
-- automatically, so the two lower tiers were each costing a full source download and a full
-- decode -- one transcode_jobs row per target, claimed independently -- for something almost
-- nothing ever asked for. Sources are kept, so this is reversible: re-adding a tier is a
-- change to monke_common::targets plus a backfill of job rows, with no schema change needed.
--
-- Their objects are deleted out of the bucket by hand, once, as part of shipping this. Not
-- from code: a cleanup that only ever runs once does not earn a permanent path through the
-- worker plus a second commit to take it out again. See the retirement note in
-- apps/common/src/targets.rs for why nothing else can delete those keys afterwards.

-- Queue rows for the retired targets, so no worker spends a claim on work whose output
-- nothing would serve. A new binary rejects one anyway -- target_bitrate only knows TARGETS
-- -- but as a terminal failure that leaves a 'failed' row behind, which is noise rather than
-- information.
--
-- Includes 'in_progress' rows. A worker still holding one finds its final UPDATE matches
-- nothing, which is already how it handles a file deleted mid-job: the transaction commits
-- and the object it wrote is swept with the file.
--
-- During a rolling deploy an api pod on the old build can still insert a retired target after
-- this runs. That row fails terminally on its first claim and is cleaned up with the file. It
-- is not worth tightening the target CHECK constraint to prevent: an old api pod would then
-- fail the whole INSERT and 500 an upload that had already streamed its bytes to Garage, and
-- a permissive constraint is what keeps re-adding a tier a code-only change.
DELETE FROM transcode_jobs WHERE target IN ('aac_64', 'aac_128');

-- The flags now describe objects that are gone. Nothing reads them -- target_column no longer
-- maps those tiers, so download_audio 404s them, and list_audio reports one state per TARGETS
-- entry -- but a column claiming a derivative that does not exist is a trap for the next
-- person to read the schema.
--
-- The columns themselves stay for now. Dropping one that older api pods are still selecting
-- mid-rollout would 500 every list_audio until they were replaced; they can go in a later
-- migration alongside the TranscodeRow fields that read them.
UPDATE audio_files SET has_aac_64 = false, has_aac_128 = false
 WHERE has_aac_64 OR has_aac_128;
