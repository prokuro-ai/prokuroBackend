# BOM sample files (optional)

Real CSV/XLSX files for **manual** or **batch** parser checks — not where unit tests live.

| Directory | Purpose |
|-----------|----------|
| `raw/` | Sample uploads |
| `expected/` | Hand-labeled expectations |

**Unit tests** belong next to parser code (`crates/prokuroParser/src/…` with `#[cfg(test)]` or `tests/`). Use small inline fixtures there.

Add files here when you want a shared library of ugly real-world BOMs across machines/CI — skip until you need it.
