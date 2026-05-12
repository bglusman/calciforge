use tokio::io::{AsyncBufReadExt, BufReader};

pub(super) fn secure_help() -> String {
    [
        "!secret subcommands (alias: !secure):",
        "  !secret input NAME [desc] — create a one-shot local-network paste link",
        "  !secret bulk [desc]     — create a one-shot local-network .env paste link",
        "  !secret list              — list stored secret names (not values)",
        "  !secret help              — show this help",
        "",
        "The input/bulk links are for browsers that can reach this Calciforge",
        "host on your LAN. For stable hostnames or off-LAN access, configure",
        "CALCIFORGE_PASTE_PUBLIC_BASE_URL behind an authenticated reverse proxy",
        "or tunnel; do not expose paste-server directly to the open internet.",
        "",
        "Equivalent host-local commands:",
        "  paste-server NAME \"description\"",
        "  paste-server --bulk env-import \"bulk .env import\"",
        "",
        "Legacy chat fallback:",
        "  !secret set NAME=value    — store a low-stakes secret by name",
        "  !secret set NAME value    — same, for mobile keyboards",
        "",
        "⚠️ `!secret set` passes through the chat transport, which retains",
        "   history and may not be end-to-end encrypted. `input`/`bulk` send",
        "   only a short-lived URL; the value is entered in the browser.",
        "",
        "Calciforge and fnox can share the same fnox.toml/profile. Installing",
        "fnox is still useful for manual `fnox set/list/tui` operations and as",
        "the default local secret backend for paste-server. On macOS the installer",
        "adds a Keychain provider; on Linux it creates a local age provider.",
    ]
    .join("\n")
}

pub(super) async fn secure_input(rest: &str, bulk: bool) -> String {
    let (name_or_label, description) = match secure_input_target(rest, bulk) {
        Ok(target) => target,
        Err(usage) => {
            return usage;
        }
    };

    let mut command = tokio::process::Command::new("paste-server");
    if bulk {
        command.arg("--bulk");
    }
    configure_paste_server_env(&mut command);
    command
        .arg(name_or_label)
        .arg(description)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(false);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return "⚠️ paste-server is not installed or not on the service PATH. Re-run the Calciforge installer or install the `paste-server` binary.".to_string();
        }
        Err(e) => return format!("⚠️ Failed to start paste-server: {e}"),
    };

    let Some(stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return "⚠️ paste-server started without a URL stream".to_string();
    };

    let mut line = String::new();
    let mut reader = BufReader::new(stdout);
    let read_result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        reader.read_line(&mut line),
    )
    .await;
    let url = match read_result {
        Ok(Ok(n)) if n > 0 => line.trim().to_string(),
        Ok(Ok(_)) => {
            let _ = child.kill().await;
            return "⚠️ paste-server exited before returning a URL".to_string();
        }
        Ok(Err(e)) => {
            let _ = child.kill().await;
            return format!("⚠️ Failed reading paste-server URL: {e}");
        }
        Err(_) => {
            let _ = child.kill().await;
            return "⚠️ paste-server did not return a URL within 3 seconds".to_string();
        }
    };

    tokio::spawn(async move {
        let _ = child.wait().await;
    });

    let mode = if bulk {
        "bulk .env paste"
    } else {
        "secret paste"
    };
    format!(
        "🔐 One-shot {mode} link:\n{url}\n\n\
         Open it from a browser that can reach the Calciforge host. The link expires quickly and stores through the configured local secret backend. \
         For phone/off-network use, use a short-lived authenticated proxy/tunnel; do not expose this paste server directly to the open internet."
    )
}

pub(super) fn secure_input_target(rest: &str, bulk: bool) -> Result<(String, String), String> {
    if bulk {
        let description = rest.trim();
        return Ok((
            "env-import".to_string(),
            if description.is_empty() {
                "Paste .env lines; each KEY=VALUE line becomes its own secret.".to_string()
            } else {
                description.to_string()
            },
        ));
    }

    let mut parts = rest.split_whitespace();
    let Some(name) = parts.next() else {
        return Err("⚠️ Usage: `!secret input NAME [description]`".to_string());
    };
    Ok((name.to_string(), parts.collect::<Vec<_>>().join(" ")))
}

