# Siku sync service — build image
FROM rust:1.78-slim AS builder
WORKDIR /build
COPY . /build
RUN cd /build/src-tauri/crates/sync-relay && cargo build --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /build/src-tauri/crates/sync-relay/target/release/siku-sync-relay /usr/local/bin/siku-sync-relay
EXPOSE 8080
ENV RELAY_HOST=0.0.0.0 \
    RELAY_PORT=8080 \
    JWT_SECRET=change-me \
    RELAY_DB_PATH=/data/accounts.json \
    RELAY_MAILBOX_DB_PATH=/data/mailbox.sqlite
VOLUME ["/data"]
HEALTHCHECK --interval=30s --timeout=3s CMD wget -qO- http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/siku-sync-relay"]
