//! Input source loading — transparent `.ifczip` decompression.
//!
//! IFC files ship in two on-disk forms:
//! - Plain STEP text (the `.ifc` extension, sometimes `.step`/`.stp`).
//! - ZIP-compressed STEP text (the `.ifczip` extension; a ZIP archive
//!   containing exactly one `.ifc` member).
//!
//! Pre-fix the parser unconditionally mmap'd whatever path it got
//! handed and fed the bytes to the STEP lexer. ZIP magic bytes
//! (`PK\x03\x04`) read as malformed STEP, the lexer found no `DATA;`
//! section, and the file silently yielded zero records — a textbook
//! reveal-all violation.
//!
//! This module dispatches on the first four bytes:
//!
//! - ZIP signature → read the file fully, decompress the single largest
//!   `.ifc` (or `.step`/`.stp`) member into an owned `Vec<u8>`, and
//!   return that. Decompressed bytes are necessarily in-RAM; mmap is
//!   not an option.
//! - Otherwise → mmap as before, zero-copy.
//!
//! Both variants converge on [`IfcSource::as_bytes`], so downstream
//! callers see a single `&[u8]` regardless of the on-disk form.

use std::fs::File;
use std::io::{self, Read};
use std::ops::Deref;
use std::path::Path;

use memmap2::Mmap;

/// Loaded IFC bytes — either a zero-copy mmap of a plain STEP file or
/// an owned in-memory buffer holding the decompressed contents of an
/// `.ifczip` archive.
pub enum IfcSource {
    /// Plain `.ifc` / `.step` — mmap'd, zero-copy.
    Mmap(Mmap),
    /// Decompressed `.ifczip` payload — owned buffer.
    Owned(Vec<u8>),
}

impl IfcSource {
    /// Borrowed view of the IFC byte stream. Identical contract for
    /// both variants — callers don't need to care which one they got.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            IfcSource::Mmap(m) => m,
            IfcSource::Owned(v) => v,
        }
    }

    /// Length of the IFC byte stream in bytes.
    pub fn len(&self) -> usize {
        self.as_bytes().len()
    }

    /// Convenience for callers that want to format empty-file errors.
    pub fn is_empty(&self) -> bool {
        self.as_bytes().is_empty()
    }
}

/// Deref coercion lets existing callers pass `&source` to any function
/// taking `&[u8]` and call slice methods like `.len()` directly,
/// matching the contract `Mmap` already offers. That keeps the
/// extension transparent at every callsite that previously took a
/// `Mmap` binding.
impl Deref for IfcSource {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}

impl AsRef<[u8]> for IfcSource {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}

/// First four bytes of a PKZIP local-file header.
const ZIP_MAGIC: [u8; 4] = [b'P', b'K', 0x03, 0x04];

/// Detect whether a buffer starts with the PKZIP local-file-header
/// signature. Used by [`open`] and exposed for callers that already
/// have a byte buffer in hand (testing, in-memory inputs).
pub fn looks_like_zip(buf: &[u8]) -> bool {
    buf.len() >= 4 && buf[..4] == ZIP_MAGIC
}

/// Open an IFC source by path, transparently decompressing `.ifczip`.
///
/// The dispatch reads the first four bytes from the file (or the
/// initial mmap view) to detect the ZIP magic — extension-based dispatch
/// is unreliable since pipelines rename files and `.ifczip` is just one
/// of several conventions.
pub fn open(path: &Path) -> io::Result<IfcSource> {
    let mut file = File::open(path)?;
    let mut peek = [0u8; 4];
    let n = file.read(&mut peek)?;
    if n >= 4 && peek == ZIP_MAGIC {
        // ZIP: read everything, decompress the largest .ifc member.
        // The Read above already consumed 4 bytes; reopen rather than
        // try to rewind (works for non-seekable inputs too if we ever
        // generalise from File).
        let mut all = Vec::with_capacity(file.metadata().map(|m| m.len() as usize).unwrap_or(0));
        let mut f = File::open(path)?;
        f.read_to_end(&mut all)?;
        let decompressed = decompress_ifczip(&all)?;
        Ok(IfcSource::Owned(decompressed))
    } else {
        // Plain IFC: mmap. SAFETY contract documented in callers.
        let mmap = unsafe { Mmap::map(&file)? };
        Ok(IfcSource::Mmap(mmap))
    }
}

