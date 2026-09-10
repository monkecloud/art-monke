// Mirrors the api's TranscodeState. One `state` field rather than a set of flags, so the UI
// renders a single switch and cannot end up painting two states at once.
export type TranscodeState = {
  target: string
  state: 'ready' | 'running' | 'pending' | 'failed'
  // Only ever set while running, and not even then until the first tick lands.
  progress: number | null
}

export type AudioFile = {
  id: number
  filename: string
  status: string
  created_at: string
  // Absent until a worker has probed the source, and on files that predate the column.
  duration_seconds: number | null
  // Always every tier the api builds, in ascending-bitrate order.
  transcodes: TranscodeState[]
}

export async function fetchAudioFiles(signal?: AbortSignal): Promise<AudioFile[]> {
  const res = await fetch('/api/audio', { signal })
  if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
  return (await res.json()) as AudioFile[]
}

export async function deleteAudioFile(id: number): Promise<void> {
  const res = await fetch(`/api/audio/${id}`, { method: 'DELETE' })
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
export function ownStreamBase(id: number): string {
  return `/api/audio/${id}`
}

export function userStreamBase(username: string, id: number): string {
  return `/api/users/${encodeURIComponent(username)}/audio/${id}`
}
