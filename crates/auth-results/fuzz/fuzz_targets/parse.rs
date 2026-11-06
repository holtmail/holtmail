#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    if let Some(ar) = holtmail_auth_results::parse_auth_results(&text) {
        for dkim in ar.dkim() {
            let _ = (dkim.signing_domain(), dkim.b_prefix());
        }
    }
    if let Some(sig) = holtmail_auth_results::parse_dkim_signature(&text) {
        let _ = (sig.h_counts(), sig.b_prefix());
    }
});
