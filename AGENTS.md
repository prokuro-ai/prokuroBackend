# Agent instructions — prokuroBackend

## Working agreement

Applies to `prokuroBackend`, `prokuroInfrastructureCDK`, and `prokuroWeb`. Keep the three copies in sync.

### Design before code

Every story gets a rough one-pager before implementation: what it does, which services and repos it touches, how it fits the current architecture, and the failure modes. Rough is fine — the point is that the team sees the architecture evolve instead of discovering it in a diff.

### Verify on a real deployed stack

A feature is not done because it worked locally or behind a throwaway UI. Deploy it and exercise it end to end on AWS. The expensive bugs are the ones that only surface when the CloudFormation stack is re-deployed against an evolved codebase.

Deploys bill by the hour. Get a go-ahead before deploying, and when verification is finished, destroy the stack or scale the Fargate service to 0 — and say which one you did.

### Small PRs, cross-reviewed

Story → tasks → one small PR per task, opened after the design doc. Never push to `main` directly; branch as `feat/<short-name>`. Whoever picks up a feature asks the other person for a sanity check before merge. Large PRs hide small mistakes — that is the entire reason for this rule.

## Repo notes

Five Rust services run in one Fargate task and talk over localhost. `docker-compose.yml` mirrors that layout locally:

| Service | Port |
|---|---|
| gateway | 3000 |
| parser | 3001 |
| enrichment | 3002 |
| tariff | 3003 |
| purchasing | 3004 |

- Analyze is cache-only on the request path. Misses enqueue to the unresolved queue and return `Pending`; a background drain worker does the live Digi-Key/Mouser lookups.
- Provider errors and rate limits are never written as `NoMatch`. Only a clean miss from both providers is a `NoMatch`.
- `Pending` and `NoMatch` score `Unknown` in `score_risk`, not Red or Yellow. Treating an unresolved line as at-risk is a false positive, not a safe default.
- `DIGIKEY_ORDERING_ENABLED` and `MOUSER_ORDERING_ENABLED` stay `false`. They place real orders against real distributor accounts.
- Enrichment requires `UNRESOLVED_DRAIN_BATCH` in env. `ENRICHMENT_DAILY_SYNC_SECS` defaults to 24h but the first sync fires seconds after startup — set it high for one-off local tests or you double Digi-Key spend.
- `Authorization: Bearer test:<user_id>` is a local/test-only auth bypass in the gateway.
