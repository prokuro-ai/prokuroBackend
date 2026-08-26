# prokuroBackend

Rust backend for BOM parsing + enrichment + analyze API.

## Services

- `prokuro-parser` (`:3001`) parses CSV/XLSX/TXT BOM files.
- `prokuro-enrichment` (`:3002`) enriches parts (Digi-Key + DynamoDB current-row cache).
- `prokuro-gateway` (`:3000`) exposes `POST /v1/analyze`.
- `prokuro-tariff` (`:3003`) tariff overlay.
- `prokuro-purchasing` (`:3004`) purchasing quote/order skeleton.

## Enrichment model (cache-first)

1. **Serve** from DynamoDB (`pk` + `sk=CURRENT`). Cache hits never call Digi-Key.
2. **Miss** → paced Digi-Key ProductDetails → upsert current row → return.
3. **Nightly sync** refreshes every known part key (target ≤24h freshness under Digi-Key quota).
4. Digi-Key **NoMatch** is stored after Digi-Key ProductDetails + KeywordSearch and optional Mouser Search all miss; the API returns **NoMatch** (not Pending) once resolved.
5. OpenTelemetry metrics are instrumented; **export is off by default** (`OTEL_SDK_DISABLED=true`).

## DynamoDB (AWS via CDK)

Tables are provisioned by `prokuroInfrastructureCDK` (`PartsStorage`):

- `prokuro-parts`: PK `pk` = `{MPN}#{MANUFACTURER}`, SK `sk` = `CURRENT`, attribute `fetched_at`
- `prokuro-unresolved`: unmatched lookups (PK `pk`, SK `first_seen`)

Deploy the CDK stack (or at least the DynamoDB tables) before running enrichment against AWS. Enrichment uses the default AWS credential chain.

## Testing

```bash
# Digi-Key mock HTTP tests (no DynamoDB)
cargo test -p prokuro-enrichment --test integration digikey_

# Unit tests
cargo test -p prokuro-enrichment --test unit

# DynamoDB-backed integration (CDK tables + AWS credentials)
RUN_DYNAMODB_TESTS=1 \
  PARTS_TABLE=prokuro-parts UNRESOLVED_TABLE=prokuro-unresolved \
  cargo test -p prokuro-enrichment --test integration
```

DynamoDB-backed cases skip unless `RUN_DYNAMODB_TESTS=1` is set.

## Digi-Key pacing

Shared limiter across enrich + nightly sync:

- concurrency 1 (full HTTP call including 429 retries)
- min interval (`DIGIKEY_MIN_INTERVAL_MS`, default 750)
- 120/min and 1000/day (overridable via `DIGIKEY_MAX_PER_MINUTE` / `DIGIKEY_MAX_PER_DAY`)
- 429 → honor `Retry-After` or exponential backoff with jitter

## Quick start (cargo, 3 terminals)

```bash
cargo build --workspace

# 1) parser
PORT=3001 cargo run -p prokuro-parser --bin prokuro-parser

# 2) enrichment (Digi-Key creds + AWS credentials for CDK DynamoDB tables)
set -a && source .env && set +a && PORT=3002 cargo run -p prokuro-enrichment --bin prokuro-enrichment

# 3) gateway
PORT=3000 PARSER_URL=http://localhost:3001 ENRICHMENT_URL=http://localhost:3002 cargo run -p prokuro-gateway --bin prokuro-gateway
```

## Team invites (local smoke)

Membership is resolved in the gateway: Cognito `sub` (or `Bearer test:<user_id>`) maps to a team `account_id`. Pending invites count toward plan seats (Free=1, Growth=2, Scale=5) until accepted, revoked, or expired (7 days).

Pilot onboarding uses the admin plan API (no customer-facing plan setter):

```bash
curl -sS -X POST http://localhost:3000/v1/billing/admin/plan \
  -H "X-Prokuro-Admin-Secret: $PROKURO_ADMIN_SECRET" \
  -H 'Content-Type: application/json' \
  -d '{"account_id":"<cognito-sub>","plan":"growth","note":"pilot"}'
```

```bash
# Gateway (memory members store; no Dynamo required)
PROKURO_LOCAL_AUTH_BYPASS=1 \
APP_BASE_URL=http://localhost:3010 \
PORT=3000 cargo run -p prokuro-gateway --bin prokuro-gateway

# Owner (Growth, 2 seats) invites a teammate
curl -sS -X POST http://localhost:3000/v1/team/invites \
  -H 'Authorization: Bearer test:owner-1:owner@example.com' \
  -H 'Content-Type: application/json' \
  -d '{"email":"reader@example.com","role":"read_only"}'
# Response includes accept_url (SES is skipped locally). Share that link, or:

curl -sS -X POST http://localhost:3000/v1/team/invites/accept \
  -H 'Authorization: Bearer test:reader-1:reader@example.com' \
  -H 'Content-Type: application/json' \
  -d '{"token":"<id from invite>"}'

# Invitee now lists the owner's BOMs
curl -sS http://localhost:3000/v1/boms \
  -H 'Authorization: Bearer test:reader-1:reader@example.com'

# Free plan (default) rejects invites with 402
PROKURO_LOCAL_AUTH_BYPASS=1 PORT=3000 cargo run -p prokuro-gateway --bin prokuro-gateway
curl -sS -X POST http://localhost:3000/v1/team/invites \
  -H 'Authorization: Bearer test:owner-1:owner@example.com' \
  -H 'Content-Type: application/json' \
  -d '{"email":"reader@example.com","role":"read_only"}'
```

`MEMBERS_TABLE=prokuro-members` persists membership in Dynamo (CDK table). Without it, the in-memory store is process-local.

## Frontend

The production frontend lives in the sibling `prokuroWeb` repository.

## License

MIT — see `LICENSE`.
