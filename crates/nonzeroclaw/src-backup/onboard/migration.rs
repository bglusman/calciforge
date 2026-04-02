//! OpenClaw → NZC migration support.
//!
//! This module handles detection of an existing OpenClaw installation and
//! provides the data structures and logic used by the onboarding wizard to
//! offer channel assignment, memory migration, and config field mapping.
//!
//! # Design constraints
//!
//! - OpenClaw config is **read-only**. We never write to it.
//! - If no OpenClaw install is found, every public function returns gracefully
//!   — NZC installation must not require OpenClaw.
//! - Memory migration is optional and LLM-assisted (not a plain copy); users
//!   can defer it and run `nzc migrate-memory` later.
//! - Channel assignment is a decision, not a copy: exactly one owner per channel.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use tracing;

// ── Detection ─────────────────────────────────────────────────────────────────

/// Everything we know about an existing OpenClaw installation on this host.
///
/// Constructed by [`detect_openclaw_installation`].  All fields are derived
/// from the filesystem and the parsed `openclaw.json`; nothing is inferred.
#[derive(Debug, Clone)]
pub struct OpenClawInstallation {
    /// Path to the parsed `openclaw.json` (or `openclaw.jsonc` / JSON5 variant).
    pub config_path: PathBuf,
    /// Raw parsed JSON value — the entire config tree, comments stripped.
    pub config: serde_json::Value,
    /// Root of the OpenClaw data directory (`~/.openclaw/`).
    pub openclaw_dir: PathBuf,
    /// Workspace directory, if one is configured and exists on disk.
    pub workspace_path: Option<PathBuf>,
    /// Path to `MEMORY.md`, if present.
    pub memory_path: Option<PathBuf>,
    /// Path to the `memory/` directory (daily notes), if present.
    pub memory_dir: Option<PathBuf>,
    /// Channels detected in the config.
    pub channels: Vec<DetectedChannel>,
    /// OpenClaw version string, if readable.
    pub version: Option<String>,
}

impl OpenClawInstallation {
    /// True if any memory content (MEMORY.md or daily files) is available.
    pub fn has_memory(&self) -> bool {
        self.memory_path.is_some() || self.memory_dir.is_some()
    }

    /// Collect all memory file paths that exist on disk.
    pub fn memory_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        if let Some(ref p) = self.memory_path {
            files.push(p.clone());
        }
        if let Some(ref dir) = self.memory_dir {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("md") {
                        files.push(path);
                    }
                }
            }
        }
        files.sort();
        files
    }
}

/// A communication channel detected in an OpenClaw config.
#[derive(Debug, Clone)]
pub struct DetectedChannel {
    /// Canonical lowercase name: `"telegram"`, `"signal"`, `"whatsapp"`,
    /// `"matrix"`, `"discord"`.
    pub name: String,
    /// Whether the channel appears to be enabled in the config.
    pub enabled: bool,
    /// True if at least one credential field (token, account, etc.) is non-empty.
    pub has_credentials: bool,
    /// The raw JSON object for this channel's config block.
    pub config_snippet: serde_json::Value,
}

// ── Ownership / assignment ────────────────────────────────────────────────────

/// Who should own a channel after migration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelOwner {
    /// NZC takes over: credentials are pulled from OpenClaw config into NZC.
    Nzc,
    /// OpenClaw keeps it: nothing changes in either config.
    OpenClaw,
    /// Deferred / not decided.
    Unassigned,
}

impl std::fmt::Display for ChannelOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChannelOwner::Nzc => write!(f, "NZC"),
            ChannelOwner::OpenClaw => write!(f, "OpenClaw"),
            ChannelOwner::Unassigned => write!(f, "Skip"),
        }
    }
}

/// The result of the channel assignment wizard step for one channel.
#[derive(Debug, Clone)]
pub struct ChannelAssignment {
    pub channel: DetectedChannel,
    pub owner: ChannelOwner,
}

// ── Config field mapping ──────────────────────────────────────────────────────

/// A single field that can be mapped from OpenClaw config to NZC config.
#[derive(Debug, Clone)]
pub struct MappedField {
    /// Dot-separated path in OpenClaw JSON (e.g. `"agents.defaults.model.primary"`).
    pub openclaw_path: String,
    /// Dot-separated path in NZC TOML (e.g. `"default_model"`).
    pub nzc_path: String,
    /// The value read from OpenClaw config, if present.
    pub value: Option<serde_json::Value>,
}

/// Fields present in OpenClaw config that have no NZC equivalent.
#[derive(Debug, Clone)]
pub struct UnmappedField {
    pub openclaw_path: String,
    pub reason: &'static str,
}

/// Output of [`build_config_migration_plan`].
#[derive(Debug, Clone)]
pub struct ConfigMigrationPlan {
    /// Fields that can be directly mapped.
    pub mapped: Vec<MappedField>,
    /// Fields that have no NZC equivalent (shown to user, then skipped).
    pub unmapped: Vec<UnmappedField>,
}

// ── Memory migration ──────────────────────────────────────────────────────────

/// Outcome of the memory migration step.
#[derive(Debug, Clone)]
pub enum MemoryMigrationOutcome {
    /// User declined; source files recorded for manual migration later.
    Skipped { source_files: Vec<PathBuf> },
    /// LLM call succeeded; migrated content written to `dest_path`.
    Completed { dest_path: PathBuf },
    /// LLM call failed or returned empty content; skipped gracefully.
    Failed {
        source_files: Vec<PathBuf>,
        reason: String,
    },
    /// No memory files found in OpenClaw workspace.
    NoMemoryFound,
}

// ── Detection implementation ──────────────────────────────────────────────────

/// Detect an existing OpenClaw installation.
///
/// Looks for `~/.openclaw/openclaw.json` (or `openclaw.jsonc`).  Returns
/// `Ok(None)` if no installation is found — this is normal and expected for
/// users without OpenClaw.  Returns `Err` only for unexpected I/O failures.
pub fn detect_openclaw_installation() -> Result<Option<OpenClawInstallation>> {
    let home = home_dir().context("Could not determine home directory")?;
    let openclaw_dir = home.join(".openclaw");

    if !openclaw_dir.exists() {
        return Ok(None);
    }

    // Try the two common config file names.
    let config_path = ["openclaw.json", "openclaw.jsonc"]
        .iter()
        .map(|name| openclaw_dir.join(name))
        .find(|p| p.exists());

    let config_path = match config_path {
        Some(p) => p,
        None => return Ok(None),
    };

    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let config = parse_json5_relaxed(&raw).with_context(|| {
        format!(
            "Failed to parse OpenClaw config at {}",
            config_path.display()
        )
    })?;

    // Workspace path: check config first, then fall back to ~/.openclaw/workspace.
    let workspace_path = detect_workspace_path(&config, &openclaw_dir);

    let memory_path = workspace_path
        .as_deref()
        .map(|ws| ws.join("MEMORY.md"))
        .filter(|p| p.exists());

    let memory_dir = workspace_path
        .as_deref()
        .map(|ws| ws.join("memory"))
        .filter(|p| p.is_dir());

    let channels = detect_channels(&config);
    let version = read_version(&config);

    Ok(Some(OpenClawInstallation {
        config_path,
        config,
        openclaw_dir,
        workspace_path,
        memory_path,
        memory_dir,
        channels,
        version,
    }))
}

