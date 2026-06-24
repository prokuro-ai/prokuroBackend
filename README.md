# prokuroBackend

Rust backend for BOM parsing + enrichment + analyze API.

## Services

- `prokuro-parser` (`:3001`) parses CSV/XLSX/TXT BOM files.
- `prokuro-enrichment` (`:3002`) enriches parts (Nexar/Octopart).
- `prokuro-gateway` (`:3000`) exposes `POST /v1/analyze`.

## Quick start

```bash
cargo build --workspace
```

### Run backend (3 terminals)

```bash
# 1) parser
PORT=3001 cargo run -p prokuro-parser --bin prokuro-parser

# 2) enrichment (requires .env with Nexar creds)
set -a && source .env && set +a && PORT=3002 cargo run -p prokuro-enrichment --bin prokuro-enrichment

# 3) gateway
PORT=3000 PARSER_URL=http://localhost:3001 ENRICHMENT_URL=http://localhost:3002 cargo run -p prokuro-gateway --bin prokuro-gateway
```

### Test backend quickly

```bash
./scripts/test-parse.sh corpus/raw/openxenium-bom.csv
./scripts/test-analyze.sh corpus/raw/openxenium-bom.csv
```

## Frontend in this repo (testing only)

`prokuroWeb` is a lightweight local testing UI for this backend.

```bash
cd prokuroWeb
npm install
npm run dev
```

Open `http://localhost:3010`.

The production frontend will live in a separate repo: `prokuro-web`.

## License

MIT — see `LICENSE`.
