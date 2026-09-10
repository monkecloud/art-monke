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
  // Always all three tiers, in ascending-bitrate order.
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
