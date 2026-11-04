// Mirrors the api's TranscodeState. One `state` field rather than a set of flags, so the UI
// renders a single switch and cannot end up painting two states at once.
export type TranscodeState = {
  target: string
  state: 'ready' | 'running' | 'pending' | 'failed'
  // Only ever set while running, and not even then until the first tick lands.
  progress: number | null
}

export type AudioFile = {
  // Opaque and unordered — the api's `public_id`, not its primary key, which never leaves the
  // cluster. Nothing may infer anything from its shape or compare two of them for order; it is
  // only ever matched for equality and put in a URL. See apps/api/migrations/0010_public_id.sql.
  id: string
  filename: string
  status: string
  created_at: string
  // Absent until a worker has probed the source, and on files that predate the column.
  duration_seconds: number | null
  // Always every tier the api builds, in ascending-bitrate order.
  transcodes: TranscodeState[]
}

// The tiers a file can actually be played from, ascending because `states` is. Taken over the
// list payload's own `transcodes` on a public library and over the live overlay on your own,
// which is why it takes the states rather than the file: what is ready is a question about the
// freshest thing the caller has, and there is one answer to it either way.
export function readyTargets(states: TranscodeState[]): string[] {
  return states.filter((s) => s.state === 'ready').map((s) => s.target)
}

// The track after `currentId` in list order that can actually be played, for autoplay when one
// ends. Forward only, and no wrap: reaching the bottom of the list is the end of the listen, not
// a cue to start it over. Files with no ready tier are skipped rather than stopped at, the same
// way their rows refuse a click — one stuck transcode should not end the run.
//
// Judged off the list payload's own `transcodes`. On your own page a row's live overlay can be
// fresher than that, so a file whose transcode finished after the last fetch is skipped here
// even though its row would play — the safe direction to be wrong in.
export function nextPlayable(
  files: AudioFile[],
  currentId: string,
): { file: AudioFile; ready: string[] } | null {
  const at = files.findIndex((f) => f.id === currentId)
  if (at === -1) return null
  for (const file of files.slice(at + 1)) {
    if (file.status !== 'uploaded') continue
    const ready = readyTargets(file.transcodes)
    if (ready.length > 0) return { file, ready }
  }
  return null
}

export async function fetchAudioFiles(signal?: AbortSignal): Promise<AudioFile[]> {
  const res = await fetch('/api/audio', { signal })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return (await res.json()) as AudioFile[]
}

export async function deleteAudioFile(id: string): Promise<void> {
  const res = await fetch(`/api/audio/${encodeURIComponent(id)}`, { method: 'DELETE' })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
}

// Distinguished from a plain Error so the shared page can render "no such user" rather than
// reporting it as a fetch that went wrong.
export class NoSuchUser extends Error {}

// One account's public library. No credentials involved — the route is unauthenticated, and a
// visitor without an account here gets the same answer the owner would.
export async function fetchUserAudioFiles(
  username: string,
  signal?: AbortSignal,
): Promise<AudioFile[]> {
  const res = await fetch(`/api/users/${encodeURIComponent(username)}/audio`, { signal })
  // An account that exists but has uploaded nothing is an empty list, not a 404, so the two
  // are worth telling apart: one page says "no files yet", the other says "no such user".
  if (res.status === 404) throw new NoSuchUser(username)
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return (await res.json()) as AudioFile[]
}

// Where the player fetches a tier from, minus the `?tier=`.
//
// Two shapes, because a public library is served by its own routes rather than by the owner's:
// /api/audio/:id is scoped to the signed-in caller and 404s for someone else's file even when
// that same file is listed publicly.
export function ownStreamBase(id: string): string {
  return `/api/audio/${encodeURIComponent(id)}`
}

export function userStreamBase(username: string, id: string): string {
  return `/api/users/${encodeURIComponent(username)}/audio/${encodeURIComponent(id)}`
}
