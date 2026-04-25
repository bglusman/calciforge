//! Vault integration for credential retrieval via VaultWarden REST API

use serde::Deserialize;
use tracing::debug;

#[derive(Debug, Clone, Deserialize)]
pub struct VaultConfig {
    pub url: String,
    pub token: String,
}

impl Default for VaultConfig {
    fn default() -> Self {
        // IMPORTANT: no hardcoded default URL or token. Either env var
        // unset → empty string → the caller's `.is_empty()` guards fire
        // and we bail with a clear message. Setting a "helpful" default
        // URL here would silently route traffic to a deployment-specific
        // host (see CLAUDE.md "Hard-coded fallback URLs") and disclose
        // infrastructure to anyone reading the repo. The repo's git
        // history has a prior iteration where exactly this mistake was
        // made — don't re-add a "default" URL "for convenience".
        Self {
            url: std::env::var("SECRETS_VAULT_URL").unwrap_or_default(),
            token: std::env::var("SECRETS_VAULT_TOKEN").unwrap_or_default(),
        }
    }
}

pub async fn get_secret(name: &str) -> anyhow::Result<String> {
    let env_var = format!("{}_API_KEY", name.to_uppercase());
    if let Ok(token) = std::env::var(&env_var) {
        debug!("Found {} in environment", env_var);
        return Ok(token);
    }

    // Try fnox (encrypted local/remote secret store). Fallthrough on
    // any failure is intentional — fnox is optional infrastructure —
    // but we log at debug so real backend problems (auth, IO) are
    // recoverable during incident investigation rather than silent.
    match get_secret_from_fnox(name).await {
        Ok(secret) => {
            debug!("Found {} in fnox", name);
            return Ok(secret);
        }
        Err(e) => {
            debug!(
                secret = %name,
                error = %e,
                "fnox lookup failed, falling through to vaultwarden"
            );
        }
    }

    let config = VaultConfig::default();
    // Both URL and token are mandatory — no hardcoded default for either.
    // If env+fnox both miss and vaultwarden isn't fully configured, we
    // stop here with a message that names every variable the operator
    // could set to advance, so debugging doesn't require reading source.
    if config.url.is_empty() || config.token.is_empty() {
        anyhow::bail!(
            "Secret '{}' not found in env or fnox; vaultwarden fallback unavailable \
             (requires SECRETS_VAULT_URL and SECRETS_VAULT_TOKEN, currently {})",
            name,
            if config.url.is_empty() && config.token.is_empty() {
                "both unset"
            } else if config.url.is_empty() {
                "URL unset"
            } else {
                "token unset"
            }
        );
    }

    debug!("Looking up {} in VaultWarden at {}", name, config.url);

    let client = reqwest::Client::new();
    let response = client
        .get(format!("{}/api/ciphers", config.url))
        .header("Authorization", format!("Bearer {}", config.token))
        .send()
        .await?;

    if !response.status().is_success() {
        anyhow::bail!("VaultWarden API error: {}", response.status());
    }

    let vault_response: VaultResponse = response.json().await?;

    // First pass: exact name match
    for cipher in &vault_response.data {
        let cipher_name = cipher.name.to_lowercase();
        debug!(
            "Checking cipher: name={}, type={}",
            cipher_name, cipher._type
        );

        if cipher_name == name.to_lowercase() || cipher_name.contains(&name.to_lowercase()) {
            debug!("Found matching cipher: {}", cipher.name);

            // Try login.password first (most common for API keys)
            if let Some(login) = &cipher.login {
                if let Some(password) = &login.password {
                    debug!("Found password in login field");
                    return Ok(password.clone());
                }
                if let Some(username) = &login.username {
                    debug!("Found username, no password");
                    // Some API keys are stored as "username" in UI
                    if !username.is_empty() {
                        return Ok(username.clone());
                    }
                }
            }

            // Try secure note
            if let Some(notes) = &cipher.notes {
                debug!("Found notes field");
                return Ok(notes.clone());
            }

            // Try custom fields (often used for API keys in UI)
            if let Some(fields) = &cipher.fields {
                for field in fields {
                    debug!(
                        "Checking field: name={:?}, type={}",
                        field.name, field._type
                    );
                    // type 0 = text, type 1 = hidden (password)
                    // Check both - encrypted field names often default to type 0
                    if field.value.is_some() && !field.value.as_ref().unwrap().is_empty() {
                        return Ok(field.value.clone().unwrap());
                    }
                }
            }
        }
    }

    // Second pass: if no name match, look for any cipher with custom fields
    // (handles encrypted cipher names)
    debug!("No name match found, trying fallback search for ciphers with custom fields");
    for cipher in &vault_response.data {
        if let Some(fields) = &cipher.fields {
            for field in fields {
                if field.value.is_some() && !field.value.as_ref().unwrap().is_empty() {
                    debug!("Found cipher with custom field value (encrypted name)");
                    return Ok(field.value.clone().unwrap());
                }
            }
        }
    }

    anyhow::bail!("Secret '{}' not found in vault", name)
}

#[derive(Debug, Deserialize)]
struct VaultResponse {
    data: Vec<Cipher>,
}

#[derive(Debug, Deserialize)]
struct Cipher {
    name: String,
    #[serde(rename = "type")]
    _type: i32,
    login: Option<Login>,
    notes: Option<String>,
    #[serde(default)]
    fields: Option<Vec<Field>>,
}

#[derive(Debug, Deserialize)]
struct Field {
    name: Option<String>,
    #[serde(rename = "type")]
    _type: i32, // 0 = text, 1 = hidden/password
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Login {
    username: Option<String>,
    password: Option<String>,
}

/// Retrieve a secret from fnox (encrypted local/remote secret store).
///
/// fnox supports age encryption, AWS Secrets Manager, Azure Key Vault,
/// GCP Secret Manager, 1Password, Bitwarden, Infisical, HashiCorp Vault, etc.
///
/// Private: the only caller is `get_secret` above. Keeping this off the
/// crate's public surface means callers depend on the aggregated
/// resolver, not a specific backend (which would leak implementation
/// choice and couple consumers to fnox).
async fn get_secret_from_fnox(name: &str) -> anyhow::Result<String> {
    use std::process::Stdio;
    use tokio::process::Command;

    debug!("Looking up {} in fnox", name);

    let output = Command::new("fnox")
        .args(["get", name])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to execute fnox: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("fnox get {} failed: {}", name, stderr);
    }

    let secret = String::from_utf8(output.stdout)
        .map_err(|e| anyhow::anyhow!("fnox returned invalid UTF-8: {}", e))?
        .trim()
        .to_string();

    if secret.is_empty() {
        anyhow::bail!("fnox returned empty secret for {}", name);
    }

    Ok(secret)
}
