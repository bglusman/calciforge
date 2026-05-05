//! Linux installer hardening: route every agent-related service through the
//! Calciforge security proxy, install the MITM CA into the system bundle and
//! Chrome's NSS DB, and verify the result by hitting a known-blocked URL.
//!
//! This module is the Linux-side parity for the macOS keychain trust pass
//! that ships in `scripts/install.sh` (PR #107). It is invoked from
//! [`super::executor::configure_remote_openclaw_proxy_env`] when the target
//! is a Linux/systemd host.
//!
//! # Scope
//!
//! - **Service discovery** — heuristically find services that can fetch
//!   HTTP/HTTPS for an agent. Heuristics: `chrome`/`chromium`/`headless` in
//!   description or ExecStart (browser services), `node` ExecStart with
//!   `OPENCLAW_*` env hints, `*claw*` description match, and an
//!   operator-supplied extras list. `openclaw-gateway` is always included
//!   when present.
//! - **Two drop-in shapes**: env-only (existing pattern) for
//!   orchestrators/gateways, and ExecStart-override for browser services
//!   (Chrome on Linux headless does not honor `HTTPS_PROXY` env reliably,
//!   so `--proxy-server=...` must be injected explicitly).
//! - **CA install**: copy to `/usr/local/share/ca-certificates/` and run
//!   `update-ca-certificates`; install into NSS DBs at `~/.pki/nssdb` and
//!   any per-service `--user-data-dir`.
//! - **Verify-or-fail**: post-install, fetch a known-blocked URL through
//!   the proxy and assert the Calciforge block page is returned. Fail
//!   loud if any service still bypasses.
//! - **Bypass audit**: warn (not error) on established :443 connections
//!   from agent processes that aren't going to the proxy.
//!
//! # Tests
//!
//! Unit tests cover the pure functions: ExecStart parsing/rewrite,
//! service-discovery filtering, and block-page detection. Integration
//! against systemctl is exercised via [`super::ssh::MockSshClient`].

use anyhow::{bail, Result};

// ---------------------------------------------------------------------------
// Banner
// ---------------------------------------------------------------------------

/// Operator-facing banner shown at the start of the Linux install pass.
///
/// We deliberately scope what Calciforge will and won't touch on a shared
/// host. `calciforge-trust-user` is a follow-up script (out of scope for
/// this PR, see TODO below).
// TODO: ship calciforge-trust-user as part of next pass
pub const SHARED_HOST_BANNER: &str = concat!(
    "\n",
    "\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\n",
    "\u{26a0}  Calciforge will configure agent-related system services to route\n",
    "   through the local security proxy (--proxy-server, env vars, CA trust).\n",
    "\n",
    "   It will NOT touch human user sessions on this host. If you share\n",
    "   this machine with humans whose own browsing should also be inspected\n",
    "   (e.g. you run agents AND personal browsers on the same box), they\n",
    "   need to opt in separately by running:\n",
    "\n",
    "      /usr/local/bin/calciforge-trust-user <username>\n",
    "\n",
    "   That command will install the Calciforge CA into <username>'s NSS\n",
    "   DB and add the proxy env to their shell profile. Without opting in,\n",
    "   a human user's traffic on this host bypasses the gateway entirely.\n",
    "\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\u{2501}\n",
);

// ---------------------------------------------------------------------------
// Service discovery
// ---------------------------------------------------------------------------

/// Whether a discovered service runs in system or user systemd scope.
///
/// User-scope services owned by a non-claw human user are filtered out by
/// the discovery step before we ever try to write a drop-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceScope {
    System,
    /// User scope, with the owning username.
    User,
}

/// Drop-in shape required for a discovered service.
///
/// Browser services hardcode the binary path in `ExecStart=` and Chrome on
/// Linux headless does not honor `HTTPS_PROXY` env, so we must override
/// `ExecStart=` to inject `--proxy-server=...`. Everything else respects
/// env vars and gets the cheaper env-only drop-in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropInShape {
    /// Env-only drop-in (HTTP_PROXY, HTTPS_PROXY, etc.).
    EnvOnly,
    /// Override ExecStart= and inject `--proxy-server=...`. Required for
    /// Chrome/Chromium services.
    ExecStartOverride,
}

