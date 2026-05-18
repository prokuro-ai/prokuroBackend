interface StatusBadgeProps {
  status: string
  type?: 'availability' | 'lifecycle' | 'match' | 'confidence'
}

const AVAILABILITY: Record<string, { label: string; cls: string }> = {
  in_stock:     { label: 'In Stock',     cls: 'bg-success/15 text-success border-success/30' },
  out_of_stock: { label: 'Out of Stock', cls: 'bg-danger/15 text-red-400 border-red-500/30' },
  eol_or_nrnd:  { label: 'EOL / NRND',  cls: 'bg-warning/15 text-amber-400 border-amber-500/30' },
  no_match:     { label: 'No Match',     cls: 'bg-surface-2 text-ink-subtle border-hairline' },
  long_lead:    { label: 'Long Lead',    cls: 'bg-yellow-900/20 text-yellow-400 border-yellow-600/30' },
}

const LIFECYCLE: Record<string, { label: string; cls: string }> = {
  active:  { label: 'Active',  cls: 'bg-success/10 text-green-400 border-green-600/30' },
  nrnd:    { label: 'NRND',    cls: 'bg-warning/15 text-amber-400 border-amber-500/30' },
  eol:     { label: 'EOL',     cls: 'bg-danger/15 text-red-400 border-red-500/30' },
  unknown: { label: 'Unknown', cls: 'bg-surface-2 text-ink-subtle border-hairline' },
}

const MATCH: Record<string, { label: string; cls: string }> = {
  matched:    { label: 'Matched',    cls: 'bg-primary/10 text-primary-hover border-primary/30' },
  no_mpn:     { label: 'No MPN',     cls: 'bg-surface-2 text-ink-subtle border-hairline' },
  not_found:  { label: 'Not Found',  cls: 'bg-surface-2 text-ink-tertiary border-hairline' },
}

export default function StatusBadge({ status, type = 'availability' }: StatusBadgeProps) {
  const map = type === 'lifecycle' ? LIFECYCLE : type === 'match' ? MATCH : AVAILABILITY
  const cfg = map[status] ?? { label: status, cls: 'bg-surface-2 text-ink-subtle border-hairline' }

  return (
    <span
      className={`inline-flex items-center rounded-full border px-2 py-0.5 text-[11px] font-medium leading-none ${cfg.cls}`}
    >
      {cfg.label}
    </span>
  )
}

export function ConfidenceBadge({ value }: { value: number }) {
  const pct = Math.round(value * 100)
  const cls =
    pct >= 70
      ? 'bg-success/10 text-green-400 border-green-600/30'
      : pct >= 40
      ? 'bg-warning/15 text-amber-400 border-amber-500/30'
      : 'bg-danger/15 text-red-400 border-red-500/30'

  return (
    <span className={`inline-flex items-center rounded-full border px-2.5 py-0.5 text-[12px] font-medium ${cls}`}>
      {pct}% confidence
    </span>
  )
}
