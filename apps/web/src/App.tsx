import { useCallback, useEffect, useState } from 'react'
import { type AudioFile, deleteAudioFile, fetchAudioFiles } from './audioFiles'
import { FileList } from './FileList'
import { LoginPage } from './LoginPage'
import { ToastStack, useToasts } from './Toasts'
import { uploadAudio } from './uploadAudio'

type Session =
  | { kind: 'loading' }
  | { kind: 'anon' }
  | { kind: 'authed'; username: string }
  | { kind: 'error'; message: string }

export default function App() {
  const [session, setSession] = useState<Session>({ kind: 'loading' })
  const [files, setFiles] = useState<AudioFile[]>([])
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
          hidden
          onChange={(e) => {
            const file = e.target.files?.[0]
            e.target.value = ''
            if (!file) return

            // Stands in for the real row until refreshFiles() replaces it: the server
            // already has an 'uploading' row for this at this point, but the client has no
            // way to know its id until the whole request settles.
            setFiles((fs) => [
              { id: -Date.now(), filename: file.name, status: 'uploading', created_at: '' },
              ...fs,
            ])
            uploadAudio(file, toasts, refreshFiles)
          }}
        />
      </label>

      <FileList
        files={files}
        onDelete={(id) => {
          deleteAudioFile(id)
            .then(refreshFiles)
            .catch(() => {
              toasts.upsert(
                { id: `delete-${id}`, kind: 'error', label: 'Failed to delete file' },
                6000,
              )
            })
        }}
      />

      <ToastStack toasts={toasts.toasts} onDismiss={toasts.dismiss} />
    </main>
  )
}
