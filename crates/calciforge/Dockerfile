FROM rust:slim-bookworm AS builder

WORKDIR /app
COPY . .

# Build ZeroClawed
RUN cargo build --release -p zeroclawed

# Runtime image
FROM debian:stable-slim

RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/zeroclawed /usr/local/bin/zeroclawed

# Create config directory
RUN mkdir -p /root/.zeroclawed

EXPOSE 18792 18793 18794 18795 18796 18797

ENTRYPOINT ["/usr/local/bin/zeroclawed"]
CMD ["--config", "/root/.zeroclawed/config.toml"]
