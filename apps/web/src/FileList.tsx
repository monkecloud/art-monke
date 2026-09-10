import type { AudioFile } from './audioFiles'
import { TranscodeBars } from './TranscodeBars'

export function FileList({
  files,
  onPlay,
  onDelete,
}: {
  files: AudioFile[]
  onPlay: (file: AudioFile) => void
  onDelete: (id: number) => void
}) {
  if (files.length === 0) {
    return <p className="muted">No files uploaded yet.</p>
  }

  return (
    <ul className="file-list">
      {files.map((file) => (
        // Two lines per row now: the file and its controls, then its transcode tiers. Keyed on
        // the id, which also remounts TranscodeBars — and so its stream — per file.
        <li key={file.id} className="file-row">
          <div className="file-main">
            <span className="file-name">{file.filename}</span>
            <span className={`status-badge status-${file.status}`}>{file.status}</span>
            {file.status === 'uploaded' && (
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
          <TranscodeBars file={file} />
        </li>
      ))}
    </ul>
  )
}
