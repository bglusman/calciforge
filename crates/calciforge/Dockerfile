FROM rust:slim-bookworm AS builder

WORKDIR /app
COPY . .

# Build runtime binaries used by the staging smoke stack.
RUN cargo build --release -p calciforge -p security-proxy

# Runtime image
FROM debian:stable-slim

RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/calciforge /usr/local/bin/calciforge
COPY --from=builder /app/target/release/security-proxy /usr/local/bin/security-proxy

# Create config directory
RUN mkdir -p /root/.config/calciforge

EXPOSE 8888 18792 18793 18794 18795 18796 18797 18798

ENTRYPOINT ["/usr/local/bin/calciforge"]
CMD ["--config", "/root/.config/calciforge/config.toml"]
