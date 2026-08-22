# ── Build stage ─────────────────────────────────────────────────────────────
FROM rust:1.78-slim AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

RUN cargo build --release

# ── Runtime stage ───────────────────────────────────────────────────────────
FROM debian:bookworm-slim

# wget 供容器健康检查使用（docker-compose.prod.yml 的 healthcheck）
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates wget \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app
COPY --from=builder /app/target/release/siku-sync-relay /usr/local/bin/siku-sync-relay

EXPOSE 8080

ENV RELAY_HOST=0.0.0.0
ENV RELAY_PORT=8080
ENV JWT_SECRET=siku-dev-secret-change-me

ENTRYPOINT ["/usr/local/bin/siku-sync-relay"]
