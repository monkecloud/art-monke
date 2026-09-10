import type { AudioFile } from './audioFiles'
import { TranscodeBars, useTranscodeStates } from './TranscodeBars'
import type { PendingUpload } from './uploadAudio'

// What, if anything, the badge should say for a file that has finished uploading.
//
// Nothing, once every tier is ready: at that point the row already shows Play and Download,
// which says "uploaded and transcoded" more directly than a badge repeating it does. The badge
// earns its place only while something is still outstanding.
function transcodeBadge(
  inFlight: boolean,
  anyFailed: boolean,
): { className: string; text: string } | null {
  if (inFlight) return { className: 'status-transcoding', text: 'transcoding' }
  // Nothing running and nothing left queued, but a tier gave up — the file is playable and
  // some tiers may be servable, so this is not a failed upload, just an incomplete set.
  if (anyFailed) return { className: 'status-transcode-failed', text: 'transcode failed' }
  return null
}

function FileRow({
  file,
  onPlay,
  onDelete,
}: {
  file: AudioFile
  onPlay: (file: AudioFile) => void
  onDelete: (id: number) => void
}) {
  const states = useTranscodeStates(file)

  const uploaded = file.status === 'uploaded'
  const allReady = states.length > 0 && states.every((s) => s.state === 'ready')
  const inFlight = states.some((s) => s.state === 'pending' || s.state === 'running')
  const anyFailed = states.some((s) => s.state === 'failed')

  // Before the upload lands, the row's own status is the only thing worth reporting. After it,
  // the transcode is the only thing still in question.
  const badge = uploaded
    ? transcodeBadge(inFlight, anyFailed)
    : { className: `status-${file.status}`, text: file.status }

  // Bars disappear along with the badge. A row of three full green bars conveys nothing that
  // the Play link does not already.
  const showBars = uploaded && states.length > 0 && !allReady

  return (
    <li className="file-row">
      <div className="file-main">
        <span className="file-name">{file.filename}</span>
        {badge && <span className={`status-badge ${badge.className}`}>{badge.text}</span>}
        {uploaded && (
          <>
            <button type="button" className="link" onClick={() => onPlay(file)}>
              Play
            </button>
            <a className="link" href={`/api/audio/${file.id}`} download={file.filename}>
              Download
            </a>
          </>
        )}
        <button type="button" className="link" onClick={() => onDelete(file.id)}>
          Delete
        </button>
      </div>
      {showBars && <TranscodeBars states={states} />}
    </li>
  )
}

// A file still going up. Same shape as a transcoding row — badge plus one bar — because it is
// the same idea at an earlier stage, and the row it becomes should not jump around when the
// upload finishes and the transcode starts.
function UploadRow({ upload }: { upload: PendingUpload }) {
  return (
    <li className="file-row">
      <div className="file-main">
        <span className="file-name">{upload.filename}</span>
        <span className="status-badge status-uploading">uploading</span>
      </div>
      <div className="transcodes">
        <div className="tier tier-running">
          <span className="tier-label">file</span>
          <div className="tier-track">
            <div className="tier-fill" style={{ width: `${upload.percent}%` }} />
          </div>
          <span className="tier-state">{upload.percent}%</span>
        </div>
      </div>
    </li>
  )
}

export function FileList({
  files,
  uploads,
  onPlay,
  onDelete,
}: {
  files: AudioFile[]
  uploads: PendingUpload[]
  onPlay: (file: AudioFile) => void
  onDelete: (id: number) => void
}) {
  if (files.length === 0 && uploads.length === 0) {
    return <p className="muted">No files uploaded yet.</p>
  }

  return (
    <ul className="file-list">
      {/* In-flight uploads are held separately from the fetched list rather than mixed into
          it: every upload that settles refreshes that list, which would otherwise replace —
          and so erase — the rows of the uploads still running alongside it. */}
      {uploads.map((upload) => (
        <UploadRow key={upload.key} upload={upload} />
      ))}
      {files.map((file) => (
        // Keyed on the id, which also remounts the row's transcode stream per file.
        <FileRow key={file.id} file={file} onPlay={onPlay} onDelete={onDelete} />
      ))}
    </ul>
  )
}
