import { useCallback, useEffect, useState } from 'react'
import { type AudioFile, deleteAudioFile, fetchAudioFiles } from './audioFiles'
import { FileList } from './FileList'
import { LoginPage } from './LoginPage'
import { Player } from './Player'
import { ToastStack, useToasts } from './Toasts'
import { uploadAll } from './uploadAudio'

type Session =
  | { kind: 'loading' }
  | { kind: 'anon' }
  | { kind: 'authed'; username: string }
  | { kind: 'error'; message: string }

export default function App() {
  const [session, setSession] = useState<Session>({ kind: 'loading' })
  const [files, setFiles] = useState<AudioFile[]>([])
  const [nowPlaying, setNowPlaying] = useState<AudioFile | null>(null)
  const toasts = useToasts()

  const refreshFiles = useCallback(() => {
    fetchAudioFiles()
      .then(setFiles)
      .catch(() => {})
  }, [])

  useEffect(() => {
    if (session.kind !== 'authed') return
    const ac = new AbortController()
    fetchAudioFiles(ac.signal)
      .then(setFiles)
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === 'AbortError') return
      })
    return () => ac.abort()
  }, [session.kind])

  useEffect(() => {
    const ac = new AbortController()

    fetch('/api/auth/me', { signal: ac.signal })
      .then(async (res) => {
        if (res.status === 401) {
          setSession({ kind: 'anon' })
          return
        }
        if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
        const { username } = (await res.json()) as { username: string }
        setSession({ kind: 'authed', username })
      })
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === 'AbortError') return
        setSession({ kind: 'error', message: err instanceof Error ? err.message : String(err) })
      })

    return () => ac.abort()
  }, [])

  if (session.kind === 'loading') {
    return (
      <main>
        <p className="muted">Loading…</p>
      </main>
    )
  }

  if (session.kind === 'error') {
    return (
      <main>
        <p className="error">Could not reach the api: {session.message}</p>
      </main>
    )
  }

  if (session.kind === 'anon') {
    return <LoginPage onSignedIn={(username) => setSession({ kind: 'authed', username })} />
  }

  return (
    <main>
      <header className="topbar">
        <span className="muted">Signed in as {session.username}</span>
        <button
          onClick={() => {
            // Fire-and-forget: the cookie is cleared client-side regardless of whether the
            // request lands, so a flaky connection can't strand the user in a signed-in UI
            // that no longer has a working session.
            fetch('/api/auth/logout', { method: 'POST' }).catch(() => {})
            setSession({ kind: 'anon' })
          }}
        >
          Log out
        </button>
      </header>

      <h1>monke-app</h1>

      <label className="upload-button">
        Upload audio
        <input
          type="file"
          accept="audio/*"
          multiple
          hidden
          onChange={(e) => {
            const chosen = Array.from(e.target.files ?? [])
            // Cleared immediately so picking the same files again still fires a change event.
            e.target.value = ''
            if (chosen.length === 0) return

            // Stand in for the real rows until refreshFiles() replaces them: the server
            // already has an 'uploading' row for each at this point, but the client has no
            // way to know the ids until each request settles. Negative ids so they cannot
            // collide with a real one, and distinct so React keys stay unique across a batch.
            setFiles((fs) => [
              ...chosen.map((file, i) => ({
                id: -Date.now() - i,
                filename: file.name,
                status: 'uploading',
                created_at: '',
                transcodes: [],
              })),
              ...fs,
            ])
            // Refreshes as each file settles rather than only at the end, so finished uploads
            // appear — and start showing transcode progress — while the rest are still going.
            void uploadAll(chosen, toasts, refreshFiles)
          }}
        />
      </label>

      <FileList
        files={files}
        onPlay={setNowPlaying}
        onDelete={(id) => {
          deleteAudioFile(id)
            .then(() => {
              setNowPlaying((f) => (f?.id === id ? null : f))
              refreshFiles()
            })
            .catch(() => {
              toasts.upsert(
                { id: `delete-${id}`, kind: 'error', label: 'Failed to delete file' },
                6000,
              )
            })
        }}
      />

      <ToastStack toasts={toasts.toasts} onDismiss={toasts.dismiss} />
      {nowPlaying && <Player key={nowPlaying.id} file={nowPlaying} />}
    </main>
  )
}
