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
        Self {
            url: std::env::var("ONECLI_VAULT_URL")
                .unwrap_or_else(|_| "https://vault.enjyn.com".to_string()),
            token: std::env::var("ONECLI_VAULT_TOKEN").unwrap_or_default(),
        }
    }
}

pub async fn get_secret(name: &str) -> anyhow::Result<String> {
    let env_var = format!("{}_API_KEY", name.to_uppercase());
    if let Ok(token) = std::env::var(&env_var) {
        debug!("Found {} in environment", env_var);
        return Ok(token);
    }

    let config = VaultConfig::default();
    if config.token.is_empty() {
        anyhow::bail!("No ONECLI_VAULT_TOKEN set and no env var for '{}'", name);
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
