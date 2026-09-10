import { useCallback, useEffect, useState } from 'react'
import { type AudioFile, deleteAudioFile, fetchAudioFiles } from './audioFiles'
import { FileList } from './FileList'
import { LoginPage } from './LoginPage'
import { Player } from './Player'
import { ToastStack, useToasts } from './Toasts'
import { type PendingUpload, uploadAll } from './uploadAudio'

type Session =
  | { kind: 'loading' }
  | { kind: 'anon' }
  | { kind: 'authed'; username: string }
  | { kind: 'error'; message: string }

export default function App() {
  const [session, setSession] = useState<Session>({ kind: 'loading' })
  const [files, setFiles] = useState<AudioFile[]>([])
  // The tiers are captured at the moment Play is clicked, which is live state from the row's
  // own stream rather than whatever the last list fetch happened to say.
  const [nowPlaying, setNowPlaying] = useState<{ file: AudioFile; ready: string[] } | null>(null)
  const [uploads, setUploads] = useState<PendingUpload[]>([])
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

            void uploadAll(chosen, {
              onQueued: (queued) => setUploads((u) => [...queued, ...u]),
              onProgress: (key, percent) =>
                setUploads((u) => u.map((x) => (x.key === key ? { ...x, percent } : x))),
              onSettled: (key, filename, outcome) => {
                setUploads((u) => u.filter((x) => x.key !== key))
                // A toast only for the two outcomes that leave no row behind to speak for
                // them. A successful upload needs none: its row is about to appear, already
                // showing what happens next.
                if (outcome === 'duplicate') {
                  toasts.upsert(
                    { id: key, kind: 'success', label: `${filename} — already uploaded` },
                    5000,
                  )
                } else if (outcome === 'failed') {
                  toasts.upsert(
                    { id: key, kind: 'error', label: `${filename} — failed to upload` },
                    6000,
                  )
                }
                // Per file rather than once at the end, so finished uploads appear — and
                // start showing transcode progress — while the rest are still going.
                refreshFiles()
              },
            })
          }}
        />
      </label>

      <FileList
        files={files}
        uploads={uploads}
        onPlay={(file, ready) => setNowPlaying({ file, ready })}
        onDelete={(id) => {
          deleteAudioFile(id)
            .then(() => {
              setNowPlaying((p) => (p?.file.id === id ? null : p))
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
      {nowPlaying && (
        <Player
          key={nowPlaying.file.id}
          file={nowPlaying.file}
          readyTargets={nowPlaying.ready}
        />
      )}
    </main>
  )
}