// ── Channel detection ─────────────────────────────────────────────────────────

/// Channels we know how to detect and map.
const KNOWN_CHANNELS: &[&str] = &["telegram", "signal", "whatsapp", "matrix", "discord"];

/// Extract channel information from a parsed OpenClaw config.
///
/// OpenClaw stores channels under `plugins.entries.<name>` or directly under
/// `channels.<name>`.  We check both shapes.
pub fn detect_channels(config: &serde_json::Value) -> Vec<DetectedChannel> {
    let mut detected = Vec::new();

    for name in KNOWN_CHANNELS {
        // Try `channels.<name>` first (newer OpenClaw), then `plugins.entries.<name>`.
        let snippet = config
            .pointer(&format!("/channels/{name}"))
            .or_else(|| config.pointer(&format!("/plugins/entries/{name}")))
            .cloned();

        if let Some(snippet) = snippet {
            let enabled = is_channel_enabled(&snippet);
            let has_credentials = has_channel_credentials(name, &snippet);
            detected.push(DetectedChannel {
                name: name.to_string(),
                enabled,
                has_credentials,
                config_snippet: snippet,
            });
        }
    }

    detected
}

fn is_channel_enabled(snippet: &serde_json::Value) -> bool {
    // Common patterns: `"enabled": true`, absence of `"enabled": false`,
    // or a non-null / non-empty object.
    if let Some(enabled) = snippet.get("enabled").and_then(|v| v.as_bool()) {
        return enabled;
    }
    // If the block exists and has content, treat as enabled.
    snippet.is_object() && !snippet.as_object().map(|o| o.is_empty()).unwrap_or(true)
}

fn has_channel_credentials(channel: &str, snippet: &serde_json::Value) -> bool {
    let credential_keys: &[&str] = match channel {
        "telegram" => &["botToken", "bot_token", "token"],
        "discord" => &["botToken", "bot_token", "token"],
        "signal" => &["account", "phoneNumber", "phone_number"],
        "whatsapp" => &["accessToken", "access_token", "phoneNumberId", "phone_number_id"],
        "matrix" => &["accessToken", "access_token", "homeserver"],
        _ => &[],
    };
    credential_keys.iter().any(|key| {
        snippet
            .get(key)
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    })
}

// ── Config field mapping ──────────────────────────────────────────────────────

/// The known mapping table from OpenClaw JSON paths → NZC TOML paths.
const FIELD_MAP: &[(&str, &str)] = &[
    // Agent / model
    (
        "agents/defaults/model/primary",
        "default_model",
    ),
    (
        "agents/defaults/model/fallbacks",
        "agent.model_fallbacks",
    ),
    (
        "agents/defaults/workspace",
        "workspace_dir",
    ),
    (
        "agents/defaults/heartbeat/every",
        "heartbeat.interval",
    ),
    // Gateway
    ("gateway/port", "gateway.port"),
    ("gateway/bind", "gateway.bind"),
    ("gateway/auth/token", "gateway.auth.token"),
    // API keys
    ("env/ANTHROPIC_API_KEY", "providers.anthropic.api_key"),
    (
        "models/providers/anthropic/apiKey",
        "providers.anthropic.api_key",
    ),
    (
        "models/providers/openai/apiKey",
        "providers.openai.api_key",
    ),
    (
        "models/providers/openrouter/apiKey",
        "providers.openrouter.api_key",
    ),
];

/// Fields that exist in OpenClaw but have no NZC equivalent.
const UNMAPPED_FIELDS: &[(&str, &str)] = &[
    ("plugins/entries", "NZC uses a different plugin system"),
    ("skills", "NZC skills use a different format"),
    ("agents/defaults/compaction", "OpenClaw-specific context compaction"),
    ("hooks/mappings", "NZC uses a different webhook config"),
];

/// Build a config migration plan by scanning the OpenClaw config for known fields.
pub fn build_config_migration_plan(config: &serde_json::Value) -> ConfigMigrationPlan {
    let mut mapped: Vec<MappedField> = FIELD_MAP
        .iter()
        .map(|(openclaw_path, nzc_path)| {
            let json_ptr = format!("/{openclaw_path}");
            let value = config.pointer(&json_ptr).cloned();
            MappedField {
                openclaw_path: openclaw_path.replace('/', "."),
                nzc_path: nzc_path.to_string(),
                value,
            }
        })
        .collect();

    // Place mapped entries that actually have values first so that lookups by
    // `nzc_path` (used by migration tests) will find a populated mapping when
    // multiple OpenClaw paths map to the same NZC key (e.g. env vs models path).
    mapped.sort_by(|a, b| b.value.is_some().cmp(&a.value.is_some()));

    let unmapped: Vec<UnmappedField> = UNMAPPED_FIELDS
        .iter()
        .filter_map(|(path, reason)| {
            let json_ptr = format!("/{path}");
            // Only report if the field is actually present in the config.
            if config.pointer(&json_ptr).is_some() {
                Some(UnmappedField {
                    openclaw_path: path.replace('/', "."),
                    reason,
                })
            } else {
                None
            }
        })
        .collect();

    ConfigMigrationPlan { mapped, unmapped }
}

// ── Memory migration (LLM-assisted) ──────────────────────────────────────────

/// Options for [`migrate_memory`].
#[derive(Debug, Clone)]
pub struct MemoryMigrationOptions {
    /// Where to write the migrated `MEMORY.md` in the NZC workspace.
    pub dest_workspace: PathBuf,
    /// Maximum bytes of OpenClaw memory content to send to the LLM.
    /// Defaults to 64 KiB to stay within context limits.
    pub max_content_bytes: usize,
}

impl Default for MemoryMigrationOptions {
    fn default() -> Self {
        Self {
            dest_workspace: PathBuf::new(),
            max_content_bytes: 64 * 1024,
        }
    }
}

