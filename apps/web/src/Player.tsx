import { useCallback, useEffect, useState } from 'react'
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

// Playback speed, in 2.5% steps either side of 100%. Held as a step count rather than as a
// rate, because 2.5 * n is exact in binary where repeatedly taking 0.025 off a rate is not —
// eight steps down that way lands on 0.8000000000000002 and the readout has to hide it.
const SPEED_STEP_PERCENT = 2.5
// Half speed to one and a half. Chrome mutes the audio outright once the rate leaves the
// range it is willing to resample, and there is no signal back when it does — so the ends
// are set well inside it rather than at it.
const MIN_SPEED_STEP = -20
const MAX_SPEED_STEP = 20

function speedPercent(step: number): number {
  return 100 + step * SPEED_STEP_PERCENT
}

// "100%", "97.5%". The trailing ".0" is dropped rather than padded — the readout is given a
// fixed width in CSS instead, so the buttons either side of it hold still regardless.
function formatSpeed(percent: number): string {
  return `${Number.isInteger(percent) ? percent : percent.toFixed(1)}%`
}

// `preservesPitch` is the standard name; the prefixed pair is what browsers older than
// Chrome 109 / Safari 16.4 / Firefox 111 answer to. Worth carrying, because the property
// defaults to *true* — a browser that ignores all three time-stretches instead, which is
// the one outcome this control exists to avoid.
type PitchPreserving = HTMLMediaElement & {
  mozPreservesPitch?: boolean
  webkitPreservesPitch?: boolean
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
export function Player({
  file,
  uploader,
  streamBase,
  readyTargets,
  audioRef,
}: {
  file: AudioFile
  // Who uploaded it: the signed-in account on your own files, and the account the library
  // belongs to on a public one. Either way it is the owner of the list the track came out of,
  // never the person listening — every list the api serves is scoped to a single `user_id`.
  uploader: string
  // The route this track streams from, minus the `?tier=`. Passed in rather than derived from
  // the id, because a public library is served by different routes from your own and the
  // player cannot tell from a file which list it came out of.
  streamBase: string
  readyTargets: string[]
  // Handed up to the caller so the file list can toggle the track it already started
  // without this component having to expose a whole control surface. A ref rather than
  // state, because nothing above needs to re-render when the element arrives.
  audioRef: React.RefObject<HTMLAudioElement | null>
}) {
  const [isPlaying, setIsPlaying] = useState(true)
  const [currentTime, setCurrentTime] = useState(0)
  const [duration, setDuration] = useState(0)
  // Passed as a function so storage is read once on mount, not on every render.
  const [volume, setVolume] = useState(readStoredVolume)
  const [audioEl, setAudioEl] = useState<HTMLAudioElement | null>(null)
  // Highest available, which is the last one: readyTargets is in ascending-bitrate order.
  // Fixed at mount: there is one tier today, and a second one landing mid-song should not
  // reload the element out from under the listener.
  const [tier] = useState(() => readyTargets[readyTargets.length - 1])
  // Steps from 100%, not a rate. Deliberately not persisted the way volume is, and reset by
  // the remount on every track change: volume is how loud the room is, speed is something
  // done to one particular track.
  const [speedStep, setSpeedStep] = useState(0)
  // The two forms it is needed in: one for the readout, one for the element.
  const percent = speedPercent(speedStep)
  const rate = percent / 100

  // One ref callback feeding both the local state and the caller's ref. Memoised because an
  // inline arrow would be a new callback on every render, and React detaches and reattaches
  // a ref whose identity changed — calling this with null and then the element again, which
  // sets state, which renders, which makes another arrow.
  const attachAudio = useCallback(
    (el: HTMLAudioElement | null) => {
      audioRef.current = el
      setAudioEl(el)
    },
    [audioRef],
  )

  // volume is not a prop on <audio>, so it has to be assigned to the element. Keyed on
  // audioEl as well as volume because the element arrives via ref *after* the first render:
  // without it a restored volume would show on the slider but play at full until touched.
  useEffect(() => {
    if (audioEl) audioEl.volume = volume
  }, [audioEl, volume])

  // Same story as volume — not a prop on <audio> — with two wrinkles of its own.
  // `defaultPlaybackRate` is what loading a resource resets `playbackRate` *to*, so it is
  // set alongside rather than left at 1. And `preservesPitch` off is the whole point: the
  // browser resamples instead of time-stretching, so pitch rides down with speed the way it
  // does on a tape rather than being corrected back up.
  useEffect(() => {
    if (!audioEl) return
    const el = audioEl as PitchPreserving
    el.preservesPitch = false
    el.mozPreservesPitch = false
    el.webkitPreservesPitch = false
    el.defaultPlaybackRate = rate
    el.playbackRate = rate
  }, [audioEl, rate])

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
      // The uploader, for want of a real one: nothing extracts the artist tag out of the
      // source file yet, and a name that came from the account at least describes the
      // track rather than being decoration.
      artist: uploader,
      // Nothing here has an album either, and the field is just a string Android draws as a
      // third line — so it carries the site's name rather than going empty. Deliberately not
      // the hostname: the same bundle ships to both namespaces, and dev is served from a
      // different one, so a hardcoded art.monke.ca would be a lie there.
      album: 'art-monke',
      artwork: MEDIA_ARTWORK,
    })

    // Otherwise the card outlives the player — closing the track, or logging out, would
    // leave a stale notification pointing at audio that is no longer playing.
    return () => {
      navigator.mediaSession.metadata = null
    }
  }, [file.filename, uploader])

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

  // Space as the transport control, the way it works everywhere else audio plays. On window
  // rather than on the bar, since the point is that it works without clicking the bar first —
  // and here rather than in App because there is nothing to toggle until a track is loaded,
  // and this component is mounted exactly when there is one.
  useEffect(() => {
    // Copied into a local, and the handler is a const arrow rather than a declaration, so
    // that the null check above still holds inside it: a hoisted function could have been
    // called before the check ran, and the narrowing does not reach into one.
    const el = audioEl
    if (!el) return

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== ' ' || e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return
      // Anything focused that answers Space itself keeps it: a button takes it as a click, a
      // text field takes it as a space, and a focused file row takes it as "play this one".
      const target = e.target
      if (
        target instanceof Element &&
        target.closest('button, a, input, select, textarea, [role="button"], [contenteditable]')
      ) {
        return
      }
      // Space scrolls the page otherwise.
      e.preventDefault()
      if (el.paused) void el.play()
      else el.pause()
    }

    window.addEventListener('keydown', onKeyDown)
    return () => window.removeEventListener('keydown', onKeyDown)
  }, [audioEl])

  function stop() {
    if (!audioEl) return
    audioEl.pause()
    audioEl.currentTime = 0
  }

  function seek(e: React.ChangeEvent<HTMLInputElement>) {
    if (!audioEl) return
    audioEl.currentTime = Number(e.target.value)
  }

  // Clamped here rather than only on the buttons' disabled state, so holding the key repeat
  // on a focused button cannot walk past the end of the range.
  function changeSpeed(steps: number) {
    setSpeedStep((step) => Math.min(MAX_SPEED_STEP, Math.max(MIN_SPEED_STEP, step + steps)))
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
        ref={attachAudio}
        // Always a tier, never the source.
        src={`${streamBase}?tier=${tier}`}
        autoPlay
        onPlay={() => setIsPlaying(true)}
        onPause={() => setIsPlaying(false)}
        onTimeUpdate={(e) => setCurrentTime(e.currentTarget.currentTime)}
        onLoadedMetadata={(e) => setDuration(e.currentTarget.duration)}
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
      {/* Varispeed rather than a time-stretch: pitch rides with speed, the way it does when a
          tape is slowed down. The clock either side of the scrubber goes on reading the
          file's own duration — at 90% a 3:00 track still says 3:00 and simply takes longer
          to get there, which is the honest reading of where you are in the file. */}
      <div className="player-speed">
        <span className="player-speed-label">Speed</span>
        <button
          type="button"
          className="player-speed-step"
          aria-label="Slower"
          disabled={speedStep <= MIN_SPEED_STEP}
          onClick={() => changeSpeed(-1)}
        >
          −
        </button>
        {/* Announced on change: the buttons say what they do, but not what they did. */}
        <span className="player-speed-value" aria-live="polite">
          {formatSpeed(percent)}
        </span>
        <button
          type="button"
          className="player-speed-step"
          aria-label="Faster"
          disabled={speedStep >= MAX_SPEED_STEP}
          onClick={() => changeSpeed(1)}
        >
          +
        </button>
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
