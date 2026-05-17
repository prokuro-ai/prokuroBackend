# prokuroBackend

BOM parser, Nexar enrichment, and public API gateway.

**GitHub:** https://github.com/prokuro-ai/prokuroBackend

## Layout

```
prokuroBackend/
  crates/
    prokuroTypes/       # shared types (from schemas/)
    prokuroParser/      # CSV/XLSX ingest
    prokuroEnrichment/  # Nexar enrichment
    prokuroGateway/     # public BFF
  schemas/              # JSON Schema + OpenAPI
  corpus/               # sample BOM files (optional, for manual/batch checks)
```

## Prerequisites

- Rust 1.75+ (`rustup`)

## Build

```bash
cargo build --workspace
```

## Running locally

### Option 1: Docker Compose (full stack)
cp .env.example .env
# edit .env and add your NEXAR_CLIENT_ID and NEXAR_CLIENT_SECRET
docker compose up --build

### Option 2: Individual services (for development)
# Terminal 1 - Parser
cargo run -p prokuro-parser
# PORT defaults to 3001

# Terminal 2 - Enrichment (requires Nexar credentials)
NEXAR_CLIENT_ID=xxx NEXAR_CLIENT_SECRET=xxx cargo run -p prokuro-enrichment
# PORT defaults to 3002

# Terminal 3 - Gateway
PARSER_URL=http://localhost:3001 ENRICHMENT_URL=http://localhost:3002 cargo run -p prokuro-gateway
# PORT defaults to 3000

### Option 3: Test parse endpoint directly
./scripts/test-parse.sh corpus/raw/openxenium-bom.csv

### Option 4: Test full analyze pipeline
./scripts/test-analyze.sh corpus/raw/openxenium-bom.csv

## Docs

Needs to be filled.

## License

MIT — see [LICENSE](./LICENSE).
