FROM rust:slim-bookworm AS builder

ARG CARGO_BUILD_JOBS=1
ARG CARGO_BUILD_PROFILE=docker

WORKDIR /app
COPY . .

# Build runtime binaries used by packaged Docker installs and staging smokes.
# Build Calciforge separately so Matrix support is enabled on the main router
# without leaking that feature flag into unrelated crates.
RUN cargo build --profile "${CARGO_BUILD_PROFILE}" -j "${CARGO_BUILD_JOBS}" -p clashd -p security-proxy -p mcp-server -p paste-server \
    && cargo build --profile "${CARGO_BUILD_PROFILE}" -j "${CARGO_BUILD_JOBS}" -p secrets-client --bin calciforge-secrets \
    && cargo build --profile "${CARGO_BUILD_PROFILE}" -j "${CARGO_BUILD_JOBS}" -p calciforge --features channel-matrix \
    && mkdir -p /app/dist-bin \
    && cp "/app/target/${CARGO_BUILD_PROFILE}/calciforge" /app/dist-bin/calciforge \
    && cp "/app/target/${CARGO_BUILD_PROFILE}/security-proxy" /app/dist-bin/security-proxy \
    && cp "/app/target/${CARGO_BUILD_PROFILE}/clashd" /app/dist-bin/clashd \
    && cp "/app/target/${CARGO_BUILD_PROFILE}/mcp-server" /app/dist-bin/mcp-server \
    && cp "/app/target/${CARGO_BUILD_PROFILE}/paste-server" /app/dist-bin/paste-server \
    && cp "/app/target/${CARGO_BUILD_PROFILE}/calciforge-secrets" /app/dist-bin/calciforge-secrets

# Runtime image
FROM debian:stable-slim

ARG FNOX_VERSION=v1.23.1

RUN apt-get update && apt-get install -y ca-certificates curl tar && rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    arch="$(dpkg --print-architecture)"; \
    case "$arch" in \
        amd64) asset="fnox-x86_64-unknown-linux-gnu.tar.gz"; sha="021c4fe683e109b616a4b936117c60e63f351b4000bd0999f0110f3088fc7f58" ;; \
        arm64) asset="fnox-aarch64-unknown-linux-gnu.tar.gz"; sha="de4c06fc8851ad4be8109d077d0362ee8603ae0aac0cbffb715ac89e2238f2c4" ;; \
        *) echo "unsupported fnox architecture: $arch" >&2; exit 1 ;; \
    esac; \
    tmp="$(mktemp -d)"; \
    curl -fsSL "https://github.com/jdx/fnox/releases/download/${FNOX_VERSION}/${asset}" -o "$tmp/fnox.tar.gz"; \
    echo "$sha  $tmp/fnox.tar.gz" | sha256sum -c -; \
    tar -xzf "$tmp/fnox.tar.gz" -C "$tmp"; \
    install -m 0755 "$tmp/fnox" /usr/local/bin/fnox; \
    rm -rf "$tmp"

COPY --from=builder /app/dist-bin/calciforge /usr/local/bin/calciforge
COPY --from=builder /app/dist-bin/security-proxy /usr/local/bin/security-proxy
COPY --from=builder /app/dist-bin/clashd /usr/local/bin/clashd
COPY --from=builder /app/dist-bin/mcp-server /usr/local/bin/mcp-server
COPY --from=builder /app/dist-bin/paste-server /usr/local/bin/paste-server
COPY --from=builder /app/dist-bin/calciforge-secrets /usr/local/bin/calciforge-secrets
COPY LICENSE /usr/local/share/calciforge/LICENSE
COPY THIRD_PARTY_NOTICES.txt /usr/local/share/calciforge/THIRD_PARTY_NOTICES.txt

# Create config directory
RUN mkdir -p /root/.config/calciforge /var/lib/calciforge

EXPOSE 8888 9001 18792 18793 18794 18795 18796 18797 18798

ENTRYPOINT ["/usr/local/bin/calciforge"]
CMD ["--config", "/root/.config/calciforge/config.toml"]
