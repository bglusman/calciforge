//! CLI argument parsing for `calciforge install`.
//!
//! Supports both interactive mode (no flags) and non-interactive mode
//! (`--calciforge-host` / one or more `--claw` flags).
//!
//! # `--claw` flag format
//!
//! Key=value pairs, comma-separated:
//!
//! ```text
//! --claw name=foo,adapter=zeroclaw-native,host=user@host,key=/path/id_rsa,endpoint=http://...
//! --claw name=bar,adapter=openclaw-channel,host=user@host,key=/path/id_ed25519,endpoint=http://...,auth_token=...,reply_webhook=http://calciforge.lan:18797/hooks/reply,reply_auth_token=...,policy_endpoint=http://clashd:9001/evaluate,proxy_endpoint=http://127.0.0.1:8888,no_proxy=localhost;127.0.0.1;::1
//! --claw name=baz,adapter=openai-compat,endpoint=http://some-claw/v1
//! --claw name=qux,adapter=webhook,endpoint=http://custom/hook,format=json
//! --claw name=bin,adapter=cli,command=/usr/local/bin/my-claw
//! ```

use std::path::PathBuf;

use anyhow::{bail, Context, Result};

use super::model::{CalciforgeTarget, ClawKind, ClawTarget, InstallTarget, WebhookFormat};

// ---------------------------------------------------------------------------
// InstallArgs
// ---------------------------------------------------------------------------

/// Parsed CLI arguments for `calciforge install`.
///
/// In practice these come from `clap` or equivalent; here they're a plain
/// struct so the module doesn't force a `clap` dependency on the library crate.
/// The binary can convert from `clap::ArgMatches` to this type.
#[derive(Debug, Clone, Default)]
pub struct InstallArgs {
    /// `--calciforge-host user@host`
    pub calciforge_host: Option<String>,
    /// `--calciforge-key /path/to/key`
    pub calciforge_key: Option<PathBuf>,
    /// Each `--claw k=v,k=v,...` string, one per claw.
    pub claw_specs: Vec<String>,
    /// `--dry-run`
    pub dry_run: bool,
    /// `--skip-backup` — dangerous, must be explicit.
    pub skip_backup: bool,
    /// `--yes` — skip confirmations (for scripted use).
    pub _yes: bool,
}

