'use client'

import { useState, useCallback } from 'react'
import NavBar from '@/components/NavBar'
import DropZone from '@/components/DropZone'
import { ParseSummaryCards, AnalyzeSummaryCards } from '@/components/SummaryCards'
import { ParseTable, AnalyzeTable } from '@/components/BomTable'
import { ConfidenceBadge } from '@/components/StatusBadge'
import { parseFile, analyzeFile } from '@/lib/api'
import type { ParseResult, AnalyzeResult, Mode } from '@/lib/types'

type View = 'upload' | 'loading' | 'results' | 'error'

export default function AnalyzerPage() {
  const [view, setView] = useState<View>('upload')
  const [mode, setMode] = useState<Mode>('parse')
  const [parseResult, setParseResult] = useState<ParseResult | null>(null)
  const [analyzeResult, setAnalyzeResult] = useState<AnalyzeResult | null>(null)
  const [error, setError] = useState<string | null>(null)

  const handleFile = useCallback(
    async (file: File) => {
      setView('loading')
      setError(null)
      try {
        if (mode === 'parse') {
          setParseResult(await parseFile(file))
        } else {
          setAnalyzeResult(await analyzeFile(file))
        }
        setView('results')
      } catch (err) {
        setError(err instanceof Error ? err.message : 'An unexpected error occurred')
        setView('error')
      }
    },
    [mode],
  )

  const reset = useCallback(() => {
    setView('upload')
    setParseResult(null)
    setAnalyzeResult(null)
    setError(null)
  }, [])

  return (
    <div className="min-h-screen bg-canvas text-ink">
      <NavBar onLogoClick={reset} />

      {view === 'upload' && (
        <UploadView mode={mode} onModeChange={setMode} onFile={handleFile} />
      )}
      {view === 'loading' && <LoadingView />}
      {view === 'results' && mode === 'parse' && parseResult && (
        <ParseResultsView result={parseResult} onReset={reset} />
      )}
      {view === 'results' && mode === 'analyze' && analyzeResult && (
        <AnalyzeResultsView result={analyzeResult} onReset={reset} />
      )}
      {view === 'error' && <ErrorView error={error!} mode={mode} onReset={reset} />}
    </div>
  )
}

function UploadView({
  mode,
  onModeChange,
  onFile,
}: {
  mode: Mode
  onModeChange: (m: Mode) => void
  onFile: (f: File) => void
}) {
  return (
    <main className="mx-auto flex max-w-2xl flex-col items-center px-6 pb-24 pt-20">
      <div className="mb-10 text-center">
        <p className="text-eyebrow mb-4 text-primary">BOM intelligence</p>
        <h1 className="text-display-lg mb-4 text-ink">
          Parse and analyze<br />your bill of materials
        </h1>
        <p className="text-[17px] leading-relaxed text-ink-subtle">
          Drop a CSV, XLSX, or TXT file. Get clean column mapping,
          part data, and component availability - instantly.
        </p>
      </div>

      <div className="mb-5 flex gap-1 rounded-full border border-hairline bg-surface-1 p-1">
        <ModeTab active={mode === 'parse'} onClick={() => onModeChange('parse')}>
          Parse only
        </ModeTab>
        <ModeTab active={mode === 'analyze'} onClick={() => onModeChange('analyze')}>
          Full analyze
        </ModeTab>
      </div>

      <p className="mb-6 text-center text-[13px] text-ink-subtle">
        {mode === 'parse'
          ? 'Auto-map BOM columns and extract structured component data'
          : 'Parse + enrich with live availability and lifecycle status from Nexar'}
      </p>

      <div className="w-full">
        <DropZone onFile={onFile} loading={false} />
      </div>

      <p className="mt-6 text-center text-[12px] text-ink-tertiary">
        Supports KiCad, Altium, EasyEDA, and generic BOM exports
      </p>
    </main>
  )
}

