import type { ParseResult, AnalyzeResult, AnalyzeSummary } from '@/lib/types'
import { ConfidenceBadge } from './StatusBadge'

interface ParseSummaryProps {
  result: ParseResult
}

interface AnalyzeSummaryProps {
  result: AnalyzeResult
}

export function ParseSummaryCards({ result }: ParseSummaryProps) {
  const { stats, mapping_confidence, warnings } = result
  return (
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
      <MetricCard
        label="Confidence"
        value={<ConfidenceBadge value={mapping_confidence} />}
      />
      <MetricCard label="Total rows" value={stats.total_rows} />
      <MetricCard label="Parsed" value={stats.parsed_rows} accent />
      <MetricCard
        label="Warnings"
        value={warnings.length}
        dimmed={warnings.length === 0}
      />
    </div>
  )
}

export function AnalyzeSummaryCards({ result }: AnalyzeSummaryProps) {
  const s: AnalyzeSummary = result.summary
  return (
    <div className="grid grid-cols-2 gap-3 sm:grid-cols-3 lg:grid-cols-6">
      <MetricCard label="Total" value={s.total} />
      <MetricCard
        label="In Stock"
        value={s.in_stock}
        accent
        colorClass="text-success"
      />
      <MetricCard
        label="Out of Stock"
        value={s.out_of_stock}
        colorClass={s.out_of_stock > 0 ? 'text-red-400' : undefined}
      />
      <MetricCard
        label="EOL / NRND"
        value={s.eol_or_nrnd}
        colorClass={s.eol_or_nrnd > 0 ? 'text-amber-400' : undefined}
      />
      <MetricCard
        label="Long Lead"
        value={s.long_lead}
        colorClass={s.long_lead > 0 ? 'text-yellow-400' : undefined}
      />
      <MetricCard
        label="No Match"
        value={s.no_match}
        dimmed={s.no_match === 0}
      />
    </div>
  )
}

interface MetricCardProps {
  label: string
  value: React.ReactNode
  accent?: boolean
  dimmed?: boolean
  colorClass?: string
}

function MetricCard({ label, value, accent, dimmed, colorClass }: MetricCardProps) {
  return (
    <div className="rounded-lg border border-hairline bg-surface-1 px-4 py-3">
      <p className="text-eyebrow text-ink-subtle">{label}</p>
      <p
        className={`mt-1.5 text-2xl font-semibold tracking-tight ${
          dimmed
            ? 'text-ink-tertiary'
            : colorClass
            ? colorClass
            : accent
            ? 'text-ink'
            : 'text-ink-muted'
        }`}
      >
        {value}
      </p>
    </div>
  )
}
