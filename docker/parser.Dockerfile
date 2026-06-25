# 1.86+ required: Cargo.lock ICU/idna crates need rustc 1.86
FROM rust:1.86-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p prokuro-parser

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/prokuro-parser /usr/local/bin/prokuro-parser
COPY corpus/synonyms.toml /app/corpus/synonyms.toml
WORKDIR /app
ENV PORT=3001
ENV PROKURO_SYNONYMS_PATH=/app/corpus/synonyms.toml
EXPOSE 3001
CMD ["prokuro-parser"]
