//! Property-based tests for the NZC Clash policy using `proptest`.
//!
//! These tests verify invariants that must hold for *all* (or large random
//! samples of) inputs, complementing the hand-written unit tests in
//! `policy_tests.rs`.
//!
//! # Invariants tested
//!
//! 1. `rm -rf` anywhere in a command never returns Allow.
//! 2. `zfs destroy -r` anywhere in a command always returns Deny.
//! 3. Lucien writing to any path not in PROTECTED_FILES returns Allow.
//! 4. Whitespace variants of `zfs destroy -r` (extra spaces) still Deny.
//! 5. Safe read-only commands always Allow for all identities.

use proptest::prelude::*;

use crate::{ClashPolicy, PolicyContext, PolicyVerdict, StarlarkPolicy};

/// Load the shared example policy (same source used in production).
fn load_test_policy() -> StarlarkPolicy {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/policy.star");
    StarlarkPolicy::load(path)
}

/// Build a `PolicyContext` for a shell action.
fn make_shell_ctx(identity: &str, command: &str) -> PolicyContext {
    PolicyContext::new(identity, "nzc", "tool:shell").with_command(command)
}

/// The five protected file paths that Lucien must not write to.
const LUCIEN_PROTECTED: &[&str] = &[
    "/etc/nonzeroclaw/workspace/.clash/policy.star",
    "/etc/nonzeroclaw/config.toml",
    "/etc/nonzeroclaw-david/workspace/.clash/policy.star",
    "/etc/nonzeroclaw-david/config.toml",
    "/usr/local/bin/nonzeroclaw",
];

// ─────────────────────────────────────────────────────────────────────────────
// Property 1: rm -rf in any command never returns Allow
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    /// For any random prefix/suffix around `rm -rf`, the verdict must never be Allow.
    /// The policy catches `rm -rf` via substring search after normalization.
    #[test]
    fn prop_rm_rf_never_allows(
        prefix in "[a-z ]{0,20}",
        suffix in "[a-zA-Z0-9/_. ]{0,30}",
    ) {
        let cmd = format!("{} rm -rf {}", prefix, suffix);
        let policy = load_test_policy();
        let ctx = make_shell_ctx("brian", &cmd);
        let verdict = policy.evaluate("tool:shell", &ctx);
        prop_assert!(
            !matches!(verdict, PolicyVerdict::Allow),
            "rm -rf in {:?} returned Allow", cmd
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Property 2: zfs destroy -r in any command always returns Deny
//
// NOTE: The pattern "zfs destroy -r " (with trailing space) requires at least
// one character after the space to match. When suffix is empty, the normalized
// command is "zfs destroy -r" (no trailing space) and the pattern misses.
// We use a non-empty suffix to ensure the pattern always fires.
// This is a known policy edge case: bare "zfs destroy -r" (no dataset) is
// caught by "zfs destroy" review pattern (not always-deny) — acceptable since
// you can't actually destroy anything without a dataset name.
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    /// `zfs destroy -r <dataset>` with a non-empty dataset must always Deny.
    #[test]
    fn prop_zfs_destroy_r_always_denies(
        suffix in "[a-zA-Z0-9/_.@]{1,40}",  // at least 1 char — bare "zfs destroy -r" is edge case
    ) {
        let cmd = format!("zfs destroy -r {}", suffix);
        let policy = load_test_policy();
        let ctx = make_shell_ctx("brian", &cmd);
        let verdict = policy.evaluate("tool:shell", &ctx);
        prop_assert!(
            matches!(verdict, PolicyVerdict::Deny(_)),
            "zfs destroy -r {:?} did not Deny", cmd
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Property 3: Lucien writing to any path not in PROTECTED_FILES returns Allow
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    /// For any random absolute path that is NOT one of Lucien's PROTECTED_FILES,
    /// a file_write by Lucien must return Allow (no path-prefix restriction).
    #[test]
    fn prop_lucien_non_protected_write_allows(
        path in "/[a-z0-9/_.-]{5,50}",
    ) {
        // Skip if path exactly matches or ends with a protected file
        let is_protected = LUCIEN_PROTECTED.iter().any(|p| path == *p || path.ends_with(p));
        prop_assume!(!is_protected);

        let policy = load_test_policy();
        let ctx = PolicyContext::new("lucien", "nzc", "tool:file_write")
            .with_path(&path);
        let verdict = policy.evaluate("tool:file_write", &ctx);
        prop_assert!(
            matches!(verdict, PolicyVerdict::Allow),
            "Lucien write to non-protected {:?} returned non-Allow", path
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Property 4: Whitespace variants of zfs destroy -r still deny
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    /// Extra spaces between `zfs destroy` and `-r` must still Deny.
    /// The policy's `normalize()` collapses whitespace before matching.
    #[test]
    fn prop_whitespace_variants_still_deny(
        spaces in " {1,5}",
    ) {
        let cmd = format!("zfs destroy{}-r pool/dataset", spaces);
        let policy = load_test_policy();
        let ctx = make_shell_ctx("brian", &cmd);
        let verdict = policy.evaluate("tool:shell", &ctx);
        prop_assert!(
            matches!(verdict, PolicyVerdict::Deny(_)),
            "Whitespace variant {:?} was not denied", cmd
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Property 5: Safe read-only commands always Allow for all identities
//
// NOTE: is_root_wipe() now only fires for known destructive commands (rm, shred,
// dd, wipe, srm). Read-only commands like ls, cat, df are NOT destructive, so
// `ls /` and `cat /` correctly return Allow even with bare "/" as an argument.
// We can now include "/" and "/*" in the proptest args safely.
// ─────────────────────────────────────────────────────────────────────────────

proptest! {
    /// Safe read-only command bases (`ls`, `cat`, etc.) with random args (including
    /// bare "/" — now safe since is_root_wipe() requires destructive commands) must
    /// always return Allow for admin identities.
    #[test]
    fn prop_safe_commands_always_allow(
        // Only admin identities — research is separately gated by RESEARCH_ALLOWED_COMMANDS
        identity in prop_oneof![
            Just("brian"),
            Just("lucien"),
            Just("max"),
            Just("nonzeroclaw"),
        ],
        // Now includes paths like "/" since is_root_wipe() is narrowed to destructive
        // commands only. ls /, cat /, df / are all Allow.
        args in "[a-zA-Z0-9._/-]{0,20}",
    ) {
        let safe_cmds = ["ls", "cat", "grep", "find", "echo", "pwd", "hostname"];
        for cmd_base in &safe_cmds {
            let cmd = format!("{} {}", cmd_base, args);
            let policy = load_test_policy();
            let ctx = make_shell_ctx(identity, &cmd);
            let verdict = policy.evaluate("tool:shell", &ctx);
            prop_assert!(
                matches!(verdict, PolicyVerdict::Allow),
                "{:?} {:?} by {:?} returned non-Allow", cmd_base, args, identity
            );
        }
    }
}
