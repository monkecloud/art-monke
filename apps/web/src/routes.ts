// The app has exactly two pages: your own files at `/`, and one account's public library at
// `/u/<username>`. Moving between them is a real link and a real navigation, so there is no
// history to manage and nothing here to install — which is why this is a function over
// `location.pathname` rather than a router dependency.

// `/u/alice` -> `alice`. Null for every other path, which is the whole rest of the SPA.
//
// One segment only: a username containing a slash would not survive a URL path, and matching
// loosely here would just mean rendering a library for a name the api is certain to 404.
export function sharedUsername(pathname: string): string | null {
  const match = /^\/u\/([^/]+)\/?$/.exec(pathname)
  if (!match) return null

  try {
    return decodeURIComponent(match[1])
  } catch {
    // A malformed %-escape decodes to nothing anyone could have registered under.
    return null
  }
}

// The public address of one account's library, as an absolute path. The single place that
// knows the shape of the route, so the link the owner shares and the route that answers it
// cannot disagree.
export function sharedPath(username: string): string {
  return `/u/${encodeURIComponent(username)}`
}
