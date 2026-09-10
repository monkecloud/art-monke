import { useEffect, useRef, useState } from 'react'
import { type AudioFile, NoSuchUser, fetchUserAudioFiles, userStreamBase } from './audioFiles'
import { FileList } from './FileList'
import { Player } from './Player'
import type { Session } from './session'

type Library =
  | { kind: 'loading' }
  // An account that exists. `files` is empty for one that has uploaded nothing, which is a
  // different page from the one below.
  | { kind: 'ready'; files: AudioFile[] }
  | { kind: 'missing' }
  | { kind: 'error'; message: string }

// Back to your own files, or an invitation if you have none yet. A real link and a real
// navigation rather than client-side routing: there are two pages, and a page load between
// them also stops whatever is playing, which is the right thing when leaving a library.
//
// Nothing at all while the session is still resolving, so the corner does not flicker from
// "Log in" to "Your files" a moment after the page paints.
function SharedNav({ session }: { session: Session }) {
  if (session.kind === 'authed') {
    return (
      <a className="link" href="/">
        Your files
      </a>
    )
  }
  if (session.kind === 'anon') {
    return (
      <a className="link" href="/">
        Log in
      </a>
    )
  }
  return null
}

// One account's public library, reached by link at /u/<username>.
//
// Read-only, and not by hiding controls that would otherwise work: the api serves this list
// from routes that have no delete and no upload, so there is nothing here for the page to
// withhold. The only thing it deliberately leaves out is the per-row transcode stream, which
// is scoped to the signed-in caller's own files and would 404 for these.
export function SharedLibrary({ username, session }: { username: string; session: Session }) {
  const [library, setLibrary] = useState<Library>({ kind: 'loading' })
  // Same shape as the owner's page: the tiers are captured when the row is clicked.
  const [nowPlaying, setNowPlaying] = useState<{ file: AudioFile; ready: string[] } | null>(null)
  const audioRef = useRef<HTMLAudioElement | null>(null)

  useEffect(() => {
    const ac = new AbortController()

    fetchUserAudioFiles(username, ac.signal)
      .then((files) => setLibrary({ kind: 'ready', files }))
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === 'AbortError') return
        if (err instanceof NoSuchUser) {
          setLibrary({ kind: 'missing' })
          return
        }
        setLibrary({ kind: 'error', message: err instanceof Error ? err.message : String(err) })
      })

    return () => ac.abort()
  }, [username])

  // So a shared link is identifiable in a tab strip, in history and in a bookmark, all of
  // which are places this URL is likely to end up. Restored on unmount for the same reason the
  // media-session card is torn down: the title should not outlive the page that set it.
  useEffect(() => {
    const previous = document.title
    document.title = `${username} | art-monke`
    return () => {
      document.title = previous
    }
  }, [username])

  // The owner looking at their own link. Worth saying out loud, because the page is otherwise
  // indistinguishable from a private one and this is exactly where someone checks what they
  // are about to send round.
  const isOwner = session.kind === 'authed' && session.username === username

  return (
    <main>
      <header className="topbar">
        {/* Not "<name>'s files" when there is no such account: a heading that names a library
            over a body saying nobody owns one reads as a page that failed to load rather than
            as an answer. */}
        <span className="muted">
          {library.kind === 'missing' ? 'Not found' : `${username}'s files`}
        </span>
        <SharedNav session={session} />
      </header>

      {isOwner && (
        <p className="notice">
          This is your public link. Anyone who has it can play these tracks, with or without an
          account here.
        </p>
      )}

      {library.kind === 'loading' && <p className="muted">Loading…</p>}

      {library.kind === 'missing' && <p className="muted">Nobody here goes by “{username}”.</p>}

      {library.kind === 'error' && (
        <p className="error">Could not load these files: {library.message}</p>
      )}

      {library.kind === 'ready' && (
        <FileList
          files={library.files}
          uploads={[]}
          playingId={nowPlaying?.file.id ?? null}
          live={false}
          emptyMessage={`${username} hasn’t uploaded anything yet.`}
          onPlay={(file, ready) => {
            // A second click on the loaded track is a pause, same as on your own page.
            if (nowPlaying?.file.id === file.id) {
              const el = audioRef.current
              if (!el) return
              if (el.paused) void el.play()
              else el.pause()
              return
            }
            setNowPlaying({ file, ready })
          }}
        />
      )}

      {nowPlaying && (
        <Player
          key={nowPlaying.file.id}
          file={nowPlaying.file}
          uploader={username}
          streamBase={userStreamBase(username, nowPlaying.file.id)}
          readyTargets={nowPlaying.ready}
          audioRef={audioRef}
        />
      )}
    </main>
  )
}