fn configure_paste_server_env(command: &mut tokio::process::Command) {
    let env = paste_server_env_from_values(
        std::env::var("CALCIFORGE_PASTE_BIND").ok(),
        std::env::var_os("PASTE_BIND").is_some(),
        std::env::var("CALCIFORGE_PASTE_PUBLIC_BASE_URL").ok(),
        std::env::var("CALCIFORGE_PASTE_PUBLIC_HOST").ok(),
        detect_lan_bind_addr(),
    );

    if let Some(bind) = env.bind {
        command.env("PASTE_BIND", bind);
    }
    if let Some(base_url) = env.public_base_url {
        command.env("PASTE_PUBLIC_BASE_URL", base_url);
    }
    if let Some(host) = env.public_host {
        command.env("PASTE_PUBLIC_HOST", host);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct PasteServerEnv {
    pub(super) bind: Option<String>,
    pub(super) public_base_url: Option<String>,
    pub(super) public_host: Option<String>,
}

pub(super) fn paste_server_env_from_values(
    calciforge_bind: Option<String>,
    inherited_paste_bind_present: bool,
    calciforge_public_base_url: Option<String>,
    calciforge_public_host: Option<String>,
    detected_lan_bind: Option<String>,
) -> PasteServerEnv {
    // Chat-triggered secret input is usually opened from a phone or
    // another LAN machine, so Calciforge binds to the detected LAN address
    // when possible. The standalone paste-server CLI keeps its localhost
    // default, and this path also falls back to that when no LAN address is
    // available.
    let bind = if calciforge_bind.is_some() {
        calciforge_bind
    } else if inherited_paste_bind_present {
        None
    } else {
        detected_lan_bind
    };

    PasteServerEnv {
        bind,
        public_base_url: calciforge_public_base_url,
        public_host: calciforge_public_host,
    }
}

fn detect_lan_bind_addr() -> Option<String> {
    for target in ["192.0.2.1:80", "198.51.100.1:80", "203.0.113.1:80"] {
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
        if socket.connect(target).is_err() {
            continue;
        }
        let ip = socket.local_addr().ok()?.ip();
        if !ip.is_loopback() && !ip.is_unspecified() {
            return Some(format!("{ip}:0"));
        }
    }
    None
}

pub(super) async fn secure_set(rest: &str) -> String {
    // Accept either `NAME=value` or `NAME value`. The `=` form is
    // natural for env-style keys; the space form is slightly easier
    // on mobile keyboards.
    let (name, value) = match rest.find('=') {
        Some(idx) => {
            let (n, v) = rest.split_at(idx);
            (n.trim().to_string(), v[1..].to_string())
        }
        None => {
            let mut parts = rest.splitn(2, ' ');
            let n = parts.next().unwrap_or("").trim().to_string();
            let v = parts.next().unwrap_or("").to_string();
            (n, v)
        }
    };

    if name.is_empty() || value.is_empty() {
        return "⚠️ Usage: `!secret set NAME=value`".to_string();
    }
    // Keep the accepted syntax narrow so names are safe as fnox keys
    // and as `{{secret:NAME}}` interpolation references in
    // crates/security-proxy/src/substitution.rs.
    if !name
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
    {
        return format!("⚠️ Invalid secret name `{name}` — allowed: A-Z a-z 0-9 _ -");
    }

    // Migrated from a bespoke `Command::new("fnox")` block to the
    // shared FnoxClient — same on-the-wire behavior, but the typed
    // FnoxError lets us distinguish "fnox not installed" from "fnox
    // failed for some other reason" without substring-matching stderr.
    let client = secrets_client::FnoxClient::new();
    match client.set(&name, &value).await {
        Ok(()) => format!(
            "✅ Stored secret `{name}`.\n\n\
             ⚠️ The value you sent is retained by the chat transport. \
             For high-value secrets, use the local paste UI \
             (`paste-server {name}`) or host-local `fnox set {name}`."
        ),
        Err(secrets_client::FnoxError::NotInstalled(e)) => format!(
            "⚠️ fnox not available: {e}. Install it (brew install fnox) \
             and run `fnox init` to enable `!secure`."
        ),
        Err(secrets_client::FnoxError::Failed { stderr, .. }) => {
            // Stderr may name the backend or config file path — that's
            // operational info the user can already read via
            // `fnox doctor`; echoing it here is fine. Crucially does
            // NOT contain the value (we never put it in stderr; fnox
            // certainly doesn't).
            format!("⚠️ fnox set {name} failed: {stderr}")
        }
        Err(other) => format!("⚠️ fnox set {name} failed: {other}"),
    }
}

pub(super) async fn secure_list() -> String {
    // Migrated to FnoxClient — see secure_set for rationale. The
    // wrapper does the defensive name-extraction parse internally,
    // so we just present the result.
    let client = secrets_client::FnoxClient::new();
    match client.list().await {
        Ok(names) if names.is_empty() => {
            "📭 No secrets stored. Use `paste-server NAME` to add one without chat history."
                .to_string()
        }
        Ok(names) => {
            let metadata = match secrets_client::metadata::metadata_for_names(&names) {
                Ok(metadata) => metadata,
                Err(error) => {
                    return format!(
                        "⚠️ Secret metadata unavailable; refusing to display incomplete destination policy: {error}"
                    );
                }
            };
            let rows = metadata
                .into_iter()
                .map(|secret| {
                    if secret.allowed_destinations.is_empty() {
                        secret.name
                    } else {
                        format!(
                            "{} — allowed: {}",
                            secret.name,
                            secret.allowed_destinations.join(", ")
                        )
                    }
                })
                .collect::<Vec<_>>();
            format!(
                "🔐 {} stored secret{}:\n  {}",
                rows.len(),
                if rows.len() == 1 { "" } else { "s" },
                rows.join("\n  ")
            )
        }
        Err(secrets_client::FnoxError::NotInstalled(e)) => format!("⚠️ fnox not available: {e}"),
        Err(e) => format!("⚠️ fnox list failed: {e}"),
    }
}
