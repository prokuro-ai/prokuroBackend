# 1.91+ required: aws-sdk-s3 and related crates need rustc 1.91.1
# bookworm builder matches debian:bookworm-slim runtime glibc (rust:1.91-slim uses trixie)
FROM rust:1.91-slim-bookworm AS builder
WORKDIR /app
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
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
