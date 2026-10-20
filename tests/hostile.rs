//! Malicious and malformed inputs must be rejected quickly, never panic or blow up.
use std::time::{Duration, Instant};

const SAMPLE: &[u8] = include_bytes!("fixtures/sample.docx");

/// A minimal zip with one stored member; sizes can lie on purpose.
fn zip(name: &str, data: &[u8], claimed_size: u32, method: u16) -> Vec<u8> {
    let n = name.as_bytes();
    let mut z = Vec::new();
    z.extend_from_slice(&0x04034b50u32.to_le_bytes());
    z.extend_from_slice(&[20, 0, 0, 0]);
    z.extend_from_slice(&method.to_le_bytes());
    z.extend_from_slice(&[0; 8]); // time, date, crc
    z.extend_from_slice(&(data.len() as u32).to_le_bytes());
    z.extend_from_slice(&claimed_size.to_le_bytes());
    z.extend_from_slice(&(n.len() as u16).to_le_bytes());
    z.extend_from_slice(&[0, 0]);
    z.extend_from_slice(n);
    z.extend_from_slice(data);
    let cd = z.len() as u32;
    z.extend_from_slice(&0x02014b50u32.to_le_bytes());
    z.extend_from_slice(&[20, 0, 20, 0, 0, 0]);
    z.extend_from_slice(&method.to_le_bytes());
    z.extend_from_slice(&[0; 8]);
    z.extend_from_slice(&(data.len() as u32).to_le_bytes());
    z.extend_from_slice(&claimed_size.to_le_bytes());
    z.extend_from_slice(&(n.len() as u16).to_le_bytes());
    z.extend_from_slice(&[0; 12]); // extra, comment, disk, attrs
    z.extend_from_slice(&0u32.to_le_bytes()); // local header offset
    z.extend_from_slice(n);
    let cdlen = z.len() as u32 - cd;
    z.extend_from_slice(&0x06054b50u32.to_le_bytes());
    z.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
    z.extend_from_slice(&cdlen.to_le_bytes());
    z.extend_from_slice(&cd.to_le_bytes());
    z.extend_from_slice(&[0, 0]);
    z
}

fn quick<T>(f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let r = f();
    assert!(t.elapsed() < Duration::from_secs(5), "took {:?}", t.elapsed());
    r
}

#[test]
fn sample_renders() {
    let rtf = word_viewer::to_rtf(SAMPLE).expect("sample should render");
    assert!(rtf.contains("Test Document") && rtf.contains("caf\\u233?"));
}

#[test]
fn every_truncation_is_rejected_or_renders() {
    for n in 0..SAMPLE.len() {
        let _ = word_viewer::to_rtf(&SAMPLE[..n]);
    }
}

#[test]
fn every_single_byte_corruption_is_survivable() {
    for i in 0..SAMPLE.len() {
        let mut d = SAMPLE.to_vec();
        d[i] ^= 0xFF;
        let _ = word_viewer::to_rtf(&d);
    }
}

#[test]
fn zip_claiming_4gb_is_rejected() {
    let xml = b"<w:document><w:body/></w:document>";
    assert!(word_viewer::to_rtf(&zip("word/document.xml", xml, u32::MAX, 8)).is_none());
    assert!(word_viewer::to_rtf(&zip("word/document.xml", xml, u32::MAX, 0)).is_none());
}

#[test]
fn rtf_injection_is_escaped() {
    let xml = br"<w:p><w:r><w:t>}{\object\objemb{\*\objdata 0102}}{\field{\*\fldinst HYPERLINK x}}</w:t></w:r></w:p>";
    let rtf = word_viewer::to_rtf(&zip("word/document.xml", xml, xml.len() as u32, 0)).unwrap();
    assert!(!rtf.contains(r"{\object") && !rtf.contains(r"{\field"));
    assert!(rtf.contains(r"\}\{\\object"));
}

#[test]
fn entity_flood_is_fast() {
    let mut xml = b"<w:p><w:r><w:t>".to_vec();
    xml.extend(std::iter::repeat_n(b'&', 20 << 20));
    xml.extend_from_slice(b"</w:t></w:r></w:p>");
    quick(|| word_viewer::to_rtf(&zip("word/document.xml", &xml, xml.len() as u32, 0)));
}

#[test]
fn tag_flood_is_fast_and_output_is_capped() {
    let xml: Vec<u8> = b"<w:tc><w:b/>".repeat(5 << 20);
    let rtf = quick(|| word_viewer::to_rtf(&zip("word/document.xml", &xml, xml.len() as u32, 0))).unwrap();
    assert!(rtf.len() <= (32 << 20) + 1);
}

#[test]
fn oversized_input_is_rejected() {
    assert!(word_viewer::to_rtf(&vec![0u8; word_viewer::MAX_FILE + 1]).is_none());
}

/// A compound file whose sector chains point at themselves.
#[test]
fn cyclic_compound_file_is_rejected() {
    let mut z = vec![0u8; 512 * 3];
    z[..8].copy_from_slice(b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1");
    z[0x1E] = 9; // 512-byte sectors
    z[0x2C] = 1; // one FAT sector...
    z[0x4C..0x50].copy_from_slice(&0u32.to_le_bytes()); // ...which is sector 0
    z[0x30..0x34].copy_from_slice(&1u32.to_le_bytes()); // directory at sector 1
    z[0x3C..0x40].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    z[0x44..0x48].copy_from_slice(&0xFFFF_FFFEu32.to_le_bytes());
    // FAT: sector 0 -> 0, sector 1 -> 1 (cycles)
    z[512..516].copy_from_slice(&0u32.to_le_bytes());
    z[516..520].copy_from_slice(&1u32.to_le_bytes());
    assert!(quick(|| word_viewer::to_rtf(&z)).is_none());
}
