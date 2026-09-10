import type { AudioFile } from './audioFiles'
import { formatDuration, formatUploadedAt } from './format'
import { TranscodeBars, useTranscodeStates } from './TranscodeBars'
import type { PendingUpload } from './uploadAudio'

// What, if anything, the badge should say for a file that has finished uploading.
//
// Nothing, once every tier is ready: at that point the row is simply playable, which the row
// says by responding to a click rather than by wearing a label. The badge earns its place
// only while something is still outstanding.
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

// Length and age, under the filename. Both are omitted rather than rendered as a
// placeholder when unavailable: a file has no duration until a worker probes it, and a
// dash where a number belongs reads as data rather than as its absence.
function FileMeta({ file }: { file: AudioFile }) {
  const duration = formatDuration(file.duration_seconds)
  const uploaded = formatUploadedAt(file.created_at)
  if (!duration && !uploaded) return null

  return (
    <div className="file-meta">
      {duration && <span className="file-duration">{duration}</span>}
      {uploaded && (
        // The relative label is not on a timer, so the exact timestamp stays reachable.
        <span title={uploaded.exact}>{uploaded.label}</span>
      )}
    </div>
  )
}

function FileRow({
  file,
  playing,
  onPlay,
  onDelete,
}: {
  file: AudioFile
  playing: boolean
  onPlay: (file: AudioFile, readyTargets: string[]) => void
  onDelete: (id: number) => void
}) {
  const states = useTranscodeStates(file)

  const uploaded = file.status === 'uploaded'
  // Ascending, because `states` is. The player is never given the source file, so until one
  // tier has landed there is nothing to play — and the row goes inert rather than misleading.
  const readyTargets = states.filter((s) => s.state === 'ready').map((s) => s.target)
  const allReady = states.length > 0 && states.every((s) => s.state === 'ready')
  const inFlight = states.some((s) => s.state === 'pending' || s.state === 'running')
  const anyFailed = states.some((s) => s.state === 'failed')

  // Before the upload lands, the row's own status is the only thing worth reporting. After it,
  // the transcode is the only thing still in question.
  const badge = uploaded
    ? transcodeBadge(inFlight, anyFailed)
    : { className: `status-${file.status}`, text: file.status }

  // Bars disappear along with the badge. A row of three full green bars conveys nothing that
  // a playable row does not already.
  const showBars = uploaded && states.length > 0 && !allReady

  // The row *is* the play control — there is no separate button — so it only behaves like one
  // once there is something to play.
  const playable = uploaded && readyTargets.length > 0
  const play = () => {
    if (playable) onPlay(file, readyTargets)
  }

  return (
    // A whole-row control rather than a button inside one: role and key handling are what
    // make that reachable without a mouse, since a <li> answers neither on its own. Delete
    // stays a real button nested in it and stops the click from reaching this.
    <li
      className={`file-row${playable ? ' file-row-playable' : ''}${playing ? ' file-row-playing' : ''}`}
      role={playable ? 'button' : undefined}
      tabIndex={playable ? 0 : undefined}
      aria-current={playing ? 'true' : undefined}
      title={uploaded && !playable ? 'Waiting for the first transcode to finish' : undefined}
      onClick={play}
      onKeyDown={(e) => {
        if (e.key !== 'Enter' && e.key !== ' ') return
        // Space scrolls the page otherwise, and Enter would submit anything wrapping this.
        e.preventDefault()
        play()
      }}
    >
      <div className="file-main">
        <span className="file-name">{file.filename}</span>
        {badge && <span className={`status-badge ${badge.className}`}>{badge.text}</span>}
        <button
          type="button"
          className="link"
          onClick={(e) => {
            // Without this the row underneath would start playing the file being deleted.
            e.stopPropagation()
            onDelete(file.id)
          }}
        >
          Delete
        </button>
      </div>
      <FileMeta file={file} />
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
  playingId,
  onPlay,
  onDelete,
}: {
  files: AudioFile[]
  uploads: PendingUpload[]
  // Which row the player is on, if any. Null while nothing is loaded.
  playingId: number | null
  onPlay: (file: AudioFile, readyTargets: string[]) => void
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
        <FileRow
          key={file.id}
          file={file}
          playing={file.id === playingId}
          onPlay={onPlay}
          onDelete={onDelete}
        />
      ))}
    </ul>
  )
}
