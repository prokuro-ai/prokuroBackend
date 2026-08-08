# bookworm builder matches debian:bookworm-slim runtime glibc
FROM rust:1.91-slim-bookworm AS builder
WORKDIR /app
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates/ crates/
RUN cargo build --release -p prokuro-purchasing

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/prokuro-purchasing /usr/local/bin/prokuro-purchasing
ENV PORT=3004
EXPOSE 3004
CMD ["prokuro-purchasing"]