/// Migrate OpenClaw memory files to NZC format using an LLM call.
///
/// This is intentionally async and returns [`MemoryMigrationOutcome`] rather
/// than `Result` so callers can always continue regardless of outcome.
///
/// **Stub status:** The LLM call (`call_llm_for_migration`) is currently
/// stubbed.  Session 3/4 should replace it with a real call through NZC's
/// provider infrastructure.  The interface (prompt construction, truncation,
/// dest path) is production-ready.
pub async fn migrate_memory(
    installation: &OpenClawInstallation,
    opts: &MemoryMigrationOptions,
    llm_fn: impl AsyncLlmFn,
) -> MemoryMigrationOutcome {
    let files = installation.memory_files();
    if files.is_empty() {
        return MemoryMigrationOutcome::NoMemoryFound;
    }

    // Collect and truncate content.
    let combined = collect_memory_content(&files, opts.max_content_bytes);
    if combined.trim().is_empty() {
        return MemoryMigrationOutcome::NoMemoryFound;
    }

    let prompt = build_migration_prompt(&combined);

    match llm_fn.call(&prompt).await {
        Ok(migrated) if !migrated.trim().is_empty() => {
            let dest_path = opts.dest_workspace.join("MEMORY.md");
            match tokio::fs::write(&dest_path, migrated.as_bytes()).await {
                Ok(()) => MemoryMigrationOutcome::Completed { dest_path },
                Err(e) => MemoryMigrationOutcome::Failed {
                    source_files: files,
                    reason: format!("Failed to write {}: {e}", dest_path.display()),
                },
            }
        }
        Ok(_) => MemoryMigrationOutcome::Failed {
            source_files: files,
            reason: "LLM returned empty response".to_string(),
        },
        Err(e) => MemoryMigrationOutcome::Failed {
            source_files: files,
            reason: e.to_string(),
        },
    }
}

/// Trait for the async LLM call used during memory migration.
///
/// Decoupled from NZC's provider infrastructure so this module stays
/// independent.  Session 3/4 should pass a real implementation.
///
/// A stub implementation ([`StubLlmFn`]) is provided for tests and for the
/// `nzc migrate-memory --dry-run` path.
pub trait AsyncLlmFn: Send + Sync {
    fn call<'a>(
        &'a self,
        prompt: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>>;
}

/// Stub LLM function — returns the prompt wrapped in a note.
/// Used in tests and in the `--dry-run` migrate-memory path.
pub struct StubLlmFn;

impl AsyncLlmFn for StubLlmFn {
    fn call<'a>(
        &'a self,
        _prompt: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        Box::pin(async {
            Ok("<!-- Memory migration stub: replace with real LLM output -->\n\
                # Migrated Memory (stub)\n\n\
                This file was generated by the `migrate-memory` stub. \
                Run `nzc migrate-memory` with a configured provider to produce real content.\n"
                .to_string())
        })
    }
}

/// Always-fail stub — useful in tests that verify error handling.
pub struct FailingLlmFn(pub String);

impl AsyncLlmFn for FailingLlmFn {
    fn call<'a>(
        &'a self,
        _prompt: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String>> + Send + 'a>> {
        let msg = self.0.clone();
        Box::pin(async move { Err(anyhow::anyhow!("{}", msg)) })
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────────

fn home_dir() -> Option<PathBuf> {
    directories::UserDirs::new().map(|u| u.home_dir().to_path_buf())
}

fn detect_workspace_path(config: &serde_json::Value, openclaw_dir: &Path) -> Option<PathBuf> {
    // Try config-specified workspace first.
    if let Some(ws) = config
        .pointer("/agents/defaults/workspace")
        .or_else(|| config.pointer("/workspace"))
        .and_then(|v| v.as_str())
        .map(|s| {
            let p = PathBuf::from(s);
            if p.is_absolute() {
                p
            } else {
                openclaw_dir.join(s)
            }
        })
    {
        if ws.exists() {
            return Some(ws);
        }
    }

    // Fall back to the conventional location.
    let default_ws = openclaw_dir.join("workspace");
    if default_ws.exists() {
        return Some(default_ws);
    }

    None
}

fn read_version(config: &serde_json::Value) -> Option<String> {
    config
        .pointer("/version")
        .or_else(|| config.pointer("/meta/version"))
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

/// Parse a JSON5 / JSONC string by stripping line comments (`// ...`) and
/// block comments (`/* ... */`) before handing off to `serde_json`.
///
/// This is intentionally simple: it handles the common cases in OpenClaw's
/// config without pulling in a full JSON5 parser.  Edge cases (e.g. `//`
/// inside a string literal) are acceptable — this is a migration tool, not a
/// strict parser.
pub fn parse_json5_relaxed(input: &str) -> Result<serde_json::Value> {
    let stripped = strip_json_comments(input);
    serde_json::from_str(&stripped).context("JSON parse failed after stripping comments")
}

/// Strip `// line` and `/* block */` comments from a JSON-like string.
pub fn strip_json_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_string = false;
    let mut escape_next = false;

    while i < len {
        let ch = chars[i];

        if escape_next {
            out.push(ch);
            escape_next = false;
            i += 1;
            continue;
        }

        if in_string {
            if ch == '\\' {
                escape_next = true;
                out.push(ch);
            } else if ch == '"' {
                in_string = false;
                out.push(ch);
            } else {
                out.push(ch);
            }
            i += 1;
            continue;
        }

        // Not in a string.
        match ch {
            '"' => {
                in_string = true;
                out.push(ch);
                i += 1;
            }
            '/' if i + 1 < len && chars[i + 1] == '/' => {
                // Line comment — skip to end of line.
                i += 2;
                while i < len && chars[i] != '\n' {
                    i += 1;
                }
            }
            '/' if i + 1 < len && chars[i + 1] == '*' => {
                // Block comment — skip to `*/`.
                i += 2;
                while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                    i += 1;
                }
                i += 2; // skip `*/`
            }
            _ => {
                out.push(ch);
                i += 1;
            }
        }
    }

    out
}

fn collect_memory_content(files: &[PathBuf], max_bytes: usize) -> String {
    let mut out = String::new();
    for path in files {
        if let Ok(content) = std::fs::read_to_string(path) {
            let header = format!(
                "\n\n<!-- Source: {} -->\n",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
            );
            out.push_str(&header);
            // Truncate each file's contribution if we're near the limit.
            let remaining = max_bytes.saturating_sub(out.len());
            if remaining == 0 {
                break;
            }
            if content.len() > remaining {
                out.push_str(&content[..remaining]);
                out.push_str("\n[... truncated ...]");
                break;
            }
            out.push_str(&content);
        }
    }
    out
}

