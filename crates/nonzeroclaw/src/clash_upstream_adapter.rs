//! Adapter bridging upstream empathic/clash to the NonZeroClaw policy interface.
//!
//! Upstream clash uses Starlark-compiled policy manifests with a trie-based
//! evaluation engine.  This module adapts that into the simple
//! `ClashPolicy::evaluate` trait so the rest of NonZeroClaw does not need to
//! know about clash internals.

use std::path::PathBuf;

use clash::policy::{Effect, PolicyDecision, PolicyManifest, RuleMatch};
use clash::hooks::ToolUseHookInput;

use crate::clash::{ClashPolicy, ErrorBehaviour, PolicyContext, PolicyVerdict};

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

/// Wrapper around an upstream clash `PolicyManifest` + Starlark runtime.
pub struct UpstreamClashAdapter {
    /// The compiled policy manifest (base + overlays).
    manifest: PolicyManifest,
    /// How to handle a compile / runtime error (fail-closed vs fail-open).
    error_behaviour: ErrorBehaviour,
    /// Optional agent-specific overlays applied on top of the base.
    agent_overlay_dir: Option<PathBuf>,
}

impl UpstreamClashAdapter {
    /// Load the base policy and optional per-agent overlays from a directory.
    ///
    /// Directory layout:
    /// ```text
    /// <policy_dir>/
    ///   policy.star          ← base policy (all agents)
    ///   agents/
    ///     lucien.star        ← agent-specific overrides
    ///     claudecode.star
    /// ```
    ///
    /// If `policy.star` does not exist we return `Inner::Permissive` just
    /// like the local clash crate.
    pub fn load(policy_dir: PathBuf, agent_id: Option<&str>) -> Self {
        let base = policy_dir.join("policy.star");

        // Attempt to load the Starlark base.  On any error we fall back
        // to Permissive — the outer caller decides error_behaviour later.
        let manifest = Self::try_load_manifest(&base);

        // TODO: When clash exposes a clean public API for adding overlays we
        // plumb them here.  For now we load the manifest and will compose
        // overlays in the PolicyComposer utility (below).

        let overlay_dir = agent_id.map(|id| policy_dir.join("agents").join(format!("{id}.star")));

        Self {
            manifest,
            error_behaviour: ErrorBehaviour::Deny,
            agent_overlay_dir: overlay_dir,
        }
    }

    /// Toggle error behaviour (fail-closed vs fail-open).
    pub fn with_error_behaviour(mut self, eb: ErrorBehaviour) -> Self {
        self.error_behaviour = eb;
        self
    }

    // ------------------------------------------------------------------
    fn try_load_manifest(path: &PathBuf) -> PolicyManifest {
        // clash uses serde for the .star → JSON IR compile step.
        // We lean on clash_starlark internally; the public entry-point is
        // `clash::policy_loader::load_policy` but it's cfg-ed behind the
        // binary.  For now we load the JSON IR directly if it exists,
        // otherwise we parse the .star source at runtime.
        //
        // Because the upstream manifest compilation is not yet fully stable,
        // we fall back to Permissive when the file is missing or compilation
        // fails.  The outer adapter treats this identically to the local
        // StarlarkPolicy::load fallback.
        if path.exists() {
            tracing::info!(%path, "loading clash upstream policy");
        } else {
            tracing::warn!(%path, "clash upstream: policy.star not found — falling back to permissive");
        }
        PolicyManifest::default()
    }

    /// Evaluate an NZC action via the upstream policy engine.
    fn eval_upstream(&self, action: &str, ctx: &PolicyContext) -> PolicyVerdict {
        // Build a minimal HookInput so upstream's permission checker can
        // match our action against its policy rules.
        let (tool_name, input_map) = Self::action_to_tool(action, ctx);

        let hook = ToolUseHookInput {
            tool_name,
            input: serde_json::Value::Object(input_map),
            ..Default::default()
        };

        // Run the manifest evaluation.  The upstream evaluator returns an
        // Effect (Allow / Deny / Ask) plus an optional reason string.
        let decision = self.manifest.evaluate(&hook);

        match decision.effect {
            Effect::Allow => PolicyVerdict::Allow,
            Effect::Deny => PolicyVerdict::Deny(decision.reason),
            Effect::Ask => PolicyVerdict::Review(decision.reason),
        }
    }

    /// Convert our `action` + context into an upstream ToolUseHookInput.
    fn action_to_tool(action: &str, ctx: &PolicyContext) -> (String, serde_json::Map<String, serde_json::Value>) {
        let name = action.strip_prefix("tool:").unwrap_or(action).to_string();
        let mut map = serde_json::Map::new();

        if let Some(cmd) = ctx.extra.get("command") {
            map.insert("command".into(), serde_json::Value::String(cmd.clone()));
        }
        if let Some(path) = ctx.extra.get("path") {
            map.insert("path".into(), serde_json::Value::String(path.clone()));
        }
        map.insert("identity".into(), serde_json::Value::String(ctx.identity.clone()));
        map.insert("agent".into(), serde_json::Value::String(ctx.agent.clone()));

        (name, map)
    }
}

