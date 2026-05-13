// Behavioral tests for `validate_config`. Round-2 test quality
// audit (2026-04-24) flagged this module as having zero tests
// despite ~290 lines of validation logic. These tests close the
// most important invariants (duplicates, dangling references,
// out-of-range fields) so future refactors can't silently regress.
use crate::config::CalciforgeConfig;

/// Minimal TOML that passes validation. Each negative test
/// derives from this by prepending/appending ONE targeted
/// deviation so the failing invariant is the only difference
/// between the valid fixture and the test under test.
pub(super) const MIN_VALID: &str = r#"
[calciforge]
version = 2

[context]
buffer_size = 20
inject_depth = 5

[[identities]]
id = "alice"
aliases = [{ channel = "telegram", id = "7000000001" }]
role = "owner"

[[agents]]
id = "bot"
kind = "cli"
command = "/bin/echo"
args = []

[[channels]]
kind = "telegram"
bot_token_file = "/tmp/nope"
"#;

pub(super) fn parse(toml: &str) -> CalciforgeConfig {
    toml::from_str(toml).expect("fixture should parse")
}
