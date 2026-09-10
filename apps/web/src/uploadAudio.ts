import type { useToasts } from './Toasts'

// 'duplicate' is not a failure: the api refused the upload because this user already has a
// file with the same name and the same bytes, which means what they wanted is already there.
export type UploadOutcome = 'uploaded' | 'duplicate' | 'failed'

// XMLHttpRequest, not fetch: only XHR exposes upload progress events across browsers, and
// the api proxies the bytes straight through to S3 rather than presigning a direct upload,
// so this progress reflects real network progress, not just "the api received it."
//
// Resolves rather than rejects on failure, because the caller uploads a whole selection and
// one bad file must not abandon the rest of the queue.
export function uploadAudio(
  file: File,
  toasts: ReturnType<typeof useToasts>,
  onSettled?: () => void,
): Promise<UploadOutcome> {
  return new Promise<UploadOutcome>((resolve) => {
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
      let outcome: UploadOutcome = 'failed'
      if (xhr.status >= 200 && xhr.status < 300) {
        outcome = 'uploaded'
        toasts.upsert({ id, kind: 'success', label: label('uploaded') }, 4000)
      } else if (xhr.status === 409) {
        // The duplicate is only detectable once the whole body has been hashed, so the bytes
        // were sent regardless — there is nothing to retry and nothing was stored twice.
        outcome = 'duplicate'
        toasts.upsert({ id, kind: 'success', label: label('already uploaded — skipped') }, 5000)
      } else {
        toasts.upsert({ id, kind: 'error', label: label('failed to upload') }, 6000)
      }
      onSettled?.()
      resolve(outcome)
    }

    xhr.onerror = () => {
      toasts.upsert({ id, kind: 'error', label: label('failed to upload') }, 6000)
      onSettled?.()
      resolve('failed')
    }

    xhr.send(file)
  })
}

// At most this many uploads in flight at once. Not unbounded: every upload streams through an
// api pod on its way to Garage, so twenty at once would compete for the same uplink, make
// every individual file slower, and hold twenty proxied request bodies open. Not one at a
// time either, which would needlessly serialise a drop of twenty small files.
const MAX_CONCURRENT_UPLOADS = 3

// Drains a whole selection through that limit. Each worker takes the next file off the shared
// queue as it frees up, so one slow large file doesn't hold back the others.
export async function uploadAll(
  files: File[],
  toasts: ReturnType<typeof useToasts>,
  onEachSettled?: () => void,
): Promise<UploadOutcome[]> {
  const queue = [...files]
  const outcomes: UploadOutcome[] = []

  const workers = Array.from({ length: Math.min(MAX_CONCURRENT_UPLOADS, queue.length) }, async () => {
    for (let next = queue.shift(); next; next = queue.shift()) {
      outcomes.push(await uploadAudio(next, toasts, onEachSettled))
    }
  })

  await Promise.all(workers)
  return outcomes
}