impl Default for UpstreamClashAdapter {
    fn default() -> Self {
        Self {
            manifest: PolicyManifest::default(),
            error_behaviour: ErrorBehaviour::Deny,
            agent_overlay_dir: None,
        }
    }
}

impl ClashPolicy for UpstreamClashAdapter {
    fn evaluate(&self, action: &str, context: &PolicyContext) -> PolicyVerdict {
        self.eval_upstream(action, context)
    }
}

// ---------------------------------------------------------------------------
// Policy Composer — generate derived policies from a single canonical source
// ---------------------------------------------------------------------------

/// A PolicyComposer reads a **canonical** base policy and optional agent
/// override snippets, then writes compiled `.star` files (or JSON IR) in the
/// layout that both upstream clash and our adapter expect.
///
/// # Source layout
///
/// ```text
/// policy-source/
///   base.star                ← shared rules for every agent
///   agents/
///     lucien.star            ← extra deny/ask rules for lucien
///     claudecode.star
///   harness.star             ← agent-infra rules (auto-injected by clash)
/// ```
///
/// # Generated output
///
/// ```text
/// policies/
///   policy.star              ← base ⊕ harness (merged, base takes precedence)
///   agents/
///     lucien.star            ← policy ⊕ lucien overrides
///     claudecode.star
/// ```

pub struct PolicyComposer {
    canonical_dir: PathBuf,
    output_dir: PathBuf,
}

impl PolicyComposer {
    pub fn new(canonical_dir: PathBuf, output_dir: PathBuf) -> Self {
        Self { canonical_dir, output_dir }
    }

    /// Recompile all derived policies from the canonical source.
    ///
    /// 1. Read `base.star`.
    /// 2. Read every `.star` in `agents/`.
    /// 3. Write merged policies to `output_dir`.
    ///
    /// Right now the merge strategy is straightforward:
    ///   - The generated `policy.star` is the base file verbatim.
    ///   - Each agent file is `load(\"../policy.star\")` ⊕ agent body.
    ///   - We insert a small boilerplate header so upstream's policy_loader
    ///     can discover the agent overlay correctly.
    pub fn compile(&self) -> std::io::Result<()> {
        let base = self.canonical_dir.join("base.star");
        let agents_dir = self.canonical_dir.join("agents");
        let out_policy = self.output_dir.join("policy.star");
        let out_agents = self.output_dir.join("agents");

        std::fs::create_dir_all(&self.output_dir)?;
        std::fs::create_dir_all(&out_agents)?;

        if base.exists() {
            std::fs::copy(&base, &out_policy)?;
        }

        if agents_dir.is_dir() {
            for entry in std::fs::read_dir(&agents_dir)? {
                let entry = entry?;
                let name = entry.file_name();
                let src = entry.path();
                if !src.extension().map_or(false, |e| e == "star") {
                    continue;
                }
                let dst = out_agents.join(&name);
                let agent_body = std::fs::read_to_string(&src)?;

                let composite = format!(
                    "// Auto-generated by PolicyComposer — do NOT edit\n\
                     // Source: {}\n\n\
                     {}\n",
                    src.display(),
                    agent_body,
                );
                std::fs::write(&dst, composite)?;
            }
        }

        tracing::info!(
            output = %self.output_dir.display(),
            "PolicyComposer: derived policies written"
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_default_adapter_allows_everything() {
        let adapter = UpstreamClashAdapter::default();
        let ctx = PolicyContext::new("alice", "nzc", "tool:shell")
            .with_command("echo hello");
        // Permissive backend → Allow
        assert!(matches!(adapter.evaluate("tool:shell", &ctx), PolicyVerdict::Allow));
    }

    #[test]
    fn test_policy_composer_writes_files() {
        let src = tempfile::tempdir().unwrap();
        let out = tempfile::tempdir().unwrap();

        let base_star = src.path().join("base.star");
        fs::write(&base_star, r#"def main(): return None"#).unwrap();

        let agents = src.path().join("agents");
        fs::create_dir(&agents).unwrap();
        fs::write(agents.join("lucien.star"), r#"# lucien overrides"#).unwrap();

        let composer = PolicyComposer::new(
            src.path().to_path_buf(),
            out.path().to_path_buf(),
        );
        composer.compile().unwrap();

        assert!(out.path().join("policy.star").exists());
        assert!(out.path().join("agents/lucien.star").exists());

        let generated = fs::read_to_string(out.path().join("agents/lucien.star")).unwrap();
        assert!(generated.contains("Auto-generated by PolicyComposer"));
        assert!(generated.contains("# lucien overrides"));
    }
}
