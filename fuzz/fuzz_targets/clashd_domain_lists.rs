#![no_main]

use clashd::domain_lists::DomainList;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() > 16 * 1024 {
        return;
    }

    let input = String::from_utf8_lossy(data);
    let mut list = DomainList::new("fuzz");
    if list.parse(&input).is_ok() {
        for probe in ["example.com", "sub.example.com", "localhost", "127.0.0.1"] {
            let _ = list.matches(probe);
        }
    }
});
