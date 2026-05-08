---
layout: default
title: Manual Installation
---

# Manual Installation Guide

Prefer the unified installer whenever possible:

```bash
cd ~/projects/calciforge
bash scripts/install.sh --yes
```

Use this page only when you need to inspect or reproduce the service layout by
hand. The older `install-security-stack.sh` flow has been removed because it
targeted the retired `security-gateway` binary; current installs use
`calciforge`, `security-proxy`, and `clashd`.

## Prerequisites

- Rust toolchain on the build machine.
- `curl` and `systemctl` on Linux targets.
- Root or per-user service-manager access, depending on where you install.
- An existing Calciforge config at `/etc/calciforge/config.toml` or
  `~/.config/calciforge/config.toml`.

## Build

```bash
cd ~/projects/calciforge
cargo build --release -p calciforge -p security-proxy -p clashd
```

The adversary detector is linked into `security-proxy`; you do not need a
separate detector service for the default local scanner path.

## Copy Binaries

For a system install:

```bash
TARGET=gateway.example.internal

ssh root@$TARGET "mkdir -p /opt/calciforge/bin /etc/calciforge"

for bin in calciforge security-proxy clashd; do
    scp target/release/$bin root@$TARGET:/opt/calciforge/bin/$bin
    ssh root@$TARGET "chmod 0755 /opt/calciforge/bin/$bin"
done
```

For a per-user install, copy to `~/.local/bin` and use systemd user units or
LaunchAgents instead of the system units below.

## Linux Systemd Units

The unit names below match `scripts/install.sh` for Linux system installs.

Create `/etc/systemd/system/calciforge.service`:

```ini
[Unit]
Description=Calciforge Router
After=network.target calciforge-clashd.service calciforge-security-proxy.service
Wants=calciforge-clashd.service calciforge-security-proxy.service

[Service]
Type=simple
ExecStart=/opt/calciforge/bin/calciforge --config /etc/calciforge/config.toml
Environment=RUST_LOG=calciforge=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Create `/etc/systemd/system/calciforge-security-proxy.service`:

```ini
[Unit]
Description=Calciforge Security Proxy
After=network.target

[Service]
Type=simple
ExecStart=/opt/calciforge/bin/security-proxy
Environment=SECURITY_PROXY_BIND=127.0.0.1
Environment=SECURITY_PROXY_PORT=8888
Environment=SECURITY_PROXY_CA_CERT=/etc/calciforge/mitm-ca.pem
Environment=SECURITY_PROXY_CA_KEY=/etc/calciforge/mitm-ca-key.pem
Environment=CALCIFORGE_CONFIG_HOME=/etc/calciforge
Environment=AGENT_CONFIG=/etc/calciforge/agents.json
Environment=RUST_LOG=security_proxy=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Create `/etc/systemd/system/calciforge-clashd.service`:

```ini
[Unit]
Description=Calciforge Clash Policy Engine
After=network.target

[Service]
Type=simple
ExecStart=/opt/calciforge/bin/clashd
Environment=CLASHD_CONFIG=/etc/calciforge/agents.json
Environment=RUST_LOG=clashd=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

Enable and start:

```bash
systemctl daemon-reload
systemctl enable --now calciforge-clashd calciforge-security-proxy calciforge
```

## Agent Proxy Environment

Do not set `HTTP_PROXY` or `HTTPS_PROXY` globally for Calciforge itself.
Configure proxy variables only on the agent daemon that should be inspected.

For a manually managed agent:

```bash
export HTTP_PROXY=http://127.0.0.1:8888
export HTTPS_PROXY=http://127.0.0.1:8888
export ALL_PROXY=http://127.0.0.1:8888
export NO_PROXY=localhost,127.0.0.1,::1
```

Only set `HTTPS_PROXY` when that runtime trusts the Calciforge MITM CA. The
unified installer can generate and trust the CA for supported local runtimes;
manual installs must do that explicitly for the operating system, container, or
process trust store in use.

For installer-managed OpenClaw hosts, prefer the `--claw ...
proxy_endpoint=http://<calciforge-host>:8888` path from `scripts/install.sh`.
That path also writes OpenClaw browser proxy settings after verifying the proxy
is reachable from the target host.

## Verify

```bash
curl -fsS http://127.0.0.1:9001/health
curl -fsS http://127.0.0.1:8888/health
/opt/calciforge/bin/calciforge doctor --config /etc/calciforge/config.toml
```

The Calciforge router port depends on the configured channels and reply
webhooks, so `calciforge doctor` is the reliable service/config validation path.

## Troubleshooting

```bash
systemctl status calciforge
systemctl status calciforge-security-proxy
systemctl status calciforge-clashd

journalctl -u calciforge -f
journalctl -u calciforge-security-proxy -f
journalctl -u calciforge-clashd -f

ss -tlnp | grep -E '8888|9001|18797'
```

If HTTPS content is not being inspected, verify the agent process is actually
using the proxy and that its TLS stack trusts the Calciforge CA.