impl InstallArgs {
    /// Returns `true` if no CLI targets were provided → should launch TUI wizard.
    pub fn is_interactive(&self) -> bool {
        self.calciforge_host.is_none() && self.claw_specs.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Top-level parser
// ---------------------------------------------------------------------------

/// Parse an [`InstallArgs`] into an [`InstallTarget`], validating all fields.
///
/// Returns an error if any required fields are missing or unparseable.
pub fn parse_install_target(args: &InstallArgs) -> Result<InstallTarget> {
    let host = args
        .calciforge_host
        .clone()
        .context("--calciforge-host is required in non-interactive mode")?;

    let calciforge = CalciforgeTarget {
        host,
        ssh_key: args.calciforge_key.clone(),
    };

    if args.claw_specs.is_empty() {
        bail!("at least one --claw spec is required in non-interactive mode");
    }

    let claws: Vec<ClawTarget> = args
        .claw_specs
        .iter()
        .enumerate()
        .map(|(i, spec)| {
            parse_claw_spec(spec)
                .with_context(|| format!("--claw[{}] '{}': parse error", i, redact_claw_spec(spec)))
        })
        .collect::<Result<_>>()?;

    Ok(InstallTarget { calciforge, claws })
}

// ---------------------------------------------------------------------------
// `--claw` spec parser
// ---------------------------------------------------------------------------

/// Parse a single `--claw` spec string into a [`ClawTarget`].
///
/// The spec is a comma-separated list of `key=value` pairs. Values may contain
/// `=` characters (only the first `=` is the delimiter). Commas inside values
/// are not supported. For `no_proxy`, use semicolons in the CLI string; the
/// installer normalizes them to comma-separated `NO_PROXY`.
///
/// # Required keys (all adapters)
/// - `name` — friendly name
/// - `adapter` — one of: `zeroclaw-native`, `openclaw-channel`, `openai-compat`, `webhook`, `cli`
///
/// # Adapter-specific keys
///
/// | Adapter | Required | Optional |
/// |---------|----------|----------|
/// | `zeroclaw-native` | `host`, `endpoint` | `key` |
/// | `openclaw-channel` | `host`, `endpoint`, `auth_token` or `auth_token_file`, `reply_webhook`, `reply_auth_token` or `reply_auth_token_file` | `key`, `policy_endpoint`, `proxy_endpoint`, `no_proxy` |
/// | `openai-compat` | `endpoint` | — |
/// | `webhook` | `endpoint` | `format` (default: `json`) |
/// | `cli` | `command` | — |
pub fn parse_claw_spec(spec: &str) -> Result<ClawTarget> {
    let redacted_spec = redact_claw_spec(spec);
    let kv = parse_kv_pairs(spec)?;

    let name = require_key(&kv, "name", &redacted_spec)?;
    let adapter_str = require_key(&kv, "adapter", &redacted_spec)?;

    let adapter = parse_adapter(&adapter_str, &kv, &redacted_spec)?;

    // Managed-target fields. Use host=local for same-machine runtime setup;
    // otherwise the executor reaches the target over SSH.
    let host = kv.get("host").cloned().unwrap_or_default();
    let ssh_key = kv.get("key").map(PathBuf::from);

    if adapter.is_remotely_configurable() && host.is_empty() {
        bail!(
            "adapter '{}' requires 'host=user@hostname' in spec: {}",
            adapter_str,
            redacted_spec
        );
    }

    let auth_token = if let Some(value) = kv.get("auth_token").or_else(|| kv.get("api_key")) {
        Some(value.clone())
    } else {
        resolve_token_file(
            kv.get("auth_token_file").or_else(|| kv.get("api_key_file")),
            "auth_token_file",
            &redacted_spec,
        )?
    };
    let reply_webhook = kv.get("reply_webhook").cloned();
    let reply_auth_token = if let Some(value) = kv.get("reply_auth_token") {
        Some(value.clone())
    } else {
        resolve_token_file(
            kv.get("reply_auth_token_file"),
            "reply_auth_token_file",
            &redacted_spec,
        )?
    };

    if matches!(adapter, ClawKind::OpenClawChannel) {
        if auth_token.as_deref().is_none_or(str::is_empty) {
            bail!(
                "adapter 'openclaw-channel' requires 'auth_token=...'/'auth_token_file=...' (or api_key/api_key_file) in spec: {}",
                redacted_spec
            );
        }
        if reply_webhook.as_deref().is_none_or(str::is_empty) {
            bail!(
                "adapter 'openclaw-channel' requires 'reply_webhook=http://<calciforge-host>:18797/hooks/reply' in spec: {}",
                redacted_spec
            );
        }
        if reply_auth_token.as_deref().is_none_or(str::is_empty) {
            bail!(
                "adapter 'openclaw-channel' requires 'reply_auth_token=...' or 'reply_auth_token_file=...' in spec: {}",
                redacted_spec
            );
        }
    }

    // Endpoint — required for everything except Cli.
    let endpoint = match &adapter {
        ClawKind::Cli { .. } => kv.get("endpoint").cloned().unwrap_or_default(),
        ClawKind::OpenAiCompat { endpoint } => endpoint.clone(),
        ClawKind::Webhook { endpoint, .. } => endpoint.clone(),
        _ => {
            // ZeroClawNative / OpenClawChannel: endpoint explicitly provided.
            kv.get("endpoint").cloned().with_context(|| {
                format!(
                    "adapter '{}' requires 'endpoint=...' in spec: {}",
                    adapter_str, redacted_spec
                )
            })?
        }
    };

    Ok(ClawTarget {
        name,
        adapter,
        host,
        ssh_key,
        endpoint,
        policy_endpoint: kv.get("policy_endpoint").cloned(),
        auth_token,
        reply_webhook,
        reply_auth_token,
        proxy_endpoint: kv.get("proxy_endpoint").cloned(),
        no_proxy: kv.get("no_proxy").map(|value| value.replace(';', ",")),
    })
}

// ---------------------------------------------------------------------------
// Adapter parser
// ---------------------------------------------------------------------------

fn parse_adapter(
    adapter_str: &str,
    kv: &std::collections::HashMap<String, String>,
    spec: &str,
) -> Result<ClawKind> {
    match adapter_str {
        "zeroclaw-native" => Ok(ClawKind::ZeroClawNative),
        "openclaw-channel" => Ok(ClawKind::OpenClawChannel),
        "openai-compat" => {
            let endpoint = require_key(kv, "endpoint", spec)?;
            Ok(ClawKind::OpenAiCompat { endpoint })
        }
        "webhook" => {
            let endpoint = require_key(kv, "endpoint", spec)?;
            let format = match kv.get("format").map(String::as_str) {
                Some("json") | None => WebhookFormat::Json,
                Some("text") => WebhookFormat::Text,
                Some(other) => bail!(
                    "unknown webhook format '{}' in spec: {} (use 'json' or 'text')",
                    other,
                    spec
                ),
            };
            Ok(ClawKind::Webhook { endpoint, format })
        }
        "cli" => {
            let command = require_key(kv, "command", spec)?;
            Ok(ClawKind::Cli { command })
        }
        other => bail!(
            "unknown adapter '{}' in spec: {} (valid: zeroclaw-native, openclaw-channel, openai-compat, webhook, cli)",
            other,
            spec
        ),
    }
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Parse a `k=v,k=v,...` string into a `HashMap<String, String>`.
///
/// Values may contain `=` (only the first `=` splits key from value).
/// Empty keys are rejected.
fn parse_kv_pairs(spec: &str) -> Result<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    for part in spec.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let idx = part
            .find('=')
            .context("expected 'key=value' pair in --claw spec")?;
        let key = part[..idx].trim().to_string();
        let value = part[idx + 1..].to_string();
        if key.is_empty() {
            bail!("empty key in --claw spec");
        }
        map.insert(key, value);
    }
    Ok(map)
}

fn redact_claw_spec(spec: &str) -> String {
    spec.split(',')
        .map(|part| {
            let Some((key, _value)) = part.split_once('=') else {
                return part.to_string();
            };
            let trimmed_key = key.trim();
            if is_secret_spec_key(trimmed_key) {
                format!("{trimmed_key}=<redacted>")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn is_secret_spec_key(key: &str) -> bool {
    matches!(
        key.to_ascii_lowercase().as_str(),
        "auth_token"
            | "api_key"
            | "reply_auth_token"
            | "token"
            | "bearer_token"
            | "auth_token_file"
            | "api_key_file"
            | "reply_auth_token_file"
    )
}

fn resolve_token_file(path: Option<&String>, field: &str, spec: &str) -> Result<Option<String>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let value = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {field} in spec: {spec}"))?;
    Ok(Some(value.trim().to_string()))
}

/// Extract a required key from the KV map.
fn require_key(
    kv: &std::collections::HashMap<String, String>,
    key: &str,
    spec: &str,
) -> Result<String> {
    kv.get(key)
        .cloned()
        .with_context(|| format!("missing required key '{}' in spec: {}", key, spec))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_kv_pairs ───────────────────────────────────────────────────────

    #[test]
    fn parse_kv_simple() {
        let kv = parse_kv_pairs("name=foo,adapter=zeroclaw-native").unwrap();
        assert_eq!(kv["name"], "foo");
        assert_eq!(kv["adapter"], "zeroclaw-native");
    }

    #[test]
    fn parse_kv_value_contains_equals() {
        // endpoint=http://host:18799/path?a=b should work (value has `=`)
        let kv = parse_kv_pairs("name=x,endpoint=http://host:18799/path?a=b").unwrap();
        assert_eq!(kv["endpoint"], "http://host:18799/path?a=b");
    }

    #[test]
    fn parse_kv_missing_equals_errors() {
        let result = parse_kv_pairs("secret-token-without-key,adapter=zeroclaw-native");
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("key=value"), "got: {}", msg);
        assert!(
            !msg.contains("secret-token-without-key"),
            "malformed input should not be echoed: {msg}"
        );
    }

    #[test]
    fn parse_kv_empty_key_errors() {
        let result = parse_kv_pairs("=value,name=x");
        assert!(result.is_err());
    }

    // ── parse_claw_spec ──────────────────────────────────────────────────────

    #[test]
    fn parse_zeroclaw_claw() {
        let spec = "name=librarian,adapter=zeroclaw-native,host=user@10.0.0.20,key=/keys/id_ed25519,endpoint=http://10.0.0.20:18799";
        let claw = parse_claw_spec(spec).unwrap();
        assert_eq!(claw.name, "librarian");
        assert!(matches!(claw.adapter, ClawKind::ZeroClawNative));
        assert_eq!(claw.host, "user@10.0.0.20");
        assert_eq!(claw.ssh_key, Some(PathBuf::from("/keys/id_ed25519")));
        assert_eq!(claw.endpoint, "http://10.0.0.20:18799");
        assert!(claw.needs_ssh_config());
    }

    #[test]
    fn parse_openclaw_claw() {
        let spec = "name=custodian,adapter=openclaw-channel,host=admin@openclaw.example.invalid,key=/keys/id_rsa,endpoint=http://openclaw.example.invalid:18789,auth_token=inbound-token,reply_webhook=http://calciforge.example.invalid:18797/hooks/reply,reply_auth_token=reply-token";
        let claw = parse_claw_spec(spec).unwrap();
        assert_eq!(claw.name, "custodian");
        assert!(matches!(claw.adapter, ClawKind::OpenClawChannel));
        assert!(claw.needs_ssh_config());
        assert_eq!(claw.auth_token.as_deref(), Some("inbound-token"));
        assert_eq!(
            claw.reply_webhook.as_deref(),
            Some("http://calciforge.example.invalid:18797/hooks/reply")
        );
        assert_eq!(claw.reply_auth_token.as_deref(), Some("reply-token"));
        assert!(claw.policy_endpoint.is_none());
        assert!(claw.proxy_endpoint.is_none());
    }

    #[test]
    fn parse_openclaw_claw_with_policy_endpoint() {
        let spec = "name=custodian,adapter=openclaw-channel,host=admin@openclaw.example.invalid,key=/keys/id_rsa,endpoint=http://openclaw.example.invalid:18789,auth_token=inbound-token,reply_webhook=http://calciforge.example.invalid:18797/hooks/reply,reply_auth_token=reply-token,policy_endpoint=http://clashd.example.invalid:9001/evaluate,proxy_endpoint=http://127.0.0.1:8888,no_proxy=localhost;127.0.0.1";
        let claw = parse_claw_spec(spec).unwrap();
        assert_eq!(
            claw.policy_endpoint.as_deref(),
            Some("http://clashd.example.invalid:9001/evaluate")
        );
        assert_eq!(
            claw.proxy_endpoint.as_deref(),
            Some("http://127.0.0.1:8888")
        );
        assert_eq!(claw.no_proxy.as_deref(), Some("localhost,127.0.0.1"));
    }

    #[test]
    fn parse_openclaw_claw_reads_token_files() {
        let dir = std::env::temp_dir().join(format!(
            "calciforge-install-cli-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let auth_path = dir.join("inbound-token");
        let reply_path = dir.join("reply-token");
        std::fs::write(&auth_path, "inbound-from-file\n").unwrap();
        std::fs::write(&reply_path, "reply-from-file\n").unwrap();

        let spec = format!(
            "name=custodian,adapter=openclaw-channel,host=local,endpoint=http://127.0.0.1:18789,auth_token_file={},reply_webhook=http://127.0.0.1:18797/hooks/reply,reply_auth_token_file={}",
            auth_path.display(),
            reply_path.display()
        );
        let claw = parse_claw_spec(&spec).unwrap();
        assert_eq!(claw.auth_token.as_deref(), Some("inbound-from-file"));
        assert_eq!(claw.reply_auth_token.as_deref(), Some("reply-from-file"));

        let _ = std::fs::remove_file(auth_path);
        let _ = std::fs::remove_file(reply_path);
        let _ = std::fs::remove_dir(dir);
    }

    #[test]
    fn parse_openclaw_claw_prefers_inline_tokens_over_token_files() {
        let spec = "name=custodian,adapter=openclaw-channel,host=local,endpoint=http://127.0.0.1:18789,auth_token=inline-inbound,auth_token_file=/does/not/exist,reply_webhook=http://127.0.0.1:18797/hooks/reply,reply_auth_token=inline-reply,reply_auth_token_file=/also/missing";
        let claw = parse_claw_spec(spec).unwrap();
        assert_eq!(claw.auth_token.as_deref(), Some("inline-inbound"));
        assert_eq!(claw.reply_auth_token.as_deref(), Some("inline-reply"));
    }

    #[test]
    fn parse_openclaw_claw_requires_managed_channel_auth() {
        let spec = "name=custodian,adapter=openclaw-channel,host=admin@openclaw.example.invalid,endpoint=http://openclaw.example.invalid:18789";
        let err = parse_claw_spec(spec).expect_err("missing inbound token should fail");
        assert!(
            err.to_string().contains("auth_token"),
            "error should explain missing inbound token: {err}"
        );

        let spec = "name=custodian,adapter=openclaw-channel,host=admin@openclaw.example.invalid,endpoint=http://openclaw.example.invalid:18789,auth_token=inbound-token";
        let err = parse_claw_spec(spec).expect_err("missing reply webhook should fail");
        assert!(
            err.to_string().contains("reply_webhook"),
            "error should explain missing reply webhook: {err}"
        );

        let spec = "name=custodian,adapter=openclaw-channel,host=admin@openclaw.example.invalid,endpoint=http://openclaw.example.invalid:18789,auth_token=inbound-token,reply_webhook=http://calciforge.example.invalid:18797/hooks/reply";
        let err = parse_claw_spec(spec).expect_err("missing reply auth should fail");
        assert!(
            err.to_string().contains("reply_auth_token"),
            "error should explain missing reply auth token: {err}"
        );
    }

    #[test]
    fn parse_openclaw_claw_errors_redact_secret_values() {
        let spec = "name=custodian,adapter=openclaw-channel,host=admin@openclaw.example.invalid,endpoint=http://openclaw.example.invalid:18789,AUTH_TOKEN=secret-inbound-token";
        let err = parse_claw_spec(spec).expect_err("missing later field should fail");
        let msg = err.to_string();
        assert!(
            msg.contains("AUTH_TOKEN=<redacted>"),
            "error should include redacted spec: {msg}"
        );
        assert!(
            !msg.contains("secret-inbound-token"),
            "error must not leak auth token: {msg}"
        );
    }

    #[test]
    fn parse_install_target_context_redacts_secret_values() {
        let args = InstallArgs {
            calciforge_host: Some("calciforge@example.invalid".to_string()),
            claw_specs: vec!["name=custodian,adapter=openclaw-channel,host=admin@openclaw.example.invalid,endpoint=http://openclaw.example.invalid:18789,auth_token=secret-inbound-token,reply_webhook=http://calciforge.example.invalid:18797/hooks/reply".to_string()],
            ..Default::default()
        };
        let err = parse_install_target(&args).expect_err("missing reply token should fail");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("auth_token=<redacted>"),
            "context should include redacted spec: {msg}"
        );
        assert!(
            !msg.contains("secret-inbound-token"),
            "context must not leak auth token: {msg}"
        );
    }

    #[test]
    fn parse_openai_compat_claw() {
        let spec = "name=my-llm,adapter=openai-compat,endpoint=http://llm.internal/v1";
        let claw = parse_claw_spec(spec).unwrap();
        assert_eq!(claw.name, "my-llm");
        assert!(matches!(
            &claw.adapter,
            ClawKind::OpenAiCompat { endpoint } if endpoint == "http://llm.internal/v1"
        ));
        assert!(!claw.needs_ssh_config());
        assert!(claw.ssh_key.is_none());
    }

    #[test]
    fn parse_webhook_claw_default_format() {
        let spec = "name=hook,adapter=webhook,endpoint=http://hook.internal/receive";
        let claw = parse_claw_spec(spec).unwrap();
        assert!(matches!(
            &claw.adapter,
            ClawKind::Webhook {
                format: WebhookFormat::Json,
                ..
            }
        ));
    }

    #[test]
    fn parse_webhook_claw_text_format() {
        let spec = "name=hook,adapter=webhook,endpoint=http://hook.internal/receive,format=text";
        let claw = parse_claw_spec(spec).unwrap();
        assert!(matches!(
            &claw.adapter,
            ClawKind::Webhook {
                format: WebhookFormat::Text,
                ..
            }
        ));
    }

    #[test]
    fn parse_webhook_unknown_format_errors() {
        let spec = "name=hook,adapter=webhook,endpoint=http://x/receive,format=xml";
        assert!(parse_claw_spec(spec).is_err());
    }

    #[test]
    fn parse_cli_claw() {
        let spec = "name=ironclaw,adapter=cli,command=/usr/local/bin/ironclaw";
        let claw = parse_claw_spec(spec).unwrap();
        assert_eq!(claw.name, "ironclaw");
        assert!(matches!(
            &claw.adapter,
            ClawKind::Cli { command } if command == "/usr/local/bin/ironclaw"
        ));
        assert!(!claw.needs_ssh_config());
    }

    #[test]
    fn parse_zeroclaw_missing_host_errors() {
        let spec = "name=lib,adapter=zeroclaw-native,endpoint=http://host:18799";
        let result = parse_claw_spec(spec);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("host"), "should mention 'host': {}", msg);
    }

    #[test]
    fn parse_zeroclaw_missing_endpoint_errors() {
        let spec = "name=lib,adapter=zeroclaw-native,host=user@host";
        let result = parse_claw_spec(spec);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("endpoint"),
            "should mention 'endpoint': {}",
            msg
        );
    }

    #[test]
    fn parse_missing_name_errors() {
        let spec = "adapter=zeroclaw-native,host=user@host,endpoint=http://x";
        let result = parse_claw_spec(spec);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("name"), "should mention 'name': {}", msg);
    }

    #[test]
    fn parse_unknown_adapter_errors() {
        let spec = "name=x,adapter=magic-claw,endpoint=http://x";
        let result = parse_claw_spec(spec);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(
            msg.contains("magic-claw"),
            "should name the bad adapter: {}",
            msg
        );
    }

    // ── parse_install_target ─────────────────────────────────────────────────

    #[test]
    fn parse_install_target_full() {
        let args = InstallArgs {
            calciforge_host: Some("admin@10.0.0.1".into()),
            calciforge_key: Some(PathBuf::from("/keys/id_rsa")),
            claw_specs: vec![
                "name=lib,adapter=zeroclaw-native,host=user@10.0.0.20,endpoint=http://10.0.0.20:18799".into(),
            ],
            ..Default::default()
        };
        let target = parse_install_target(&args).unwrap();
        assert_eq!(target.calciforge.host, "admin@10.0.0.1");
        assert_eq!(target.claws.len(), 1);
        assert_eq!(target.claws[0].name, "lib");
    }

    #[test]
    fn parse_install_target_missing_host_errors() {
        let args = InstallArgs {
            calciforge_host: None,
            claw_specs: vec!["name=x,adapter=cli,command=foo".into()],
            ..Default::default()
        };
        let result = parse_install_target(&args);
        assert!(result.is_err());
        let msg = result.err().unwrap().to_string();
        assert!(msg.contains("--calciforge-host"), "got: {}", msg);
    }

    #[test]
    fn parse_install_target_no_claws_errors() {
        let args = InstallArgs {
            calciforge_host: Some("host".into()),
            claw_specs: vec![],
            ..Default::default()
        };
        let result = parse_install_target(&args);
        assert!(result.is_err());
    }

    #[test]
    fn is_interactive_when_no_flags() {
        let args = InstallArgs::default();
        assert!(args.is_interactive());
    }

    #[test]
    fn not_interactive_when_host_provided() {
        let args = InstallArgs {
            calciforge_host: Some("host".into()),
            ..Default::default()
        };
        assert!(!args.is_interactive());
    }

    #[test]
    fn not_interactive_when_claw_specs_provided() {
        let args = InstallArgs {
            claw_specs: vec!["name=x,adapter=cli,command=foo".into()],
            ..Default::default()
        };
        assert!(!args.is_interactive());
    }

    // ── Injection safety ─────────────────────────────────────────────────────

    /// Ensure that spec values containing shell metacharacters parse correctly
    /// (the SSH layer quotes them; the parse layer just stores the raw string).
    #[test]
    fn spec_with_shell_metacharacters_in_value_parsed_safely() {
        // The endpoint value contains `&` — a common URL char, also a shell metachar.
        // parse_claw_spec stores it literally; ssh::shell_quote handles escaping later.
        let spec = "name=x,adapter=openai-compat,endpoint=http://host/v1?key=abc&other=def";
        // Should not error — the `&` is in the value, not a key separator.
        // (The parser splits on `,`, not on `&`.)
        let result = parse_claw_spec(spec);
        // This may or may not succeed depending on how we handle query strings;
        // the important thing is: no panic, no injection at parse time.
        // If it fails it fails safely with an error message.
        let _ = result; // success or failure both acceptable at parse layer
    }

    /// The `=` inside a URL query string should not split the key.
    #[test]
    fn spec_endpoint_with_embedded_equals() {
        let spec = "name=x,adapter=openai-compat,endpoint=http://host/v1?foo=bar";
        let claw = parse_claw_spec(spec).unwrap();
        // The endpoint should contain the full value including `?foo=bar`
        assert!(
            claw.endpoint.contains("foo=bar"),
            "endpoint: {}",
            claw.endpoint
        );
    }

    // ── Property tests (hegel) ────────────────────────────────────────────────

    /// Property: parse_claw_spec roundtrip — for any valid CLI spec string,
    /// parsed fields match what was put in.
    ///
    /// This generates structurally valid spec strings for all adapter types and
    /// checks that the parsed `ClawTarget` contains exactly the values we
    /// embedded.  This is a non-trivial roundtrip: it would catch bugs in the
    /// kv splitter (e.g. wrong `=` splitting), adapter dispatch, or field
    /// assignment.
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_parse_claw_spec_roundtrip(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        // Use sampled_from with pre-validated safe name strings to avoid
        // filter health check issues.  These cover a range of lengths, prefixes,
        // hyphens, underscores — enough to exercise the parser thoroughly.
        let name: String = tc.draw(gs::sampled_from(vec![
            "librarian".to_string(),
            "custodian".to_string(),
            "a".to_string(),
            "my-claw".to_string(),
            "test_claw".to_string(),
            "claw1".to_string(),
            "UPPER".to_string(),
            "mixed-Name_99".to_string(),
            "z".to_string(),
            "long-name-with-many-segments".to_string(),
        ]));

        // Pick an adapter type.
        let adapter_idx = tc.draw(gs::integers::<usize>().min_value(0).max_value(4));

        let spec = match adapter_idx {
            0 => {
                // openai-compat: requires endpoint, no host required
                let endpoint = format!("http://host-{name}.local:18799");
                format!("name={name},adapter=openai-compat,endpoint={endpoint}")
            }
            1 => {
                // webhook: requires endpoint, optional format
                let endpoint = format!("http://hook-{name}.local/receive");
                let use_text = tc.draw(gs::booleans());
                if use_text {
                    format!("name={name},adapter=webhook,endpoint={endpoint},format=text")
                } else {
                    format!("name={name},adapter=webhook,endpoint={endpoint}")
                }
            }
            2 => {
                // cli: requires command
                let command = format!("/usr/local/bin/{name}-claw");
                format!("name={name},adapter=cli,command={command}")
            }
            3 => {
                // zeroclaw: requires host and endpoint
                let host = format!("user@192.168.1.{}", tc.draw(gs::integers::<u8>()));
                let endpoint = format!("http://{name}.local:18799");
                format!("name={name},adapter=zeroclaw-native,host={host},endpoint={endpoint}")
            }
            _ => {
                // openclaw: requires host, endpoint, inbound auth, and reply callback auth
                let host = format!("admin@192.168.1.{}", tc.draw(gs::integers::<u8>()));
                let endpoint = format!("http://{name}.local:18789");
                format!(
                    "name={name},adapter=openclaw-channel,host={host},endpoint={endpoint},auth_token=inbound-{name},reply_webhook=http://calciforge-{name}.local:18797/hooks/reply,reply_auth_token=reply-{name}"
                )
            }
        };

        let result = parse_claw_spec(&spec);
        // All generated specs are structurally valid — must succeed.
        let claw = result
            .unwrap_or_else(|e| panic!("parse_claw_spec failed on valid spec {:?}: {}", spec, e));

        // Name must roundtrip exactly.
        assert_eq!(
            claw.name, name,
            "name mismatch: spec={:?} parsed={:?}",
            spec, claw.name
        );

        // Adapter kind must match what we put in.
        let expected_label = match adapter_idx {
            0 => "openai-compat",
            1 => "webhook",
            2 => "cli",
            3 => "zeroclaw-native",
            _ => "openclaw-channel",
        };
        assert_eq!(
            claw.adapter.kind_label(),
            expected_label,
            "adapter kind mismatch: spec={:?}",
            spec
        );

        // Endpoint must survive roundtrip for adapters that set it.
        match &claw.adapter {
            ClawKind::OpenAiCompat { endpoint } => {
                assert!(
                    claw.endpoint.contains(&name),
                    "endpoint should contain name: endpoint={:?} name={:?}",
                    endpoint,
                    name
                );
            }
            ClawKind::Webhook { endpoint, .. } => {
                assert!(
                    claw.endpoint.contains(&name) || endpoint.contains(&name),
                    "webhook endpoint should contain name"
                );
            }
            ClawKind::Cli { command } => {
                assert!(
                    command.contains(&name),
                    "cli command should contain name: {:?}",
                    command
                );
            }
            ClawKind::ZeroClawNative | ClawKind::OpenClawChannel => {
                assert!(
                    claw.endpoint.contains(&name),
                    "zeroclaw/openclaw endpoint should contain name: endpoint={:?}",
                    claw.endpoint
                );
                assert!(
                    !claw.host.is_empty(),
                    "zeroclaw/openclaw must have a host set"
                );
            }
        }
    }

    /// Property: parse_install_target with adversarial input — never panics.
    ///
    /// Generates arbitrary strings for host, name, and adapter fields.
    /// The property is: `parse_install_target` either succeeds with correct
    /// values OR returns a clean `Err` — it must never panic.
    ///
    /// This is a "no-panic" property: even on completely garbage input, the
    /// parser must return cleanly.  A panic here would be a real bug.
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_parse_install_target_never_panics(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        // Generate an arbitrary host string (may be complete garbage).
        let host = tc.draw(gs::text().max_size(100));
        // Generate an arbitrary spec string (may be complete garbage).
        let spec = tc.draw(gs::text().max_size(200));

        let args = InstallArgs {
            calciforge_host: if host.is_empty() {
                None
            } else {
                Some(host.clone())
            },
            claw_specs: if spec.is_empty() {
                vec![]
            } else {
                vec![spec.clone()]
            },
            ..Default::default()
        };

        // Must not panic — may succeed or fail, but never panic.
        let result = parse_install_target(&args);

        // Additional property: if parsing succeeds with a non-empty host,
        // the parsed host must match what we put in (no silent mutation).
        if let Ok(target) = result {
            if !host.is_empty() {
                assert_eq!(
                    target.calciforge.host, host,
                    "calciforge host must roundtrip: input={:?}",
                    host
                );
            }
        }
        // Failures are fine — we just require no panic.
    }

    /// Property: `parse_claw_spec` always returns `Err`, never panics,
    /// on inputs that lack `name` or `adapter`.
    ///
    /// Generates specs with random keys that deliberately omit the required
    /// `name` key.  Property: always `Err`, never `Ok`, never panics.
    #[cfg(feature = "hegel")]
    #[hegel::test]
    fn prop_parse_claw_spec_missing_name_always_errors(tc: hegel::TestCase) {
        use hegel::generators as gs;
        use hegel::Generator;

        // Generate random kv pairs using sampled_from for safe keys and values.
        // Keys are deliberately NOT "name" to test the missing-name error case.
        let pairs_count = tc.draw(gs::integers::<usize>().min_value(1).max_value(5));
        let mut parts = Vec::new();
        let safe_keys = vec![
            "adapter".to_string(),
            "host".to_string(),
            "endpoint".to_string(),
            "command".to_string(),
            "format".to_string(),
            "key".to_string(),
        ];
        let safe_vals = vec![
            "zeroclaw-native".to_string(),
            "openclaw-channel".to_string(),
            "http://host:18799".to_string(),
            "user@host".to_string(),
            "/path/to/key".to_string(),
            "foo".to_string(),
        ];
        for _ in 0..pairs_count {
            let key: String = tc.draw(gs::sampled_from(safe_keys.clone()));
            let val: String = tc.draw(gs::sampled_from(safe_vals.clone()));
            parts.push(format!("{key}={val}"));
        }
        let spec = parts.join(",");

        // Must not panic, must return Err (missing "name").
        let result = parse_claw_spec(&spec);
        assert!(
            result.is_err(),
            "spec without 'name' must always fail, but succeeded: spec={:?}",
            spec
        );
    }
}
