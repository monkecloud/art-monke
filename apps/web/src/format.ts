// Shared by the file list and the player, so a track's length reads identically in both.

/// `null` for anything that cannot be rendered as a length — a file the worker has not
/// probed yet, and the NaN duration an <audio> element reports before its metadata lands.
/// Callers decide what to show instead; the list omits the field, the player shows 0:00.
export function formatDuration(totalSeconds: number | null | undefined): string | null {
  if (totalSeconds === null || totalSeconds === undefined) return null
  if (!Number.isFinite(totalSeconds) || totalSeconds < 0) return null

  const whole = Math.floor(totalSeconds)
  const hours = Math.floor(whole / 3600)
  const minutes = Math.floor((whole % 3600) / 60)
  const seconds = whole % 60

  // Minutes are zero-padded only when an hours field precedes them, so a short track is
  // "4:07" rather than "04:07" while a long one stays unambiguous at "1:04:07".
  const mm = hours > 0 ? String(minutes).padStart(2, '0') : String(minutes)
  const ss = String(seconds).padStart(2, '0')
  return hours > 0 ? `${hours}:${mm}:${ss}` : `${mm}:${ss}`
}

const MINUTE = 60_000
const HOUR = 60 * MINUTE
const DAY = 24 * HOUR

/// When a file was uploaded, as a short label plus the full local timestamp behind it.
///
/// Relative while that is the more useful answer, absolute once "37d ago" stops being one.
/// `exact` always carries the unabbreviated version for the row's tooltip, because the
/// relative label is deliberately lossy and is not re-rendered on a timer.
export function formatUploadedAt(iso: string): { label: string; exact: string } | null {
  const then = new Date(iso)
  if (Number.isNaN(then.getTime())) return null

  const exact = then.toLocaleString(undefined, { dateStyle: 'medium', timeStyle: 'short' })
  const elapsed = Date.now() - then.getTime()

  // A row timestamped in the future — clock skew between this browser and the database —
  // falls through to the absolute date rather than rendering a negative age.
  if (elapsed < 0) return { label: exact, exact }
  if (elapsed < MINUTE) return { label: 'just now', exact }
  if (elapsed < HOUR) return { label: `${Math.floor(elapsed / MINUTE)}m ago`, exact }
  if (elapsed < DAY) return { label: `${Math.floor(elapsed / HOUR)}h ago`, exact }
  if (elapsed < 7 * DAY) return { label: `${Math.floor(elapsed / DAY)}d ago`, exact }

  // The year is dropped for this year's uploads, which is most of them, and kept once it
  // is genuinely load-bearing.
  const sameYear = then.getFullYear() === new Date().getFullYear()
  return {
    label: then.toLocaleDateString(undefined, {
      day: 'numeric',
      month: 'short',
      ...(sameYear ? {} : { year: 'numeric' }),
    }),
    exact,
  }
}
