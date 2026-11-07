#![no_main]

use holtmail_links::{display_host, registrable_domain, strip_tracking, tracker, unwrap};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let unwrapped = unwrap(&input);
    match unwrapped.via {
        None => assert_eq!(unwrapped.destination, input),
        Some(_) => {
            let parsed = url::Url::parse(&unwrapped.destination).expect("destination parses");
            assert!(matches!(parsed.scheme(), "http" | "https"));
            assert!(parsed.username().is_empty() && parsed.password().is_none());
        }
    }
    let _ = strip_tracking(&input);
    let _ = strip_tracking(&unwrapped.destination);
    let _ = tracker(&input);
    let _ = tracker(&unwrapped.destination);
    if let Some(domain) = registrable_domain(&input) {
        assert_eq!(registrable_domain(&domain).as_deref(), Some(domain.as_str()));
    }
    let _ = display_host(&input);
});
