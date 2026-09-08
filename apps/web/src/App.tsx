import { useState } from 'react'

export default function App() {
  const [count, setCount] = useState(0)

  return (
    <main>
      <h1>monke-app</h1>
      <p>TypeScript + Vite + React.</p>
      <button onClick={() => setCount((c) => c + 1)}>clicked {count} times</button>
    </main>
  )
}