/// A service Calciforge will configure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredService {
    /// Service name with `.service` suffix (e.g. `chrome-cdp.service`).
    pub name: String,
    pub scope: ServiceScope,
    pub shape: DropInShape,
    /// Match reason for logs/diagnostics.
    pub reason: String,
}

/// Heuristic match against `systemctl list-units --type=service` output plus
/// optional `systemctl cat <unit>` body. Pure function; kept testable.
///
/// `description` is the unit `Description=` line value (or empty).
/// `exec_start` is the full `ExecStart=` value, possibly multi-line; only
/// substring matches are used.
/// `extras` is the operator-supplied extra service-name allowlist (names
/// without `.service` suffix are normalized).
///
/// Returns the matching `DropInShape` (and a reason) if the service should
/// be hardened, or `None` if it should be ignored.
pub fn classify_service(
    name: &str,
    description: &str,
    exec_start: &str,
    extras: &[String],
) -> Option<(DropInShape, String)> {
    let name_lc = name.to_ascii_lowercase();
    let desc_lc = description.to_ascii_lowercase();
    let exec_lc = exec_start.to_ascii_lowercase();

    // Always include openclaw-gateway when present.
    if name_lc.starts_with("openclaw-gateway") {
        return Some((DropInShape::EnvOnly, "openclaw-gateway".into()));
    }

    // Browser services: ExecStart override required.
    let browser_hit = ["chrome", "chromium", "headless"]
        .iter()
        .find(|n| desc_lc.contains(*n) || exec_lc.contains(*n) || name_lc.contains(*n));
    if let Some(hit) = browser_hit {
        return Some((
            DropInShape::ExecStartOverride,
            format!("browser hint: {hit}"),
        ));
    }

    // `*claw*` substring in the unit name OR description (case-
    // insensitive). The intent is to catch claw-family agent services
    // even when their Description= line is empty or doesn't follow a
    // convention (which is common — most users pick a unit name and
    // skip the description). Reason string mirrors which side hit so
    // it's debuggable in logs.
    if name_lc.contains("claw") {
        return Some((DropInShape::EnvOnly, "claw in unit name".into()));
    }
    if desc_lc.contains("claw") {
        return Some((DropInShape::EnvOnly, "claw in description".into()));
    }

    // node ExecStart with OPENCLAW_* env hint.
    if exec_lc.contains("node") && exec_start.contains("OPENCLAW_") {
        return Some((DropInShape::EnvOnly, "node + OPENCLAW_ env".into()));
    }

    // Operator-supplied extras (exact name match, with or without .service).
    for extra in extras {
        let extra_lc = extra.to_ascii_lowercase();
        let extra_norm = extra_lc.trim_end_matches(".service");
        let name_norm = name_lc.trim_end_matches(".service");
        if extra_norm == name_norm {
            return Some((DropInShape::EnvOnly, format!("operator extra: {extra}")));
        }
    }

    None
}

// ---------------------------------------------------------------------------
// ExecStart parser + rewriter
// ---------------------------------------------------------------------------

