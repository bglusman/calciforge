//! Starlark-backed clash policy engine.
//!
//! Loads a `.star` file and evaluates the `evaluate(action, identity, agent)`
//! function on every policy check.
//!
//! # Script contract
//!
//! The policy file must define a top-level function:
//!
//! ```python
//! def evaluate(action, identity, agent, command="", path=""):
//!     # Return "allow", "deny:<reason>", or "review:<reason>"
//!     return "allow"
//! ```
//!
//! # Thread safety
//!
//! The compiled Starlark module is frozen after loading, making its values
//! immutable and `Send + Sync`. Each `evaluate()` call creates a fresh
//! `Module` + `Evaluator` pair (cheap stack allocations), so concurrent
//! evaluation is safe without any locking.

use std::path::PathBuf;
use std::sync::Arc;

use starlark::environment::{FrozenModule, Globals, Module};
use starlark::eval::Evaluator;
use starlark::syntax::{AstModule, Dialect};
use starlark::values::OwnedFrozenValue;

use crate::{ClashPolicy, PolicyContext, PolicyVerdict};

/// Starlark-backed policy engine.
///
/// Constructed via [`StarlarkPolicy::load`] or [`StarlarkPolicy::from_source`].
/// Falls back to permissive (allow all) when the policy file is missing or
/// fails to compile.
pub struct StarlarkPolicy {
    inner: Inner,
}

enum Inner {
    /// Compiled Starlark module + the `evaluate` function extracted from it.
    Loaded {
        /// The frozen module keeps the heap alive for `evaluate_fn`.
        _module: Arc<FrozenModule>,
        /// The `evaluate` Starlark function.
        evaluate_fn: OwnedFrozenValue,
    },
    /// Permissive fallback — used when no policy file is found or loading fails.
    Permissive,
}

impl StarlarkPolicy {
    /// Load a policy from a file path.
    ///
    /// If the file does not exist, a warning is logged and the policy falls
    /// back to permissive mode (allow all). If the file exists but fails to
    /// parse or compile, an error is logged and the policy also falls back.
    pub fn load(path: PathBuf) -> Self {
        if !path.exists() {
            tracing::warn!(
                path = %path.display(),
                "clash: policy file not found — falling back to permissive mode"
            );
            return Self { inner: Inner::Permissive };
        }

        let source = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(
                    path = %path.display(),
                    error = %e,
                    "clash: failed to read policy file — falling back to permissive mode"
                );
                return Self { inner: Inner::Permissive };
            }
        };

        let filename = path.to_string_lossy().into_owned();
        Self::from_source(&filename, &source)
    }

    /// Build a policy from a Starlark source string.
    ///
    /// Useful for testing and for embedding policy snippets directly.
    /// Falls back to permissive if the source fails to parse or compile.
    pub fn from_source(filename: &str, source: &str) -> Self {
        match Self::compile(filename, source) {
            Ok(inner) => Self { inner },
            Err(e) => {
                tracing::error!(
                    filename = %filename,
                    error = %e,
                    "clash: policy compilation failed — falling back to permissive mode"
                );
                Self { inner: Inner::Permissive }
            }
        }
    }

    fn compile(filename: &str, source: &str) -> anyhow::Result<Inner> {
        let ast =
            AstModule::parse(filename, source.to_string(), &Dialect::Standard).map_err(|e| {
                anyhow::anyhow!("Starlark parse error in {filename}: {e}")
            })?;

        let globals = Globals::standard();
        let module = Module::new();

        {
            let mut eval = Evaluator::new(&module);
            eval.eval_module(ast, &globals)
                .map_err(|e| anyhow::anyhow!("Starlark eval error in {filename}: {e}"))?;
        }

        let frozen = module
            .freeze()
            .map_err(|e| anyhow::anyhow!("Starlark freeze error in {filename}: {e:?}"))?;

        let evaluate_fn = frozen
            .get("evaluate")
            .map_err(|e| anyhow::anyhow!("clash: could not find `evaluate` in {filename}: {e}"))?;

        let arc_module = Arc::new(frozen);

        Ok(Inner::Loaded {
            _module: arc_module,
            evaluate_fn,
        })
    }

    /// Call the Starlark `evaluate(action, identity, agent, command="", path="")` function and
    /// parse the return string into a [`PolicyVerdict`].
    fn call_evaluate(&self, evaluate_fn: &OwnedFrozenValue, action: &str, context: &PolicyContext) -> PolicyVerdict {
        let env = Module::new();
        let mut eval = Evaluator::new(&env);

        let command = context.extra.get("command").map(|s| s.as_str()).unwrap_or("");
        let path = context.extra.get("path").map(|s| s.as_str()).unwrap_or("");

        let heap = eval.heap();
        let arg_action = heap.alloc(action);
        let arg_identity = heap.alloc(context.identity.as_str());
        let arg_agent = heap.alloc(context.agent.as_str());
        let arg_command = heap.alloc(command);
        let arg_path = heap.alloc(path);

        let result = eval.eval_function(
            evaluate_fn.value(),
            &[arg_action, arg_identity, arg_agent, arg_command, arg_path],
            &[],
        );

        match result {
            Ok(val) => Self::parse_verdict(&val.to_str()),
            Err(e) => {
                tracing::error!(
                    action = %action,
                    identity = %context.identity,
                    error = %e,
                    "clash: Starlark evaluation error — failing open (Allow)"
                );
                PolicyVerdict::Allow
            }
        }
    }

    /// Parse a verdict string into a [`PolicyVerdict`].
    ///
    /// Accepted formats:
    /// - `"allow"` → `Allow`
    /// - `"deny:<reason>"` → `Deny(reason)`
    /// - `"review:<reason>"` → `Review(reason)`
    ///
    /// Any unrecognised return value defaults to `Allow` with a warning.
    fn parse_verdict(s: &str) -> PolicyVerdict {
        // Strip surrounding quotes that Starlark's to_str() may include.
        let s = s.trim_matches('"');

        if s == "allow" {
            return PolicyVerdict::Allow;
        }
        if let Some(reason) = s.strip_prefix("deny:") {
            return PolicyVerdict::Deny(reason.to_string());
        }
        if let Some(reason) = s.strip_prefix("review:") {
            return PolicyVerdict::Review(reason.to_string());
        }

        tracing::warn!(
            verdict = %s,
            "clash: unrecognised verdict from policy script — defaulting to Allow"
        );
        PolicyVerdict::Allow
    }
}

impl ClashPolicy for StarlarkPolicy {
    fn evaluate(&self, action: &str, context: &PolicyContext) -> PolicyVerdict {
        match &self.inner {
            Inner::Permissive => PolicyVerdict::Allow,
            Inner::Loaded { evaluate_fn, .. } => {
                self.call_evaluate(evaluate_fn, action, context)
            }
        }
    }
}

// SAFETY: `OwnedFrozenValue` is `Send + Sync` (frozen heap is immutable).
// `FrozenModule` is also `Send + Sync`. Both are documented as thread-safe.
unsafe impl Send for StarlarkPolicy {}
unsafe impl Sync for StarlarkPolicy {}
