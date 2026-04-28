FROM rust:slim-bookworm AS builder

WORKDIR /app
COPY . .

# Build Calciforge
RUN cargo build --release -p calciforge

# Runtime image
FROM debian:stable-slim

RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/calciforge /usr/local/bin/calciforge

# Create config directory
RUN mkdir -p /root/.calciforge

EXPOSE 18792 18793 18794 18795 18796 18797

ENTRYPOINT ["/usr/local/bin/calciforge"]
CMD ["--config", "/root/.calciforge/config.toml"]
