import type { BomLine, AnalyzedLine, ParseResult, AnalyzeResult } from '@/lib/types'
import StatusBadge from './StatusBadge'

// ── Parse table ────────────────────────────────────────────────────────────────

export function ParseTable({ result }: { result: ParseResult }) {
  const { lines } = result
  if (lines.length === 0) return <EmptyTable />

  const extraCols = Array.from(
    new Set(lines.flatMap((l) => Object.keys(l.extras))),
  )

  return (
    <TableShell rowCount={lines.length}>
      <thead>
        <tr>
          <Th>Ref</Th>
          <Th>MPN</Th>
          <Th>Manufacturer</Th>
          <Th right>Qty</Th>
          <Th>Description</Th>
          <Th>Footprint</Th>
          {extraCols.map((c) => <Th key={c}>{c}</Th>)}
        </tr>
      </thead>
      <tbody>
        {lines.map((line, i) => (
          <tr
            key={line.row_index}
            className={`border-b border-hairline/50 transition-colors hover:bg-surface-2/50 ${
              i % 2 === 0 ? 'bg-canvas' : 'bg-surface-1/40'
            }`}
          >
            <Td mono>{line.refdes}</Td>
            <Td mono highlight>{line.mpn}</Td>
            <Td>{line.manufacturer}</Td>
            <Td right mono>{line.quantity?.toString()}</Td>
            <Td muted>{line.description}</Td>
            <Td mono small>{line.footprint}</Td>
            {extraCols.map((c) => <Td key={c} small>{line.extras[c]}</Td>)}
          </tr>
        ))}
      </tbody>
    </TableShell>
  )
}

// ── Analyze table ──────────────────────────────────────────────────────────────

export function AnalyzeTable({ result }: { result: AnalyzeResult }) {
  const { lines } = result
  if (lines.length === 0) return <EmptyTable />

  return (
    <TableShell rowCount={lines.length}>
      <thead>
        <tr>
          <Th>Ref</Th>
          <Th>MPN</Th>
          <Th>Manufacturer</Th>
          <Th right>Qty</Th>
          <Th>Description</Th>
          <Th>Availability</Th>
          <Th>Lifecycle</Th>
          <Th right>Stock</Th>
          <Th right>Lead (days)</Th>
        </tr>
      </thead>
      <tbody>
        {lines.map((line, i) => (
          <tr
            key={line.row_index}
            className={`border-b border-hairline/50 transition-colors hover:bg-surface-2/50 ${
              i % 2 === 0 ? 'bg-canvas' : 'bg-surface-1/40'
            }`}
          >
            <Td mono>{line.refdes}</Td>
            <Td mono highlight>{line.mpn}</Td>
            <Td>{line.manufacturer}</Td>
            <Td right mono>{line.quantity?.toString()}</Td>
            <Td muted>{line.description}</Td>
            <Td>
              <StatusBadge status={line.availability_status} type="availability" />
            </Td>
            <Td>
              <StatusBadge status={line.lifecycle_status} type="lifecycle" />
            </Td>
            <Td right mono>
              {line.total_avail > 0
                ? line.total_avail.toLocaleString()
                : <Dash />}
            </Td>
            <Td right mono>
              {line.factory_lead_days != null ? line.factory_lead_days : <Dash />}
            </Td>
          </tr>
        ))}
      </tbody>
    </TableShell>
  )
}

// ── Shared primitives ──────────────────────────────────────────────────────────

function TableShell({ children, rowCount }: { children: React.ReactNode; rowCount: number }) {
  return (
    <div className="rounded-xl border border-hairline bg-surface-1 overflow-hidden">
      <div className="flex items-center justify-between border-b border-hairline px-4 py-2.5">
        <span className="text-[13px] text-ink-subtle">
          {rowCount.toLocaleString()} {rowCount === 1 ? 'component' : 'components'}
        </span>
      </div>
      <div className="overflow-x-auto">
        <table className="w-full min-w-[640px] border-collapse text-sm">
          {children}
        </table>
      </div>
    </div>
  )
}

function Th({ children, right }: { children: React.ReactNode; right?: boolean }) {
  return (
    <th
      className={`
        border-b border-hairline bg-surface-2 px-4 py-2.5
        text-left text-[11px] font-medium uppercase tracking-eyebrow text-ink-subtle
        first:pl-4 last:pr-4
        ${right ? 'text-right' : ''}
      `}
    >
      {children}
    </th>
  )
}

interface TdProps {
  children: React.ReactNode
  mono?: boolean
  highlight?: boolean
  right?: boolean
  muted?: boolean
  small?: boolean
}

function Td({ children, mono, highlight, right, muted, small }: TdProps) {
  const isEmpty = children == null || children === ''
  return (
    <td
      className={`
        max-w-[240px] truncate px-4 py-2.5 first:pl-4 last:pr-4
        ${mono ? 'font-mono' : ''}
        ${small ? 'text-[12px]' : 'text-[13px]'}
        ${right ? 'text-right' : ''}
        ${isEmpty
          ? 'text-ink-tertiary'
          : highlight
          ? 'text-primary-hover'
          : muted
          ? 'text-ink-muted'
          : 'text-ink'
        }
      `}
      title={typeof children === 'string' ? children : undefined}
    >
      {isEmpty ? <Dash /> : children}
    </td>
  )
}

function Dash() {
  return <span className="text-ink-tertiary">—</span>
}

function EmptyTable() {
  return (
    <div className="rounded-xl border border-hairline bg-surface-1 py-16 text-center">
      <p className="text-sm text-ink-subtle">No components found</p>
    </div>
  )
}
