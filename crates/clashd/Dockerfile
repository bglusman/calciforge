FROM rust:1.75-slim as builder

WORKDIR /app
COPY . .

# Install build dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# Build clashd
RUN cargo build --release --bin clashd

# Runtime stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y ca-certificates libssl3 && rm -rf /var/lib/apt/lists/*

# Copy binary from builder
COPY --from=builder /app/target/release/clashd /usr/local/bin/clashd

# Create non-root user
RUN useradd -m -u 1000 clashd
USER clashd

# Expose port
EXPOSE 9001

# Default command
ENTRYPOINT ["/usr/local/bin/clashd"]