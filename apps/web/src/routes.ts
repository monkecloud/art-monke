import { speedRateParam, speedStepFromRate } from './speed'

// The app has exactly two pages: your own files at `/`, and one account's public library at
// `/u/<username>`, optionally pointed at one track by `/u/<username>/t/<id>`. Moving between
// the two pages is a real link and a real navigation, so there is no history to manage and
// nothing here to install — which is why this is a function over `location.pathname` rather
// than a router dependency. The track is not a third page: it is the same library with one
// row already loaded into the player, and it is written into the address bar with
// `replaceState` so that stays true.

export type SharedRoute = {
  username: string
  // The track the link points at, or null for a library link. Opaque: this is matched against
  // the ids the api served and nothing else, never parsed. A track that is not in the library
  // is not distinguishable here — that takes the list, which only the page has.
  trackId: string | null
  // Steps either side of 100%, off `?speed=`. Null when the link named no speed, which is
  // different from naming 100%: only the first starts wherever the session had got to.
  speedStep: number | null
}

// `/u/alice` and `/u/alice/t/a3f9c21e8b40d7f5`. Null for every other path, which is the whole
// rest of the SPA.
//
// One segment each, because neither a username nor an id containing a slash would survive a
// URL path. The id is matched as any segment rather than as the hex the api currently mints:
// `/t/` is what marks this a track link, and the shape of an id is the api's business — a
// link carrying one this build does not recognise should say the track is not here, which is
// true, rather than fall through to the login page. Anything else under `/u/` still does fall
// through, which is where `/u/alice/anything` went before there was a track route at all.
export function sharedRoute(pathname: string, search: string): SharedRoute | null {
  const match = /^\/u\/([^/]+)(?:\/t\/([^/]+))?\/?$/.exec(pathname)
  if (!match) return null

  let username: string
  try {
    username = decodeURIComponent(match[1])
  } catch {
    // A malformed %-escape decodes to nothing anyone could have registered under.
    return null
  }

  return { username, trackId: trackId(match[2]), speedStep: speedStepFromSearch(search) }
}

// Undecodable is not the same answer as absent: a mangled escape is still a link that named a
// track, so the raw segment is kept and simply matches no file — the page then says the track
// is not in the library, rather than silently rendering as though the link had named none.
function trackId(segment: string | undefined): string | null {
  if (segment === undefined) return null
  try {
    return decodeURIComponent(segment)
  } catch {
    return segment
  }
}

// `?speed=0.85` -> -6 steps. Null when absent or unusable, never a throw: this runs on
// whatever a link happened to carry, and a query string is the easiest part of a URL for a
// chat client to mangle.
function speedStepFromSearch(search: string): number | null {
  const raw = new URLSearchParams(search).get('speed')
  // Checked before coercing, because Number('') is 0 rather than NaN — `?speed=` would
  // otherwise read as a rate of zero instead of as a parameter nobody filled in.
  if (raw === null || raw.trim() === '') return null
  return speedStepFromRate(Number(raw))
}

// The public address of one account's library, as an absolute path. The single place that
// knows the shape of the route, so the link the owner shares and the route that answers it
// cannot disagree.
export function sharedPath(username: string): string {
  return `/u/${encodeURIComponent(username)}`
}

// The same library, pointed at one track and one speed — what the address bar shows while a
// track is playing, and so what gets copied out of it.
//
// `speed` is left off at 100% rather than written as `speed=1`: a link that names no speed
// and a link that names the default play identically, and the shorter one is the one worth
// handing to someone.
export function sharedTrackPath(username: string, trackId: string, speedStep: number): string {
  const path = `${sharedPath(username)}/t/${encodeURIComponent(trackId)}`
  return speedStep === 0 ? path : `${path}?speed=${speedRateParam(speedStep)}`
}
