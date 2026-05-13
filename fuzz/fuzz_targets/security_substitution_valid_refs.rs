#![no_main]

use arbitrary::{Arbitrary, Unstructured};
use libfuzzer_sys::fuzz_target;
use security_proxy::substitution::{find_refs, substitute};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Arbitrary)]
struct ValidRefCase {
    prefix: Vec<u8>,
    refs: Vec<GeneratedRef>,
    suffix: Vec<u8>,
}

#[derive(Debug, Arbitrary)]
struct GeneratedRef {
    before: Vec<u8>,
    name: Vec<u8>,
    value: Vec<u8>,
}

fuzz_target!(|data: &[u8]| {
    let mut raw = Unstructured::new(data);
    let Ok(case) = ValidRefCase::arbitrary(&mut raw) else {
        return;
    };

    let mut input = safe_literal(&case.prefix);
    let mut expected = input.clone();
    let mut resolved = HashMap::new();
    let mut expected_names = HashSet::new();

    for generated in case.refs.into_iter().take(32) {
        let before = safe_literal(&generated.before);
        input.push_str(&before);
        expected.push_str(&before);

        let name = valid_secret_name(&generated.name);
        let value = safe_literal(&generated.value);
        input.push_str("{{secret:");
        input.push_str(&name);
        input.push_str("}}");
        expected.push_str(&value);
        expected_names.insert(name.clone());
        resolved.entry(name).or_insert(value);
    }

    let suffix = safe_literal(&case.suffix);
    input.push_str(&suffix);
    expected.push_str(&suffix);

    let names = find_refs(&input).expect("generated references must parse");
    assert_eq!(names, expected_names);
    let rendered = substitute(&input, &resolved).expect("generated references must resolve");
    assert_eq!(rendered.as_ref(), expected);
});

fn valid_secret_name(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
    let len = bytes.len().clamp(1, 64);
    (0..len)
        .map(|idx| {
            ALPHABET[bytes.get(idx).copied().unwrap_or(idx as u8) as usize % ALPHABET.len()] as char
        })
        .collect()
}

fn safe_literal(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .replace("{{secret:", "{secret:")
        .replace("}}", "}")
}
