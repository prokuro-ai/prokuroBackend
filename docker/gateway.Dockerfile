FROM rust:1.78-slim AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p prokuro-gateway

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/prokuro-gateway /usr/local/bin/prokuro-gateway
ENV PORT=3000
EXPOSE 3000
CMD ["prokuro-gateway"]
