//! Any input, in any format, must either read or be rejected: no panic, hang, or blow-up.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = word_viewer::read(data, "input.txt");
    let _ = word_viewer::read(data, "input.md");
    let _ = word_viewer::to_rtf_named(data, "input.html");
});
