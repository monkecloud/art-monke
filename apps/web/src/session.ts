import { useEffect, useState } from 'react'

// Who, if anyone, is signed in on this browser.
//
// 'anon' is an ordinary outcome rather than a failure. A public library is readable without an
// account, so a 401 from /api/auth/me is an answer to the question, not a problem — which is
// why it is a state of its own and not folded into 'error'.
export type Session =
  | { kind: 'loading' }
  | { kind: 'anon' }
  | { kind: 'authed'; username: string }
  | { kind: 'error'; message: string }

// Resolved once on mount. The setter is handed back because signing in and out are things the
// page does rather than things it discovers: both already know the outcome and should not have
// to round-trip to /api/auth/me to find out what they just did.
export function useSession(): [Session, (session: Session) => void] {
  const [session, setSession] = useState<Session>({ kind: 'loading' })

  useEffect(() => {
    const ac = new AbortController()

    fetch('/api/auth/me', { signal: ac.signal })
      .then(async (res) => {
        if (res.status === 401) {
          setSession({ kind: 'anon' })
          return
        }
        if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
        const { username } = (await res.json()) as { username: string }
        setSession({ kind: 'authed', username })
      })
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === 'AbortError') return
        setSession({ kind: 'error', message: err instanceof Error ? err.message : String(err) })
      })

    return () => ac.abort()
  }, [])

  return [session, setSession]
}
