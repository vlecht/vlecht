FROM docker.io/library/rust:1.96-slim AS builder
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY vlecht ./vlecht
COPY vlecht-atp ./vlecht-atp
COPY vlecht-db ./vlecht-db
COPY vlecht-git ./vlecht-git
RUN cargo build --release --locked --bin vlecht

FROM docker.io/library/debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY --from=builder /src/target/release/vlecht /usr/local/bin/vlecht

# Shares the Go knotserver's KNOT_SERVER_* env vocabulary so a pod spec
# transplant Just Works. DB + repos + host key persist under /app and
# /home/git/repositories (hostPath mounts in k8s).
ENV KNOT_SERVER_LISTEN_ADDR=0.0.0.0:5555 \
    KNOT_SERVER_SSH_PORT=2222 \
    KNOT_SERVER_DB_PATH=/app/vlecht.db \
    KNOT_REPO_SCAN_PATH=/home/git/repositories \
    VLECHT_SSH_HOST_KEY_PATH=/app/ssh-host-key

EXPOSE 5555 2222
ENTRYPOINT ["/usr/local/bin/vlecht", "server"]
