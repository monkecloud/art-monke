import { useEffect, useState } from 'react'

// Mirrors the Placeholder struct in apps/api/src/main.rs.
type Placeholder = {
  service: string
  status: string
  bananas: number
}

type State =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | { kind: 'ready'; data: Placeholder }

export default function App() {
  const [state, setState] = useState<State>({ kind: 'loading' })

  useEffect(() => {
    // Relative, not an absolute URL: the Ingress serves web and api from one hostname, so
    // this is same-origin in the cluster and needs no CORS or configured base URL. Locally
    // the Vite dev proxy stands in for the Ingress.
    const ac = new AbortController()

    fetch('/api/hello', { signal: ac.signal })
      .then(async (res) => {
        if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
        return (await res.json()) as Placeholder
      })
      .then((data) => setState({ kind: 'ready', data }))
      .catch((err: unknown) => {
        // StrictMode mounts twice in dev, so the first request is aborted by design.
        if (err instanceof DOMException && err.name === 'AbortError') return
        setState({ kind: 'error', message: err instanceof Error ? err.message : String(err) })
      })

    return () => ac.abort()
  }, [])

  return (
    <main>
      <h1>monke-app</h1>
      <p>TypeScript + Vite + React, reading from Rust + axum.</p>

      {state.kind === 'loading' && <p className="muted">Loading…</p>}

      {state.kind === 'error' && (
        <p className="error">Could not reach the api: {state.message}</p>
      )}

      {state.kind === 'ready' && (
        <dl>
          <dt>service</dt>
          <dd>{state.data.service}</dd>
          <dt>status</dt>
          <dd>{state.data.status}</dd>
          <dt>bananas</dt>
          <dd>{state.data.bananas}</dd>
        </dl>
      )}
    </main>
  )
}
