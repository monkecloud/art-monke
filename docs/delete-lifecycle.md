# Deleting an audio file

Implemented in `delete_audio` (`apps/api/src/main.rs`), `apps/worker/src/sweep.rs`, the
orphan-cleanup branch of `run_job` (`apps/worker/src/main.rs`), and
`apps/api/migrations/0008_deleted_at.sql`.

## The constraint everything follows from

A worker's PUT to Garage cannot be made atomic with its status check against Postgres.
`run_job` reads `audio_files.status` at the top, then downloads, probes and transcodes for
minutes, and only then PUTs the derivative. Any status check can be overtaken by a PUT that is
already in flight.

So delete does not prevent the write. It means **the delete is not finished until cleanup has
confirmed the bucket is clean.** Everything below is downstream of that.

## The four objects

A file has at most four objects, and only one is recorded anywhere:

- the source, at `audio_files.s3_key`
- three derivatives, at `derivative_key(s3_key, target)` for each of `TARGETS`
  (`apps/common/src/targets.rs`)

Derivative keys are a **pure function** of `s3_key`; nothing stores them. So cleanup never has
to know what a worker actually managed to write — it deletes all four unconditionally. That is
what makes a worker dying at an arbitrary point safe, rather than needing a case per point.

It is also why the row outlives its objects: `s3_key` is the only handle on all four.

**`s3_key` must stay unique per upload.** It is `{user_id}/{32 random bytes}`, so deleting a
file and re-uploading the identical bytes produces an unrelated key and the sweep for the old
row cannot reach the new file's objects. Making the key deterministic -- content-addressing it
on `content_hash`, say -- would break that: the two rows would share all four keys, and
finalizing the deleted one would delete the live one's objects.

## Lifecycle

```
click delete → audio_files.status = 'delete_pending'   (synchronous, in the API)
sweep pass   → queue rows cleared, four objects deleted, status = 'deleted' + deleted_at
             → row kept indefinitely
```

### 1. The API marks intent and does no S3

`delete_audio` sets `status = 'delete_pending'`, guarded on `status IN ('uploading','uploaded')`
so a second click on a row already sweeping is a no-op, and returns 204. One Postgres write
that cannot half-fail, so Garage being unreachable never makes the button fail on a user who
has asked for the file gone. The file leaves `list_audio` immediately, which already filters
that status out.

### 2. The worker cleans up after itself

`run_job`'s final transaction guards the flag update on `status = 'uploaded'`. In the
`rows_affected() == 0` branch a delete landed while the job ran, and the worker is holding the
exact key it wrote — so it deletes that object after the commit, and returns without publishing
`PROGRESS_READY` (no flag was set, so no tier is ready).

Best-effort: the sweep deletes the same key anyway. This collapses the common race from a pass
to a few seconds.

### 3. The sweep

`sweep_once`, run from the worker's claim loop on every replica every `SWEEP_EVERY` (60s):

1. **`clear_resolved_jobs`** — delete the file's `transcode_jobs` rows that are not
   `in_progress`. `pending` would otherwise be claimed and start work on a deleting file;
   `done`/`failed` are inert but keep the file off the barrier check.
2. **`claim_free_files`** — select `delete_pending` rows with no queue rows left at all. After
   step 1 the only rows that can remain are `in_progress`, so this is the barrier.
3. **`finalize`** — delete all four keys, then set `deleted` + `deleted_at`, guarded on
   `delete_pending`. The status write happens **last, and only if every delete succeeded**;
   otherwise the row stays `delete_pending` and the next pass retries.

No `SKIP LOCKED`. Every step is idempotent — S3 DELETE succeeds on a key never written, and the
finalizing UPDATE is guarded — so two replicas landing on one file duplicate work but cannot
corrupt it. Locking would mean holding a transaction open across several calls to Garage, a
worse trade than occasionally deleting an absent key twice.

## Why `in_progress` is the barrier