function ModeTab({
  active,
  onClick,
  children,
}: {
  active: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return (
    <button
      onClick={onClick}
      className={`rounded-full px-4 py-1.5 text-[13px] font-medium transition-all duration-150 ${
        active
          ? 'bg-surface-2 text-ink shadow-sm'
          : 'text-ink-subtle hover:text-ink-muted'
      }`}
    >
      {children}
    </button>
  )
}

function LoadingView() {
  return (
    <main className="flex min-h-[calc(100vh-3.5rem)] items-center justify-center">
      <div className="flex flex-col items-center gap-5 text-center">
        <svg className="h-10 w-10 animate-spin text-primary" fill="none" viewBox="0 0 24 24">
          <circle className="opacity-20" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="2" />
          <path className="opacity-80" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
        </svg>
        <div>
          <p className="text-[15px] font-medium text-ink">Processing your BOM</p>
          <p className="mt-1 text-sm text-ink-subtle">Mapping columns and fetching part data...</p>
        </div>
      </div>
    </main>
  )
}

function ParseResultsView({ result, onReset }: { result: ParseResult; onReset: () => void }) {
  return (
    <main className="mx-auto max-w-screen-xl px-6 pb-24 pt-8">
      <ResultsHeader
        filename={result.source_filename}
        sheetName={result.sheet_name}
        confidence={result.mapping_confidence}
        onReset={onReset}
        badge={<ConfidenceBadge value={result.mapping_confidence} />}
      />

      <section className="mt-6">
        <ParseSummaryCards result={result} />
      </section>

      {result.warnings.length > 0 && (
        <section className="mt-4">
          <WarningBanner warnings={result.warnings.map((w) => `Row ${w.row_index}: ${w.code}`)} />
        </section>
      )}

      <section className="mt-6">
        <SectionLabel>Components</SectionLabel>
        <div className="mt-3">
          <ParseTable result={result} />
        </div>
      </section>

      {Object.keys(result.column_mapping).length > 0 && (
        <section className="mt-6">
          <SectionLabel>Column mapping</SectionLabel>
          <ColumnMappingGrid mapping={result.column_mapping} />
        </section>
      )}
    </main>
  )
}

function AnalyzeResultsView({ result, onReset }: { result: AnalyzeResult; onReset: () => void }) {
  return (
    <main className="mx-auto max-w-screen-xl px-6 pb-24 pt-8">
      <ResultsHeader
        filename={result.source_filename}
        sheetName={result.sheet_name}
        confidence={result.mapping_confidence}
        onReset={onReset}
        badge={<ConfidenceBadge value={result.mapping_confidence} />}
      />

      <section className="mt-6">
        <AnalyzeSummaryCards result={result} />
      </section>

      <section className="mt-6">
        <SectionLabel>Components</SectionLabel>
        <div className="mt-3">
          <AnalyzeTable result={result} />
        </div>
      </section>
    </main>
  )
}

function ErrorView({ error, mode, onReset }: { error: string; mode: Mode; onReset: () => void }) {
  const lower = error.toLowerCase()
  const backendUnavailable =
    lower.includes('could not reach parser service') ||
    lower.includes('could not reach gateway service') ||
    lower.includes('failed to fetch') ||
    lower.includes('http 502')

  return (
    <main className="flex min-h-[calc(100vh-3.5rem)] items-center justify-center px-6">
      <div className="w-full max-w-md rounded-xl border border-red-500/20 bg-surface-1 p-8 text-center">
        <div className="mx-auto mb-4 flex h-12 w-12 items-center justify-center rounded-full border border-red-500/20 bg-danger/10">
          <ErrorIcon />
        </div>
        <h2 className="text-card-title text-ink">Could not parse file</h2>
        <p className="mt-2 text-sm text-ink-subtle">{error}</p>
        {backendUnavailable && (
          <div className="mt-4 rounded-md border border-hairline bg-surface-2 p-3 text-left">
            <p className="text-[12px] font-medium text-ink">Backend service is not reachable.</p>
            <p className="mt-1 text-[12px] text-ink-subtle">Start required services in separate terminals:</p>
            <pre className="mt-2 overflow-x-auto rounded bg-canvas px-2 py-1.5 text-[11px] text-ink-subtle">
              <code>{`# parser (required)
PORT=3001 cargo run -p prokuro-parser --bin prokuro-parser

# gateway (required)
PORT=3000 \
PARSER_URL=http://localhost:3001 \
ENRICHMENT_URL=http://localhost:3002 \
cargo run -p prokuro-gateway --bin prokuro-gateway${
  mode === 'analyze'
    ? `

# enrichment (required for Full analyze)
PORT=3002 cargo run -p prokuro-enrichment --bin prokuro-enrichment`
    : ''
}`}</code>
            </pre>
          </div>
        )}
        <button
          onClick={onReset}
          className="mt-6 rounded-md bg-surface-2 px-4 py-2 text-[13px] font-medium text-ink transition-colors hover:bg-surface-3"
        >
          Try another file
        </button>
      </div>
    </main>
  )
}

function ResultsHeader({
  filename,
  sheetName,
  confidence,
  onReset,
  badge,
}: {
  filename: string
  sheetName: string | null
  confidence: number
  onReset: () => void
  badge: React.ReactNode
}) {
  return (
    <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <h1 className="truncate text-[18px] font-semibold tracking-tight text-ink">
            {filename}
          </h1>
          {sheetName && (
            <span className="rounded border border-hairline bg-surface-2 px-2 py-0.5 font-mono text-[11px] text-ink-subtle">
              {sheetName}
            </span>
          )}
          {badge}
        </div>
      </div>
      <button
        onClick={onReset}
        className="flex-shrink-0 rounded-md border border-hairline bg-surface-1 px-3.5 py-1.5 text-[13px] font-medium text-ink-muted transition-colors hover:bg-surface-2 hover:text-ink"
      >
        <- Parse another
      </button>
    </div>
  )
}

function SectionLabel({ children }: { children: React.ReactNode }) {
  return (
    <p className="text-eyebrow text-ink-subtle">{children}</p>
  )
}

function ColumnMappingGrid({ mapping }: { mapping: Record<string, string> }) {
  return (
    <div className="mt-3 grid grid-cols-2 gap-2 sm:grid-cols-3 lg:grid-cols-4">
      {Object.entries(mapping).map(([canonical, original]) => (
        <div
          key={canonical}
          className="flex items-center gap-2 rounded-lg border border-hairline bg-surface-1 px-3 py-2.5"
        >
          <span className="min-w-0 truncate font-mono text-[11px] text-primary-hover" title={original}>
            {original}
          </span>
          <span className="text-ink-tertiary">-></span>
          <span className="min-w-0 truncate text-[11px] font-medium text-ink-subtle" title={canonical}>
            {canonical}
          </span>
        </div>
      ))}
    </div>
  )
}

function WarningBanner({ warnings }: { warnings: string[] }) {
  return (
    <div className="rounded-lg border border-amber-500/20 bg-amber-900/10 px-4 py-3">
      <p className="text-eyebrow mb-2 text-amber-400">Warnings</p>
      <ul className="space-y-1">
        {warnings.slice(0, 5).map((w, i) => (
          <li key={i} className="text-[13px] text-amber-300/80">{w}</li>
        ))}
        {warnings.length > 5 && (
          <li className="text-[12px] text-ink-subtle">+{warnings.length - 5} more</li>
        )}
      </ul>
    </div>
  )
}

function ErrorIcon() {
  return (
    <svg className="h-5 w-5 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={1.5}>
      <path strokeLinecap="round" strokeLinejoin="round" d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z" />
    </svg>
  )
}
