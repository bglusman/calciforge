# OneCLI VaultWarden Setup Guide

## Overview

OneCLI can retrieve secrets from two sources:
1. **Environment Variables** (plaintext, immediate, recommended for API keys)
2. **VaultWarden** (encrypted at rest, requires decryption for use)

## Current Limitation

VaultWarden's REST API returns **encrypted values**. To use vault-stored secrets for API calls, you need either:

- **Option A:** Store secrets in environment variables (simpler, no encryption at rest)
- **Option B:** Provide master password for decryption (more secure, more complex)

## Recommended Setup

### 1. Environment Variables (Works Today)

Edit `/etc/onecli/env`:

```bash
# VaultWarden connection (for listing ciphers)
ONECLI_VAULT_URL=https://vault.enjyn.com
ONECLI_VAULT_TOKEN=eyJ0eXAiOiJKV1QiLCJhbGc...

# API Keys (plaintext, used for proxy injection)
GITHUB_API_KEY=ghp_xxxxxxxxxxxxxxxxxxxx
OPENAI_API_KEY=sk-xxxxxxxxxxxxxxxxxxxx
BRAVE_API_KEY=BSxxxxxxxxxxxxxxxxxxxxxx
ANTHROPIC_API_KEY=sk-ant-xxxxxxxxxxxxxx
```

Restart OneCLI:
```bash
systemctl restart onecli
```

Test:
```bash
# This returns the plaintext key
curl http://localhost:8081/vault/github

# This proxies to GitHub API with Authorization header injected
curl http://localhost:8081/proxy/github/user/repos
```

### 2. VaultWarden-Only (Requires Decryption)

To use encrypted vault storage without env vars, OneCLI needs your master password to decrypt.

**Not yet implemented.** Would require:
- Adding `ONECLI_VAULT_MASTER_PASSWORD` to env
- Implementing AES-256-CBC decryption in Rust
- Decrypting cipher keys before field access

## How Retrieval Works

### Priority Order

1. **Environment Variable**: `{NAME}_API_KEY` (e.g., `GITHUB_API_KEY`)
2. **VaultWarden Lookup**: Matches cipher by name
3. **Cipher Fields Checked**:
   - `login.password`
   - `login.username`
   - `notes`
   - Custom fields (type 0 or 1)

### Field Types

| VaultWarden Field | API Field Path | Encrypted? |
|-------------------|----------------|------------|
| Login → Password | `login.password` | Yes |
| Login → Username | `login.username` | Yes |
| Notes | `notes` | Yes |
| Custom (text) | `fields[].value` (type=0) | Yes |
| Custom (hidden) | `fields[].value` (type=1) | Yes |

**All values are encrypted at rest.** The API returns ciphertext like:
```
2.IJpysvyNBS9KWmdGiIZJwA==|moYR2kToignZBIbeoV9mLN2u9OwHEAHHrryMXBcGCtsCk9DN6v7aWGMoV/PX0fUi|cYPVNV5bx99OJe3N0URmsNlybKiJfga5AXmEliEM80A=
```

## Getting a VaultWarden API Token

1. Log into VaultWarden web UI: https://vault.enjyn.com
2. Go to **Settings** → **My Account**
3. Scroll to **API Key** section
4. Click **View**
5. Copy the token (starts with `eyJ0eXAiOiJKV1Qi...`)

Token expires after ~2 hours. Refresh via OAuth:

```bash
curl -X POST https://vault.enjyn.com/identity/connect/token \
  -d "grant_type=client_credentials" \
  -d "client_id=user.xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx" \
  -d "client_secret=xxxxxxxxxxxxxxxx" \
  -d "scope=api" \
  -d "device_identifier=onecli-server" \
  -d "device_name=OneCLI" \
  -d "device_type=SDK"
```

## Testing

### Test Environment Variable
```bash
# Set a test key
export TEST_API_KEY="sk-test-12345"

# Verify retrieval
curl http://localhost:8081/vault/test
# {"secret":"test","status":"ok","token":"sk-test-12345"}
```

### Test VaultWarden Connection
```bash
# List available ciphers (requires ONECLI_VAULT_TOKEN)
curl http://localhost:8081/vault/ 2>&1 | head

# Retrieve specific cipher
curl http://localhost:8081/vault/github
# Returns encrypted value if stored in vault
```

### Test Proxy Injection
```bash
# Proxy request with credential injection
curl -v http://localhost:8081/proxy/openai/v1/models

# Check logs to verify injection
journalctl -u onecli -f
```

## Troubleshooting

### "Secret not found"
- Cipher name doesn't match lookup key
- Try lowercase/uppercase variations
- Check if cipher is in organization (different access)

### "Subscription token invalid" (Brave, etc.)
- API key is wrong or expired
- Verify in env file and restart OneCLI

### 401 from VaultWarden API
- `ONECLI_VAULT_TOKEN` expired
- Refresh token via OAuth flow

### Encrypted value returned
- Expected behavior for vault storage
- Use environment variable for plaintext
- Or implement decryption (future)

## Security Considerations

| Approach | At Rest | In Transit | In Memory |
|----------|---------|------------|-----------|
| Env Vars | Plaintext (file) | Encrypted (TLS) | Plaintext |
| VaultWarden | Encrypted | Encrypted | Encrypted* |

*Would require decryption implementation

## Future Improvements

1. **Master Password Decryption**: Add `ONECLI_VAULT_MASTER_PASSWORD` env var
2. **Bitwarden CLI Integration**: Use `bw get password` instead of REST API
3. **Key Derivation**: Cache derived keys for performance
4. **Organization Support**: Access org-level ciphers

## See Also

- [VaultWarden API Docs](https://github.com/dani-garcia/vaultwarden/wiki)
- [Bitwarden CLI](https://bitwarden.com/help/article/cli/)
- `src/vault.rs` - Implementation details