/// Inject `--proxy-server=<endpoint>` after the binary path in the given
/// `ExecStart=` value.
///
/// Idempotent: if `--proxy-server=` is already present anywhere in the
/// args, returns the input unchanged.
///
/// `exec_start` should be the raw value of the `ExecStart=` line (with
/// `ExecStart=` already stripped, possibly with line continuations
/// preserved). We preserve line continuations and indentation as-is by
/// splitting on the first whitespace token (the binary) and inserting the
/// flag after it. The original args are kept verbatim.
pub fn inject_proxy_server(exec_start: &str, proxy_endpoint: &str) -> String {
    if exec_start.contains("--proxy-server=") {
        return exec_start.to_string();
    }

    // Find first non-whitespace run = binary path. Preserve any leading
    // whitespace (rare but possible for systemd line-continuation indents).
    let trimmed_start = exec_start.trim_start();
    let leading = &exec_start[..exec_start.len() - trimmed_start.len()];

    let (binary, rest) = match trimmed_start.find(char::is_whitespace) {
        Some(idx) => (&trimmed_start[..idx], &trimmed_start[idx..]),
        None => (trimmed_start, ""),
    };

    if binary.is_empty() {
        return exec_start.to_string();
    }

    // If the rest begins with " \\\n", preserve the multi-line shape by
    // putting --proxy-server on its own continuation line; otherwise just
    // jam it in inline.
    if rest.starts_with(" \\\n") || rest.starts_with("\\\n") {
        // Find the indentation used on the next line so we match it.
        // Default to four spaces if we can't tell.
        let next_line_indent = rest
            .split_once('\n')
            .map(|(_, after)| {
                after
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect::<String>()
            })
            .unwrap_or_else(|| "    ".to_string());
        format!(
            "{leading}{binary} \\\n{next_line_indent}--proxy-server={proxy_endpoint}{rest}",
            leading = leading,
            binary = binary,
            next_line_indent = next_line_indent,
            proxy_endpoint = proxy_endpoint,
            rest = rest,
        )
    } else if rest.is_empty() {
        format!("{leading}{binary} --proxy-server={proxy_endpoint}")
    } else {
        format!("{leading}{binary} --proxy-server={proxy_endpoint}{rest}")
    }
}

/// Extract the full `ExecStart=` value from a `systemctl cat <unit>` body,
/// honoring line continuations (`\` at end of line).
///
/// Returns `None` if no `ExecStart=` line is found.
///
/// Notes:
/// - Comments (`#...`) are skipped.
/// - We only return the first `ExecStart=` we find (a unit may have
///   multiple, e.g. when it has its own drop-in, but our caller is asking
///   about the canonical executable to wrap).
/// - The returned string includes line continuations verbatim, so
///   [`inject_proxy_server`] can re-emit a multi-line shape.
pub fn extract_exec_start(systemctl_cat_body: &str) -> Option<String> {
    let mut lines = systemctl_cat_body.lines();
    while let Some(line) = lines.next() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            continue;
        }
        let Some(rest) = trimmed.strip_prefix("ExecStart=") else {
            continue;
        };
        let mut value = rest.to_string();
        // Honor line continuations: trailing `\` before newline.
        while value.trim_end().ends_with('\\') {
            let Some(next) = lines.next() else {
                break;
            };
            value.push('\n');
            value.push_str(next);
        }
        return Some(value);
    }
    None
}

/// Detect the binary path from an `ExecStart=` value. Returns the first
/// whitespace-delimited token after stripping any leading systemd prefix
/// modifiers (`@`, `-`, `+`, `!`, `!!`).
pub fn detect_binary_path(exec_start: &str) -> Option<String> {
    let trimmed = exec_start.trim_start();
    // Strip systemd prefix chars per `man systemd.service`.
    let mut s = trimmed;
    for _ in 0..3 {
        if let Some(stripped) = s
            .strip_prefix('@')
            .or_else(|| s.strip_prefix('-'))
            .or_else(|| s.strip_prefix('+'))
            .or_else(|| s.strip_prefix('!'))
        {
            s = stripped;
        } else {
            break;
        }
    }
    let token: String = s.chars().take_while(|c| !c.is_whitespace()).collect();
    if token.is_empty() {
        None
    } else {
        Some(token)
    }
}

