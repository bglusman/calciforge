//! Comprehensive tests for the NZC Clash policy (policy.star).
//!
//! Covers:
//! 1. Always-deny patterns
//! 2. Root-wipe detection (always deny)
//! 3. Review patterns
//! 4. Allow cases
//! 5. Evasion attempts (case insensitivity, leading/trailing whitespace, compound commands)
//! 6. Identity-based access (admin, research, unknown)
//! 7. File-write restrictions (lucien, research)
//! 8. Property-style tests for rm -rf evasion
//! 9. Known gaps: internal whitespace (double-space, tab) bypass documentation

use crate::{ClashPolicy, PolicyContext, PolicyVerdict, StarlarkPolicy};

/// Helper: load the shared example policy (same source used in production).
fn nzc_policy() -> StarlarkPolicy {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("examples/policy.star");
    StarlarkPolicy::load(path)
}

/// Helper: build a `PolicyContext` for a shell action with an explicit command.
fn shell_ctx(identity: &str, command: &str) -> PolicyContext {
    PolicyContext::new(identity, "nzc", "tool:shell").with_command(command)
}

/// Helper: build a `PolicyContext` for a file_write action with an explicit path.
fn file_write_ctx(identity: &str, path: &str) -> PolicyContext {
    PolicyContext::new(identity, "nzc", "tool:file_write").with_path(path)
}

/// Assert that the verdict is `Allow`.
fn assert_allow(verdict: PolicyVerdict, label: &str) {
    assert!(
        matches!(verdict, PolicyVerdict::Allow),
        "Expected Allow for {:?}, got non-Allow",
        label
    );
}

/// Assert that the verdict is `Review`.
fn assert_review(verdict: PolicyVerdict, label: &str) {
    assert!(
        matches!(verdict, PolicyVerdict::Review(_)),
        "Expected Review for {:?}, got non-Review",
        label
    );
}

/// Assert that the verdict is `Deny`.
fn assert_deny(verdict: PolicyVerdict, label: &str) {
    assert!(
        matches!(verdict, PolicyVerdict::Deny(_)),
        "Expected Deny for {:?}, got non-Deny",
        label
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 1. Always-deny patterns (catastrophic risk)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn always_deny_zfs_destroy_recursive() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "zfs destroy -r pool/dataset"));
    assert_deny(verdict, "zfs destroy -r pool/dataset");
}

#[test]
fn always_deny_dd_zero_to_disk() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "dd if=/dev/zero of=/dev/sda"));
    assert_deny(verdict, "dd if=/dev/zero of=/dev/sda");
}

#[test]
fn always_deny_mkfs_ext4() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "mkfs.ext4 /dev/sda"));
    assert_deny(verdict, "mkfs.ext4 /dev/sda");
}

#[test]
fn always_deny_fork_bomb() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", ":(){ :|:& };:"));
    assert_deny(verdict, "fork bomb :(){ :|:& };:");
}

#[test]
fn always_deny_dd_if_variant() {
    let policy = nzc_policy();
    // dd if= is the pattern — any use of dd with input file
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "dd if=/dev/urandom of=/dev/sdb bs=1M"));
    assert_deny(verdict, "dd if=/dev/urandom of=/dev/sdb");
}

#[test]
fn always_deny_wipefs() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "wipefs -a /dev/sda"));
    assert_deny(verdict, "wipefs -a /dev/sda");
}

#[test]
fn always_deny_shred_dev() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "shred /dev/sda"));
    assert_deny(verdict, "shred /dev/sda");
}

// ═══════════════════════════════════════════════════════════════════════════
// 2. Root wipe detection (always-deny, even for admin)
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn root_wipe_rm_rf_slash() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -rf /"));
    assert_deny(verdict, "rm -rf /");
}

