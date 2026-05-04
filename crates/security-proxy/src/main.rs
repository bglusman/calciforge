use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{error, info, warn};

use adversary_detector::{RateLimitConfig, ScannerCheckConfig, ScannerConfig};
use security_proxy::agent_config::AgentsConfig;
use security_proxy::config::GatewayConfig;
use security_proxy::mitm::{install_default_crypto_provider, load_rcgen_authority, serve_mitm};
use security_proxy::proxy::SecurityProxy;

/// Default location for the auto-generated MITM CA. Picked over
/// `/etc/calciforge/...` because the systemd unit installer ships the
/// CA into `/etc/calciforge/secrets/...` itself; if that's already
/// populated, operators set `SECURITY_PROXY_CA_*` and we never touch
/// `/var/lib`. The standalone-test path uses /var/lib so an ad-hoc
/// `cargo run -p security-proxy` works without root-owning /etc.
const DEFAULT_CA_DIR: &str = "/var/lib/calciforge";
const DEFAULT_CA_CERT: &str = "/var/lib/calciforge/ca.pem";
const DEFAULT_CA_KEY: &str = "/var/lib/calciforge/ca.key";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "security_proxy=info".into()),
        )
        .init();
    install_default_crypto_provider();

    // Load gateway config: TOML file first (deserialises every field
    // including `[security.agent_web]`), then env-var overrides on top
    // for the legacy/operator-friendly knobs. Env wins so a deployed
    // box can flip a runtime flag without re-deploying the file.
    //
    // SECURITY_PROXY_CONFIG → explicit path (preferred when set)
    // /etc/calciforge/security-proxy.toml → default
    // missing file → fall back to GatewayConfig::default()
    let config_path = std::env::var("SECURITY_PROXY_CONFIG")
        .unwrap_or_else(|_| "/etc/calciforge/security-proxy.toml".to_string());

    let mut config: GatewayConfig = match std::fs::read_to_string(&config_path) {
        Ok(toml_str) => match toml::from_str::<GatewayConfig>(&toml_str) {
            Ok(cfg) => {
                info!(
                    "loaded security-proxy config from {} ({} bytes)",
                    config_path,
                    toml_str.len()
                );
                cfg
            }
            Err(e) => {
                error!(
                    "failed to parse security-proxy config at {}: {}; using defaults",
                    config_path, e
                );
                GatewayConfig::default()
            }
        },
        Err(_) => {
            // File missing is the common case for fresh installs; not an error.
            GatewayConfig::default()
        }
    };

    // Env-var override for port keeps the legacy operator knob working
    // even when the TOML config sets it differently. SECURITY_PROXY_MITM_ENABLED
    // is no longer parsed: MITM is the only mode after PR #112.
    if let Ok(p) = std::env::var("SECURITY_PROXY_PORT") {
        if let Ok(p) = p.parse() {
            config.port = p;
        }
    }
    if let Ok(path) = std::env::var("SECURITY_PROXY_CA_CERT") {
        if !path.trim().is_empty() {
            config.ca_cert_path = Some(path);
        }
    }
    if let Ok(path) = std::env::var("SECURITY_PROXY_CA_KEY") {
        if !path.trim().is_empty() {
            config.ca_key_path = Some(path);
        }
    }
    if let Ok(url) = std::env::var("SECURITY_PROXY_REMOTE_SCANNER_URL") {
        if !url.trim().is_empty() {
            let fail_closed = std::env::var("SECURITY_PROXY_REMOTE_SCANNER_FAIL_CLOSED")
                .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
                .unwrap_or(false);
            if config.scanner_checks.is_empty() {
                config.scanner_checks = ScannerConfig::default_checks();
            }
            let already_configured = config.scanner_checks.iter().any(|check| {
                matches!(
                    check,
                    ScannerCheckConfig::RemoteHttp {
                        url: configured,
                        ..
                    } if configured == &url
                )
            });
            if !already_configured {
                config
                    .scanner_checks
                    .push(ScannerCheckConfig::RemoteHttp { url, fail_closed });
            }
        }
    }
    // Surface operator mistakes in policy config — e.g. "gate enabled
    // but matching list is empty so nothing is actually inspected".
    config.agent_web.warn_on_inconsistent_policy();

    let scanner_config = ScannerConfig {
        checks: config.scanner_checks.clone(),
        ..ScannerConfig::default()
    };

    // Load credentials config (host→secret mappings with injection methods).
    // Falls back to built-in provider table if file doesn't exist.
    let credentials_config_path = std::env::var("CALCIFORGE_CREDENTIALS_CONFIG")
        .ok()
        .or_else(|| {
            std::env::var("CALCIFORGE_CONFIG_HOME")
                .ok()
                .map(|home| format!("{home}/credentials.toml"))
        })
        .unwrap_or_else(|| "/etc/calciforge/credentials.toml".into());
    let credentials_config =
        security_proxy::credentials::CredentialInjector::load_config(&credentials_config_path);

    // Build unified security proxy
    let mut proxy = SecurityProxy::with_credentials_config(
        config.clone(),
        scanner_config,
        RateLimitConfig::default(),
        credentials_config,
    )
    .await;

    // Load credentials from ZEROGATE_KEY_* env vars (legacy)
    proxy.credentials.load_from_env();

    // Load from agents.json config
    let agents_config_path =
        std::env::var("AGENT_CONFIG").unwrap_or_else(|_| "/etc/calciforge/agents.json".into());

    if let Ok(agents_config) = AgentsConfig::load(&agents_config_path) {
        info!(
            "Loaded {} agent(s) from {}",
            agents_config.agents.len(),
            agents_config_path
        );

        for provider in agents_config.all_providers() {
            if let Ok(api_key) = std::env::var(&provider.env_key) {
                proxy.credentials.add(&provider.name, &api_key);
                info!(
                    "Loaded credential for {} from ${}",
                    provider.name, provider.env_key
                );
            } else {
                info!(
                    "No credential found for {} (${} not set)",
                    provider.name, provider.env_key
                );
            }
        }
    } else {
        error!(
            "Could not load agents config from {}, using env vars only",
            agents_config_path
        );
    }

    let state = Arc::new(proxy);

    // Resolve CA paths: configured → file → auto-generate at default
    // location → error. MITM is the only mode in 2026, so the binary
    // must come up with a CA on its own for ad-hoc testing while still
    // honoring the install-script-provisioned CA in production.
    let (cert_path, key_path) = resolve_or_generate_ca(
        config.ca_cert_path.as_deref(),
        config.ca_key_path.as_deref(),
    )?;
    let ca = load_rcgen_authority(&cert_path, &key_path)?;

    let bind_host = std::env::var("SECURITY_PROXY_BIND").unwrap_or_else(|_| "127.0.0.1".into());
    let addr = format!("{}:{}", bind_host, config.port).parse()?;
    serve_mitm(addr, state, ca).await?;
    Ok(())
}