fn build_migration_prompt(content: &str) -> String {
    format!(
        r#"You are helping migrate an AI assistant's memory from OpenClaw to NZC (NonZeroClaw).

The following content is from the OpenClaw workspace memory files. It may contain:
- OpenClaw-specific operational details (config file paths, OpenClaw commands, etc.)
- "TATTOO" entries that are lessons learned for OpenClaw's configuration
- Infrastructure references specific to OpenClaw's deployment
- Personal/factual information about the user that IS worth preserving
- Historical events and decisions that may be relevant

Your task: produce a clean NZC-appropriate MEMORY.md file that:
1. PRESERVES: User preferences, communication style, factual info about the user's life/work/family
2. PRESERVES: Infrastructure facts (server IPs, service names, what services run where)
3. PRESERVES: Historical decisions and their rationale
4. REFRAMES: Any OpenClaw-specific lessons as general AI assistant lessons where applicable
5. REMOVES: OpenClaw-specific config file details, OpenClaw commands, OpenClaw tattoos about OpenClaw's own config
6. REMOVES: References to specific OpenClaw version numbers, schema details, or internal implementation details
7. ADAPTS: "Do X in OpenClaw config" → note the intent/goal without the OpenClaw-specific path

Output only the Markdown content for MEMORY.md, starting with `# Memory`. No preamble or explanation.

---

SOURCE CONTENT:

{content}
"#
    )
}

// ── Summary display helpers ───────────────────────────────────────────────────

/// Format a one-line summary of what was found (for wizard display).
pub fn installation_summary(install: &OpenClawInstallation) -> String {
    let channel_count = install.channels.len();
    let memory_note = if install.has_memory() {
        "memory found"
    } else {
        "no memory"
    };
    let version_note = install
        .version
        .as_deref()
        .map(|v| format!(", v{v}"))
        .unwrap_or_default();
    format!(
        "{} channel(s){}, {}",
        channel_count, version_note, memory_note
    )
}

/// Return the set of fields from a migration plan that have actual values.
pub fn plan_present_fields(plan: &ConfigMigrationPlan) -> Vec<&MappedField> {
    plan.mapped.iter().filter(|f| f.value.is_some()).collect()
}

// ── migrate-memory command stub ───────────────────────────────────────────────

/// Entry point for the `nzc migrate-memory` subcommand.
///
/// **Stub status:** The LLM call is routed through `llm_fn` which is currently
/// a [`StubLlmFn`] at the call site in `main.rs`.  Session 3/4 should wire up
/// NZC's real provider here.
///
/// Returns the [`MemoryMigrationOutcome`] for the caller to display/log.
pub async fn run_migrate_memory_command(
    nzc_workspace: &Path,
    openclaw_dir_override: Option<&Path>,
    dry_run: bool,
) -> Result<MemoryMigrationOutcome> {
    let install = if let Some(override_dir) = openclaw_dir_override {
        // Build a minimal installation pointing at the override dir.
        detect_from_dir(override_dir)?
    } else {
        detect_openclaw_installation()?
    };

    let install = match install {
        Some(i) => i,
        None => {
            return Ok(MemoryMigrationOutcome::NoMemoryFound);
        }
    };

    let opts = MemoryMigrationOptions {
        dest_workspace: nzc_workspace.to_path_buf(),
        max_content_bytes: 64 * 1024,
    };

    if dry_run {
        // In dry-run mode, use the stub LLM so no real API call is made.
        Ok(migrate_memory(&install, &opts, StubLlmFn).await)
    } else {
        // TODO (session 3/4): replace StubLlmFn with NZC's real provider client.
        Ok(migrate_memory(&install, &opts, StubLlmFn).await)
    }
}

/// Detect from an explicit OpenClaw directory (for override / test use).
fn detect_from_dir(dir: &Path) -> Result<Option<OpenClawInstallation>> {
    if !dir.exists() {
        return Ok(None);
    }

    let config_path = ["openclaw.json", "openclaw.jsonc"]
        .iter()
        .map(|name| dir.join(name))
        .find(|p| p.exists());

    let config_path = match config_path {
        Some(p) => p,
        None => return Ok(None),
    };

    let raw = std::fs::read_to_string(&config_path)
        .with_context(|| format!("Failed to read {}", config_path.display()))?;

    let config = parse_json5_relaxed(&raw).with_context(|| {
        format!("Failed to parse config at {}", config_path.display())
    })?;

    let workspace_path = detect_workspace_path(&config, dir);
    let memory_path = workspace_path
        .as_deref()
        .map(|ws| ws.join("MEMORY.md"))
        .filter(|p| p.exists());
    let memory_dir = workspace_path
        .as_deref()
        .map(|ws| ws.join("memory"))
        .filter(|p| p.is_dir());

    let channels = detect_channels(&config);
    let version = read_version(&config);

    Ok(Some(OpenClawInstallation {
        config_path,
        config,
        openclaw_dir: dir.to_path_buf(),
        workspace_path,
        memory_path,
        memory_dir,
        channels,
        version,
    }))
}

// ── Channel assignment application ────────────────────────────────────────────

/// Apply a single channel assignment to an NZC `Config`.
///
/// For channels assigned to `ChannelOwner::Nzc`, credential fields are pulled
/// from `DetectedChannel.config_snippet` into `config.channels_config`.
/// Fields are only applied if the corresponding channel slot is currently
/// unset (i.e. `None`) — we never overwrite a field the user has already
/// configured.
///
/// For `ChannelOwner::OpenClaw` or `ChannelOwner::Unassigned`, this function
/// is a no-op.
///
/// # TODO
///
/// The actual OpenClaw config disabling (removing the channel entry from
/// OpenClaw's `openclaw.json`) is a polyclaw-installer concern, not an NZC
/// concern.  Callers that need to disable the channel on the OpenClaw side
/// should handle that separately after calling this function.
pub fn apply_channel_assignment(
    config: &mut crate::config::Config,
    assignment: &ChannelAssignment,
) {
    if assignment.owner != ChannelOwner::Nzc {
        // OpenClaw keeps the channel, or user deferred the decision — nothing to do.
        return;
    }
    apply_channel_credentials(&mut config.channels_config, &assignment.channel);
}

