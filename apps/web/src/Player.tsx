import { useEffect, useRef, useState } from 'react'
import type { AudioFile } from './audioFiles'
import { formatDuration } from './format'

// The element reports NaN for both of these until its metadata lands, and formatDuration
// returns null rather than guessing — a clock that has not started reads 0:00.
function formatTime(seconds: number): string {
  return formatDuration(seconds) ?? '0:00'
}

// Volume is a per-browser preference, not per-track and not per-session, so it outlives both
// this component and the page. localStorage rather than a cookie or the account: it never
// needs to reach the server, and it should not follow the user onto a different machine.
const VOLUME_KEY = 'monke-app:volume'

// Every access is guarded. Reading or writing localStorage *throws* outright in a browser
// configured to block site data, so an unguarded read here would take the whole player down
// rather than just losing a preference.
function readStoredVolume(): number {
  try {
    const raw = window.localStorage.getItem(VOLUME_KEY)
    // Checked before coercing, and not folded into the range test below, because
    // Number(null) is 0 rather than NaN — a missing key would otherwise read as a
    // perfectly valid "silent" and every fresh browser would start muted. Number('')
    // is 0 too, hence rejecting blanks rather than just null.
    if (raw === null || raw.trim() === '') return 1

    const stored = Number(raw)
    // NaN (something else wrote junk under this key) and anything outside the range an
    // <audio> element will accept.
    if (!Number.isFinite(stored) || stored < 0 || stored > 1) return 1
    return stored
  } catch {
    return 1
  }
}

function storeVolume(volume: number) {
  try {
    window.localStorage.setItem(VOLUME_KEY, String(volume))
  } catch {
    // A preference that cannot be saved is not worth interrupting playback over.
  }
}

// `aac_128` -> `128k`.
function tierLabel(target: string): string {
  return `${target.replace(/^aac_/, '')}k`
}

// What Android puts on the lock screen and in the notification shade. Served from
// `public/`, so these are plain paths in the built image rather than anything the api or
// Garage has to hand out — the same "baked in, versioned with the code" deal as the rest of
// the site's static content.
//
// Two sizes because Android picks the closest to whatever surface it is drawing: the shade
// wants something small, the lock screen blows one up full-width. JPEG rather than PNG: it
// is a photograph, and 512x512 of it as a PNG is roughly eight times the bytes.
const MEDIA_ARTWORK = [
  { src: '/media-art-192.jpg', sizes: '192x192', type: 'image/jpeg' },
  { src: '/media-art-512.jpg', sizes: '512x512', type: 'image/jpeg' },
]

