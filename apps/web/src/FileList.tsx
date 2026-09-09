import type { AudioFile } from './audioFiles'

export function FileList({
  files,
  onDelete,
}: {
  files: AudioFile[]
  onDelete: (id: number) => void
}) {
  if (files.length === 0) {
    return <p className="muted">No files uploaded yet.</p>
  }

  return (
    <ul className="file-list">
      {files.map((file) => (
        <li key={file.id} className="file-row">
          <span className="file-name">{file.filename}</span>
          <span className={`status-badge status-${file.status}`}>{file.status}</span>
          {file.status === 'uploaded' && (
            <a className="link" href={`/api/audio/${file.id}`} download={file.filename}>
              Download
            </a>
          )}
          <button type="button" className="link" onClick={() => onDelete(file.id)}>
            Delete
          </button>
        </li>
      ))}
    </ul>
  )
}
