//! Agent instruction-file support for Calciforge-managed installs.
//!
//! These files are prompt surface, so writes are explicit and bounded by
//! managed markers. The default behavior is to print the block for review.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

const BEGIN_MARKER: &str = "<!-- BEGIN CALCIFORGE MANAGED INSTRUCTIONS -->";
const END_MARKER: &str = "<!-- END CALCIFORGE MANAGED INSTRUCTIONS -->";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstructionTarget {
    Print,
    File(PathBuf),
    Workspace(PathBuf),
}

impl InstructionTarget {
    pub fn path(&self) -> Option<PathBuf> {
        match self {
            InstructionTarget::Print => None,
            InstructionTarget::File(path) => Some(path.clone()),
            InstructionTarget::Workspace(path) => Some(path.join("AGENTS.md")),
        }
    }
}

pub fn render_calciforge_instructions() -> String {
    format!(
        r#"{BEGIN_MARKER}
## Calciforge

This workspace can use Calciforge for secret references, policy checks, and
agent-facing coordination APIs.

- Do not ask the user to paste plaintext secrets into chat.
- When a secret is needed, ask the user to store it with Calciforge, then use
  the reference form `{{{{secret:NAME}}}}` exactly.
- Prefer the `calciforge-secrets` CLI only when it is available on `PATH` on
  this same agent host and configured to talk to the intended central
  Calciforge secret API. In multi-node deployments, do not assume a CLI
  installed on another node is reachable locally.
- Calciforge-managed installs place an API-backed helper wrapper at
  `$HOME/.local/bin/calciforge-secrets` on managed agent hosts. If
  `calciforge-secrets` is not on `PATH`, use that absolute path. Do not create
  a second local fnox vault on an agent node unless the operator explicitly
  asks for a separate local secret store.
- If local CLI access is unavailable, use the configured Calciforge MCP server
  or operator-documented Calciforge API endpoint for secret-name discovery and
  references.
- If you generate a new credential locally, pipe it directly into
  `calciforge-secrets set NAME --stdin` instead of printing it in chat. Use
  `fnox` directly only when you are explicitly running on the Calciforge host.
- Treat Calciforge MCP/API surfaces as optional capabilities. If MCP is not
  configured, use the CLI and documented HTTP endpoints instead.
- Calciforge may enforce destination-scoped secret substitution and security
  gateway policy outside your control. Do not try to bypass it.

Useful commands:

```bash
calciforge-secrets list
printf '%s' "$VALUE" | calciforge-secrets set NAME --stdin
```
{END_MARKER}
"#
    )
}

pub fn upsert_managed_section(existing: &str, section: &str) -> String {
    if let Some(begin) = existing.find(BEGIN_MARKER)
        && let Some(end_rel) = existing[begin..].find(END_MARKER)
    {
        let end = begin + end_rel + END_MARKER.len();
        let mut out = String::new();
        out.push_str(existing[..begin].trim_end());
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(section.trim_end());
        out.push_str(existing[end..].trim_start_matches(['\r', '\n']));
        if !out.ends_with('\n') {
            out.push('\n');
        }
        return out;
    }

    let mut out = existing.trim_end().to_string();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(section.trim_end());
    out.push('\n');
    out
}

pub fn write_instruction_file(path: &Path, dry_run: bool) -> Result<String> {
    let section = render_calciforge_instructions();
    if dry_run {
        return Ok(format!(
            "would create or update managed Calciforge section in {}",
            path.display()
        ));
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create instruction-file directory {}",
                parent.display()
            )
        })?;
    }

    let existing = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => {
            return Err(err).with_context(|| {
                format!(
                    "failed to read existing instruction file {}",
                    path.display()
                )
            });
        }
    };
    let updated = upsert_managed_section(&existing, &section);
    fs::write(path, updated)
        .with_context(|| format!("failed to write instruction file {}", path.display()))?;

    Ok(format!(
        "created or updated managed Calciforge section in {}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rendered_block_teaches_secret_references_and_cli_pipe() {
        let block = render_calciforge_instructions();
        assert!(block.contains("{{secret:NAME}}"));
        assert!(block.contains("calciforge-secrets set NAME --stdin"));
        assert!(block.contains("$HOME/.local/bin/calciforge-secrets"));
        assert!(block.contains("Do not ask the user to paste plaintext secrets"));
        assert!(block.contains("In multi-node deployments"));
        assert!(block.contains("If local CLI access is unavailable"));
    }

    #[test]
    fn upsert_appends_when_no_managed_section_exists() {
        let section = render_calciforge_instructions();
        let updated = upsert_managed_section("# Existing\n", &section);
        assert!(updated.starts_with("# Existing\n\n<!-- BEGIN CALCIFORGE"));
        assert_eq!(updated.matches(BEGIN_MARKER).count(), 1);
    }

    #[test]
    fn upsert_replaces_existing_managed_section() {
        let section = render_calciforge_instructions();
        let old =
            format!("# Existing\n\n{BEGIN_MARKER}\nold secret guidance\n{END_MARKER}\n\n# Later\n");
        let updated = upsert_managed_section(&old, &section);
        assert!(!updated.contains("old secret guidance"));
        assert!(updated.contains("{{secret:NAME}}"));
        assert!(updated.contains("# Later"));
        assert_eq!(updated.matches(BEGIN_MARKER).count(), 1);
    }
}