/// Decompress an `.ifczip` payload (already in memory) and return the
/// raw STEP bytes from its largest `.ifc` / `.step` / `.stp` member.
///
/// Strategy: walk every entry, pick the largest one whose name ends in
/// a known STEP extension. That's robust to archives that also carry
/// thumbnails, change-history XML, or sidecar metadata files — we just
/// want the IFC.
pub fn decompress_ifczip(zip_bytes: &[u8]) -> io::Result<Vec<u8>> {
    use std::io::Cursor;
    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!(".ifczip: {e}")))?;

    // Find the largest STEP member by uncompressed size. Walking by
    // index avoids holding two mutable borrows on the archive.
    let mut best: Option<(usize, u64)> = None;
    for i in 0..archive.len() {
        let f = archive
            .by_index(i)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!(".ifczip entry {i}: {e}")))?;
        let name = f.name().to_ascii_lowercase();
        if name.ends_with(".ifc") || name.ends_with(".step") || name.ends_with(".stp") {
            let size = f.size();
            if best.map(|(_, s)| size > s).unwrap_or(true) {
                best = Some((i, size));
            }
        }
    }

    let (idx, _size) = best.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            ".ifczip: archive contains no .ifc / .step / .stp member",
        )
    })?;
    let mut entry = archive
        .by_index(idx)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!(".ifczip member: {e}")))?;
    let mut buf = Vec::with_capacity(entry.size() as usize);
    entry.read_to_end(&mut buf)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a tiny in-memory ZIP archive containing one .ifc member.
    fn make_zip(name: &str, contents: &[u8]) -> Vec<u8> {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            let opts: zip::write::SimpleFileOptions =
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated);
            zw.start_file(name, opts).unwrap();
            zw.write_all(contents).unwrap();
            zw.finish().unwrap();
        }
        buf
    }

    #[test]
    fn zip_magic_detected() {
        let pk = [b'P', b'K', 0x03, 0x04, 0x00];
        let not = b"ISO-10303-21;";
        assert!(looks_like_zip(&pk));
        assert!(!looks_like_zip(not));
        assert!(!looks_like_zip(b""));
        assert!(!looks_like_zip(b"PK"));
    }

    #[test]
    fn decompress_recovers_payload() {
        let payload = b"ISO-10303-21;\nHEADER;\n";
        let archive = make_zip("model.ifc", payload);
        let got = decompress_ifczip(&archive).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn decompress_picks_largest_step_member() {
        // Two members; we should pick the larger one. The smaller is
        // a token sidecar; the larger is the real IFC.
        let small = b"sidecar";
        let big = b"ISO-10303-21;\nHEADER;\nFILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";

        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            let opts: zip::write::SimpleFileOptions =
                zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored);
            zw.start_file("notes.ifc", opts).unwrap();
            zw.write_all(small).unwrap();
            zw.start_file("model.ifc", opts).unwrap();
            zw.write_all(big).unwrap();
            zw.finish().unwrap();
        }

        let got = decompress_ifczip(&buf).unwrap();
        assert_eq!(got, big);
    }

    #[test]
    fn decompress_errors_when_no_ifc_member() {
        // Archive holds only a non-STEP file; must surface as an
        // explicit InvalidData error — not return an empty buffer
        // (which would re-introduce the silent-drop bug).
        let archive = make_zip("README.txt", b"this is not an IFC file");
        let err = decompress_ifczip(&archive).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn open_dispatches_zip_via_magic_bytes() {
        let payload = b"ISO-10303-21;\nHEADER;\nENDSEC;\nEND-ISO-10303-21;\n";
        let archive = make_zip("model.ifc", payload);

        let tmp = std::env::temp_dir().join(format!(
            "ifcfast-source-test-{}.ifczip",
            std::process::id()
        ));
        std::fs::write(&tmp, &archive).unwrap();

        let src = open(&tmp).expect("zip open");
        assert!(matches!(src, IfcSource::Owned(_)));
        assert_eq!(src.as_bytes(), payload);

        // Plain IFC path also works → Mmap variant.
        let plain = std::env::temp_dir().join(format!(
            "ifcfast-source-test-{}.ifc",
            std::process::id()
        ));
        std::fs::write(&plain, payload).unwrap();
        let src2 = open(&plain).expect("plain open");
        assert!(matches!(src2, IfcSource::Mmap(_)));
        assert_eq!(src2.as_bytes(), payload);

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&plain);
    }
}