A worker holds its `transcode_jobs` row at `in_progress` across the whole of `run_job`,
**including the PUT**. The lease is not a substitute: it can lapse while the worker is alive and
about to write.

`in_progress` means *claimed and unresolved*, not *actively transcoding* — `claim_job` sets it
before any work starts, and only the final transaction or `record_failure` clears it, so a dead
worker leaves it set indefinitely. That is fine, because the barrier is **presence, not
liveness**. Nothing has to guess whether a worker is alive, which matters because dead and slow
look identical from Postgres.

Two conditions make it hold, both enforced in `clear_resolved_jobs`:

- **Live `in_progress` rows are never deleted.** Clearing the queue wholesale would make the
  next pass see an empty queue and conclude nothing is running.
- **`in_progress` always eventually resolves.** The lease lapses, another worker re-claims, hits
  `delete_pending` at the status check (before the download, so it is cheap), fails Terminal,
  and the row lands on `failed` for the next pass.

One case does not resolve on its own: `claim_job` only considers rows under `MAX_ATTEMPTS` (8),
so a row at the cap whose worker died is never re-claimed and never leaves `in_progress`, and
the file would block forever. `clear_resolved_jobs` treats exactly that predicate —
`attempts >= MAX_ATTEMPTS` **and** a lease lapsed longer than `STRANDED_AFTER` (1 hour) — as
dead. Narrow, so the common path stays purely barrier-driven; generous, because it is a
backstop on a path nobody waits on.

## Why rows are kept indefinitely

Not for history. The row is a tombstone holding `s3_key`: a worker that is partitioned, paused
or draining can still PUT a derivative after the file is fully finalized, and the row is the
only thing that knows the key to remove.

Any deadline on the row is a deadline on that handle — a late write past it would leak with
nothing pointing at it. Rows are small; keeping them means a stray object is always
reconcilable.

Note `0004_audio_files.sql` gives a different reason for the row surviving ("so history isn't
lost"). `0008` supersedes it.

## The invariant that keeps re-upload safe

Every existence check against `audio_files` is scoped to `status = 'uploaded'` -- the duplicate
lookup behind the 409, both `download_audio` queries, the SSE snapshot, and the partial unique
index backing duplicate detection. `list_audio` excludes `deleted` and `delete_pending`
explicitly.

Nothing else reads or writes a row that is not `uploaded`, apart from the status transitions
themselves. Keep it that way: a check that can see a tombstone would block a re-upload, or
answer a 409 with the id of a file the user cannot see.

## Failure scenarios

| failure | what happens |
|---|---|
| worker dies mid-transcode, before the PUT | job stays `in_progress`; sweep skips the file; lease lapses, re-claim hits `delete_pending`, fails Terminal → `failed`; next pass clears it, queue empties, cleanup runs |
| worker dies right after the PUT | same path — the sweep deletes all four keys regardless, so the orphan goes with them |
| pod rolled out mid-job | SIGTERM handler finishes the job in hand; PUT lands, flag guard returns 0 rows, worker deletes its own object, job → `done`. If the 180s grace cuts it off, degenerates to one of the rows above |
| sweep dies mid-sweep | next pass redoes it; every step is idempotent and the status write is last |
| Garage down during a pass | deletes fail, row stays `delete_pending`, next pass retries |
| node lost entirely | pod is gone, no further writes possible; resolves like row one |
| delete arrives mid-transcode | job is `in_progress`, sweep waits; worker finishes, self-deletes its object, sweep cleans up |

## Known residual

A worker that can reach Garage but **not** Postgres writes its object and cannot run its own
cleanup, since that needs Postgres. Nothing notices until something looks again. The row keeps
the key recoverable, so this is a leak until reconciled rather than a permanent one. There is
no reconciliation pass today.

## Failed uploads use the same path

`delete_pending` is also what a failed PUT leaves behind (see `0006_content_hash.sql`). Those
rows never have queue rows, because `transcode_jobs` are only inserted once the
`uploading → uploaded` flip succeeds, so the sweep finalizes them directly.