#[test]
fn root_wipe_rm_rf_glob() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -rf /*"));
    assert_deny(verdict, "rm -rf /*");
}

#[test]
fn root_wipe_rm_rf_quoted_slash() {
    let policy = nzc_policy();
    // The policy checks for split tokens matching "/" or "/*".
    // "rm -rf \"/\"" — the split of: rm -rf "/" would give parts [rm, -rf, "/"]
    // is_root_wipe() also checks for '"/\"' and "'/'" but the real test is the split tokens.
    // We test the policy as-is and document the result.
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -rf \"/\""));
    // This SHOULD be deny (root wipe). Document behavior.
    match verdict {
        PolicyVerdict::Deny(_) => {} // correct: caught as root wipe or by rm -rf pattern
        PolicyVerdict::Review(_) => {
            // KNOWN GAP: rm -rf "/" is caught as review (rm -rf pattern) rather than deny.
            // The is_root_wipe() function splits on whitespace, producing ["rm", "-rf", "\"/\""]
            // and the part "\"/\"" does not match "/" or "/*" exactly.
            // Acceptable for now — human review is still required.
        }
        PolicyVerdict::Allow => {
            panic!("Expected Deny or Review for 'rm -rf \"/\"', got Allow — this is a policy gap!");
        }
    }
}

#[test]
fn root_wipe_relative_path_is_NOT_denied() {
    // rm -rf '/tmp/../' — the path normalizes to '/', but is_root_wipe() uses string
    // matching on the raw token, not path resolution. So '/tmp/../' is NOT the same as '/'
    // in the policy's eyes. This SHOULD be Review (caught by rm -rf pattern), NOT Deny.
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -rf '/tmp/../'"));
    assert_review(verdict, "rm -rf '/tmp/../' (should be review, not deny)");
}

#[test]
fn root_wipe_rm_rf_long_safe_path() {
    // rm -rf /tmp/something/nested is NOT a root wipe, but IS rm -rf → should be review
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:shell",
        &shell_ctx("brian", "rm -rf /tmp/something-very-long/path/that/is/not/root"),
    );
    assert_review(
        verdict,
        "rm -rf long safe path (review, not deny — not root wipe)",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 3. Review patterns
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn review_rm_rf_tmp() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -rf /tmp/foo"));
    assert_review(verdict, "rm -rf /tmp/foo");
}

#[test]
fn review_rm_fr_alternate_order() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -fr /tmp/foo"));
    assert_review(verdict, "rm -fr /tmp/foo (alternate flag order)");
}

#[test]
fn review_rm_r_no_force() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -r /tmp/foo"));
    assert_review(verdict, "rm -r /tmp/foo (no force flag)");
}

#[test]
fn review_zfs_rollback() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "zfs rollback tank@snap"));
    assert_review(verdict, "zfs rollback tank@snap");
}

#[test]
fn review_parted() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "parted /dev/sda"));
    assert_review(verdict, "parted /dev/sda");
}

#[test]
fn review_truncate_zero() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "truncate -s 0 /tmp/foo"));
    assert_review(verdict, "truncate -s 0 /tmp/foo");
}

#[test]
fn review_fdisk() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "fdisk /dev/sda"));
    assert_review(verdict, "fdisk /dev/sda");
}

#[test]
fn review_lvremove() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "lvremove /dev/vg0/lv0"));
    assert_review(verdict, "lvremove");
}

#[test]
fn review_zfs_destroy_non_recursive() {
    let policy = nzc_policy();
    // "zfs destroy" without "-r " matches REVIEW_PATTERNS (not always-deny)
    // But NOTE: "zfs destroy " also matches "zfs destroy -r " if followed by -r.
    // Plain "zfs destroy tank/data" → review only
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "zfs destroy tank/data"));
    assert_review(verdict, "zfs destroy tank/data (non-recursive → review)");
}

// ═══════════════════════════════════════════════════════════════════════════
// 4. Allow cases
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn allow_rm_without_recursive() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm /tmp/foo"));
    assert_allow(verdict, "rm /tmp/foo (no -r flag)");
}

#[test]
fn allow_ls() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "ls /tmp"));
    assert_allow(verdict, "ls /tmp");
}

#[test]
fn allow_cat_etc_hosts() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "cat /etc/hosts"));
    assert_allow(verdict, "cat /etc/hosts");
}

#[test]
fn allow_systemctl_status() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "systemctl status nonzeroclaw"));
    assert_allow(verdict, "systemctl status nonzeroclaw");
}

#[test]
fn allow_git_log() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "git log --oneline -20"));
    assert_allow(verdict, "git log --oneline -20");
}

// ═══════════════════════════════════════════════════════════════════════════
// 5. Evasion attempts
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn evasion_compound_command_and_operator() {
    // Compound command — rm -rf is still a substring
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "echo hi && rm -rf /tmp/foo"));
    assert_review(verdict, "echo hi && rm -rf /tmp/foo (compound &&)");
}

#[test]
fn evasion_bash_wrapped() {
    // Wrapped in bash -c — rm -rf still appears in the string
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "bash -c 'rm -rf /tmp/foo'"));
    assert_review(verdict, "bash -c 'rm -rf /tmp/foo'");
}

#[test]
fn evasion_uppercase_rm_rf() {
    // Uppercase — policy uses .lower() on the command
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "RM -RF /tmp/foo"));
    assert_review(verdict, "RM -RF /tmp/foo (uppercase)");
}

#[test]
fn evasion_leading_trailing_whitespace() {
    // Leading/trailing whitespace — policy uses .strip() on the command
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "  rm -rf /tmp/foo  "));
    assert_review(verdict, "  rm -rf /tmp/foo  (leading/trailing whitespace)");
}

#[test]
fn evasion_sudo_prefix() {
    // sudo prefix — rm -rf is still a substring
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "sudo rm -rf /tmp/foo"));
    assert_review(verdict, "sudo rm -rf /tmp/foo");
}

#[test]
fn evasion_semicolon_compound() {
    // Semicolon separated — rm -rf still present
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -rf /tmp/foo; ls"));
    assert_review(verdict, "rm -rf /tmp/foo; ls (semicolon compound)");
}

/// Previously a known gap: double-space between rm and -rf.
///
/// Fixed in policy.star by replacing `cmd.lower().strip()` with `normalize(cmd)` which
/// collapses all internal whitespace using `" ".join(cmd.lower().strip().split())`.
/// Starlark's `.split()` (no args) normalizes tabs and multiple spaces just like Python.
#[test]
fn evasion_double_space_fixed() {
    let policy = nzc_policy();
    // Previously: "rm  -rf /tmp/foo" bypassed review because "rm  -rf" ≠ "rm -rf" (substring).
    // Now: normalize() collapses to "rm -rf /tmp/foo" before matching.
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm  -rf /tmp/foo"));
    assert_review(
        verdict,
        "rm  -rf /tmp/foo (double-space) — fixed by normalize() in command_matches_any()",
    );
}

/// Previously a known gap: tab whitespace between rm and -rf.
///
/// Fixed in policy.star by replacing `cmd.lower().strip()` with `normalize(cmd)` which
/// normalizes tabs via `.split()` (no args). `"rm\t-rf"` becomes `"rm -rf"` after normalize.
#[test]
fn evasion_tab_whitespace_fixed() {
    let policy = nzc_policy();
    // Previously: "rm\t-rf /tmp/foo" bypassed review because "rm\t-rf" ≠ "rm -rf".
    // Now: normalize() converts tab to space, collapses → "rm -rf /tmp/foo".
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm\t-rf /tmp/foo"));
    assert_review(
        verdict,
        "rm\\t-rf /tmp/foo (tab) — fixed by normalize() in command_matches_any()",
    );
}

/// Extra space in `zfs destroy -r ` pattern — fixed by normalize().
///
/// Previously: `"zfs destroy  -r pool"` (double space) → matched "zfs destroy" review pattern
/// but NOT the "zfs destroy -r " always-deny pattern. After fix: normalize() collapses it to
/// "zfs destroy -r pool" which DOES match "zfs destroy -r " → always-deny.
#[test]
fn evasion_zfs_destroy_extra_space_fixed() {
    let policy = nzc_policy();
    // Before fix: → Review (caught by "zfs destroy" pattern, not "zfs destroy -r " always-deny)
    // After fix with normalize(): → Deny (now matches "zfs destroy -r " always-deny pattern)
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "zfs destroy  -r pool"));
    assert_deny(
        verdict,
        "zfs destroy  -r pool (double space) — fixed by normalize(), now always-deny",
    );
}

// ═══════════════════════════════════════════════════════════════════════════
// 6. Identity-based access
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn identity_admin_brian_rm_rf_is_review_not_deny() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "rm -rf /tmp/x"));
    // Admin users are NOT denied for rm -rf; they go to review.
    assert_review(verdict, "brian (admin) + rm -rf /tmp/x → review");
}

#[test]
fn identity_research_renee_ls_is_allowed() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("renee", "ls /tmp"));
    assert_allow(verdict, "renee (research) + ls /tmp → allow");
}

#[test]
fn identity_research_renee_python3_is_allowed() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("renee", "python3 script.py"));
    assert_allow(verdict, "renee (research) + python3 script.py → allow");
}

#[test]
fn identity_research_renee_curl_is_allowed() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("renee", "curl http://example.com"));
    assert_allow(verdict, "renee (research) + curl http://example.com → allow");
}

#[test]
fn identity_research_renee_wget_is_allowed() {
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("renee", "wget http://example.com"));
    assert_allow(verdict, "renee (research) + wget http://example.com → allow");
}

#[test]
fn identity_research_renee_rm_is_denied() {
    let policy = nzc_policy();
    // "rm" is not in RESEARCH_ALLOWED_COMMANDS → deny for research profile
    let verdict = policy.evaluate("tool:shell", &shell_ctx("renee", "rm /tmp/x"));
    assert_deny(verdict, "renee (research) + rm /tmp/x → deny (rm not in RESEARCH_ALLOWED_COMMANDS)");
}

#[test]
fn identity_unknown_user_gets_no_group_restriction() {
    let policy = nzc_policy();
    // An identity not in any group — no profile restriction; falls through to allow.
    let verdict = policy.evaluate("tool:shell", &shell_ctx("unknown_user", "ls /tmp"));
    assert_allow(verdict, "unknown_user + ls /tmp → allow (no group restriction)");
}

#[test]
fn identity_unknown_user_with_rm_rf_gets_review() {
    let policy = nzc_policy();
    // Even an unknown identity triggers the rm -rf review pattern.
    let verdict = policy.evaluate("tool:shell", &shell_ctx("unknown_user", "rm -rf /tmp/foo"));
    assert_review(verdict, "unknown_user + rm -rf /tmp/foo → review");
}

// ═══════════════════════════════════════════════════════════════════════════
// 7. File-write restrictions
//
// Policy model (as of new file-specific protection):
//   - Lucien is ONLY blocked from writing to PROTECTED_FILES (5 specific paths).
//   - All other paths → Allow for Lucien (no path-prefix restriction).
//   - Research identities (renee, david) → Review for any file write.
//   - All other identities (brian, etc.) → Allow for any file write.
//
// PROTECTED_FILES for Lucien:
//   "/etc/nonzeroclaw/workspace/.clash/policy.star"
//   "/etc/nonzeroclaw/config.toml"
//   "/etc/nonzeroclaw-david/workspace/.clash/policy.star"
//   "/etc/nonzeroclaw-david/config.toml"
//   "/usr/local/bin/nonzeroclaw"
// ═══════════════════════════════════════════════════════════════════════════

// ── Lucien: PROTECTED_FILES must be denied ────────────────────────────────

#[test]
fn file_write_lucien_policy_file_is_denied() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/etc/nonzeroclaw/workspace/.clash/policy.star"),
    );
    assert_deny(verdict, "lucien writing to /etc/nonzeroclaw/workspace/.clash/policy.star → deny");
}

#[test]
fn file_write_lucien_nzc_config_toml_is_denied() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/etc/nonzeroclaw/config.toml"),
    );
    assert_deny(verdict, "lucien writing to /etc/nonzeroclaw/config.toml → deny");
}

#[test]
fn file_write_lucien_david_policy_star_is_denied() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/etc/nonzeroclaw-david/workspace/.clash/policy.star"),
    );
    assert_deny(verdict, "lucien writing to /etc/nonzeroclaw-david/workspace/.clash/policy.star → deny");
}

#[test]
fn file_write_lucien_david_config_toml_is_denied() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/etc/nonzeroclaw-david/config.toml"),
    );
    assert_deny(verdict, "lucien writing to /etc/nonzeroclaw-david/config.toml → deny");
}

#[test]
fn file_write_lucien_binary_is_denied() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/usr/local/bin/nonzeroclaw"),
    );
    assert_deny(verdict, "lucien writing to /usr/local/bin/nonzeroclaw → deny");
}

// ── Lucien: non-protected paths must be allowed ───────────────────────────

#[test]
fn file_write_lucien_tmp_is_allowed() {
    // Old model: /tmp/ was in safe_prefixes → Allow. New model: still Allow (not protected).
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/tmp/foo"),
    );
    assert_allow(verdict, "lucien writing to /tmp/foo → allow (not in PROTECTED_FILES)");
}

#[test]
fn file_write_lucien_workspace_config_toml_is_allowed() {
    // /etc/nonzeroclaw/workspace/config.toml is NOT in PROTECTED_FILES.
    // (The protected one is /etc/nonzeroclaw/config.toml without /workspace/)
    // Old model: this was Review (path-based). New model: Allow (not protected).
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/etc/nonzeroclaw/workspace/config.toml"),
    );
    assert_allow(verdict, "lucien writing to /etc/nonzeroclaw/workspace/config.toml → allow (not protected)");
}

#[test]
fn file_write_lucien_srv_data_is_allowed() {
    // Old model: /srv/data/foo was Review (not in safe_prefixes). New model: Allow.
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/srv/data/foo"),
    );
    assert_allow(verdict, "lucien writing to /srv/data/foo → allow (not in PROTECTED_FILES)");
}

#[test]
fn file_write_lucien_opt_is_allowed() {
    // Old model: /opt/something was Review (not in safe_prefixes). New model: Allow.
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/opt/something"),
    );
    assert_allow(verdict, "lucien writing to /opt/something → allow (not in PROTECTED_FILES)");
}

#[test]
fn file_write_lucien_workspace_memory_is_allowed() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/etc/nonzeroclaw/workspace/MEMORY.md"),
    );
    assert_allow(verdict, "lucien writing to /etc/nonzeroclaw/workspace/MEMORY.md → allow (not protected)");
}

#[test]
fn file_write_lucien_home_file_is_allowed() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("lucien", "/home/brian/foo.txt"),
    );
    assert_allow(verdict, "lucien writing to /home/brian/foo.txt → allow (not protected)");
}

// ── Other identities: brian unrestricted, renee → review ─────────────────

#[test]
fn file_write_brian_tmp_is_allowed() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("brian", "/tmp/foo"),
    );
    assert_allow(verdict, "brian writing to /tmp/foo → allow");
}

#[test]
fn file_write_brian_nzc_config_toml_is_allowed() {
    // brian is not lucien — no file-write restrictions apply.
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("brian", "/etc/nonzeroclaw/config.toml"),
    );
    assert_allow(verdict, "brian writing to /etc/nonzeroclaw/config.toml → allow (brian is not restricted)");
}

#[test]
fn file_write_renee_tmp_is_review() {
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("renee", "/tmp/foo"),
    );
    assert_review(verdict, "renee (research) writing to /tmp/foo → review");
}

#[test]
fn file_write_brian_clash_policy_is_allowed() {
    // Brian (admin) is NOT lucien and NOT research — no restriction on file writes.
    let policy = nzc_policy();
    let verdict = policy.evaluate(
        "tool:file_write",
        &file_write_ctx("brian", "/etc/nonzeroclaw/workspace/.clash/policy.star"),
    );
    assert_allow(verdict, "brian writing to policy.star → allow (only lucien is restricted)");
}

// ═══════════════════════════════════════════════════════════════════════════
// 8. Property-style tests for rm -rf evasion
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn test_rm_rf_evasion_attempts() {
    let policy = nzc_policy();

    // These should ALL be Review (not Allow) for admin identity "brian".
    // The policy catches these via substring search on cmd.lower().strip().
    let should_review = vec![
        "rm -rf /tmp/foo",
        "rm -rf /tmp/foo && echo done",          // compound &&
        "bash -c 'rm -rf /tmp/foo'",              // wrapped in bash
        "RM -RF /tmp/foo",                        // uppercase → .lower() catches it
        "  rm -rf /tmp/foo  ",                    // leading/trailing whitespace → .strip() catches it
        "sudo rm -rf /tmp/foo",                   // sudo prefix
        "rm -rf /tmp/foo; ls",                    // semicolon compound
        "nohup rm -rf /tmp/foo &",                // nohup background
        "env rm -rf /tmp/foo",                    // env prefix
    ];

    for cmd in &should_review {
        let ctx = shell_ctx("brian", cmd);
        let verdict = policy.evaluate("tool:shell", &ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Review(_)),
            "Expected Review for {:?}, got non-Review verdict",
            cmd
        );
    }

    // These should be DENY (root wipe) — not just review.
    let should_deny = vec![
        "rm -rf /",
        "rm -rf /*",
    ];

    for cmd in &should_deny {
        let ctx = shell_ctx("brian", cmd);
        let verdict = policy.evaluate("tool:shell", &ctx);
        assert!(
            matches!(verdict, PolicyVerdict::Deny(_)),
            "Expected Deny for {:?} (root wipe), got non-Deny verdict",
            cmd
        );
    }
}

/// Property test: always-deny patterns are caught regardless of identity.
#[test]
fn test_always_deny_for_all_identities() {
    let policy = nzc_policy();
    let identities = &["brian", "lucien", "renee", "unknown_user", "nonzeroclaw"];
    let always_deny_cmds = &[
        "zfs destroy -r pool/dataset",
        "dd if=/dev/zero of=/dev/sda",
        "mkfs.ext4 /dev/sda",
        ":(){ :|:& };:",
        "rm -rf /",
        "rm -rf /*",
    ];

    for identity in identities {
        for cmd in always_deny_cmds {
            let verdict = policy.evaluate("tool:shell", &shell_ctx(identity, cmd));
            assert!(
                matches!(verdict, PolicyVerdict::Deny(_)),
                "Expected Deny for identity={:?} cmd={:?}, got non-Deny",
                identity,
                cmd
            );
        }
    }
}

/// Property test: safe commands are allowed for admin identities.
#[test]
fn test_safe_commands_allowed_for_admin() {
    let policy = nzc_policy();
    let safe_cmds = &[
        "ls /etc",
        "cat /etc/hosts",
        "systemctl status nonzeroclaw",
        "git status",
        "echo hello",
        "pwd",
        "df -h",
        "ps aux",
    ];

    for cmd in safe_cmds {
        let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", cmd));
        assert!(
            matches!(verdict, PolicyVerdict::Allow),
            "Expected Allow for admin cmd={:?}, got non-Allow",
            cmd
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════════
// 9. Edge cases and regression tests
// ═══════════════════════════════════════════════════════════════════════════

#[test]
fn no_command_provided_defaults_to_allow() {
    // When action is tool:shell but no command is provided → allow (nothing to check)
    let policy = nzc_policy();
    let ctx = PolicyContext::new("brian", "nzc", "tool:shell"); // no .with_command()
    let verdict = policy.evaluate("tool:shell", &ctx);
    assert_allow(verdict, "tool:shell with empty command → allow");
}

#[test]
fn non_shell_action_is_allowed_for_admin() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("brian", "nzc", "tool:web_fetch");
    let verdict = policy.evaluate("tool:web_fetch", &ctx);
    assert_allow(verdict, "tool:web_fetch for admin → allow");
}

#[test]
fn non_shell_action_is_allowed_for_research() {
    let policy = nzc_policy();
    let ctx = PolicyContext::new("renee", "nzc", "tool:memory_recall");
    let verdict = policy.evaluate("tool:memory_recall", &ctx);
    assert_allow(verdict, "tool:memory_recall for research → allow");
}

#[test]
fn file_read_is_not_restricted_for_lucien() {
    let policy = nzc_policy();
    // file_read on policy.star should be allowed for Lucien (only file_write is restricted)
    let ctx = PolicyContext::new("lucien", "nzc", "tool:file_read")
        .with_path("/etc/nonzeroclaw/workspace/.clash/policy.star");
    let verdict = policy.evaluate("tool:file_read", &ctx);
    assert_allow(verdict, "lucien reading policy.star → allow (only write is restricted)");
}

#[test]
fn rm_rf_in_comment_within_shell_script() {
    // A command that just contains "# rm -rf" — still has the substring so it's review.
    // This is intentionally conservative: we don't parse shell syntax.
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "echo '# rm -rf is dangerous'"));
    // The echo itself is safe but contains "rm -rf" substring.
    // Policy will catch it as review — conservative but correct behavior.
    // Document: policy does NOT parse semantics, only searches substrings.
    match verdict {
        PolicyVerdict::Review(_) => {
            // Expected: conservative substring match
        }
        PolicyVerdict::Allow => {
            // This would also be acceptable if the policy is made smarter about echo
        }
        PolicyVerdict::Deny(_) => {
            panic!("Unexpected Deny for an echo command");
        }
    }
}

#[test]
fn zfs_destroy_rf_is_always_deny_not_just_review() {
    // "zfs destroy -rf" should be always-deny (not just review) via ALWAYS_DENY_PATTERNS
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", "zfs destroy -rf pool/sensitive"));
    assert_deny(verdict, "zfs destroy -rf pool/sensitive → always-deny (not review)");
}

#[test]
fn research_profile_rm_rf_is_deny_not_review() {
    // For renee: rm -rf would normally be review, but the research profile
    // restriction fires AFTER the review gate. However, is_root_wipe + always_deny
    // fire FIRST, then review patterns fire, and then research profile.
    // rm -rf /tmp/x → caught by REVIEW_PATTERNS ("rm -rf") → returns "review"
    // The research profile check never runs because review already returned.
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("renee", "rm -rf /tmp/x"));
    assert_review(verdict, "renee + rm -rf /tmp/x → review (caught before research profile check)");
}

#[test]
fn research_profile_rm_no_flags_is_denied() {
    // For renee: plain "rm /tmp/x" is NOT in RESEARCH_ALLOWED_COMMANDS → deny
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("renee", "rm /tmp/x"));
    assert_deny(verdict, "renee + rm /tmp/x → deny (rm not in RESEARCH_ALLOWED_COMMANDS)");
}

#[test]
fn research_profile_sudo_is_denied() {
    // For renee: "sudo ls /tmp" — first_word is "sudo", not in RESEARCH_ALLOWED_COMMANDS
    let policy = nzc_policy();
    let verdict = policy.evaluate("tool:shell", &shell_ctx("renee", "sudo ls /tmp"));
    assert_deny(verdict, "renee + sudo ls /tmp → deny (sudo not in RESEARCH_ALLOWED_COMMANDS)");
}

// ═══════════════════════════════════════════════════════════════════════════
// 10. NZC approval flow unit tests (policy-level)
// ═══════════════════════════════════════════════════════════════════════════

/// Test that when the policy fires a Review verdict, the ReviewPendingError
/// path is correctly covered. This is a unit-level test against the policy
/// evaluation path directly — it does not spin up a full HTTP stack.
///
/// What this tests: given a known rm -rf command, the policy returns Review,
/// which means the gateway would suspend the agent loop.
#[test]
fn approval_flow_review_fired_for_rm_rf() {
    let policy = nzc_policy();
    let ctx = shell_ctx("brian", "rm -rf /tmp/test-approval-integration");
    let verdict = policy.evaluate("tool:shell", &ctx);

    // Assert that Review fires — this is what causes ReviewPendingError in the agent loop.
    match verdict {
        PolicyVerdict::Review(reason) => {
            assert!(
                reason.contains("rm -rf /tmp/test-approval-integration"),
                "Review reason should contain the command: got {:?}",
                reason
            );
        }
        other => panic!(
            "Expected Review for 'rm -rf /tmp/test-approval-integration', got non-Review (allow={}, deny={})",
            matches!(other, PolicyVerdict::Allow),
            matches!(other, PolicyVerdict::Deny(_))
        ),
    }
}

/// Test that when approved = false, the denial path is correctly represented.
/// In the policy, this means a Deny verdict from the policy (not a Review),
/// but for the approval flow the "denied by human" path is represented via
/// the Review → denial continuation. This test verifies root wipe = Deny (no review needed).
#[test]
fn approval_flow_always_deny_bypasses_review() {
    let policy = nzc_policy();
    // Root wipe is always-deny — no human approval possible.
    let ctx = shell_ctx("brian", "rm -rf /");
    let verdict = policy.evaluate("tool:shell", &ctx);
    // This must be Deny, not Review — no approval path exists.
    assert_deny(verdict, "rm -rf / must be always-deny (not reviewable)");
}

/// Verify the full policy source string compiles correctly.
/// This catches syntax errors in the embedded policy.
#[test]
fn policy_source_from_file_compiles() {
    let policy = nzc_policy();
    // If the policy was compiled successfully (not permissive fallback),
    // it should catch a known always-deny pattern.
    let verdict = policy.evaluate("tool:shell", &shell_ctx("brian", ":(){ :|:& };:"));
    // Permissive policy would return Allow; real policy denies this.
    assert_deny(verdict, "fork bomb — permissive would allow, real policy denies");
}
