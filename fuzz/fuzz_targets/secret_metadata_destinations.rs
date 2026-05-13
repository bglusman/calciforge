#![no_main]

use libfuzzer_sys::fuzz_target;
use secrets_client::metadata::parse_destinations;

fuzz_target!(|data: &[u8]| {
    if data.len() > 16 * 1024 {
        return;
    }

    let input = String::from_utf8_lossy(data);
    if let Ok(destinations) = parse_destinations(&input) {
        let mut seen = std::collections::HashSet::new();
        for destination in destinations {
            assert_eq!(destination, destination.to_ascii_lowercase());
            assert!(!destination.ends_with('/'));
            assert!(!destination.contains("://"));
            assert!(seen.insert(destination));
        }
    }
});
