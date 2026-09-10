import { useState } from 'react'
import type { AudioFile } from './audioFiles'

function formatTime(seconds: number): string {
  if (!Number.isFinite(seconds)) return '0:00'
  const m = Math.floor(seconds / 60)
  const s = Math.floor(seconds % 60)
  return `${m}:${s.toString().padStart(2, '0')}`
}

// Mounted once, keyed on file.id by the caller so switching tracks remounts it fresh
// (new <audio> element, reset time/duration state) instead of trying to patch one up.
export function Player({ file }: { file: AudioFile }) {
  const [isPlaying, setIsPlaying] = useState(true)
  const [currentTime, setCurrentTime] = useState(0)
  const [duration, setDuration] = useState(0)
  const [volume, setVolume] = useState(1)
  const [audioEl, setAudioEl] = useState<HTMLAudioElement | null>(null)

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
    if (audioEl) audioEl.volume = v
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
