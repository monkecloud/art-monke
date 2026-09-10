import { useEffect, useState } from 'react'
import type { AudioFile, TranscodeState } from './audioFiles'

// `aac_128` -> `128k`, for a label narrow enough to sit in a row of three.
function tierLabel(target: string): string {
  return `${target.replace(/^aac_/, '')}k`
}

// A tier is still moving if it is queued or running. Once every tier is ready or failed there
// is nothing further to hear about, which is what lets the stream below be closed.
function isSettled(states: TranscodeState[]): boolean {
  return states.every((s) => s.state === 'ready' || s.state === 'failed')
}

// Live per-tier transcode status for one file.
//
// The list payload is the baseline and the SSE stream is an overlay on top of it, rather than
// the stream's events being copied into a state array seeded from props. That ordering matters:
// props get a fresh identity on every refetch, so seeding from them would clobber live progress
// with whatever the last list fetch happened to say. An overlay only ever moves a tier forward.
export function TranscodeBars({ file }: { file: AudioFile }) {
  const [live, setLive] = useState<Record<string, TranscodeState>>({})

  const states = file.transcodes.map((t) => live[t.target] ?? t)
  const settled = isSettled(states)

  useEffect(() => {
    // The api answers this route 404 for anything that isn't an uploaded row of the caller's,
    // and there is nothing left to report once every tier has finished.
    if (file.status !== 'uploaded' || settled || file.transcodes.length === 0) return

    // EventSource rather than fetch: it reconnects on its own when the connection drops, and
    // each reconnect re-sends the snapshot, so a dropped stream self-heals to the true state.
    const source = new EventSource(`/api/audio/${file.id}/progress`)

    source.addEventListener('snapshot', (e) => {
      const snapshot = JSON.parse(e.data) as TranscodeState[]
      setLive(Object.fromEntries(snapshot.map((s) => [s.target, s])))
    })

    source.addEventListener('progress', (e) => {
      const update = JSON.parse(e.data) as TranscodeState
      setLive((prev) => ({ ...prev, [update.target]: update }))
    })

    return () => source.close()
  }, [file.id, file.status, file.transcodes.length, settled])

  if (file.transcodes.length === 0) return null

  return (
    <div className="transcodes">
      {states.map((tier) => (
        <div key={tier.target} className={`tier tier-${tier.state}`}>
          <span className="tier-label">{tierLabel(tier.target)}</span>
          <div className="tier-track">
            {/* Ready fills the bar outright; a running tier that has not reported a
                percentage yet sits at zero width, same as pending. */}
            <div
              className="tier-fill"
              style={{
                width:
                  tier.state === 'ready'
                    ? '100%'
                    : tier.state === 'running'
                      ? `${tier.progress ?? 0}%`
                      : '0%',
              }}
            />
          </div>
          <span className="tier-state">
            {tier.state === 'running' && tier.progress !== null
              ? `${tier.progress}%`
              : tier.state}
          </span>
        </div>
      ))}
    </div>
  )
}