/// Decide which CA cert/key paths to hand to the MITM authority loader.
///
/// Preference order:
///   1. Both `cert` and `key` env-overrides set → use them as-is. The
///      loader will surface a useful error if either path is missing.
///   2. Both unset and the default cert+key files already exist → load
///      the persistent on-disk pair.
///   3. Both unset and at least one is missing → generate a new
///      self-signed CA at the default location and persist it (key
///      mode 0600). This makes `cargo run -p security-proxy` work for
///      ad-hoc local testing without requiring the install script.
///   4. Default-dir not writeable → return an error pointing the
///      operator at `scripts/install.sh` (which provisions the CA in
///      `/etc/calciforge/secrets`).
///
/// The "exactly one configured" case is rejected explicitly — pairing a
/// custom cert with the default-location key (or vice-versa) is almost
/// always a misconfiguration and would race with the auto-generate path.
fn resolve_or_generate_ca(
    cert: Option<&str>,
    key: Option<&str>,
) -> anyhow::Result<(String, String)> {
    match (cert, key) {
        (Some(c), Some(k)) => Ok((c.to_string(), k.to_string())),
        (Some(_), None) | (None, Some(_)) => Err(anyhow::anyhow!(
            "SECURITY_PROXY_CA_CERT and SECURITY_PROXY_CA_KEY must both be set or both unset; \
             configuring only one of the pair is almost always a mistake"
        )),
        (None, None) => ensure_default_ca(),
    }
}