/// Render a full ExecStart-override drop-in body.
///
/// Two `ExecStart=` lines: one empty (clears) and one with the rewritten
/// value. We also include the env vars so HTTP libraries inside the
/// browser process (e.g. Node companions, page-fetch helpers) honor them.
pub fn render_exec_start_override(
    exec_start: &str,
    proxy_endpoint: &str,
    no_proxy: &str,
) -> String {
    let rewritten = inject_proxy_server(exec_start, proxy_endpoint);
    let proxy_q = systemd_environment_value(proxy_endpoint);
    let no_proxy_q = systemd_environment_value(no_proxy);
    let ca_q = systemd_environment_value("%h/.config/calciforge/secrets/mitm-ca.pem");
    format!(
        "# Managed by calciforge install. Forces this service to route all\n\
         # traffic through the local Calciforge MITM proxy. Chrome on Linux in\n\
         # headless mode does not reliably honor HTTPS_PROXY env, so this passes\n\
         # the flag explicitly. The empty ExecStart= line clears the original;\n\
         # the second replaces it with a copy that has --proxy-server injected.\n\
         [Service]\n\
         Environment=\"HTTP_PROXY={proxy}\"\n\
         Environment=\"HTTPS_PROXY={proxy}\"\n\
         Environment=\"ALL_PROXY={proxy}\"\n\
         Environment=\"NO_PROXY={no_proxy}\"\n\
         Environment=\"NODE_USE_SYSTEM_CA=1\"\n\
         Environment=\"NODE_EXTRA_CA_CERTS={ca}\"\n\
         Environment=\"SSL_CERT_FILE={ca}\"\n\
         Environment=\"REQUESTS_CA_BUNDLE={ca}\"\n\
         Environment=\"CURL_CA_BUNDLE={ca}\"\n\
         Environment=\"GIT_SSL_CAINFO={ca}\"\n\
         ExecStart=\n\
         ExecStart={rewritten}\n",
        proxy = proxy_q,
        no_proxy = no_proxy_q,
        ca = ca_q,
        rewritten = rewritten,
    )
}

/// Escape a value for use inside a quoted systemd `Environment="..."`
/// line. Per systemd.exec(5) and systemd-escape(1), the quoted form
/// must escape `\` and `"`. Other characters survive verbatim.
///
/// Without this, an `endpoint` (or `no_proxy` list) carrying a quote
/// or backslash would terminate the quoted string early and produce a
/// malformed drop-in — at best the unit fails to start, at worst the
/// rest of the value is interpreted as systemd directive syntax.
fn systemd_environment_value(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

/// Default URL we hit through the proxy to confirm blocking actually
/// happens. Operator can override via `verify_url` knob.
///
/// `ref.jock.pl/modern-web/` is a stable third-party page that the
/// Calciforge default deny-list blocks; any allowlist-based deployment
/// that doesn't include it will reproduce the block. If you've taught
/// your proxy to allow it, override.
pub const DEFAULT_VERIFY_URL: &str = "https://ref.jock.pl/modern-web/";

/// Marker substring that must appear in the response body of a
/// proxy-blocked request.
pub const BLOCK_PAGE_MARKER: &str = "Page blocked by Calciforge security gateway";

/// Marker header that must appear (case-insensitive) in the response.
pub const BLOCK_HEADER_MARKER: &str = "X-Calciforge-Blocked: true";

/// Result of inspecting a `curl -i` (or `curl -D - -o ... `) combined-headers
/// + body output for the block page markers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyOutcome {
    /// Block page was returned. We're good.
    Blocked,
    /// Response did not match the block markers — verification failed.
    NotBlocked { snippet: String },
}

/// Inspect a combined-headers-and-body response for the Calciforge block
/// page. Used by the post-install verification step. Pure function so
/// tests can feed in canned curl output.
pub fn check_block_response(combined: &str) -> VerifyOutcome {
    let body_has_marker = combined.contains(BLOCK_PAGE_MARKER);
    let header_has_marker = combined.lines().any(|line| {
        line.to_ascii_lowercase()
            .contains("x-calciforge-blocked: true")
    });
    if body_has_marker && header_has_marker {
        VerifyOutcome::Blocked
    } else {
        // Truncate to a reasonable size for the operator-facing error.
        let snippet: String = combined.chars().take(400).collect();
        VerifyOutcome::NotBlocked { snippet }
    }
}

// ---------------------------------------------------------------------------
// Package-manager detection
// ---------------------------------------------------------------------------

/// Detected package manager for installing `libnss3-tools` (provides
/// `certutil`, needed for Chrome NSS DB CA trust).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageManager {
    Apt,
    Dnf,
    Yum,
    Pacman,
}

