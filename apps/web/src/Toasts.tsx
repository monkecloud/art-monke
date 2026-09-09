import { useCallback, useRef, useState } from 'react'

type Toast =
  | { id: string; kind: 'progress'; label: string; percent: number }
  | { id: string; kind: 'success'; label: string }
  | { id: string; kind: 'error'; label: string }

export function useToasts() {
  const [toasts, setToasts] = useState<Toast[]>([])
  const timers = useRef(new Map<string, ReturnType<typeof setTimeout>>())

  const dismiss = useCallback((id: string) => {
    clearTimeout(timers.current.get(id))
    timers.current.delete(id)
    setToasts((ts) => ts.filter((t) => t.id !== id))
  }, [])

  // Upserts by id so a progress toast can be updated in place and then flipped to
  // success/error without ever becoming a second toast.
  const upsert = useCallback(
    (toast: Toast, autoDismissMs?: number) => {
      setToasts((ts) => [...ts.filter((t) => t.id !== toast.id), toast])
      clearTimeout(timers.current.get(toast.id))
      if (autoDismissMs) {
        timers.current.set(
          toast.id,
          setTimeout(() => dismiss(toast.id), autoDismissMs),
        )
      }
    },
    [dismiss],
  )

  return { toasts, upsert, dismiss }
}

export function ToastStack({
  toasts,
  onDismiss,
}: {
  toasts: Toast[]
  onDismiss: (id: string) => void
}) {
  if (toasts.length === 0) return null

  return (
    <div className="toast-stack">
      {toasts.map((toast) => (
        <div key={toast.id} className={`toast toast-${toast.kind}`}>
          <div className="toast-row">
            <span>{toast.label}</span>
            <button
              type="button"
              className="toast-close"
              onClick={() => onDismiss(toast.id)}
              aria-label="Dismiss"
            >
              ×
            </button>
          </div>
          {toast.kind === 'progress' && (
            <div className="toast-progress-track">
              <div className="toast-progress-fill" style={{ width: `${toast.percent}%` }} />
            </div>
          )}
        </div>
      ))}
    </div>
  )
}
