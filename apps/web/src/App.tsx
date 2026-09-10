import { useCallback, useEffect, useRef, useState } from 'react'
import { type AudioFile, deleteAudioFile, fetchAudioFiles, ownStreamBase } from './audioFiles'
import { FileList } from './FileList'
import { LoginPage } from './LoginPage'
import { Player } from './Player'
import { sharedPath, sharedUsername } from './routes'
import { useSession } from './session'
import { SharedLibrary } from './SharedLibrary'
import { ToastStack, useToasts } from './Toasts'
import { type PendingUpload, uploadAll } from './uploadAudio'

export default function App() {
  const [session, setSession] = useSession()

  // Read during render rather than held in state: it only changes on a navigation, and every
  // navigation here is a real one that reloads the page.
  const shared = sharedUsername(window.location.pathname)

  // Checked ahead of every session branch below. A public library is readable without an
  // account, so neither the login page nor the "could not reach the api" screen has any
  // business standing in front of one — the session is still resolved, but only to decide
  // what the corner of the page offers.
  if (shared !== null) {
    return <SharedLibrary username={shared} session={session} />
  }

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
    <SignedInApp username={session.username} onSignedOut={() => setSession({ kind: 'anon' })} />
  )
}

// Everything here belongs to one signed-in account, so it lives below the session rather than
// beside it: logging out unmounts this component and React throws the lot away — the file
// list, the uploads, the toasts, and the playing track. Held in App instead, it would survive
// the logout, and the next sign-in would remount the Player on the previous account's track
// and start it playing again.
function SignedInApp({ username, onSignedOut }: { username: string; onSignedOut: () => void }) {
  const [files, setFiles] = useState<AudioFile[]>([])
  // The tiers are captured at the moment a row is clicked, which is live state from that
  // row's own stream rather than whatever the last list fetch happened to say.
  const [nowPlaying, setNowPlaying] = useState<{ file: AudioFile; ready: string[] } | null>(null)
  const [uploads, setUploads] = useState<PendingUpload[]>([])
  // The Player's <audio>, handed back up so a click on the row that is already playing can
  // toggle it. Held here rather than in the Player because the file list is what needs it.
  const audioRef = useRef<HTMLAudioElement | null>(null)
  const toasts = useToasts()

  const refreshFiles = useCallback(() => {
    fetchAudioFiles()
      .then(setFiles)
      .catch(() => {})
  }, [])

  useEffect(() => {
    const ac = new AbortController()
    fetchAudioFiles(ac.signal)
      .then(setFiles)
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === 'AbortError') return
      })
    return () => ac.abort()
  }, [])

  return (
    <main className="library">
      <header className="topbar">
        <span className="muted">Signed in as {username}</span>
        <div className="topbar-actions">
          {/* A link to your own public page rather than a copy-to-clipboard button: following
              it shows you exactly what a visitor sees, which is worth more than the URL on its
              own when what you are about to hand out is a public link. The address bar is then
              the thing to copy. */}
          <a className="link" href={sharedPath(username)}>
            Share
          </a>
          <button
            onClick={() => {
              // Fire-and-forget: the cookie is cleared client-side regardless of whether
              // the request lands, so a flaky connection can't strand the user in a signed-in
              // UI that no longer has a working session.
              fetch('/api/auth/logout', { method: 'POST' }).catch(() => {})
              onSignedOut()
            }}
          >
            Log out
          </button>
        </div>
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
        playingId={nowPlaying?.file.id ?? null}
        live
        emptyMessage="No files uploaded yet."
        onPlay={(file, ready) => {
          // A second click on the track already loaded is a pause, not a restart — the same
          // thing the bar's own button and the spacebar do.
          if (nowPlaying?.file.id === file.id) {
            const el = audioRef.current
            if (!el) return
            if (el.paused) void el.play()
            else el.pause()
            return
          }
          setNowPlaying({ file, ready })
        }}
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
          uploader={username}
          streamBase={ownStreamBase(nowPlaying.file.id)}
          readyTargets={nowPlaying.ready}
          audioRef={audioRef}
        />
      )}
    </main>
  )
}