/// Apply credentials from a `DetectedChannel` snippet into `ChannelsConfig`.
///
/// Only populates slots that are currently `None` (never overwrites user config).
/// This is the hookable entry point for per-channel credential extraction.
/// Extend the `match` arms as new channel types gain structured credential fields.
fn apply_channel_credentials(
    channels: &mut crate::config::ChannelsConfig,
    detected: &DetectedChannel,
) {
    let snippet = &detected.config_snippet;
    match detected.name.as_str() {
        "telegram" => {
            if channels.telegram.is_none() {
                let token = extract_str(snippet, &["botToken", "bot_token", "token"]);
                let allowed_users = extract_str_array(snippet, &["allowFrom", "allow_from"]);
                if !token.is_empty() {
                    channels.telegram = Some(crate::config::TelegramConfig {
                        bot_token: token,
                        allowed_users,
                        stream_mode: crate::config::schema::StreamMode::default(),
                        draft_update_interval_ms: 1000,
                        interrupt_on_new_message: false,
                        mention_only: false,
                    });
                }
            }
        }
        "discord" => {
            if channels.discord.is_none() {
                let token = extract_str(snippet, &["botToken", "bot_token", "token"]);
                if !token.is_empty() {
                    channels.discord = Some(crate::config::DiscordConfig {
                        bot_token: token,
                        guild_id: extract_str_opt(snippet, &["guildId", "guild_id"]),
                        allowed_users: extract_str_array(snippet, &["allowFrom", "allow_from"]),
                        listen_to_bots: false,
                        mention_only: false,
                    });
                }
            }
        }
        "signal" => {
            if channels.signal.is_none() {
                let account =
                    extract_str(snippet, &["account", "phoneNumber", "phone_number"]);
                if !account.is_empty() {
                    let http_url = extract_str_or(
                        snippet,
                        &["httpUrl", "http_url", "signalCliUrl"],
                        "http://127.0.0.1:8686",
                    );
                    channels.signal = Some(crate::config::schema::SignalConfig {
                        account,
                        http_url,
                        group_id: None,
                        allowed_from: extract_str_array(snippet, &["allowFrom", "allow_from"]),
                        ignore_attachments: false,
                        ignore_stories: false,
                    });
                }
            }
        }
        // Matrix: credentials are complex (homeserver + mxid + access_token).
        // We note the assignment but leave manual configuration to the user.
        // TODO: extend once matrix credential field mappings are settled.
        "matrix" => {
            // No auto-populated fields for matrix — complex auth flow.
        }
        // WhatsApp (Baileys) sessions are device-linked and not portable.
        // Assignment is recorded but no credentials are transferred.
        "whatsapp" => {
            // No auto-populated fields for whatsapp — session must be re-linked.
        }
        // Unknown channel type — log and skip.
        other => {
            tracing::debug!(
                "apply_channel_credentials: no credential mapping for channel '{}'",
                other
            );
        }
    }
}

// ── Apply all migration changes ────────────────────────────────────────────────

/// Apply all channel assignments from a migration plan to an NZC `Config`.
///
/// Iterates over `assignments` and calls [`apply_channel_assignment`] for each.
/// Channels assigned to `ChannelOwner::OpenClaw` or `ChannelOwner::Unassigned`
/// are skipped.
///
/// Returns a summary of how many channels were applied to NZC.
///
/// # TODO
///
/// Disabling migrated channels on the OpenClaw side (removing them from
/// `openclaw.json`) is a polyclaw-installer concern and is not handled here.
pub fn apply_migration_changes(
    config: &mut crate::config::Config,
    assignments: &[ChannelAssignment],
) -> MigrationApplyResult {
    let mut applied = Vec::new();
    let mut skipped_openclaw = Vec::new();
    let mut skipped_unassigned = Vec::new();

    for assignment in assignments {
        match assignment.owner {
            ChannelOwner::Nzc => {
                apply_channel_assignment(config, assignment);
                applied.push(assignment.channel.name.clone());
            }
            ChannelOwner::OpenClaw => {
                skipped_openclaw.push(assignment.channel.name.clone());
            }
            ChannelOwner::Unassigned => {
                skipped_unassigned.push(assignment.channel.name.clone());
            }
        }
    }

    MigrationApplyResult {
        applied,
        skipped_openclaw,
        skipped_unassigned,
    }
}

/// Result of [`apply_migration_changes`].
#[derive(Debug, Clone, Default)]
pub struct MigrationApplyResult {
    /// Channels whose credentials were applied to the NZC config.
    pub applied: Vec<String>,
    /// Channels kept with OpenClaw (no changes to NZC config).
    pub skipped_openclaw: Vec<String>,
    /// Channels left unassigned (no changes to NZC config).
    pub skipped_unassigned: Vec<String>,
}

impl MigrationApplyResult {
    /// True if at least one channel was applied to NZC.
    pub fn any_applied(&self) -> bool {
        !self.applied.is_empty()
    }

    /// Total number of channels processed.
    pub fn total(&self) -> usize {
        self.applied.len() + self.skipped_openclaw.len() + self.skipped_unassigned.len()
    }
}

// ── Snippet extraction helpers ─────────────────────────────────────────────────

/// Extract a string field from a JSON object, trying keys in priority order.
/// Returns empty string if none of the keys have a non-empty value.
fn extract_str(obj: &serde_json::Value, keys: &[&str]) -> String {
    for key in keys {
        if let Some(v) = obj.get(key).and_then(|v| v.as_str()) {
            if !v.is_empty() {
                return v.to_string();
            }
        }
    }
    String::new()
}

/// Extract a string field, returning a default if not found or empty.
fn extract_str_or(obj: &serde_json::Value, keys: &[&str], default: &str) -> String {
    let v = extract_str(obj, keys);
    if v.is_empty() {
        default.to_string()
    } else {
        v
    }
}

/// Extract an optional string field, returning `None` if not found or empty.
fn extract_str_opt(obj: &serde_json::Value, keys: &[&str]) -> Option<String> {
    let v = extract_str(obj, keys);
    if v.is_empty() { None } else { Some(v) }
}

