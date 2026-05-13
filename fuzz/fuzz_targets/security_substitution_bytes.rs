#![no_main]

use libfuzzer_sys::fuzz_target;
use security_proxy::substitution::{
    find_placeholder_tokens, find_refs, substitute, substitute_placeholders,
};
use std::collections::HashMap;

fuzz_target!(|data: &[u8]| {
    if data.len() > 16 * 1024 {
        return;
    }

    let input = String::from_utf8_lossy(data);
    let refs = find_refs(&input);
    let placeholders = find_placeholder_tokens(&input);

    if let Ok(names) = refs {
        let resolved = names
            .into_iter()
            .map(|name| (name, "resolved".to_string()))
            .collect::<HashMap<_, _>>();
        let _ = substitute(&input, &resolved);
    } else {
        let empty = HashMap::<String, String>::new();
        let _ = substitute(&input, &empty);
    }

    let placeholder_values = placeholders
        .into_iter()
        .map(|token| (token, "resolved".to_string()))
        .collect::<HashMap<_, _>>();
    let _ = substitute_placeholders(&input, &placeholder_values);
});
