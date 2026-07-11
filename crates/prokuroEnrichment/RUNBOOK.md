# Enrichment cache runbook

## After deploying the Error vs NoMatch fix

Quota/auth/timeout failures used to be stored as `NoMatch` and cached for 24 hours.
After deploying the fix that maps those to `availability_status: Error` (and never
caches Error), clear poisoned rows:

```sql
DELETE FROM part_cache WHERE result->>'availability_status' = 'NoMatch';
```

Safe to over-delete genuine NoMatch entries — they re-fetch correctly on the next
enrich. Do **not** run destructive SQL from application code.
