//! Converts Word documents (.docx and legacy .doc) to RTF for display.
//!
//! Every input is treated as hostile. This crate is 100% safe Rust (unsafe is
//! forbidden), all sizes read from a file are checked before use, work and
//! memory are capped, and document text is always escaped so a file can never
//! inject RTF control words (objects, fields, links) into the viewer.
#![forbid(unsafe_code)]

mod doc;
mod docx;
mod inflate;
pub use inflate::inflate;
mod rtf;
mod text;
mod zip;

/// Largest input file accepted.
pub const MAX_FILE: usize = 64 << 20;
/// Largest uncompressed word/document.xml accepted.
const MAX_XML: usize = 64 << 20;

const CFB_MAGIC: &[u8] = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1";

/// Render a .docx or .doc file to an RTF string, or None if it can't be read.
pub fn to_rtf(data: &[u8]) -> Option<String> {
    if data.len() > MAX_FILE {
        return None;
    }
    if data.starts_with(CFB_MAGIC) {
        doc::render(data)
    } else {
        let xml = zip::extract(data, "word/document.xml", MAX_XML)?;
        Some(docx::render(&xml))
    }
}

/// Read a .docx or .doc file as plain-text paragraphs, or None if it can't be read.
pub fn to_paragraphs(data: &[u8]) -> Option<Vec<String>> {
    to_rtf(data).map(|rtf| text::paragraphs(&rtf))
}
