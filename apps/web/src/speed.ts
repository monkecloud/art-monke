// The playback-speed grid, shared by the player that steps along it and the route that
// carries a point on it in a URL. Both have to agree on which rates exist, so neither owns
// it: a link naming a speed the steppers cannot reach would leave the readout showing a
// number the buttons then jump away from.

// Speed is held everywhere as a step count either side of 100%, not as a rate, because
// 2.5 * n is exact in binary where repeatedly taking 0.025 off a rate is not — eight steps
// down that way lands on 0.8000000000000002 and the readout has to hide it.
export const SPEED_STEP_PERCENT = 2.5
// Half speed to one and a half. Chrome mutes the audio outright once the rate leaves the
// range it is willing to resample, and there is no signal back when it does — so the ends
// are set well inside it rather than at it.
export const MIN_SPEED_STEP = -20
export const MAX_SPEED_STEP = 20

export function speedPercent(step: number): number {
  return 100 + step * SPEED_STEP_PERCENT
}

// "100%", "97.5%". The trailing ".0" is dropped rather than padded — the readout is given a
// fixed width in CSS instead, so the buttons either side of it hold still regardless.
export function formatSpeed(percent: number): string {
  return `${Number.isInteger(percent) ? percent : percent.toFixed(1)}%`
}

export function clampSpeedStep(step: number): number {
  return Math.min(MAX_SPEED_STEP, Math.max(MIN_SPEED_STEP, step))
}

// A rate as a URL carries it — `0.85` — back to a step on the grid. Null for anything that is
// not a usable rate, so the caller can fall back rather than render NaN%.
//
// Snapped rather than rejected when it lands between steps: `speed=0.86` is someone typing a
// number at a player whose own controls only offer 85% and 87.5%, and honouring the nearest
// one is closer to what they asked for than ignoring them and playing at 100%. Clamped for
// the same reason — `speed=4` is out of range, but 150% is the answer nearest to it.
export function speedStepFromRate(rate: number): number | null {
  // Excludes NaN, both infinities, and 0 or below — a rate of zero is not slow playback, it
  // is an <audio> element that has stopped, which is not something a link should be able to
  // ask for.
  if (!Number.isFinite(rate) || rate <= 0) return null
  return clampSpeedStep(Math.round((rate * 100 - 100) / SPEED_STEP_PERCENT))
}

// The inverse, as the `speed=` value: 0.85, 0.975, 1.15. Exact decimals, because every
// percent on the grid is a multiple of 2.5 and dividing by 100 only moves the point.
export function speedRateParam(step: number): string {
  return String(speedPercent(step) / 100)
}

// The speed carries across a track change but not across a page load, and this is the whole
// of that: a module-level value, deliberately not storage. A speed set for one listening
// session should ride through the rest of it, but coming back tomorrow to a library that
// plays everything at 85%, with no memory of having asked for it, is the worse surprise.
//
// A link that names a speed seeds this before the first player mounts, so the URL's speed is
// what the first track starts at and what the ones after it carry — the same as if the
// listener had pressed the steppers themselves.
let carried = 0

export function carriedSpeedStep(): number {
  return carried
}

export function carrySpeedStep(step: number): void {
  carried = step
}
