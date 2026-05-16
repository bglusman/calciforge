use super::validator_test_support::{MIN_VALID, parse};
use super::*;

#[test]
fn wardwright_backend_type_validates_from_shared_allowlist() {
    let fixture = format!(
        "{MIN_VALID}\n[proxy]\nenabled = true\nbind = \"127.0.0.1:18083\"\nbackend_type = \"wardwright\"\nbackend_url = \"https://gateway.example.invalid/v1\"\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);
    assert!(
        result.is_valid(),
        "wardwright should be accepted as an OpenAI-compatible provider adapter; errors: {:?}",
        result.errors
    );
}

#[test]
fn legacy_synthetic_selectors_warn_to_prefer_wardwright() {
    let fixture = format!(
        "{MIN_VALID}\n[[dispatchers]]\nid = \"balanced\"\nname = \"Balanced\"\n\n[[dispatchers.models]]\nmodel = \"local-small\"\ncontext_window = 32000\n"
    );
    let config = parse(&fixture);
    let result = validate_config(&config);

    assert!(
        result.is_valid(),
        "legacy synthetic selectors should remain valid while migration is optional: {:?}",
        result.errors
    );
    assert!(
        result.warnings.iter().any(|warning| {
            warning.contains("legacy compatibility") && warning.contains("Wardwright")
        }),
        "synthetic selector configs should point operators toward Wardwright; warnings: {:?}",
        result.warnings
    );
}
