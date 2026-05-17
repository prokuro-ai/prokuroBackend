FROM rust:bookworm AS builder
WORKDIR /app

COPY . .
RUN cargo build --release -p prokuro-parser

FROM debian:bookworm-slim
COPY --from=builder /app/target/release/prokuro-parser /usr/local/bin/prokuro-parser

EXPOSE 3001
CMD ["/usr/local/bin/prokuro-parser"]
