# Tariff data review runbook

Human-verified updates on a cadence — not live scraping. Before a customer demo,
spot-check that `meta.next_review_due` in the data files has not passed; if
either file is overdue, run this review before relying on the numbers.

## What to check

1. **HTS (Column 1 rates)** — [hts.usitc.gov](https://hts.usitc.gov/)
   - Confirm the current HTS revision still matches `hts_revision` on each entry
     in `data/hts_electronics.json`.
   - Spot-check Column 1 General (and Special, when present) for codes you care
     about this cycle.

2. **Section 301** — USTR Section 301 notices / exclusion & modification pages
   via [ustr.gov Section 301 investigations](https://ustr.gov/issue-areas/enforcement/section-301-investigations)
   - Confirm list membership and additional rates for electronics prefixes in
     `data/section_301.json` (especially semiconductors and batteries — these
     move more often).

## After reviewing (even if nothing changed)

Update the top-level `meta` block in **each** file you reviewed:

| Field | What to set |
| --- | --- |
| `retrieved_at` | Today's date (`YYYY-MM-DD`) — **always bump**, even when rates are unchanged, so the review clock resets |
| `source_url` | Keep the official URL you checked (USITC or USTR) |
| `reviewed_by` | `human` (or your name/handle if you want an audit trail) |
| `next_review_due` | `retrieved_at` + **90 days** for `hts_electronics.json`; `retrieved_at` + **30 days** for `section_301.json` |

If rates *did* change: edit the entry fields in that JSON file, then update
`meta` as above. Rebuild/restart `prokuro-tariff`.
