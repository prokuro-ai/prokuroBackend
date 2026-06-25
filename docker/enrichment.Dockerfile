# 1.86+ required: Cargo.lock ICU/idna crates need rustc 1.86
FROM rust:1.86-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p prokuro-enrichment

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/prokuro-enrichment /usr/local/bin/prokuro-enrichment
ENV PORT=3002
EXPOSE 3002
CMD ["prokuro-enrichment"]
