# prokuroBackend

Rust backend for BOM parsing + enrichment + analyze API.

## Services

- `prokuro-parser` (`:3001`) parses CSV/XLSX/TXT BOM files.
- `prokuro-enrichment` (`:3002`) enriches parts (Digi-Key + DynamoDB cache).
- `prokuro-gateway` (`:3000`) exposes `POST /v1/analyze`.

## Testing

```bash
# Unit tests
cargo test -p prokuro-enrichment --test unit

# Digi-Key mock HTTP tests (no DynamoDB)
cargo test -p prokuro-enrichment --test integration digikey_

# DynamoDB-backed integration tests (optional; needs real AWS tables + credentials)
PARTS_TABLE=prokuro-parts UNRESOLVED_TABLE=prokuro-unresolved \
  cargo test -p prokuro-enrichment --test integration
```

DynamoDB-backed cases skip (with a stderr message) unless `RUN_DYNAMODB_TESTS=1` is set.

## DynamoDB

Tables are provisioned by `prokuroInfrastructureCDK` (`PartsStorage`):

- `prokuro-parts`: PK `pk` = `{MPN}#{MANUFACTURER}`, SK `fetched_at` (append-only snapshots)
- `prokuro-unresolved`: unmatched lookups (PK `pk`, SK `first_seen`)

Enrich reads the latest snapshot (`Query` Limit 1, newest first). A daily job
refreshes every known part key. First-seen misses return `Pending` and enqueue
one lookup.

## Quick start

```bash
cargo build --workspace
```

### Run backend (3 terminals)

```bash
# 1) parser
PORT=3001 cargo run -p prokuro-parser --bin prokuro-parser

# 2) enrichment (requires .env with Digi-Key creds + AWS credentials for DynamoDB)
set -a && source .env && set +a && PORT=3002 cargo run -p prokuro-enrichment --bin prokuro-enrichment

# 3) gateway
PORT=3000 PARSER_URL=http://localhost:3001 ENRICHMENT_URL=http://localhost:3002 cargo run -p prokuro-gateway --bin prokuro-gateway
```

## Frontend

The production frontend lives in the sibling `prokuroWeb` repository.

## License

MIT — see `LICENSE`.
