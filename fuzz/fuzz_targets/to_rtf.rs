//! Any input must either render or be rejected: no panic, hang, or blow-up.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = word_viewer::to_rtf(data);
});
