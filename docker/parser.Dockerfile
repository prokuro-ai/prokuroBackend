FROM rust:1.78-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p prokuro-parser

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/prokuro-parser /usr/local/bin/prokuro-parser
ENV PORT=3001
EXPOSE 3001
CMD ["prokuro-parser"]
