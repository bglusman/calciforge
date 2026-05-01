---
layout: default
title: Manual Installation
---

# Manual Installation Guide (Fallback)

If the automated `install-security-stack.sh` fails, follow these steps manually on each target host.

## Prerequisites

- Root SSH access to target host
- Rust toolchain on build machine
- `curl`, `systemctl` on target host

## Step 1: Build (on build machine)

```bash
cd /root/projects/calciforge
cargo build --release -p adversary-detector -p security-gateway -p clashd
```

## Step 2: Copy binaries

```bash
TARGET=gateway.example.internal  # change per host

ssh -i ~/.ssh/id_ed25519 root@$TARGET "mkdir -p /opt/calciforge/bin /etc/calciforge"

for bin in adversary-detector security-gateway clashd; do
    scp -i ~/.ssh/id_ed25519 \
        target/release/$bin \
        root@$TARGET:/opt/calciforge/bin/$bin
done

scp -i ~/.ssh/id_ed25519 \
    crates/clashd/config/agents.json \
    root@$TARGET:/etc/calciforge/agents.json

scp -i ~/.ssh/id_ed25519 \
    crates/clashd/config/default-policy.star \
    root@$TARGET:/etc/calciforge/default-policy.star
```

## Step 3: Create systemd services

SSH into the target and create these three files:

### `/etc/systemd/system/adversary-detector.service`
```ini
[Unit]
Description=Calciforge Adversary Detector
After=network.target

[Service]
Type=simple
ExecStart=/opt/calciforge/bin/adversary-detector
Environment=ADVERSARY_DETECTOR_PORT=9800
Environment=RUST_LOG=adversary_detector=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

### `/etc/systemd/system/security-gateway.service`
```ini
[Unit]
Description=Calciforge Security Gateway
After=network.target adversary-detector.service

[Service]
Type=simple
ExecStart=/opt/calciforge/bin/security-gateway
Environment=AGENT_CONFIG=/etc/calciforge/agents.json
Environment=ADVERSARY_DETECTOR_PORT=9800
Environment=RUST_LOG=security_gateway=info
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

### `/etc/systemd/system/clashd.service`
```ini
[Unit]
Description=Calciforge Clashd Policy Engine
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

## Step 4: Enable and start services

```bash
systemctl daemon-reload
systemctl enable adversary-detector security-gateway clashd
systemctl start adversary-detector security-gateway clashd
```

## Step 5: Configure optional external-agent proxy env

Do not set `HTTP_PROXY` or `HTTPS_PROXY` globally for Calciforge itself. The
Calciforge service should call providers, channels, and control-plane endpoints
directly unless a stronger host/container boundary is configured.

For an external agent daemon that you have tested with `security-proxy`, set
proxy environment in that daemon's service manager instead of in
`/etc/profile.d`. For example:

```bash
export HTTP_PROXY=http://127.0.0.1:8080
export NO_PROXY=localhost,127.0.0.1,10.*.*.*,172.16.*.*,192.168.*.*
```

`HTTPS_PROXY` is intentionally omitted from the basic setup because standard
HTTPS proxying uses CONNECT tunnels that Calciforge cannot inspect without a
separate MITM design. Use explicit Calciforge fetch/tool integration for HTTPS
content that must be scanned or rewritten, or run the agent inside a controlled
container/VM profile that forces egress through Calciforge services.

## Step 6: Set API credentials

Edit `/etc/calciforge/agents.json` or set env vars:
```bash
export OPENAI_API_KEY=sk-...
export ANTHROPIC_API_KEY=sk-ant-...
```

## Step 7: Verify

```bash
curl -s http://127.0.0.1:9800/health  # adversary-detector
curl -s http://127.0.0.1:8080/health  # security-gateway
curl -s http://127.0.0.1:9001/health  # clashd
```

All should return JSON with `"status": "ok"`.

## Troubleshooting

```bash
# Check service status
systemctl status adversary-detector
systemctl status security-gateway
journalctl -u security-gateway -f  # live logs

# Check if port is listening
ss -tlnp | grep -E '8080|9001|9800'

# Test without proxy (bypass)
curl --noproxy '*' http://127.0.0.1:8080/health
```
