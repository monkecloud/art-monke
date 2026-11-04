import { useCallback, useEffect, useRef, useState } from 'react'
import {
  type AudioFile,
  NoSuchUser,
  fetchUserAudioFiles,
  nextPlayable,
  readyTargets,
  userStreamBase,
} from './audioFiles'
import { FileList } from './FileList'
import { Player } from './Player'
import { sharedTrackPath } from './routes'
import type { Session } from './session'
import { carriedSpeedStep } from './speed'

// Why a link's track is not the one playing. Null when it is, and when the link named none.
// Told apart because they are different things to be told: one is a link to a file that is not
// here, the other a link to one that is here and not playable *yet*.
type TrackNotice =
  // No file in this library has that id — a mistyped link, or a file the owner has deleted.
  | 'missing'
  // The file is listed, but no tier has finished, so there is nothing to stream. The rows say
  // the same thing with a badge and a progress bar; this says why the player did not open.
  | 'unplayable'

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
export function SharedLibrary({
  username,
  trackId,
  session,
}: {
  username: string
  // The track the link pointed at, or null for a plain library link. Only read when the list
  // lands: whether an id is playable is a question about the list, not about the URL.
  trackId: string | null
  session: Session
}) {
  const [library, setLibrary] = useState<Library>({ kind: 'loading' })
  // Same shape as the owner's page: the tiers are captured when the row is clicked, or when a
  // link's track is opened below.
  const [nowPlaying, setNowPlaying] = useState<{ file: AudioFile; ready: string[] } | null>(null)
  const [trackNotice, setTrackNotice] = useState<TrackNotice | null>(null)
  // Mirrored out of the player purely so the address bar can carry it. Seeded from the value a
  // link already set, so the first write is right even if it beats the player's first report.
  const [speedStep, setSpeedStep] = useState(carriedSpeedStep)
  const audioRef = useRef<HTMLAudioElement | null>(null)

  useEffect(() => {
    const ac = new AbortController()

    fetchUserAudioFiles(username, ac.signal)
      .then((files) => {
        setLibrary({ kind: 'ready', files })
        if (trackId === null) return

        // Resolved off the payload rather than from a row, because the rows have not rendered
        // yet — and on a public library that is the same answer a row would give: `live` is
        // false here, so no progress stream ever overlays what the list said.
        const file = files.find((f) => f.id === trackId)
        if (!file) {
          setTrackNotice('missing')
          return
        }
        const ready = file.status === 'uploaded' ? readyTargets(file.transcodes) : []
        if (ready.length === 0) {
          setTrackNotice('unplayable')
          return
        }
        setNowPlaying({ file, ready })
      })
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === 'AbortError') return
        if (err instanceof NoSuchUser) {
          setLibrary({ kind: 'missing' })
          return
        }
        setLibrary({ kind: 'error', message: err instanceof Error ? err.message : String(err) })
      })

    return () => ac.abort()
  }, [username, trackId])

  // The address bar follows the player, so the link to hand someone is the one already in it —
  // which is what the owner's Share link has always counted on. `replaceState` rather than
  // `pushState`: playing a second track is not a page to go Back to, and this page is reached
  // by a real navigation whose Back should leave the library rather than walk the tracks
  // played inside it.
  //
  // Only ever written while something is playing. Nothing here closes the player once it is
  // open, so the URL never has to go back to the bare library — and not writing means a link
  // whose track went missing keeps the id it named, instead of being quietly tidied away
  // underneath the notice that explains it.
  useEffect(() => {
    if (!nowPlaying) return
    window.history.replaceState(null, '', sharedTrackPath(username, nowPlaying.file.id, speedStep))
  }, [username, nowPlaying, speedStep])

  // Memoised because the player reports through it from an effect keyed on the callback: a new
  // arrow each render would re-run that effect every render.
  const onSpeedStepChange = useCallback((step: number) => setSpeedStep(step), [])

  // So a shared link is identifiable in a tab strip, in history and in a bookmark, all of
  // which are places this URL is likely to end up. Restored on unmount for the same reason the
  // media-session card is torn down: the title should not outlive the page that set it.
  //
  // The track goes in front of the name once one is playing, for the same reason: a link that
  // opens on one song should be identifiable as that song, not as the library it came out of.
  const playingName = nowPlaying?.file.filename
  useEffect(() => {
    const previous = document.title
    document.title = playingName
      ? `${playingName} — ${username} | art-monke`
      : `${username} | art-monke`
    return () => {
      document.title = previous
    }
  }, [username, playingName])

  // The owner looking at their own link. Worth saying out loud, because the page is otherwise
  // indistinguishable from a private one and this is exactly where someone checks what they
  // are about to send round.
  // Same as on your own page: known up front, so the lock screen only offers Next when there
  // is somewhere to go.
  const nextTrack =
    nowPlaying && library.kind === 'ready' ? nextPlayable(library.files, nowPlaying.file.id) : null

  const isOwner = session.kind === 'authed' && session.username === username

  return (
    <main className="library">
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

      {/* Under the owner's notice, and only once the list has settled enough to know: both of
          these are answers about a file, so neither should appear while the library itself is
          still loading or has failed to. */}
      {trackNotice === 'missing' && (
        <p className="notice">That link points at a track that is not in this library.</p>
      )}
      {trackNotice === 'unplayable' && (
        <p className="notice">
          That link’s track is still being transcoded — nothing to play from it yet.
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
            // Whatever the link could not open, the visitor has now picked something that
            // plays — and the URL is about to stop naming the track the notice is about.
            setTrackNotice(null)
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
          onSpeedStepChange={onSpeedStepChange}
          // The address bar follows on its own, through the replaceState effect above.
          onNext={nextTrack ? () => setNowPlaying(nextTrack) : undefined}
        />
      )}
    </main>
  )
}
