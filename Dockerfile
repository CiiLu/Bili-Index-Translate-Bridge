FROM rust:bookworm AS builder

WORKDIR /app

COPY Cargo.toml Cargo.lock* ./
COPY src ./src

RUN cargo build --release

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY --from=builder /app/target/release/Bili-Index-Translate-Bridge /usr/local/bin/bili-index-translate-bridge

ENV HOST=0.0.0.0 \
    PORT=8080 \
    API_BASE="https://index-translate.bilibili.com/v1"

EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/bili-index-translate-bridge"]
