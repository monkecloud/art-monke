import { useEffect, useState } from 'react'
import type { AudioFile } from './audioFiles'

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds)) return '0:00'
  const m = Math.floor(seconds / 60)
  const s = Math.floor(seconds % 60)
  return `${m}:${s.toString().padStart(2, '0')}`
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

// Mounted once, keyed on file.id by the caller so switching tracks remounts it fresh
// (new <audio> element, reset time/duration state) instead of trying to patch one up.
//
// That remount is why volume is read from storage rather than just held in state: every
// track change throws this component's state away, so a plain useState(1) would snap the
// slider back to full on each new song as well as on each page load.
export function Player({ file }: { file: AudioFile }) {
  const [isPlaying, setIsPlaying] = useState(true)
  const [currentTime, setCurrentTime] = useState(0)
  const [duration, setDuration] = useState(0)
  // Passed as a function so storage is read once on mount, not on every render.
  const [volume, setVolume] = useState(readStoredVolume)
  const [audioEl, setAudioEl] = useState<HTMLAudioElement | null>(null)

  // volume is not a prop on <audio>, so it has to be assigned to the element. Keyed on
  // audioEl as well as volume because the element arrives via ref *after* the first render:
  // without it a restored volume would show on the slider but play at full until touched.
  useEffect(() => {
    if (audioEl) audioEl.volume = volume
  }, [audioEl, volume])

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
        src={`/api/audio/${file.id}`}
        autoPlay
        onPlay={() => setIsPlaying(true)}
        onPause={() => setIsPlaying(false)}
        onTimeUpdate={(e) => setCurrentTime(e.currentTarget.currentTime)}
        onLoadedMetadata={(e) => setDuration(e.currentTarget.duration)}
        onEnded={() => setIsPlaying(false)}
      />
      <span className="player-name">{file.filename}</span>
      <button type="button" onClick={togglePlay}>
        {isPlaying ? 'Pause' : 'Play'}
      </button>
      <button type="button" onClick={stop}>
        Stop
      </button>
      <span className="player-time">{formatTime(currentTime)}</span>
      <input
        type="range"
        className="player-seek"
        min={0}
        max={duration || 0}
        step={0.1}
        value={currentTime}
        onChange={seek}
      />
      <span className="player-time">{formatTime(duration)}</span>
      <input
        type="range"
        className="player-volume"
        min={0}
        max={1}
        step={0.01}
        value={volume}
        onChange={changeVolume}
      />
    </div>
  )
}
