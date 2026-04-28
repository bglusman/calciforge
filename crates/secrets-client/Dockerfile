FROM rust:slim-bookworm AS builder

WORKDIR /app
COPY . .

# Build the non-MCP secret discovery CLI. This image intentionally exposes no
# HTTP service and never prints secret values.
RUN cargo build --release -p secrets-client --bin calciforge-secrets

# Runtime image
FROM debian:stable-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/calciforge-secrets /usr/local/bin/calciforge-secrets

ENTRYPOINT ["/usr/local/bin/calciforge-secrets"]
CMD ["help"]