/// Load the default-location CA pair, generating it on first start if
/// absent. The generated cert is a self-signed CA with subject
/// `CN=Calciforge Local MITM CA`, valid for 10 years; the key is
/// written with mode 0600 to deny world/group access on multi-user
/// hosts. If the default directory isn't writeable we surface a clear
/// error that points the operator at the install script.
fn ensure_default_ca() -> anyhow::Result<(String, String)> {
    let cert_path = PathBuf::from(DEFAULT_CA_CERT);
    let key_path = PathBuf::from(DEFAULT_CA_KEY);
    if cert_path.exists() && key_path.exists() {
        info!(
            "Using existing MITM CA at {} / {}",
            cert_path.display(),
            key_path.display()
        );
        return Ok((
            cert_path.to_string_lossy().into_owned(),
            key_path.to_string_lossy().into_owned(),
        ));
    }
    if cert_path.exists() ^ key_path.exists() {
        return Err(anyhow::anyhow!(
            "incomplete MITM CA at default location: cert={} key={} (one exists, the other does not). \
             Delete the orphan or set SECURITY_PROXY_CA_CERT and SECURITY_PROXY_CA_KEY explicitly.",
            cert_path.display(),
            key_path.display()
        ));
    }

    if let Err(err) = std::fs::create_dir_all(DEFAULT_CA_DIR) {
        return Err(anyhow::anyhow!(
            "no MITM CA configured and default location {DEFAULT_CA_DIR} is not writeable ({err}); \
             set SECURITY_PROXY_CA_CERT/_KEY or run scripts/install.sh"
        ));
    }
    generate_persistent_ca(&cert_path, &key_path)?;
    warn!(
        "Generated new MITM CA at {} — install in agent trust store before agent traffic will work",
        cert_path.display()
    );
    Ok((
        cert_path.to_string_lossy().into_owned(),
        key_path.to_string_lossy().into_owned(),
    ))
}

/// Generate a self-signed CA pair and persist it. Uses `rcgen` directly
/// (already a dep of the crate via the hudsucker authority loader) so we
/// don't shell out to openssl. Key file gets 0600 to keep prying eyes
/// off in the multi-user case; cert file is 0644.
fn generate_persistent_ca(cert_path: &Path, key_path: &Path) -> anyhow::Result<()> {
    use rcgen::{BasicConstraints, CertificateParams, DistinguishedName, DnType, IsCa, KeyPair};

    let key_pair = KeyPair::generate().map_err(|e| anyhow::anyhow!("generate CA key: {e}"))?;
    let mut params = CertificateParams::default();
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, "Calciforge Local MITM CA");
    params.distinguished_name = dn;
    // Default not_before/not_after from rcgen is 4 years; explicitly
    // bump to ~10y so a long-lived dev box doesn't churn through
    // expirations annually. Operators who care about rotation use the
    // install-script CA, which sets its own validity window.
    let now = std::time::SystemTime::now();
    let in_ten_years = now + std::time::Duration::from_secs(60 * 60 * 24 * 365 * 10);
    params.not_before = now.into();
    params.not_after = in_ten_years.into();

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| anyhow::anyhow!("self-sign CA: {e}"))?;

    std::fs::write(cert_path, cert.pem())
        .map_err(|e| anyhow::anyhow!("write CA cert {}: {e}", cert_path.display()))?;

    // Open the key file with mode 0o600 ATOMICALLY at create time so
    // there is no window where the file is readable by group/world.
    // (Previously we wrote then chmod'd; depending on process umask,
    // another local process could have read the key in between.)
    write_key_with_restricted_perms(key_path, &key_pair.serialize_pem())
        .map_err(|e| anyhow::anyhow!("write CA key {}: {e}", key_path.display()))?;

    Ok(())
}

#[cfg(unix)]
fn write_key_with_restricted_perms(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn write_key_with_restricted_perms(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    // Non-Unix platforms: best-effort write. Windows ACL handling for
    // private keys is a separate matter; if/when this ships there, do
    // the equivalent of CreateFile with restrictive DACLs.
    std::fs::write(path, contents)
}