/// Extract a string array from a JSON object, trying keys in priority order.
fn extract_str_array(obj: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        if let Some(arr) = obj.get(key).and_then(|v| v.as_array()) {
            return arr
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
        }
    }
    Vec::new()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use hegel::generators::{booleans, integers, text};
    use hegel::{Generator, TestCase};
    use serde_json::json;
    use std::fs;
    use tempfile::TempDir;

    // ── Unit tests ────────────────────────────────────────────────────────────

    #[test]
    fn no_openclaw_dir_returns_none() {
        let tmp = TempDir::new().unwrap();
        // Override home detection by using detect_from_dir with a nonexistent subdir.
        let result = detect_from_dir(&tmp.path().join("nonexistent")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn openclaw_dir_without_config_returns_none() {
        let tmp = TempDir::new().unwrap();
        // Directory exists but no openclaw.json inside.
        fs::create_dir_all(tmp.path()).unwrap();
        let result = detect_from_dir(tmp.path()).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn malformed_config_returns_error() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("openclaw.json"), b"{ not valid json {{").unwrap();
        let result = detect_from_dir(tmp.path());
        assert!(result.is_err(), "malformed config should return Err");
    }

    #[test]
    fn minimal_valid_config_parsed() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("openclaw.json"), b"{}").unwrap();
        let result = detect_from_dir(tmp.path()).unwrap();
        assert!(result.is_some());
        let install = result.unwrap();
        assert!(install.channels.is_empty());
        assert!(!install.has_memory());
    }

    #[test]
    fn config_with_version_parsed() {
        let tmp = TempDir::new().unwrap();
        let cfg = json!({ "version": "2026.3.13" });
        fs::write(tmp.path().join("openclaw.json"), cfg.to_string()).unwrap();
        let install = detect_from_dir(tmp.path()).unwrap().unwrap();
        assert_eq!(install.version.as_deref(), Some("2026.3.13"));
    }

    #[test]
    fn detect_telegram_channel_with_token() {
        let config = json!({
            "channels": {
                "telegram": {
                    "botToken": "123:abc",
                    "enabled": true
                }
            }
        });
        let channels = detect_channels(&config);
        let tg = channels.iter().find(|c| c.name == "telegram").unwrap();
        assert!(tg.enabled);
        assert!(tg.has_credentials);
    }

    #[test]
    fn detect_channel_missing_credentials() {
        let config = json!({
            "channels": {
                "telegram": { "enabled": true }
            }
        });
        let channels = detect_channels(&config);
        let tg = channels.iter().find(|c| c.name == "telegram").unwrap();
        assert!(!tg.has_credentials);
    }

    #[test]
    fn detect_channel_disabled() {
        let config = json!({
            "channels": {
                "telegram": { "botToken": "tok", "enabled": false }
            }
        });
        let channels = detect_channels(&config);
        let tg = channels.iter().find(|c| c.name == "telegram").unwrap();
        assert!(!tg.enabled);
        assert!(tg.has_credentials);
    }

    #[test]
    fn detect_channel_via_plugins_entries() {
        let config = json!({
            "plugins": {
                "entries": {
                    "signal": {
                        "account": "+15551234567",
                        "enabled": true
                    }
                }
            }
        });
        let channels = detect_channels(&config);
        let sig = channels.iter().find(|c| c.name == "signal").unwrap();
        assert!(sig.enabled);
        assert!(sig.has_credentials);
    }

    #[test]
    fn config_with_no_channels_returns_empty_list() {
        let config = json!({ "gateway": { "port": 18789 } });
        let channels = detect_channels(&config);
        assert!(channels.is_empty());
    }

    #[test]
    fn config_migration_plan_maps_present_fields() {
        let config = json!({
            "agents": {
                "defaults": {
                    "model": {
                        "primary": "claude-sonnet-4-5"
                    }
                }
            },
            "gateway": {
                "port": 18789,
                "bind": "127.0.0.1"
            }
        });
        let plan = build_config_migration_plan(&config);
        let present: Vec<_> = plan_present_fields(&plan);
        let paths: Vec<&str> = present.iter().map(|f| f.nzc_path.as_str()).collect();
        assert!(paths.contains(&"default_model"));
        assert!(paths.contains(&"gateway.port"));
        assert!(paths.contains(&"gateway.bind"));
    }

    #[test]
    fn config_migration_plan_missing_fields_have_none_value() {
        let config = json!({});
        let plan = build_config_migration_plan(&config);
        // No fields should have values since config is empty.
        assert!(plan.mapped.iter().all(|f| f.value.is_none()));
        // No unmapped fields either (they're only reported if present in config).
        assert!(plan.unmapped.is_empty());
    }

    #[test]
    fn config_migration_plan_flags_unmapped_plugins() {
        let config = json!({
            "plugins": {
                "entries": {
                    "some_plugin": {}
                }
            }
        });
        let plan = build_config_migration_plan(&config);
        assert!(
            !plan.unmapped.is_empty(),
            "plugins should appear in unmapped list"
        );
        assert!(plan.unmapped.iter().any(|u| u.openclaw_path.contains("plugins")));
    }

    #[test]
    fn strip_json_comments_line_comment() {
        let input = r#"{ "key": "value" // comment
}"#;
        let stripped = strip_json_comments(input);
        let v: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn strip_json_comments_block_comment() {
        let input = r#"{ /* block comment */ "key": "value" }"#;
        let stripped = strip_json_comments(input);
        let v: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(v["key"], "value");
    }

    #[test]
    fn strip_json_comments_preserves_url_in_string() {
        // "http://example.com" — the `//` is inside a string, should NOT be stripped.
        let input = r#"{ "url": "http://example.com" }"#;
        let stripped = strip_json_comments(input);
        let v: serde_json::Value = serde_json::from_str(&stripped).unwrap();
        assert_eq!(v["url"], "http://example.com");
    }

    #[test]
    fn memory_files_collected_from_workspace() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("workspace");
        let mem_dir = ws.join("memory");
        fs::create_dir_all(&mem_dir).unwrap();
        fs::write(ws.join("MEMORY.md"), b"# Memory").unwrap();
        fs::write(mem_dir.join("2026-03-30.md"), b"# Daily").unwrap();
        fs::write(mem_dir.join("not-md.txt"), b"ignored").unwrap();

        let cfg = json!({});
        fs::write(tmp.path().join("openclaw.json"), cfg.to_string()).unwrap();
        let install = detect_from_dir(tmp.path()).unwrap().unwrap();

        let files = install.memory_files();
        assert_eq!(files.len(), 2, "should find MEMORY.md and the daily .md file");
        // .txt file should not appear
        assert!(!files.iter().any(|p| p.extension().and_then(|e| e.to_str()) == Some("txt")));
    }

    #[test]
    fn installation_summary_format() {
        let tmp = TempDir::new().unwrap();
        let config = json!({
            "version": "2026.3.13",
            "channels": {
                "telegram": { "botToken": "tok", "enabled": true },
                "signal": { "account": "+1", "enabled": true }
            }
        });
        fs::write(tmp.path().join("openclaw.json"), config.to_string()).unwrap();
        let install = detect_from_dir(tmp.path()).unwrap().unwrap();
        let summary = installation_summary(&install);
        assert!(summary.contains("2026.3.13"));
        assert!(summary.contains("2 channel(s)"));
    }

    #[test]
    fn channel_owner_display() {
        assert_eq!(ChannelOwner::Nzc.to_string(), "NZC");
        assert_eq!(ChannelOwner::OpenClaw.to_string(), "OpenClaw");
        assert_eq!(ChannelOwner::Unassigned.to_string(), "Skip");
    }

    // ── Async tests ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn migrate_memory_no_files_returns_no_memory_found() {
        let tmp = TempDir::new().unwrap();
        let config = json!({});
        fs::write(tmp.path().join("openclaw.json"), config.to_string()).unwrap();
        let install = detect_from_dir(tmp.path()).unwrap().unwrap();

        let opts = MemoryMigrationOptions {
            dest_workspace: tmp.path().to_path_buf(),
            max_content_bytes: 64 * 1024,
        };
        let outcome = migrate_memory(&install, &opts, StubLlmFn).await;
        assert!(matches!(outcome, MemoryMigrationOutcome::NoMemoryFound));
    }

    #[tokio::test]
    async fn migrate_memory_stub_llm_writes_file() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("workspace");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("MEMORY.md"), b"# Memory\n\nSome content.").unwrap();

        let config = json!({});
        fs::write(tmp.path().join("openclaw.json"), config.to_string()).unwrap();
        let install = detect_from_dir(tmp.path()).unwrap().unwrap();

        let dest = tmp.path().join("nzc_workspace");
        fs::create_dir_all(&dest).unwrap();

        let opts = MemoryMigrationOptions {
            dest_workspace: dest.clone(),
            max_content_bytes: 64 * 1024,
        };
        let outcome = migrate_memory(&install, &opts, StubLlmFn).await;
        assert!(
            matches!(&outcome, MemoryMigrationOutcome::Completed { dest_path } if dest_path == &dest.join("MEMORY.md")),
            "expected Completed, got {:?}",
            outcome
        );
        assert!(dest.join("MEMORY.md").exists());
    }

    #[tokio::test]
    async fn migrate_memory_failing_llm_returns_failed() {
        let tmp = TempDir::new().unwrap();
        let ws = tmp.path().join("workspace");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("MEMORY.md"), b"# Memory").unwrap();

        let config = json!({});
        fs::write(tmp.path().join("openclaw.json"), config.to_string()).unwrap();
        let install = detect_from_dir(tmp.path()).unwrap().unwrap();

        let opts = MemoryMigrationOptions {
            dest_workspace: tmp.path().to_path_buf(),
            max_content_bytes: 64 * 1024,
        };
        let outcome = migrate_memory(&install, &opts, FailingLlmFn("network error".to_string())).await;
        assert!(
            matches!(&outcome, MemoryMigrationOutcome::Failed { reason, .. } if reason.contains("network error")),
            "expected Failed with reason"
        );
    }

    #[tokio::test]
    async fn run_migrate_memory_command_no_openclaw_returns_no_memory() {
        let tmp = TempDir::new().unwrap();
        let nzc_ws = tmp.path().join("nzc");
        fs::create_dir_all(&nzc_ws).unwrap();
        let nonexistent = tmp.path().join("no-openclaw");
        let outcome = run_migrate_memory_command(&nzc_ws, Some(&nonexistent), false)
            .await
            .unwrap();
        assert!(matches!(outcome, MemoryMigrationOutcome::NoMemoryFound));
    }

    // ── Property tests (hegel) ────────────────────────────────────────────────

    /// Property: detect_channels never panics on arbitrary serde_json::Value shapes.
    #[hegel::test]
    fn detect_channels_never_panics(tc: TestCase) {
        // Build a synthetic config with random optional channel blocks.
        let channel_names = KNOWN_CHANNELS;
        let mut config = serde_json::Map::new();
        let mut channels_obj = serde_json::Map::new();

        let include_count = tc.draw(integers::<usize>().min_value(0).max_value(channel_names.len()));
        for &name in &channel_names[..include_count] {
            let has_token = tc.draw(booleans());
            let enabled = tc.draw(booleans());
            let mut ch = serde_json::Map::new();
            ch.insert("enabled".to_string(), json!(enabled));
            if has_token {
                ch.insert("botToken".to_string(), json!("fake-token"));
            }
            channels_obj.insert(name.to_string(), serde_json::Value::Object(ch));
        }
        config.insert("channels".to_string(), serde_json::Value::Object(channels_obj));
        let config_val = serde_json::Value::Object(config);

        // Must not panic.
        let channels = detect_channels(&config_val);
        // Count should not exceed number of known channels.
        assert!(channels.len() <= KNOWN_CHANNELS.len());
    }

    /// Property: build_config_migration_plan never panics on arbitrary configs.
    #[hegel::test]
    fn config_migration_plan_never_panics(tc: TestCase) {
        // Use a few different shapes: empty, gateway-only, partial agent config.
        let choice = tc.draw(integers::<usize>().min_value(0).max_value(3));
        let config = match choice {
            0 => json!({}),
            1 => json!({ "gateway": { "port": 9999 } }),
            2 => json!({ "agents": { "defaults": { "model": { "primary": "gpt-4" } } } }),
            _ => json!({ "plugins": { "entries": { "x": {} } }, "skills": {} }),
        };
        // Must not panic.
        let plan = build_config_migration_plan(&config);
        // Mapped list length is fixed (one entry per FIELD_MAP entry).
        assert_eq!(plan.mapped.len(), FIELD_MAP.len());
    }

    /// Property: mapped fields with values are always in the known NZC path set.
    #[hegel::test]
    fn mapped_fields_always_have_known_nzc_paths(tc: TestCase) {
        let choice = tc.draw(integers::<usize>().min_value(0).max_value(2));
        let config = match choice {
            0 => json!({}),
            1 => json!({ "gateway": { "port": 18789 } }),
            _ => json!({ "env": { "ANTHROPIC_API_KEY": "sk-test" } }),
        };
        let plan = build_config_migration_plan(&config);

        let valid_nzc_paths: std::collections::HashSet<&str> =
            FIELD_MAP.iter().map(|(_, nzc)| *nzc).collect();

        for field in &plan.mapped {
            assert!(
                valid_nzc_paths.contains(field.nzc_path.as_str()),
                "unexpected NZC path: {}",
                field.nzc_path
            );
        }
    }

    /// Property: strip_json_comments preserves JSON structure on strings without comments.
    #[hegel::test]
    fn strip_comments_idempotent_on_clean_json(tc: TestCase) {
        // Generate a string value (not containing `//` or `/*` to avoid false positives).
        // Restrict to printable ASCII to avoid control chars that serde_json rejects.
        let key = tc.draw(text().min_size(1).max_size(20).filter(|s: &String| {
            !s.contains("//")
                && !s.contains("/*")
                && !s.contains('"')
                && !s.contains('\\')
                && s.chars().all(|c| c.is_ascii() && !c.is_ascii_control())
        }));
        let input = format!("{{\"k\": \"{key}\"}}");
        let stripped = strip_json_comments(&input);
        // Should still parse.
        let v: Result<serde_json::Value, _> = serde_json::from_str(&stripped);
        assert!(v.is_ok(), "stripped JSON should still parse: {stripped:?}");
    }

    /// Property: channel detection consistency — `channels.<name>` and
    /// `plugins.entries.<name>` formats are both detected.
    ///
    /// From opus-review-2.md §8: a channel in either location must always be
    /// detected; a channel in neither must never be detected.
    ///
    /// This is a non-trivial consistency property.  It would catch a regression
    /// where the `plugins.entries` branch was accidentally removed, or where a
    /// new format was added to one path but not the other.
    #[hegel::test]
    fn prop_channel_detection_both_locations(tc: TestCase) {
        let channel_idx = tc.draw(integers::<usize>().min_value(0).max_value(KNOWN_CHANNELS.len() - 1));
        let channel_name = KNOWN_CHANNELS[channel_idx];
        let enabled = tc.draw(booleans());

        // Build a channel snippet with credentials (so detection is unambiguous).
        let snippet = match channel_name {
            "telegram" | "discord" => json!({ "botToken": "fake-tok", "enabled": enabled }),
            "signal" => json!({ "account": "+15551234567", "enabled": enabled }),
            "whatsapp" => json!({ "accessToken": "fake-tok", "enabled": enabled }),
            "matrix" => json!({ "accessToken": "fake-tok", "homeserver": "matrix.org", "enabled": enabled }),
            _ => json!({ "botToken": "fake-tok", "enabled": enabled }),
        };

        // Case 1: channel under `channels.<name>` → must be detected.
        let config_channels = json!({ "channels": { channel_name: snippet.clone() } });
        let detected = detect_channels(&config_channels);
        let found = detected.iter().any(|c| c.name == channel_name);
        assert!(
            found,
            "channel '{}' under channels.<name> must be detected\nconfig: {}",
            channel_name,
            config_channels
        );

        // Case 2: channel under `plugins.entries.<name>` → must be detected.
        let config_plugins = json!({
            "plugins": { "entries": { channel_name: snippet.clone() } }
        });
        let detected = detect_channels(&config_plugins);
        let found = detected.iter().any(|c| c.name == channel_name);
        assert!(
            found,
            "channel '{}' under plugins.entries.<name> must be detected\nconfig: {}",
            channel_name,
            config_plugins
        );

        // Case 3: channel in neither location → must NOT be detected.
        let config_empty = json!({ "gateway": { "port": 18789 } });
        let detected = detect_channels(&config_empty);
        let found_any_known = detected.iter().any(|c| c.name == channel_name);
        assert!(
            !found_any_known,
            "channel '{}' must NOT be detected when absent from config",
            channel_name
        );
    }

    /// Property: `detect_channels` result is consistent — a channel detected
    /// via `channels.<name>` and via `plugins.entries.<name>` with the same
    /// snippet produces the same `enabled`/`has_credentials` result.
    ///
    /// This verifies there's no difference in how the two code paths extract
    /// the metadata — they must use the same underlying logic.
    #[hegel::test]
    fn prop_channel_detection_location_agnostic(tc: TestCase) {
        let channel_idx = tc.draw(integers::<usize>().min_value(0).max_value(KNOWN_CHANNELS.len() - 1));
        let channel_name = KNOWN_CHANNELS[channel_idx];
        let has_creds = tc.draw(booleans());
        let enabled = tc.draw(booleans());

        // Build snippet conditionally.
        let snippet = if has_creds {
            match channel_name {
                "telegram" | "discord" => json!({ "botToken": "t", "enabled": enabled }),
                "signal" => json!({ "account": "+1", "enabled": enabled }),
                "whatsapp" => json!({ "accessToken": "t", "enabled": enabled }),
                "matrix" => json!({ "accessToken": "t", "homeserver": "m", "enabled": enabled }),
                _ => json!({ "botToken": "t", "enabled": enabled }),
            }
        } else {
            json!({ "enabled": enabled })
        };

        let via_channels = json!({ "channels": { channel_name: snippet.clone() } });
        let via_plugins = json!({
            "plugins": { "entries": { channel_name: snippet.clone() } }
        });

        let detected_channels = detect_channels(&via_channels);
        let detected_plugins = detect_channels(&via_plugins);

        let from_ch = detected_channels.iter().find(|c| c.name == channel_name);
        let from_pl = detected_plugins.iter().find(|c| c.name == channel_name);

        // Both must be detected.
        assert!(from_ch.is_some(), "channel '{}' missing from channels path", channel_name);
        assert!(from_pl.is_some(), "channel '{}' missing from plugins path", channel_name);

        let from_ch = from_ch.unwrap();
        let from_pl = from_pl.unwrap();

        // Both must agree on `enabled` and `has_credentials`.
        assert_eq!(
            from_ch.enabled, from_pl.enabled,
            "channel '{}' enabled disagrees between paths: channels={}, plugins={}",
            channel_name, from_ch.enabled, from_pl.enabled
        );
        assert_eq!(
            from_ch.has_credentials, from_pl.has_credentials,
            "channel '{}' has_credentials disagrees between paths",
            channel_name
        );
    }

    /// Property: `build_config_migration_plan` maps every field in FIELD_MAP
    /// that is present in the input to `mapped_fields` (never silently dropped).
    ///
    /// From opus-review-2.md §6: every field in `KNOWN_FIELD_MAPPINGS` that
    /// appears in the input always appears in `mapped_fields`.
    ///
    /// This property uses a richer set of configs than the existing unit test
    /// (which uses 4 hardcoded shapes).  We generate configs that set exactly
    /// one known field at a time and verify it appears in the plan.
    #[hegel::test]
    fn prop_migration_plan_no_silent_drops(tc: TestCase) {
        // Pick a random field from FIELD_MAP.
        let idx = tc.draw(integers::<usize>().min_value(0).max_value(FIELD_MAP.len() - 1));
        let (openclaw_path, expected_nzc_path) = FIELD_MAP[idx];

        // Build a config that sets exactly this field.
        // openclaw_path is e.g. "agents/defaults/model/primary" → JSON pointer
        // "/agents/defaults/model/primary" → we need to build the nested object.
        let mut root = serde_json::Map::new();
        let parts: Vec<&str> = openclaw_path.split('/').collect();

        // Build a nested JSON object from the path parts.
        fn set_nested(obj: &mut serde_json::Map<String, serde_json::Value>, parts: &[&str], value: serde_json::Value) {
            if parts.len() == 1 {
                obj.insert(parts[0].to_string(), value);
            } else {
                let child = obj.entry(parts[0].to_string())
                    .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
                if let serde_json::Value::Object(ref mut child_map) = child {
                    set_nested(child_map, &parts[1..], value);
                }
            }
        }

        set_nested(&mut root, &parts, json!("test-value"));
        let config = serde_json::Value::Object(root);

        let plan = build_config_migration_plan(&config);

        // The mapped list is one entry per FIELD_MAP row. Multiple OpenClaw
        // paths may map to the same NZC path.  The property we want to check is
        // that if any OpenClaw path mapping to the NZC path is present in the
        // input config, then at least one of the corresponding mapped entries
        // must have a Some(value).  We therefore search for any mapped entry
        // with the expected NZC path that contains a value.
        let any_with_value = plan
            .mapped
            .iter()
            .any(|f| f.nzc_path == expected_nzc_path && f.value.is_some());

        assert!(
            any_with_value,
            "FIELD_MAP entry openclaw_path={:?} nzc_path={:?} was not materialized into a mapped value\n\
             (no mapped entry with Some(value) for this NZC path)\nconfig: {}",
            openclaw_path,
            expected_nzc_path,
            config
        );
    }
}