// Mounted once, keyed on file.id by the caller so switching tracks remounts it fresh
// (new <audio> element, reset time/duration state) instead of trying to patch one up.
//
// That remount is why volume is read from storage rather than just held in state: every
// track change throws this component's state away, so a plain useState(1) would snap the
// slider back to full on each new song as well as on each page load.
//
// `readyTargets` is ascending, and never empty: the caller only offers Play once a tier
// exists, because the source audio is never played. That is the whole point of transcoding —
// the source can be a 1GB WAV, while every tier is a faststart MP4 that seeks properly.
export function Player({ file, readyTargets }: { file: AudioFile; readyTargets: string[] }) {
  const [isPlaying, setIsPlaying] = useState(true)
  const [currentTime, setCurrentTime] = useState(0)
  const [duration, setDuration] = useState(0)
  // Passed as a function so storage is read once on mount, not on every render.
  const [volume, setVolume] = useState(readStoredVolume)
  const [audioEl, setAudioEl] = useState<HTMLAudioElement | null>(null)
  // Highest available, which is the last one: readyTargets is in ascending-bitrate order.
  const [tier, setTier] = useState(() => readyTargets[readyTargets.length - 1])
  // Where to pick playback back up after a tier change swaps the element's src out from
  // under it. A ref, not state: it is consumed once, by an event handler, and re-rendering
  // on it would be pointless.
  const resumeRef = useRef<{ time: number; playing: boolean } | null>(null)

  // volume is not a prop on <audio>, so it has to be assigned to the element. Keyed on
  // audioEl as well as volume because the element arrives via ref *after* the first render:
  // without it a restored volume would show on the slider but play at full until touched.
  useEffect(() => {
    if (audioEl) audioEl.volume = volume
  }, [audioEl, volume])

  // The OS-level media card: Chrome hands this to Android, which is what turns a locked
  // phone or a minimised browser into something with a title, artwork and transport
  // controls. Without it Android still shows a card — audio is playing, after all — but
  // falls back to the page title and no art.
  //
  // Keyed on the filename rather than set once, so it follows a track change. The component
  // remounts per track anyway (the caller keys it on file.id), but that is the caller's
  // choice to make, not something this effect should quietly depend on.
  useEffect(() => {
    if (!('mediaSession' in navigator)) return

    navigator.mediaSession.metadata = new MediaMetadata({
      title: file.filename,
      artist: 'monke',
      artwork: MEDIA_ARTWORK,
    })

    // Otherwise the card outlives the player — closing the track, or logging out, would
    // leave a stale notification pointing at audio that is no longer playing.
    return () => {
      navigator.mediaSession.metadata = null
    }
  }, [file.filename])

  // Separate from the metadata above because it changes on a different beat: every
  // play/pause toggles this, and rebuilding MediaMetadata each time would make Android
  // re-fetch and re-decode the artwork on every tap.
  useEffect(() => {
    if (!('mediaSession' in navigator)) return
    navigator.mediaSession.playbackState = isPlaying ? 'playing' : 'paused'
  }, [isPlaying])

  // Chrome derives play/pause from the <audio> element on its own, but the rest of the
  // buttons only appear if there is a handler behind them — and a scrubber on the lock
  // screen needs `seekto` specifically.
  useEffect(() => {
    if (!('mediaSession' in navigator) || !audioEl) return

    navigator.mediaSession.setActionHandler('play', () => void audioEl.play())
    navigator.mediaSession.setActionHandler('pause', () => audioEl.pause())
    navigator.mediaSession.setActionHandler('stop', () => {
      audioEl.pause()
      audioEl.currentTime = 0
    })
    navigator.mediaSession.setActionHandler('seekto', (details) => {
      if (details.seekTime === undefined) return
      // Honoured when Android is scrubbing continuously: it asks for the position without
      // committing to it, so seeking the element outright would fight the user's finger.
      if (details.fastSeek && 'fastSeek' in audioEl) {
        audioEl.fastSeek(details.seekTime)
        return
      }
      audioEl.currentTime = details.seekTime
    })

    return () => {
      for (const action of ['play', 'pause', 'stop', 'seekto'] as const) {
        navigator.mediaSession.setActionHandler(action, null)
      }
    }
  }, [audioEl])

  function togglePlay() {
    if (!audioEl) return
    if (audioEl.paused) audioEl.play()
    else audioEl.pause()
  }

  function stop() {
    if (!audioEl) return
    audioEl.pause()
    audioEl.currentTime = 0
  }

  function seek(e: React.ChangeEvent<HTMLInputElement>) {
    if (!audioEl) return
    audioEl.currentTime = Number(e.target.value)
  }

  // Changing tier reloads the media, which resets position and pauses. Both are captured
  // first and restored once the new tier has enough metadata to seek — so switching bitrate
  // mid-song is continuous rather than starting the track over.
  function changeTier(next: string) {
    if (audioEl) {
      resumeRef.current = { time: audioEl.currentTime, playing: !audioEl.paused }
    }
    setTier(next)
  }

  function changeVolume(e: React.ChangeEvent<HTMLInputElement>) {
    const v = Number(e.target.value)
    setVolume(v)
    // Persisted on change rather than in the effect above, so what gets saved is always a
    // deliberate choice and never a restored value being written straight back.
    storeVolume(v)
    // The effect above is what applies it to the element.
  }

  return (
    <div className="player-bar">
      {/* The Range header this needs for seeking is sent by the browser itself off of this
          src; the download route already proxies it through to Garage and mirrors back
          whatever Garage answers. */}
      <audio
        ref={setAudioEl}
        // Always a tier, never the source.
        src={`/api/audio/${file.id}?tier=${tier}`}
        autoPlay
        onPlay={() => setIsPlaying(true)}
        onPause={() => setIsPlaying(false)}
        onTimeUpdate={(e) => setCurrentTime(e.currentTarget.currentTime)}
        onLoadedMetadata={(e) => {
          setDuration(e.currentTarget.duration)
          const resume = resumeRef.current
          if (!resume) return
          resumeRef.current = null
          e.currentTarget.currentTime = resume.time
          // Volume survives a src change on its own — it is a property of the element, not
          // of the media — so only position and play state need restoring here.
          if (resume.playing) void e.currentTarget.play()
        }}
        onEnded={() => setIsPlaying(false)}
      />
      <span className="player-name">{file.filename}</span>
      {/* Transport and scrubber are grouped rather than loose in the bar so that at phone
          width the bar can wrap into rows — name, scrubber, controls — instead of squeezing
          nine flex children onto one line and leaving the seek bar a few pixels wide. */}
      <div className="player-transport">
        <button type="button" onClick={togglePlay}>
          {isPlaying ? 'Pause' : 'Play'}
        </button>
        <button type="button" onClick={stop}>
          Stop
        </button>
      </div>
      <div className="player-scrub">
        <span className="player-time">{formatTime(currentTime)}</span>
        <input
          type="range"
          className="player-seek"
          aria-label="Seek"
          min={0}
          max={duration || 0}
          step={0.1}
          value={currentTime}
          onChange={seek}
        />
        <span className="player-time">{formatTime(duration)}</span>
      </div>
      {/* One button per available tier rather than a <select>, so the bitrate in use is
          readable at a glance instead of needing to be opened. A single ready tier still
          renders, because "what am I hearing" is worth answering even without a choice. */}
      <div className="player-tiers">
        {readyTargets.map((target) => (
          <button
            key={target}
            type="button"
            className={`player-tier${target === tier ? ' player-tier-active' : ''}`}
            aria-pressed={target === tier}
            onClick={() => changeTier(target)}
          >
            {tierLabel(target)}
          </button>
        ))}
      </div>
      <input
        type="range"
        className="player-volume"
        aria-label="Volume"
        min={0}
        max={1}
        step={0.01}
        value={volume}
        onChange={changeVolume}
      />
    </div>
  )
}
