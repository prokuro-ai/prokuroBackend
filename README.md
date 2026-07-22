# prokuroBackend

Rust backend for BOM parsing + enrichment + analyze API.

## Services

- `prokuro-parser` (`:3001`) parses CSV/XLSX/TXT BOM files.
- `prokuro-enrichment` (`:3002`) enriches parts (Digi-Key + DynamoDB current-row cache).
- `prokuro-gateway` (`:3000`) exposes `POST /v1/analyze`.
- `prokuro-tariff` (`:3003`) tariff overlay.

## Enrichment model (cache-first)

1. **Serve** from DynamoDB (`pk` + `sk=CURRENT`). Cache hits never call Digi-Key.
2. **Miss** → paced Digi-Key ProductDetails → upsert current row → return.
3. **Nightly sync** refreshes every known part key (target ≤24h freshness under Digi-Key quota).
4. Digi-Key **NoMatch** is stored for ops; the API returns **Pending** to the customer.
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

## Frontend

The production frontend lives in the sibling `prokuroWeb` repository.

## License

MIT — see `LICENSE`.
