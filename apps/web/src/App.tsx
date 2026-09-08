import { useEffect, useState } from 'react'

type Visits = {
  visits: number
}

type State =
  | { kind: 'loading' }
  | { kind: 'error'; message: string }
  | { kind: 'ready'; visits: number }

export default function App() {
  const [state, setState] = useState<State>({ kind: 'loading' })

  useEffect(() => {
    // Relative, not an absolute URL: the Ingress serves web and api from one hostname, so
    // this is same-origin in the cluster and needs no CORS or configured base URL. Locally
    // the Vite dev proxy stands in for the Ingress.
    //
    // POST because the call has an effect. Note that StrictMode mounts effects twice in
    // development, so `npm run dev` counts two visits per load; a production build does
    // not, and the deployed bundle is a production build.
    const ac = new AbortController()

    fetch('/api/visits', { method: 'POST', signal: ac.signal })
      .then(async (res) => {
        if (!res.ok) throw new Error(`${res.status} ${res.statusText}`)
        return (await res.json()) as Visits
      })
      .then(({ visits }) => setState({ kind: 'ready', visits }))
      .catch((err: unknown) => {
        if (err instanceof DOMException && err.name === 'AbortError') return
        setState({ kind: 'error', message: err instanceof Error ? err.message : String(err) })
      })

    return () => ac.abort()
  }, [])

  return (
    <main>
      <h1>monke-app</h1>

      {state.kind === 'loading' && <p className="muted">Counting…</p>}

      {state.kind === 'error' && (
        <p className="error">Could not reach the api: {state.message}</p>
      )}

      {state.kind === 'ready' && (
        <>
          <p className="count">{state.visits.toLocaleString()}</p>
          <p className="muted">
            page {state.visits === 1 ? 'load' : 'loads'}, counted in Postgres
          </p>
        </>
      )}
    </main>
  )
}
