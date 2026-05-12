//! Secret resolver for credential retrieval via environment variables and fnox.

use tracing::debug;

pub async fn get_secret(name: &str) -> anyhow::Result<String> {
    let env_var = format!("{}_API_KEY", name.to_uppercase());
    if let Ok(token) = std::env::var(&env_var) {
        debug!("Found {} in environment", env_var);
        return Ok(token);
    }

    // Try fnox (encrypted local/remote secret store). fnox owns any
    // provider-specific storage integrations; Calciforge does not call
    // credential-store APIs directly from this resolver.
    match get_secret_from_fnox(name).await {
        Ok(secret) => {
            debug!("Found {} in fnox", name);
            Ok(secret)
        }
        Err(err) => {
            debug!(
                secret = %name,
                error = %err,
                "fnox lookup failed"
            );
            anyhow::bail!("Secret '{}' not found in env or fnox: {}", name, err);
        }
    }
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
    debug!("Looking up {} in fnox", name);
    crate::FnoxClient::new()
        .get(name)
        .await
        .map_err(|e| anyhow::anyhow!("fnox get {} failed: {}", name, e))
}
