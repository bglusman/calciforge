FROM rust:slim-bookworm AS builder

WORKDIR /app
COPY . .

# Build runtime binaries used by packaged Docker installs and staging smokes.
# Build Calciforge separately so Matrix support is enabled on the main router
# without leaking that feature flag into unrelated crates.
RUN cargo build --release -p clashd -p security-proxy -p mcp-server -p paste-server \
    && cargo build --release -p secrets-client --bin calciforge-secrets \
    && cargo build --release -p calciforge --features channel-matrix

# Runtime image
FROM debian:stable-slim

RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/calciforge /usr/local/bin/calciforge
COPY --from=builder /app/target/release/security-proxy /usr/local/bin/security-proxy
COPY --from=builder /app/target/release/clashd /usr/local/bin/clashd
COPY --from=builder /app/target/release/mcp-server /usr/local/bin/mcp-server
COPY --from=builder /app/target/release/paste-server /usr/local/bin/paste-server
COPY --from=builder /app/target/release/calciforge-secrets /usr/local/bin/calciforge-secrets

# Create config directory
RUN mkdir -p /root/.config/calciforge /var/lib/calciforge

EXPOSE 8888 9001 18792 18793 18794 18795 18796 18797 18798

ENTRYPOINT ["/usr/local/bin/calciforge"]
CMD ["--config", "/root/.config/calciforge/config.toml"]
