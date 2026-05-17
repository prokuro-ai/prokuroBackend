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

## Docs

Needs to be filled.

## License

MIT — see [LICENSE](./LICENSE).
