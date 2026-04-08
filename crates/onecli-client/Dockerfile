FROM rust:slim-bookworm AS builder

WORKDIR /app
COPY . .

# Build OneCLI
RUN cargo build --release -p onecli-client

# Runtime image
FROM debian:stable-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/onecli-client /usr/local/bin/onecli

EXPOSE 8081

ENTRYPOINT ["/usr/local/bin/onecli"]
CMD ["serve"]
