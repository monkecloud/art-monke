// A file being uploaded right now. Deliberately not an AudioFile: it has no server id yet, and
// it is tracked separately from the fetched list so that refreshing that list — which happens
// every time any upload settles — cannot wipe the rows of the uploads still in flight.
export type PendingUpload = { key: string; filename: string; percent: number }

// 'duplicate' is not a failure: the api refused the upload because this user already has a
// file with the same name and the same bytes, which means what they wanted is already there.
export type UploadOutcome = 'uploaded' | 'duplicate' | 'failed'

export type UploadCallbacks = {
  onQueued: (uploads: PendingUpload[]) => void
  onProgress: (key: string, percent: number) => void
  onSettled: (key: string, filename: string, outcome: UploadOutcome) => void
}

// XMLHttpRequest, not fetch: only XHR exposes upload progress events across browsers, and
// the api proxies the bytes straight through to S3 rather than presigning a direct upload,
// so this progress reflects real network progress, not just "the api received it."
//
// Resolves rather than rejects on failure, because the caller uploads a whole selection and
// one bad file must not abandon the rest of the queue.
function uploadOne(file: File, onProgress: (percent: number) => void): Promise<UploadOutcome> {
  return new Promise<UploadOutcome>((resolve) => {
    const xhr = new XMLHttpRequest()
    xhr.open('POST', `/api/audio?filename=${encodeURIComponent(file.name)}`)

    xhr.upload.onprogress = (e) => {
      if (!e.lengthComputable) return
      onProgress(Math.round((e.loaded / e.total) * 100))
    }

    // Fires only once the api has responded, which it only does after the object landed in
    // S3 and the row was flipped to 'uploaded' — not merely once the browser finished sending.
    xhr.onload = () => {
      if (xhr.status >= 200 && xhr.status < 300) resolve('uploaded')
      // The duplicate is only detectable once the whole body has been hashed, so the bytes
      // were sent regardless — there is nothing to retry and nothing was stored twice.
      else if (xhr.status === 409) resolve('duplicate')
      else resolve('failed')
    }
    xhr.onerror = () => resolve('failed')

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
//
// Keys are minted here rather than by the caller so there is one owner of the identity that
// ties a file to its row and its progress.
export async function uploadAll(files: File[], cb: UploadCallbacks): Promise<void> {
  const queued = files.map((file) => ({
    key: crypto.randomUUID(),
    filename: file.name,
    percent: 0,
    file,
  }))
  cb.onQueued(queued.map(({ key, filename, percent }) => ({ key, filename, percent })))

  const pool = [...queued]
  const workers = Array.from({ length: Math.min(MAX_CONCURRENT_UPLOADS, pool.length) }, async () => {
    for (let next = pool.shift(); next; next = pool.shift()) {
      const outcome = await uploadOne(next.file, (percent) => cb.onProgress(next!.key, percent))
      cb.onSettled(next.key, next.filename, outcome)
    }
  })

  await Promise.all(workers)
}
