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

// One fixed layout rather than a locale's own: `2026-09-06 13:46 EST`. Big-endian and
// 24-hour so a column of them sorts and scans by eye, which a locale-ordered date does not
// — but the *value* is local, and the zone is named rather than assumed, because the row
// is answering "when did I upload this" for whoever is reading it.
//
// en-CA is the vehicle for that layout, not a choice about the reader: it is the locale
// whose numeric date is already ISO-ordered, and asking for the parts by name means the
// output does not depend on how it chooses to punctuate them.
const STAMP = new Intl.DateTimeFormat('en-CA', {
  year: 'numeric',
  month: '2-digit',
  day: '2-digit',
  hour: '2-digit',
  minute: '2-digit',
  // h23 rather than hour12: false, which some engines render midnight as 24:00 under.
  hourCycle: 'h23',
  // The abbreviation where the zone has one (EST, EDT, JST) and a GMT offset where it does
  // not. Deliberately not the offset alone: the abbreviation is what a person recognises,
  // and it also distinguishes standard from daylight time on a stamp months old.
  timeZoneName: 'short',
})

/// When a file was uploaded, as an absolute local timestamp.
///
/// `null` only for a timestamp that cannot be parsed at all, which the caller omits rather
/// than rendering as a placeholder.
export function formatUploadedAt(iso: string): string | null {
  const then = new Date(iso)
  if (Number.isNaN(then.getTime())) return null

  // By part rather than by formatting the whole thing, so the separators are ours: the
  // formatter would otherwise put its own comma between the date and the time.
  const parts: Partial<Record<Intl.DateTimeFormatPartTypes, string>> = {}
  for (const part of STAMP.formatToParts(then)) parts[part.type] = part.value

  const { year, month, day, hour, minute, timeZoneName } = parts
  // Every one of these is requested above, so a missing part means the runtime gave us
  // something unexpected — better to show nothing than half a timestamp.
  if (!year || !month || !day || !hour || !minute || !timeZoneName) return null

  return `${year}-${month}-${day} ${hour}:${minute} ${timeZoneName}`
}
