# word-viewer

A tiny, read-only document viewer for Windows. One small exe, no installer, no dependencies.

- Opens Word (`.docx`, `.doc`), PDF, RTF, OpenDocument (`.odt`), EPUB, HTML, Markdown and plain text
- Word documents show headings, bold, italic, underline, bullets, simple tables and indents; other formats show their text as paragraphs
- PDFs: text is rebuilt into paragraphs (words hyphenated across lines joined, page numbers and running headers removed). Scanned PDFs have no text to show, and password-protected PDFs aren't supported yet
- Never modifies the file; text can be selected and copied

## Use

Download `word-viewer.exe` from [Releases](../../releases), then open a document with **File → Open** (Ctrl+O), drag one onto the window, or use **Open with** on a file.

Releases are built by GitHub Actions from the tagged source. To check that an exe is genuine:

```
gh attestation verify word-viewer.exe --repo paul-atx/word-viewer
```

## Security

Documents are untrusted input, so the viewer is built to be safe to open them with:

- **Memory-safe parsing.** All file parsing (zip, deflate, .docx XML, .doc compound files, PDF, RTF, ODT, EPUB, HTML, text encodings) is safe Rust with `unsafe` forbidden. Only the small Win32 window code uses `unsafe`, and it never touches file contents.
- **No code paths from documents.** Document text is always escaped before display, so a file cannot inject RTF objects, fields, or links. Macros, embedded objects, and external references are never processed.
- **Bounded work.** Input (64 MB; 1 GB for PDFs, which are mostly images), decompressed size, and output (32 MB) are capped; zip and PDF compression bombs, cyclic sector chains and page trees, runaway nesting, and overlapping text pieces are rejected.
- **No DLL planting.** The exe only imports DLLs that Windows always loads from System32 (the C runtime is linked in statically, and CI checks every build), and the RichEdit DLL is loaded from System32 explicitly.
- **Crash-safe.** Arithmetic overflow is checked in release builds, and any parser failure shows "Could not read this file" instead of crashing.
- **Tested against hostile input.** `tests/hostile.rs` runs malicious samples on every commit, and `fuzz/` contains a [cargo-fuzz](https://github.com/rust-fuzz/cargo-fuzz) target.

Found a problem? Please open an issue, or for anything sensitive use GitHub's private vulnerability reporting.

## Build

With Rust installed on Windows:

```
cargo build --release
```

The exe is at `target\release\word-viewer.exe`. The icon is embedded with `rc.exe` from the Windows SDK (or `windres`, or any rc-compatible compiler set in `RC`); without one, the build still works, just without the icon.

Run the tests with `cargo test`, and fuzz with `cargo +nightly fuzz run to_rtf`.

## License

MIT
