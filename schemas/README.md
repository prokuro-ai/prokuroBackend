# Schemas

Contract-first API definitions for all Prokuro backend services.

| Path | Purpose |
|------|---------|
| `json/` | JSON Schema for `ParseResult`, `EnrichResult`, `AnalyzeResult`, enums |
| `examples/` | Valid example payloads |
| `openapi/` | Public HTTP API (`prokuro-gateway`) |

**Workflow:** Define schemas here first (Epic S1), then implement types in `crates/prokuroTypes/`.

Tag releases: `schemas/v0.1.0`
