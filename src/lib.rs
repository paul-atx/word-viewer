//! Reads documents for display (as RTF) or reading aloud (as paragraphs):
//! Word (.docx, .doc), PDF, RTF, OpenDocument (.odt), EPUB, HTML, Markdown and
//! plain text.
//!
//! Every input is treated as hostile. This crate is 100% safe Rust (unsafe is
//! forbidden), all sizes read from a file are checked before use, work and
//! memory are capped, and document text is always escaped so a file can never
//! inject RTF control words (objects, fields, links) into the viewer.
#![forbid(unsafe_code)]

mod doc;
mod docx;
mod epub;
mod inflate;
pub use inflate::inflate;
mod markup;
mod odt;
mod pdf;
pub use pdf::PdfError;
mod plain;
mod rtf;
mod rtfread;
mod text;
mod zip;

/// Largest input file accepted (except PDFs, see MAX_PDF).
pub const MAX_FILE: usize = 64 << 20;
/// Largest PDF accepted: illustrated books run to hundreds of MB, nearly all of
/// it images, which are never decoded.
pub const MAX_PDF: usize = 1 << 30;
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
    to_paragraphs_named(data, "")
}

/// What kind of document `data` is, judged by its content; the file name
/// (`name`) only breaks ties (Markdown vs plain text, HTML without a doctype).
#[derive(Debug, PartialEq, Clone, Copy)]
enum Kind {
    Word,
    Pdf,
    Rtf,
    Odt,
    Epub,
    Html,
    Markdown,
    Text,
}

fn kind(data: &[u8], name: &str) -> Option<Kind> {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    if data.starts_with(CFB_MAGIC) {
        return Some(Kind::Word);
    }
    if data[..data.len().min(1024)].windows(5).any(|w| w == b"%PDF-") {
        return Some(Kind::Pdf);
    }
    if data.starts_with(b"PK\x03\x04") {
        if zip::extract(data, "word/document.xml", MAX_XML).is_some() {
            return Some(Kind::Word);
        }
        let mimetype = zip::extract(data, "mimetype", 256).unwrap_or_default();
        return match mimetype.trim_ascii() {
            b"application/epub+zip" => Some(Kind::Epub),
            b"application/vnd.oasis.opendocument.text" => Some(Kind::Odt),
            _ if zip::extract(data, "META-INF/container.xml", 1 << 20).is_some() => Some(Kind::Epub),
            _ => None,
        };
    }
    let head = String::from_utf8_lossy(&data[..data.len().min(512)]).trim_start_matches('\u{FEFF}').trim_start().to_ascii_lowercase();
    if head.starts_with("{\\rtf") {
        return Some(Kind::Rtf);
    }
    if matches!(ext.as_str(), "html" | "htm" | "xhtml") || head.starts_with("<!doctype html") || head.starts_with("<html") {
        return Some(Kind::Html);
    }
    if matches!(ext.as_str(), "md" | "markdown") {
        return Some(Kind::Markdown);
    }
    Some(Kind::Text)
}

/// Mostly control characters: a binary file, not text.
fn looks_binary(text: &str) -> bool {
    let sample: Vec<char> = text.chars().take(8192).collect();
    let bad = sample.iter().filter(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t' | '\u{C}')).count();
    bad * 20 > sample.len().max(1)
}

/// Why a document couldn't be read.
#[derive(Debug, PartialEq)]
pub enum ReadError {
    /// Not a format we can read, or damaged.
    Unsupported,
    /// A password-protected PDF.
    Encrypted,
    /// Readable, but with no text (for a PDF, most likely scanned pages).
    NoText,
}

impl ReadError {
    /// A sentence for the user (plain ASCII, safe to put in RTF).
    pub fn message(&self) -> &'static str {
        match self {
            ReadError::Unsupported => "This file could not be read. Supported formats: Word (.docx, .doc), PDF, RTF, OpenDocument (.odt), EPUB, HTML, Markdown and plain text.",
            ReadError::Encrypted => "This PDF is password-protected, which isn't supported yet.",
            ReadError::NoText => "This document has no text to read. If it is a scanned PDF, its pages are pictures of text.",
        }
    }
}

