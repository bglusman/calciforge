//! Vault integration for credential retrieval via VaultWarden REST API

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    pub url: String,
    pub token: String,
}

impl Default for VaultConfig {
    fn default() -> Self {
        Self {
            url: std::env::var("ONECLI_VAULT_URL")
                .unwrap_or_else(|_| "https://vault.enjyn.com".to_string()),
            token: std::env::var("ONECLI_VAULT_TOKEN")
                .unwrap_or_default(),
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
    
    for cipher in vault_response.data {
        let cipher_name = cipher.name.to_lowercase();
        if cipher_name == name.to_lowercase() || cipher_name.contains(&name.to_lowercase()) {
            if let Some(login) = cipher.login {
                if let Some(password) = login.password {
                    return Ok(password);
                }
            }
            if let Some(notes) = cipher.notes {
                return Ok(notes);
            }
        }
    }
    
    anyhow::bail!("Secret '{}' not found in vault", name)
}

pub async fn vault_available() -> bool {
    let config = VaultConfig::default();
    !config.token.is_empty()
}

#[derive(Debug, Deserialize)]
struct VaultResponse {
    data: Vec<Cipher>,
}

#[derive(Debug, Deserialize)]
struct Cipher {
    name: String,
    #[serde(rename = "type")]
    type_: i32,
    login: Option<Login>,
    notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Login {
    username: Option<String>,
    password: Option<String>,
}
