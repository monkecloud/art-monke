export type AudioFile = {
  id: number
  filename: string
  status: string
  created_at: string
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
