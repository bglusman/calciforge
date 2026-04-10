# Manual Installation Guide (Fallback)

If the automated `install-security-stack.sh` fails, follow these steps manually on each target host.

## Prerequisites

- Root SSH access to target host
- Rust toolchain on build machine
- `curl`, `systemctl` on target host

## Step 1: Build (on build machine)

```bash
cd /root/projects/zeroclawed
cargo build --release -p adversary-detector -p security-gateway -p clashd
```

## Step 2: Copy binaries

```bash
TARGET=192.168.1.210  # change per host

ssh -i ~/.ssh/id_ed25519 root@$TARGET "mkdir -p /opt/zeroclawed/bin /etc/zeroclawed"

for bin in adversary-detector security-gateway clashd; do
    scp -i ~/.ssh/id_ed25519 \
        target/release/$bin \
        root@$TARGET:/opt/zeroclawed/bin/$bin
done

scp -i ~/.ssh/id_ed25519 \
    crates/clashd/config/agents.json \
    root@$TARGET:/etc/zeroclawed/agents.json

scp -i ~/.ssh/id_ed25519 \
    crates/clashd/config/default-policy.star \
    root@$TARGET:/etc/zeroclawed/default-policy.star
```

## Step 3: Create systemd services

SSH into the target and create these three files:

### `/etc/systemd/system/adversary-detector.service`
```ini
[Unit]
Description=ZeroClawed Adversary Detector
After=network.target

[Service]
Type=simple
ExecStart=/opt/zeroclawed/bin/adversary-detector
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
Description=ZeroClawed Security Gateway
After=network.target adversary-detector.service

[Service]
Type=simple
ExecStart=/opt/zeroclawed/bin/security-gateway
Environment=AGENT_CONFIG=/etc/zeroclawed/agents.json
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
Description=ZeroClawed Clashd Policy Engine
After=network.target

[Service]
Type=simple
ExecStart=/opt/zeroclawed/bin/clashd
Environment=CLASHD_CONFIG=/etc/zeroclawed/agents.json
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

## Step 5: Set up proxy env vars

Create `/etc/profile.d/zeroclawed-proxy.sh`:
```bash
export HTTP_PROXY=http://127.0.0.1:8080
export HTTPS_PROXY=http://127.0.0.1:8080
export NO_PROXY=localhost,127.0.0.1,192.168.1.*,10.*.*.*
```

```bash
chmod +x /etc/profile.d/zeroclawed-proxy.sh
```

## Step 6: Set API credentials

Edit `/etc/zeroclawed/agents.json` or set env vars:
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
