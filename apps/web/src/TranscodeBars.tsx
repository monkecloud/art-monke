import { useEffect, useState } from 'react'
import type { AudioFile, TranscodeState } from './audioFiles'

// `aac_224` -> `224k`, for a label narrow enough to sit beside a bar.
function tierLabel(target: string): string {
  return `${target.replace(/^aac_/, '')}k`
}

// Nothing further will happen to a tier that is ready or has given up.
function isSettled(states: TranscodeState[]): boolean {
  return states.every((s) => s.state === 'ready' || s.state === 'failed')
}

// Live per-tier transcode state for one file.
//
// A hook rather than state inside the bars, because the row's badge is derived from exactly the
// same thing: whether a file is still transcoding is not knowable from the list payload alone
// once a stream is running, and two copies of this would disagree.
//
// The list payload is the baseline and the stream is an overlay on top of it, rather than state
// seeded from props. That ordering matters: props get a fresh identity on every refetch, so
// seeding from them would clobber live progress with whatever the last fetch happened to say.
//
// `live` is what a public library turns off. The progress route takes AuthUser and is scoped to
// the caller's own files, so it has nothing to say about someone else's — and with no stream
// the payload's own states stand, which is the state as of page load. A visitor watching a
// track that is still transcoding reloads to see it finish.
export function useTranscodeStates(file: AudioFile, live: boolean): TranscodeState[] {
  const [overlay, setOverlay] = useState<Record<string, TranscodeState>>({})

  const states = file.transcodes.map((t) => overlay[t.target] ?? t)
  const settled = isSettled(states)

  useEffect(() => {
    // The api answers this route 404 for anything that isn't an uploaded row of the caller's,
    // and there is nothing left to report once every tier has finished.
    if (!live || file.status !== 'uploaded' || settled || file.transcodes.length === 0) return

    // EventSource rather than fetch: it reconnects on its own when the connection drops, and
    // each reconnect re-sends the snapshot, so a dropped stream self-heals to the true state.
    const source = new EventSource(`/api/audio/${file.id}/progress`)

    source.addEventListener('snapshot', (e) => {
      const snapshot = JSON.parse(e.data) as TranscodeState[]
      setOverlay(Object.fromEntries(snapshot.map((s) => [s.target, s])))
    })

    source.addEventListener('progress', (e) => {
      const update = JSON.parse(e.data) as TranscodeState
      setOverlay((prev) => ({ ...prev, [update.target]: update }))
    })

    return () => source.close()
  }, [live, file.id, file.status, file.transcodes.length, settled])

  return states
}

// Purely presentational: the caller owns the state and decides whether these are worth showing
// at all, since a fully transcoded file shows no bars.
export function TranscodeBars({ states }: { states: TranscodeState[] }) {
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
