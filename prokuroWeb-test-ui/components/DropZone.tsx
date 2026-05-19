'use client'

import { useRef, useState, useCallback } from 'react'

interface DropZoneProps {
  onFile: (file: File) => void
  loading: boolean
}

const ACCEPTED = ['.csv', '.xlsx', '.txt']
const ACCEPT_MIME = 'text/csv,text/plain,application/vnd.openxmlformats-officedocument.spreadsheetml.sheet,.csv,.xlsx,.txt'

export default function DropZone({ onFile, loading }: DropZoneProps) {
  const [dragOver, setDragOver] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)

  const handleFile = useCallback(
    (file: File) => {
      const ext = '.' + file.name.split('.').pop()?.toLowerCase()
      if (!ACCEPTED.includes(ext)) return
      onFile(file)
    },
    [onFile],
  )

  const onDragOver = (e: React.DragEvent) => {
    e.preventDefault()
    setDragOver(true)
  }
  const onDragLeave = () => setDragOver(false)
  const onDrop = (e: React.DragEvent) => {
    e.preventDefault()
    setDragOver(false)
    const file = e.dataTransfer.files[0]
    if (file) handleFile(file)
  }
  const onInputChange = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0]
    if (file) handleFile(file)
    e.target.value = ''
  }

  return (
    <div
      role="button"
      tabIndex={0}
      aria-label="Upload BOM file"
      onClick={() => !loading && inputRef.current?.click()}
      onKeyDown={(e) => e.key === 'Enter' && !loading && inputRef.current?.click()}
      onDragOver={onDragOver}
      onDragLeave={onDragLeave}
      onDrop={onDrop}
      className={`
        relative flex min-h-[280px] cursor-pointer flex-col items-center justify-center
        rounded-xl border-2 border-dashed p-10 transition-all duration-200
        focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-primary focus-visible:ring-offset-2 focus-visible:ring-offset-canvas
        ${loading
          ? 'cursor-default border-hairline bg-surface-1 opacity-60'
          : dragOver
          ? 'border-primary bg-primary/5'
          : 'border-hairline bg-surface-1 hover:border-hairline-strong hover:bg-surface-2'
        }
      `}
    >
      <input
        ref={inputRef}
        type="file"
        accept={ACCEPT_MIME}
        onChange={onInputChange}
        className="sr-only"
      />

      {loading ? (
        <LoadingState />
      ) : dragOver ? (
        <DropState />
      ) : (
        <IdleState />
      )}
    </div>
  )
}

function IdleState() {
  return (
    <div className="flex flex-col items-center gap-4 text-center">
      <div className="flex h-14 w-14 items-center justify-center rounded-xl border border-hairline bg-surface-2">
        <UploadIcon />
      </div>
      <div>
        <p className="text-[15px] font-medium text-ink">
          Drop your BOM here
        </p>
        <p className="mt-1 text-sm text-ink-subtle">
          or click to browse — CSV, XLSX, or TXT
        </p>
      </div>
      <div className="flex flex-wrap justify-center gap-2">
        {['.csv', '.xlsx', '.txt'].map((ext) => (
          <span
            key={ext}
            className="rounded-md border border-hairline bg-surface-2 px-2.5 py-1 font-mono text-[12px] text-ink-subtle"
          >
            {ext}
          </span>
        ))}
      </div>
    </div>
  )
}

function DropState() {
  return (
    <div className="flex flex-col items-center gap-3 text-center">
      <div className="flex h-14 w-14 items-center justify-center rounded-xl border border-primary/40 bg-primary/10">
        <UploadIcon className="text-primary" />
      </div>
      <p className="text-[15px] font-medium text-primary">Release to upload</p>
    </div>
  )
}

function LoadingState() {
  return (
    <div className="flex flex-col items-center gap-4 text-center">
      <Spinner />
      <p className="text-sm text-ink-subtle">Parsing your BOM…</p>
    </div>
  )
}

function UploadIcon({ className = 'text-ink-subtle' }: { className?: string }) {
  return (
    <svg
      className={`h-6 w-6 ${className}`}
      fill="none"
      viewBox="0 0 24 24"
      stroke="currentColor"
      strokeWidth={1.5}
    >
      <path
        strokeLinecap="round"
        strokeLinejoin="round"
        d="M3 16.5v2.25A2.25 2.25 0 005.25 21h13.5A2.25 2.25 0 0021 18.75V16.5m-13.5-9L12 3m0 0l4.5 4.5M12 3v13.5"
      />
    </svg>
  )
}

function Spinner() {
  return (
    <svg
      className="h-8 w-8 animate-spin text-primary"
      fill="none"
      viewBox="0 0 24 24"
    >
      <circle
        className="opacity-25"
        cx="12"
        cy="12"
        r="10"
        stroke="currentColor"
        strokeWidth="2"
      />
      <path
        className="opacity-75"
        fill="currentColor"
        d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z"
      />
    </svg>
  )
}