impl PackageManager {
    /// Shell snippet that probes the target for which package manager
    /// exists. Echoes one of `apt`/`dnf`/`yum`/`pacman`/`none`.
    pub const PROBE_COMMAND: &'static str =
        "if command -v apt-get >/dev/null 2>&1; then echo apt; \
         elif command -v dnf >/dev/null 2>&1; then echo dnf; \
         elif command -v yum >/dev/null 2>&1; then echo yum; \
         elif command -v pacman >/dev/null 2>&1; then echo pacman; \
         else echo none; fi";

    /// Parse the probe-command output into a typed PackageManager.
    pub fn parse(probe_output: &str) -> Result<Self> {
        match probe_output.trim() {
            "apt" => Ok(Self::Apt),
            "dnf" => Ok(Self::Dnf),
            "yum" => Ok(Self::Yum),
            "pacman" => Ok(Self::Pacman),
            other => bail!(
                "no supported package manager found on target (got '{}'); \
                 cannot install libnss3-tools / nss-tools for Chrome CA trust",
                other
            ),
        }
    }

    /// Install command for the libnss3-tools / nss-tools package
    /// (provides `certutil`).
    pub fn install_nss_tools(&self) -> &'static str {
        match self {
            Self::Apt => "DEBIAN_FRONTEND=noninteractive apt-get install -y libnss3-tools",
            Self::Dnf => "dnf install -y nss-tools",
            Self::Yum => "yum install -y nss-tools",
            Self::Pacman => "pacman -S --noconfirm nss",
        }
    }

    /// Refresh-system-CA-bundle command after dropping a cert into
    /// the per-distro CA anchor directory (see `system_ca_anchor_dir`).
    pub fn update_ca_certificates(&self) -> &'static str {
        match self {
            Self::Apt => "update-ca-certificates",
            Self::Dnf | Self::Yum => "update-ca-trust extract",
            Self::Pacman => "trust extract-compat || update-ca-trust extract",
        }
    }

    /// Per-distro directory where operator-supplied CA certs live BEFORE
    /// the refresh command above is run. Each distro/package-manager has
    /// its own canonical "anchors" location; dropping certs into the
    /// wrong dir means the trust store never picks them up.
    pub fn system_ca_anchor_dir(&self) -> &'static str {
        match self {
            // Debian/Ubuntu: update-ca-certificates picks up `*.crt`
            // from /usr/local/share/ca-certificates/.
            Self::Apt => "/usr/local/share/ca-certificates",
            // RHEL/Fedora: update-ca-trust(8) reads from
            // /etc/pki/ca-trust/source/anchors/.
            Self::Dnf | Self::Yum => "/etc/pki/ca-trust/source/anchors",
            // Arch: ca-certificates-utils provides update-ca-trust which
            // reads from /etc/ca-certificates/trust-source/anchors/.
            Self::Pacman => "/etc/ca-certificates/trust-source/anchors",
        }
    }

    /// Per-distro path of the merged trust-store bundle that tools like
    /// `curl --cacert` accept. Used by the post-install verification step
    /// so we don't hardcode the Debian path on Fedora hosts (where the
    /// merged bundle lives at `/etc/pki/tls/certs/ca-bundle.crt`).
    pub fn system_ca_bundle_path(&self) -> &'static str {
        match self {
            Self::Apt => "/etc/ssl/certs/ca-certificates.crt",
            // RHEL/Fedora ship a symlinked bundle at this path; the
            // /etc/ssl/certs/ca-bundle.crt symlink exists too on most
            // installs, but `/etc/pki/tls/certs/ca-bundle.crt` is the
            // canonical one.
            Self::Dnf | Self::Yum => "/etc/pki/tls/certs/ca-bundle.crt",
            // Arch: ca-certificates package places the merged bundle here.
            Self::Pacman => "/etc/ssl/certs/ca-certificates.crt",
        }
    }

    /// Per-distro filename (not path) for the dropped cert. Most distros
    /// expect `*.crt`; Pacman/RHEL take `*.pem` too. We standardise on
    /// `.crt` since update-ca-certificates(Debian) requires it; the
    /// other distros accept either.
    pub fn ca_anchor_filename(&self) -> &'static str {
        "calciforge-ca.crt"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_browser_service_via_description() {
        let r = classify_service(
            "chrome-cdp.service",
            "Chrome Headless (CDP on 18802, behind CDP Guard)",
            "/opt/google/chrome/chrome --headless=new ...",
            &[],
        )
        .expect("should classify");
        assert_eq!(r.0, DropInShape::ExecStartOverride);
    }

    #[test]
    fn classify_chromium_service_via_exec() {
        let r = classify_service(
            "browser.service",
            "Some browser",
            "/usr/bin/chromium --headless ...",
            &[],
        )
        .expect("should classify");
        assert_eq!(r.0, DropInShape::ExecStartOverride);
    }

    #[test]
    fn classify_openclaw_gateway_is_env_only() {
        let r = classify_service(
            "openclaw-gateway.service",
            "OpenClaw gateway",
            "/usr/bin/openclaw gateway",
            &[],
        )
        .expect("should classify");
        assert_eq!(r.0, DropInShape::EnvOnly);
    }

    #[test]
    fn classify_node_with_openclaw_env_is_env_only() {
        let r = classify_service(
            "agent-orchestrator.service",
            "Agent orchestrator",
            "/usr/bin/node /srv/agent/index.js\nEnvironment=OPENCLAW_HOME=/srv",
            &[],
        )
        .expect("should classify");
        assert_eq!(r.0, DropInShape::EnvOnly);
    }

    #[test]
    fn classify_unrelated_service_is_skipped() {
        assert!(
            classify_service("nginx.service", "nginx web server", "/usr/sbin/nginx", &[]).is_none()
        );
    }

    #[test]
    fn classify_extras_match_with_or_without_dot_service() {
        let extras = vec!["my-agent".into()];
        assert!(classify_service(
            "my-agent.service",
            "Some agent",
            "/usr/bin/my-agent",
            &extras
        )
        .is_some());
        let extras2 = vec!["my-agent.service".into()];
        assert!(classify_service("my-agent.service", "x", "/y", &extras2).is_some());
    }

    #[test]
    fn classify_claw_description_match_case_insensitive() {
        let r = classify_service("nz.service", "NonZeroClaw runtime", "/x", &[])
            .expect("should classify");
        assert_eq!(r.0, DropInShape::EnvOnly);
        assert!(r.1.contains("claw"));
    }

    #[test]
    fn inject_proxy_into_inline_exec() {
        let out = inject_proxy_server("/usr/bin/chrome --foo --bar", "http://127.0.0.1:18888");
        assert_eq!(
            out,
            "/usr/bin/chrome --proxy-server=http://127.0.0.1:18888 --foo --bar"
        );
    }

    #[test]
    fn inject_proxy_idempotent() {
        let original = "/usr/bin/chrome --proxy-server=http://127.0.0.1:18888 --foo";
        assert_eq!(
            inject_proxy_server(original, "http://127.0.0.1:18888"),
            original
        );
    }

    #[test]
    fn inject_proxy_into_multiline_exec() {
        let original = "/opt/google/chrome/chrome \\\n    --remote-debugging-port=18802 \\\n    --headless=new";
        let out = inject_proxy_server(original, "http://127.0.0.1:18888");
        assert!(out.starts_with(
            "/opt/google/chrome/chrome \\\n    --proxy-server=http://127.0.0.1:18888"
        ));
        assert!(out.contains("--remote-debugging-port=18802"));
        assert!(out.contains("--headless=new"));
    }

    #[test]
    fn inject_proxy_no_args() {
        let out = inject_proxy_server("/usr/bin/chrome", "http://127.0.0.1:18888");
        assert_eq!(out, "/usr/bin/chrome --proxy-server=http://127.0.0.1:18888");
    }

    #[test]
    fn extract_exec_start_handles_continuations() {
        let body = "[Unit]\nDescription=Foo\n\n[Service]\nExecStart=/opt/google/chrome/chrome \\\n    --remote-debugging-port=18802 \\\n    --headless=new\nType=simple\n";
        let v = extract_exec_start(body).expect("should extract");
        assert!(v.starts_with("/opt/google/chrome/chrome"));
        assert!(v.contains("--headless=new"));
        // The Type=simple line must NOT be slurped in.
        assert!(!v.contains("Type=simple"));
    }

    #[test]
    fn extract_exec_start_skips_comments_and_returns_none() {
        let body = "[Service]\n# ExecStart=/should/not/match\nType=oneshot\n";
        assert!(extract_exec_start(body).is_none());
    }

    #[test]
    fn detect_binary_strips_prefix_modifiers() {
        assert_eq!(
            detect_binary_path("@/usr/bin/foo --x").as_deref(),
            Some("/usr/bin/foo")
        );
        assert_eq!(
            detect_binary_path("-/usr/bin/foo --x").as_deref(),
            Some("/usr/bin/foo")
        );
        assert_eq!(
            detect_binary_path("/usr/bin/chrome --x").as_deref(),
            Some("/usr/bin/chrome")
        );
    }

    #[test]
    fn render_override_includes_two_exec_starts() {
        let body = render_exec_start_override(
            "/opt/google/chrome/chrome --headless=new",
            "http://127.0.0.1:18888",
            "localhost,127.0.0.1,::1",
        );
        // Empty clearing line.
        assert!(body.contains("\nExecStart=\nExecStart=/opt/google/chrome/chrome"));
        assert!(body.contains("--proxy-server=http://127.0.0.1:18888"));
        assert!(body.contains("Environment=\"HTTPS_PROXY=http://127.0.0.1:18888\""));
        assert!(body.contains("Environment=\"NO_PROXY=localhost,127.0.0.1,::1\""));
        assert!(body.contains(
            "Environment=\"NODE_EXTRA_CA_CERTS=%h/.config/calciforge/secrets/mitm-ca.pem\""
        ));
        assert!(body
            .contains("Environment=\"SSL_CERT_FILE=%h/.config/calciforge/secrets/mitm-ca.pem\""));
        assert!(body.contains(
            "Environment=\"REQUESTS_CA_BUNDLE=%h/.config/calciforge/secrets/mitm-ca.pem\""
        ));
    }

    #[test]
    fn check_block_response_happy_path() {
        let resp = "HTTP/1.1 200 OK\r\nX-Calciforge-Blocked: true\r\nContent-Type: text/html\r\n\r\n<html><body>Page blocked by Calciforge security gateway: bad.com</body></html>";
        assert_eq!(check_block_response(resp), VerifyOutcome::Blocked);
    }

    #[test]
    fn check_block_response_missing_header_fails() {
        let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\nPage blocked by Calciforge security gateway";
        match check_block_response(resp) {
            VerifyOutcome::NotBlocked { snippet } => {
                assert!(snippet.contains("Page blocked"));
            }
            other => panic!("expected NotBlocked, got {:?}", other),
        }
    }

    #[test]
    fn check_block_response_unrelated_response_fails() {
        let resp = "HTTP/1.1 200 OK\r\n\r\nWelcome to nginx";
        assert!(matches!(
            check_block_response(resp),
            VerifyOutcome::NotBlocked { .. }
        ));
    }

    #[test]
    fn package_manager_parse_known() {
        assert_eq!(PackageManager::parse("apt\n").unwrap(), PackageManager::Apt);
        assert_eq!(PackageManager::parse("dnf").unwrap(), PackageManager::Dnf);
        assert_eq!(PackageManager::parse("yum").unwrap(), PackageManager::Yum);
        assert_eq!(
            PackageManager::parse("pacman").unwrap(),
            PackageManager::Pacman
        );
    }

    #[test]
    fn package_manager_parse_none_errors() {
        assert!(PackageManager::parse("none").is_err());
    }

    #[test]
    fn package_manager_install_nss_tools_apt_uses_libnss3_tools() {
        assert!(PackageManager::Apt
            .install_nss_tools()
            .contains("libnss3-tools"));
        assert!(PackageManager::Dnf
            .install_nss_tools()
            .contains("nss-tools"));
    }

    #[test]
    fn user_scope_round_trip() {
        // Trivial smoke test for the enum derivations.
        assert_ne!(ServiceScope::System, ServiceScope::User);
    }
}