/// Read any supported document as plain-text paragraphs, or None if it can't be
/// read. `name` is the file name, used only to tell Markdown from plain text.
pub fn to_paragraphs_named(data: &[u8], name: &str) -> Option<Vec<String>> {
    read(data, name).ok()
}

/// Read any supported document as plain-text paragraphs, saying why on failure.
/// `name` is the file name, used only to tell Markdown from plain text.
pub fn read(data: &[u8], name: &str) -> Result<Vec<String>, ReadError> {
    read_book(data, name).map(|b| b.paras)
}

/// A document's paragraphs and what its markup says about their structure.
#[derive(Debug, Default)]
pub struct Book {
    pub paras: Vec<String>,
    /// Paragraphs that are major headings (HTML and EPUB h1 to h3).
    pub headings: Vec<usize>,
    /// Where each of an EPUB's files starts; each is usually a chapter.
    pub parts: Vec<usize>,
    /// The story itself, without an EPUB's front and back matter (cover, title
    /// page, contents; notes, index, ads); all the paragraphs for other formats.
    pub body: std::ops::Range<usize>,
}

/// Like `read`, keeping the headings and parts the document's markup marks.
pub fn read_book(data: &[u8], name: &str) -> Result<Book, ReadError> {
    let kind = kind(data, name).ok_or(ReadError::Unsupported)?;
    if data.len() > if kind == Kind::Pdf { MAX_PDF } else { MAX_FILE } {
        return Err(ReadError::Unsupported);
    }
    let mut book = Book::default();
    let paras = match kind {
        Kind::Word => text::paragraphs(&to_rtf(data).ok_or(ReadError::Unsupported)?),
        Kind::Pdf => pdf::paragraphs(data).map_err(|e| match e {
            PdfError::Encrypted => ReadError::Encrypted,
            PdfError::NoText => ReadError::NoText,
            PdfError::Unreadable => ReadError::Unsupported,
        })?,
        Kind::Rtf => rtfread::paragraphs(data),
        Kind::Odt => {
            let xml = zip::extract(data, "content.xml", MAX_XML).ok_or(ReadError::Unsupported)?;
            odt::paragraphs(&String::from_utf8_lossy(&xml))
        }
        Kind::Epub => {
            let (blocks, parts, body) = epub::blocks(data).ok_or(ReadError::Unsupported)?;
            book.parts = parts;
            book.body = body;
            outline(&mut book, blocks)
        }
        Kind::Html | Kind::Markdown | Kind::Text => {
            let text = plain::decode(data);
            if looks_binary(&text) {
                return Err(ReadError::Unsupported);
            }
            match kind {
                Kind::Html => outline(&mut book, markup::html_blocks(&text)),
                Kind::Markdown => plain::markdown_paragraphs(&text),
                _ => plain::paragraphs(&text),
            }
        }
    };
    if paras.is_empty() {
        return Err(ReadError::NoText);
    }
    if kind != Kind::Epub {
        book.body = 0..paras.len();
    }
    book.paras = paras;
    Ok(book)
}

/// Note which blocks are headings, and return the paragraphs.
fn outline(book: &mut Book, blocks: Vec<markup::Block>) -> Vec<String> {
    book.headings = blocks.iter().enumerate().filter(|(_, (_, h))| *h).map(|(i, _)| i).collect();
    blocks.into_iter().map(|(p, _)| p).collect()
}

/// Render any supported document to RTF for display: Word documents keep their
/// formatting, other formats show as plain paragraphs.
pub fn to_rtf_named(data: &[u8], name: &str) -> Result<String, ReadError> {
    if kind(data, name) == Some(Kind::Word) {
        return to_rtf(data).ok_or(ReadError::Unsupported);
    }
    let paras = read(data, name)?;
    let mut r = rtf::Rtf::new();
    for p in &paras {
        r.put("\\pard\\sa160 ");
        for c in p.chars() {
            r.ch(c as u32);
        }
        r.put("\\par\n");
        if r.full {
            break;
        }
    }
    Ok(r.finish())
}
