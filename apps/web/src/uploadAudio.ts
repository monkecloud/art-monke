import type { useToasts } from './Toasts'

// XMLHttpRequest, not fetch: only XHR exposes upload progress events across browsers, and
// the api proxies the bytes straight through to S3 rather than presigning a direct upload,
// so this progress reflects real network progress, not just "the api received it."
export function uploadAudio(
  file: File,
  toasts: ReturnType<typeof useToasts>,
  onSettled?: () => void,
) {
  const id = crypto.randomUUID()
  const label = (suffix: string) => `${file.name} ${suffix}`

  toasts.upsert({ id, kind: 'progress', label: label('uploading…'), percent: 0 })

  const xhr = new XMLHttpRequest()
  xhr.open('POST', `/api/audio?filename=${encodeURIComponent(file.name)}`)

  xhr.upload.onprogress = (e) => {
    if (!e.lengthComputable) return
    const percent = Math.round((e.loaded / e.total) * 100)
    toasts.upsert({ id, kind: 'progress', label: label('uploading…'), percent })
  }

  // Fires only once the api has responded, which it only does after the object landed in
  // S3 and the row was flipped to 'uploaded' — not merely once the browser finished sending.
  xhr.onload = () => {
    if (xhr.status >= 200 && xhr.status < 300) {
      toasts.upsert({ id, kind: 'success', label: label('uploaded') }, 4000)
    } else {
      toasts.upsert({ id, kind: 'error', label: label('failed to upload') }, 6000)
    }
    onSettled?.()
  }

  xhr.onerror = () => {
    toasts.upsert({ id, kind: 'error', label: label('failed to upload') }, 6000)
    onSettled?.()
  }

  xhr.send(file)
}
