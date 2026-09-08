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
//!   not an option — which is exactly why the inflate is bounded. See
//!   [`DecompressLimits`] (GH #175): a ZIP's declared sizes are
//!   attacker-controlled, and a "zip bomb" that a compressed-size check
//!   upstream cannot see would otherwise take the process (or, since
//!   GH #172 put this code in the browser, the visitor's tab) down.
//! - Otherwise → mmap as before, zero-copy.
//!
//! Both variants converge on [`IfcSource::as_bytes`], so downstream
//! callers see a single `&[u8]` regardless of the on-disk form.

#[cfg(feature = "mmap")]
use std::fs::File;
use std::io::{self, Read};
use std::ops::Deref;
#[cfg(feature = "mmap")]
use std::path::Path;

#[cfg(feature = "mmap")]
use memmap2::Mmap;

/// Loaded IFC bytes — either a zero-copy mmap of a plain STEP file or
/// an owned in-memory buffer holding the decompressed contents of an
/// `.ifczip` archive.
///
/// The `Mmap` variant lives behind the default-on `mmap` Cargo feature
/// (GH #172): `memmap2` has no `wasm32-unknown-unknown` implementation,
/// and the browser build only ever has bytes in hand anyway — it loads
/// through [`open_bytes`] into `Owned`.
pub enum IfcSource {
    /// Plain `.ifc` / `.step` — mmap'd, zero-copy.
    #[cfg(feature = "mmap")]
    Mmap(Mmap),
    /// Decompressed `.ifczip` payload — owned buffer.
    Owned(Vec<u8>),
}

impl IfcSource {
    /// Borrowed view of the IFC byte stream. Identical contract for
    /// both variants — callers don't need to care which one they got.
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            #[cfg(feature = "mmap")]
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

/// STEP exchange-structure trailer. Every conformant ISO-10303-21
/// writer terminates the file with `END-ISO-10303-21;`. We probe for
/// the keyword without the trailing `;` so a missing or stray-whitespace
/// terminator still matches a legitimately-complete file.
const STEP_TRAILER: &[u8] = b"END-ISO-10303-21";

/// How far back from EOF to look for [`STEP_TRAILER`]. A conformant
/// trailer sits in the final ~20 bytes; 256 absorbs trailing whitespace,
/// CRLF, and the occasional vendor comment after the trailer.
const TRAILER_PROBE_BYTES: usize = 256;

/// Refuse a truncated / unterminated plain-STEP buffer loudly.
///
/// The record scanner consumes entities until EOF, so a file cut
/// mid-stream parses cleanly to a *partial* model — silently wrong QTO
/// sums, diffs, and clash runs at exit 0. The absence of the
/// `END-ISO-10303-21;` trailer in the file tail is the deterministic
/// signal of an interrupted download / copy, so we reject it here at the
/// single open choke-point. Because every `_core.*` entry point (bundle,
/// mesh, clash, psets, …) loads through [`open`], they all inherit the
/// refusal by construction rather than relying on a Python-side wrapper.
///
/// ZIP (`.ifczip`) inputs never reach this check: a truncated archive
/// fails its own central-directory validation in [`decompress_ifczip`]
/// before any bytes are returned, and the decompressed member is whole
/// or the inflate errored. Empty buffers are left to downstream
/// empty-file handling — they are not "truncated STEP".
fn check_step_trailer(buf: &[u8]) -> io::Result<()> {
    if buf.is_empty() {
        return Ok(());
    }
    let probe_start = buf.len().saturating_sub(TRAILER_PROBE_BYTES);
    let tail = &buf[probe_start..];
    if tail.windows(STEP_TRAILER.len()).any(|w| w == STEP_TRAILER) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "IFC file is truncated or unterminated (no END-ISO-10303-21 \
                 trailer in the last {} bytes)",
                tail.len()
            ),
        ))
    }
}

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
#[cfg(feature = "mmap")]
pub fn open(path: &Path) -> io::Result<IfcSource> {
    open_with(path, &DecompressLimits::default())
}

/// [`open`] with caller-chosen `.ifczip` decompression bounds.
///
/// Only the ZIP branch consults `limits`; a plain STEP file is mmap'd
/// exactly as [`open`] does it.
#[cfg(feature = "mmap")]
pub fn open_with(path: &Path, limits: &DecompressLimits) -> io::Result<IfcSource> {
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
        let decompressed = decompress_ifczip_with(&all, limits)?;
        Ok(IfcSource::Owned(decompressed))
    } else {
        // Plain IFC: mmap. SAFETY contract documented in callers.
        let mmap = unsafe { Mmap::map(&file)? };
        // Refuse a truncated plain-STEP file at the choke-point so every
        // `_core.*` entry inherits the guard (GH #89). ZIP inputs took
        // the branch above and never reach here.
        check_step_trailer(&mmap)?;
        Ok(IfcSource::Mmap(mmap))
    }
}

/// Load an IFC source from bytes already in memory — the same
/// magic-byte dispatch [`open`] performs, minus the filesystem.
///
/// Plain STEP bytes are taken verbatim (after the truncation guard);
/// a PKZIP payload is decompressed to its largest STEP member. This is
/// the only entry point available on targets without `mmap` (the
/// browser build, GH #172), and it is the same code path a native
/// caller gets for `.ifczip` input, so the two agree by construction.
pub fn open_bytes(bytes: Vec<u8>) -> io::Result<IfcSource> {
    open_bytes_with(bytes, &DecompressLimits::default())
}

/// [`open_bytes`] with caller-chosen `.ifczip` decompression bounds.
///
/// Only the ZIP branch consults `limits`; plain STEP bytes are taken
/// verbatim after the truncation guard, exactly as [`open_bytes`] does.
pub fn open_bytes_with(bytes: Vec<u8>, limits: &DecompressLimits) -> io::Result<IfcSource> {
    if looks_like_zip(&bytes) {
        return Ok(IfcSource::Owned(decompress_ifczip_with(&bytes, limits)?));
    }
    check_step_trailer(&bytes)?;
    Ok(IfcSource::Owned(bytes))
}

/// Hard bounds applied while inflating an `.ifczip` (GH #175).
///
/// A ZIP is a self-describing container whose *declared* sizes are
/// attacker-controlled. A few hundred kilobytes of deflate can inflate
/// to gigabytes ("zip bomb"), which on the desktop wheel means an OOM
/// abort and in the browser (GH #172 — ifcfast.com parses dropped files
/// in the tab) means the visitor's tab dies. Compressed-size checks
/// upstream — ifcfast.com caps a drop at 300 MB — see only the *packed*
/// bytes and cannot detect this at all.
///
/// So the inflate is streamed against two independent caps, plus two
/// bounds on the container walk itself:
///
/// * [`Self::max_decompressed_bytes`] — absolute ceiling on the STEP
///   bytes produced. The backstop that holds even when every declared
///   field in the archive lies.
/// * [`Self::max_expansion_ratio`] — decompressed / compressed for the
///   chosen member, enforced only once
///   [`Self::ratio_floor_bytes`] have been produced (small archives
///   have noisy ratios and cannot hurt anyone). Measured deflate ratios
///   on real IFC: 4.77x (G55_RIV, 101 MB MEP), 4.84x (G55_ARK, 72 MB),
///   5.14x (Duplex, 2.4 MB), 5.28x (G55_RIE, 23 MB); the repo's small
///   STEP fixtures land at 1.8-2.7x. The 200x default is ~38x headroom
///   over the worst real file we have.
/// * [`Self::max_members`] and [`Self::max_name_len`] — bound the
///   central-directory walk, so a crafted archive cannot make the
///   member *scan* pathological before a single byte is inflated. The
///   member count is read out of the End-Of-Central-Directory record
///   directly (see [`declared_member_count`]) and refused *before* the
///   buffer is handed to the ZIP reader.
///
/// [`Default`] is what every existing entry point uses; pass your own
/// through [`decompress_ifczip_with`] / [`open_with`] / [`open_bytes_with`]
/// to widen or tighten them deliberately.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DecompressLimits {
    /// Absolute ceiling on decompressed bytes for the chosen member.
    pub max_decompressed_bytes: u64,
    /// Ceiling on decompressed / compressed for the chosen member.
    pub max_expansion_ratio: f64,
    /// The ratio cap only applies once this many bytes have inflated.
    pub ratio_floor_bytes: u64,
    /// Ceiling on the archive's member count.
    pub max_members: u64,
    /// Ceiling on a single member-name length, in bytes.
    pub max_name_len: usize,
}

/// Default [`DecompressLimits::max_decompressed_bytes`] on native
/// targets: 4 GiB. Larger than any IFC we have ever seen (the biggest
/// in the corpus is ~100 MB) while still bounded well under a 16 GB
/// desktop's RAM.
#[cfg(not(target_arch = "wasm32"))]
pub const DEFAULT_MAX_DECOMPRESSED_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Default [`DecompressLimits::max_decompressed_bytes`] in the browser:
/// 1 GiB. wasm32 is a 32-bit address space and a tab cannot hold more —
/// past this the allocation fails as an unrecoverable wasm abort rather
/// than a catchable error, so the guard has to fire first.
#[cfg(target_arch = "wasm32")]
pub const DEFAULT_MAX_DECOMPRESSED_BYTES: u64 = 1024 * 1024 * 1024;

/// Default [`DecompressLimits::max_expansion_ratio`] — see the struct
/// docs for the measured real-IFC ratios this is calibrated against.
pub const DEFAULT_MAX_EXPANSION_RATIO: f64 = 200.0;

/// Default [`DecompressLimits::ratio_floor_bytes`] — 8 MiB. Below this
/// the ratio is both noisy and harmless.
pub const DEFAULT_RATIO_FLOOR_BYTES: u64 = 8 * 1024 * 1024;

/// Default [`DecompressLimits::max_members`]. Real `.ifczip` containers
/// hold one model plus a handful of sidecars.
pub const DEFAULT_MAX_MEMBERS: u64 = 4096;

/// Default [`DecompressLimits::max_name_len`], in bytes. ZIP allows
/// 65535; no real member name comes close to 1 KiB.
pub const DEFAULT_MAX_NAME_LEN: usize = 1024;

impl Default for DecompressLimits {
    fn default() -> Self {
        Self {
            max_decompressed_bytes: DEFAULT_MAX_DECOMPRESSED_BYTES,
            max_expansion_ratio: DEFAULT_MAX_EXPANSION_RATIO,
            ratio_floor_bytes: DEFAULT_RATIO_FLOOR_BYTES,
            max_members: DEFAULT_MAX_MEMBERS,
            max_name_len: DEFAULT_MAX_NAME_LEN,
        }
    }
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg)
}

/// End-Of-Central-Directory record signature.
const EOCD_SIG: [u8; 4] = [b'P', b'K', 0x05, 0x06];
/// Zip64 End-Of-Central-Directory *locator* signature.
const ZIP64_LOCATOR_SIG: [u8; 4] = [b'P', b'K', 0x06, 0x07];
/// Zip64 End-Of-Central-Directory *record* signature.
const ZIP64_EOCD_SIG: [u8; 4] = [b'P', b'K', 0x06, 0x06];
/// A conformant EOCD is 22 bytes plus a comment of at most 64 KiB, so it
/// always begins within this many bytes of EOF.
const EOCD_MAX_SEARCH: usize = 22 + 0xFFFF;

/// Read the member count an archive *declares* in its End-Of-Central-
/// Directory record, without parsing the directory itself.
///
/// This runs before the buffer reaches `zip::ZipArchive::new`, which
/// pre-allocates one entry struct per declared member: on a crafted
/// archive that count is bounded only by the input length, so a modest
/// upload can ask for a multi-gigabyte reservation before any of our own
/// checks would get a turn. Reading the two fields ourselves costs a
/// backwards scan of at most 64 KiB.
///
/// Returns `None` when no EOCD is found — that is a malformed archive,
/// and the ZIP reader's own error is the better message for it.
pub fn declared_member_count(zip_bytes: &[u8]) -> Option<u64> {
    let start = zip_bytes.len().saturating_sub(EOCD_MAX_SEARCH);
    let window = &zip_bytes[start..];
    if window.len() < 22 {
        return None;
    }
    // Rightmost EOCD wins: a member whose *content* happens to contain
    // the signature must not shadow the real record.
    let rel = (0..=window.len() - 22)
        .rev()
        .find(|&i| window[i..i + 4] == EOCD_SIG)?;
    let eocd = start + rel;

    let total = u16::from_le_bytes([zip_bytes[eocd + 10], zip_bytes[eocd + 11]]) as u64;
    if total != 0xFFFF {
        return Some(total);
    }

    // 0xFFFF is the zip64 escape: the real count lives in the zip64 EOCD
    // record, found through the 20-byte locator immediately preceding.
    let loc = eocd.checked_sub(20)?;
    if zip_bytes[loc..loc + 4] != ZIP64_LOCATOR_SIG {
        return Some(total);
    }
    let off: usize = u64::from_le_bytes(zip_bytes[loc + 8..loc + 16].try_into().ok()?)
        .try_into()
        .ok()?;
    if off.checked_add(40)? > zip_bytes.len() || zip_bytes[off..off + 4] != ZIP64_EOCD_SIG {
        return Some(total);
    }
    Some(u64::from_le_bytes(
        zip_bytes[off + 32..off + 40].try_into().ok()?,
    ))
}

/// Inflate `reader` into an owned buffer, refusing loudly the moment it
/// crosses either cap.
///
/// The prospective length is checked *before* the chunk is appended, so
/// the buffer never grows past the cap and a rejected member never
/// yields a partial buffer — the `Err` drops it whole.
fn read_capped<R: Read>(
    reader: &mut R,
    member: &str,
    compressed: u64,
    hint: usize,
    limits: &DecompressLimits,
) -> io::Result<Vec<u8>> {
    const CHUNK: usize = 256 * 1024;
    let mut out: Vec<u8> = Vec::with_capacity(hint);
    let mut chunk = vec![0u8; CHUNK];
    loop {
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let got = out.len() as u64 + n as u64;
        if got > limits.max_decompressed_bytes {
            return Err(invalid(format!(
                ".ifczip: member {member:?} inflates past the decompressed-size limit of \
                 {} bytes (DecompressLimits::max_decompressed_bytes) — {} bytes produced \
                 before the stream was cut. Refusing to buffer it; raise the limit \
                 deliberately if this archive is genuine.",
                limits.max_decompressed_bytes, got
            )));
        }
        if got >= limits.ratio_floor_bytes {
            let ratio = got as f64 / compressed as f64;
            if ratio > limits.max_expansion_ratio {
                return Err(invalid(format!(
                    ".ifczip: member {member:?} inflates past the expansion-ratio limit of \
                     {:.0}x (DecompressLimits::max_expansion_ratio) — {} compressed bytes \
                     had already produced {} bytes ({:.1}x). Real IFC deflates ~2-6x, so \
                     this reads as a zip bomb; raise the limit deliberately if this \
                     archive is genuine.",
                    limits.max_expansion_ratio, compressed, got, ratio
                )));
            }
        }
        out.extend_from_slice(&chunk[..n]);
    }
    Ok(out)
}

/// Decompress an `.ifczip` payload (already in memory) and return the
/// raw STEP bytes from its largest `.ifc` / `.step` / `.stp` member,
/// under [`DecompressLimits::default`].
///
/// Strategy: walk every entry, pick the largest one whose name ends in
/// a known STEP extension. That's robust to archives that also carry
/// thumbnails, change-history XML, or sidecar metadata files — we just
/// want the IFC.
pub fn decompress_ifczip(zip_bytes: &[u8]) -> io::Result<Vec<u8>> {
    decompress_ifczip_with(zip_bytes, &DecompressLimits::default())
}

/// [`decompress_ifczip`] with caller-chosen bounds. See
/// [`DecompressLimits`] for what each one protects against.
///
/// Every failure is an `io::ErrorKind::InvalidData` naming the limit, the
/// observed size or ratio, and the member — never a partial buffer, never
/// a silent truncation.
pub fn decompress_ifczip_with(zip_bytes: &[u8], limits: &DecompressLimits) -> io::Result<Vec<u8>> {
    use std::io::Cursor;

    // Bound the container walk BEFORE the ZIP reader allocates one entry
    // struct per declared member (see `declared_member_count`).
    if let Some(declared) = declared_member_count(zip_bytes) {
        if declared > limits.max_members {
            return Err(invalid(format!(
                ".ifczip: archive declares {declared} members, over the limit of {} \
                 (DecompressLimits::max_members). Refusing to walk the central directory.",
                limits.max_members
            )));
        }
    }

    let cursor = Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| invalid(format!(".ifczip: {e}")))?;

    if archive.len() as u64 > limits.max_members {
        return Err(invalid(format!(
            ".ifczip: archive holds {} members, over the limit of {} \
             (DecompressLimits::max_members).",
            archive.len(),
            limits.max_members
        )));
    }

    // Find the largest STEP member by uncompressed size. Walking by
    // index avoids holding two mutable borrows on the archive.
    let mut best: Option<(usize, u64)> = None;
    for i in 0..archive.len() {
        let f = archive
            .by_index(i)
            .map_err(|e| invalid(format!(".ifczip entry {i}: {e}")))?;
        let raw_name = f.name();
        if raw_name.len() > limits.max_name_len {
            return Err(invalid(format!(
                ".ifczip: entry {i} carries a {}-byte member name, over the limit of {} \
                 (DecompressLimits::max_name_len).",
                raw_name.len(),
                limits.max_name_len
            )));
        }
        let name = raw_name.to_ascii_lowercase();
        if name.ends_with(".ifc") || name.ends_with(".step") || name.ends_with(".stp") {
            let size = f.size();
            if best.map(|(_, s)| size > s).unwrap_or(true) {
                best = Some((i, size));
            }
        }
    }

    let (idx, _size) = best.ok_or_else(|| {
        invalid(".ifczip: archive contains no .ifc / .step / .stp member".to_string())
    })?;
    let mut entry = archive
        .by_index(idx)
        .map_err(|e| invalid(format!(".ifczip member: {e}")))?;
    let member = entry.name().to_string();

    // Cheap refusal first: a header that *declares* more than the cap
    // never gets inflated at all.
    let declared = entry.size();
    if declared > limits.max_decompressed_bytes {
        return Err(invalid(format!(
            ".ifczip: member {member:?} declares {declared} decompressed bytes, over the \
             limit of {} (DecompressLimits::max_decompressed_bytes). Refusing to inflate it.",
            limits.max_decompressed_bytes
        )));
    }

    // Ratio denominator: the member's declared compressed size, clamped
    // to the archive length — a member cannot really carry more packed
    // bytes than the whole file holds, so a header inflating the
    // denominator to dodge the ratio cap gets clamped back. `max(1)`
    // keeps a stored/empty member from dividing by zero.
    let compressed = entry.compressed_size().min(zip_bytes.len() as u64).max(1);

    // Never trust the ZIP header's declared uncompressed size for the
    // allocation: it is attacker- (or corruption-) controlled, and a
    // member declaring 100 GB aborts the process on allocation failure
    // rather than surfacing a catchable error (GH #159). Reserve a
    // bounded hint and let the streaming read grow the buffer to
    // whatever the inflate stream actually produces — a genuinely large
    // member costs a few doublings, a lying header costs nothing.
    let hint = (declared as usize).min(MAX_ZIP_PREALLOC);
    read_capped(&mut entry, &member, compressed, hint, limits)
}

/// Upper bound on the up-front allocation for a decompressed `.ifczip`
/// member. 64 MiB covers most real IFCs outright; larger ones grow
/// geometrically from here.
const MAX_ZIP_PREALLOC: usize = 64 * 1024 * 1024;

/// Compress STEP bytes into a single-member `.ifczip` (ZIP/deflate)
/// archive — the inverse of [`decompress_ifczip`], used by the write
/// axis when a caller's `out_path` ends in `.ifczip` (GH #132 item 7).
/// `inner_name` is the archive member name (conventionally
/// `<stem>.ifc`).
pub fn compress_ifczip(step_bytes: &[u8], inner_name: &str) -> io::Result<Vec<u8>> {
    use std::io::Write;
    let mut buf = Vec::with_capacity(step_bytes.len() / 4);
    {
        let cursor = std::io::Cursor::new(&mut buf);
        let mut zw = zip::ZipWriter::new(cursor);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .large_file(step_bytes.len() as u64 >= 0xFFFF_FFFF);
        zw.start_file(inner_name, opts)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!(".ifczip: {e}")))?;
        zw.write_all(step_bytes)?;
        zw.finish()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!(".ifczip: {e}")))?;
    }
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
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
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
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
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

        let tmp =
            std::env::temp_dir().join(format!("ifcfast-source-test-{}.ifczip", std::process::id()));
        std::fs::write(&tmp, &archive).unwrap();

        let src = open(&tmp).expect("zip open");
        assert!(matches!(src, IfcSource::Owned(_)));
        assert_eq!(src.as_bytes(), payload);

        // Plain IFC path also works → Mmap variant.
        let plain =
            std::env::temp_dir().join(format!("ifcfast-source-test-{}.ifc", std::process::id()));
        std::fs::write(&plain, payload).unwrap();
        let src2 = open(&plain).expect("plain open");
        assert!(matches!(src2, IfcSource::Mmap(_)));
        assert_eq!(src2.as_bytes(), payload);

        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(&plain);
    }

    #[test]
    fn trailer_guard_accepts_terminated_buffer() {
        let whole = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";
        assert!(check_step_trailer(whole).is_ok());
        // Trailer in the very last bytes (no trailing newline) still ok.
        let tight = b"ISO-10303-21;\nDATA;\nENDSEC;\nEND-ISO-10303-21;";
        assert!(check_step_trailer(tight).is_ok());
        // Empty buffer is not "truncated STEP" — left to empty-file handling.
        assert!(check_step_trailer(b"").is_ok());
    }

    #[test]
    fn trailer_guard_refuses_truncated_buffer() {
        // A file cut mid-DATA: well-formed prefix, no trailer.
        let truncated =
            b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=IFCWALL('guid',$,$,$,$,$,$,$,$";
        let err = check_step_trailer(truncated).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("truncated"));
    }

    #[test]
    fn open_refuses_truncated_plain_ifc() {
        // The whole point of GH #89: a direct `open` (the choke-point
        // every `_core.*` entry funnels through) must refuse a truncated
        // plain IFC, not just the Python `header()` wrapper.
        let truncated = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\n#1=IFCWALL('g',$,$,$,$,$,$,$";
        let tmp =
            std::env::temp_dir().join(format!("ifcfast-trunc-test-{}.ifc", std::process::id()));
        std::fs::write(&tmp, truncated).unwrap();
        let err = match open(&tmp) {
            Ok(_) => panic!("truncated plain IFC must be refused at open"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = std::fs::remove_file(&tmp);
    }

    // ---- .ifczip decompression limits (GH #175) ----------------------

    /// A member that is pure `'0'` — deflate crushes 8 MiB of it to a few
    /// KB, which is the whole shape of a zip bomb in miniature.
    fn make_bomb(name: &str, uncompressed: usize) -> Vec<u8> {
        make_zip(name, &vec![b'0'; uncompressed])
    }

    #[test]
    fn bomb_refused_on_declared_size() {
        // 8 MiB member, 1 MiB cap. The declared uncompressed size is
        // honest here, so the cheap pre-check fires and nothing inflates.
        let archive = make_bomb("bomb.ifc", 8 * 1024 * 1024);
        let limits = DecompressLimits {
            max_decompressed_bytes: 1024 * 1024,
            ..Default::default()
        };
        let err = decompress_ifczip_with(&archive, &limits).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("bomb.ifc"), "{msg}");
        assert!(msg.contains("1048576"), "{msg}");
        assert!(msg.contains("8388608"), "{msg}");
        assert!(msg.contains("max_decompressed_bytes"), "{msg}");
    }

    #[test]
    fn bomb_refused_mid_inflate_when_the_header_lies() {
        // The declared-size pre-check is only as good as the header. A
        // crafted archive can declare 1 KB and inflate forever, so the
        // streaming cap is the one that actually has to hold. Exercised
        // directly on `read_capped` because the ZIP writer will not emit
        // a lying header for us.
        let limits = DecompressLimits {
            max_decompressed_bytes: 1024 * 1024,
            ..Default::default()
        };
        let stream = vec![b'0'; 8 * 1024 * 1024];
        let mut cursor = std::io::Cursor::new(stream);
        let err = read_capped(&mut cursor, "liar.ifc", 1024, 0, &limits).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("liar.ifc"), "{msg}");
        assert!(msg.contains("1048576"), "{msg}");
        assert!(msg.contains("max_decompressed_bytes"), "{msg}");
    }

    #[test]
    fn bomb_refused_on_expansion_ratio() {
        // Absolute cap wide open (64 MiB); the ratio is what must catch
        // this. 8 MiB of `'0'` deflates to a few KB — hundreds of x,
        // against real IFC's measured 4.8-5.3x.
        let archive = make_bomb("bomb.ifc", 8 * 1024 * 1024);
        let limits = DecompressLimits {
            max_decompressed_bytes: 64 * 1024 * 1024,
            max_expansion_ratio: 10.0,
            ratio_floor_bytes: 1024 * 1024,
            ..Default::default()
        };
        let err = decompress_ifczip_with(&archive, &limits).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("bomb.ifc"), "{msg}");
        assert!(msg.contains("expansion-ratio"), "{msg}");
        assert!(msg.contains("10x"), "{msg}");
        assert!(msg.contains("max_expansion_ratio"), "{msg}");
    }

    #[test]
    fn ratio_floor_lets_small_high_ratio_members_through() {
        // A tiny member can trivially beat 10x without being a threat;
        // the floor is what keeps the ratio cap from false-flagging it.
        let payload = vec![b'0'; 64 * 1024];
        let archive = make_zip("small.ifc", &payload);
        let limits = DecompressLimits {
            max_expansion_ratio: 10.0,
            ratio_floor_bytes: 1024 * 1024,
            ..Default::default()
        };
        let got = decompress_ifczip_with(&archive, &limits).unwrap();
        assert_eq!(got, payload);
    }

    #[test]
    fn member_count_is_bounded_before_the_directory_walk() {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zw = zip::ZipWriter::new(cursor);
            let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored);
            for i in 0..40 {
                zw.start_file(format!("m{i}.ifc"), opts).unwrap();
                zw.write_all(b"x").unwrap();
            }
            zw.finish().unwrap();
        }
        // The EOCD read must agree with what the archive actually holds.
        assert_eq!(declared_member_count(&buf), Some(40));

        let limits = DecompressLimits {
            max_members: 8,
            ..Default::default()
        };
        let err = decompress_ifczip_with(&buf, &limits).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("declares 40 members"), "{msg}");
        assert!(msg.contains("max_members"), "{msg}");
    }

    #[test]
    fn member_name_length_is_bounded() {
        let long = format!("{}.ifc", "a".repeat(2000));
        let archive = make_zip(&long, b"ISO-10303-21;\n");
        let limits = DecompressLimits {
            max_name_len: 1024,
            ..Default::default()
        };
        let err = decompress_ifczip_with(&archive, &limits).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let msg = err.to_string();
        assert!(msg.contains("2004-byte member name"), "{msg}");
        assert!(msg.contains("max_name_len"), "{msg}");
    }

    #[test]
    fn real_ifc_fixture_opens_under_default_limits() {
        // The guard must be invisible to real files. `minimal.ifc` is the
        // repo's own STEP fixture; zipped, it round-trips byte-for-byte
        // through the default limits, and its deflate ratio sits far
        // under the 200x cap (real IFCs measure 1.8-5.3x).
        let step = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/minimal.ifc"),
        )
        .expect("tests/fixtures/minimal.ifc");
        let archive = make_zip("minimal.ifc", &step);
        let got = decompress_ifczip(&archive).expect("real fixture opens");
        assert_eq!(got, step);

        let ratio = step.len() as f64 / archive.len() as f64;
        let limits = DecompressLimits::default();
        assert!(
            ratio < limits.max_expansion_ratio,
            "fixture ratio {ratio:.2}x should be far under the {}x cap",
            limits.max_expansion_ratio
        );

        // …and through the byte entry point the browser build uses.
        let src = open_bytes(archive).expect("open_bytes on a real ifczip");
        assert_eq!(src.as_bytes(), &step[..]);
    }

    #[test]
    fn open_bytes_with_applies_the_caller_limits() {
        let archive = make_bomb("bomb.ifc", 8 * 1024 * 1024);
        let limits = DecompressLimits {
            max_decompressed_bytes: 1024 * 1024,
            ..Default::default()
        };
        let err = match open_bytes_with(archive, &limits) {
            Ok(_) => panic!("a bomb must be refused at open_bytes_with"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("max_decompressed_bytes"));
    }

    #[test]
    fn open_with_applies_the_caller_limits() {
        let archive = make_bomb("bomb.ifc", 8 * 1024 * 1024);
        let tmp =
            std::env::temp_dir().join(format!("ifcfast-bomb-test-{}.ifczip", std::process::id()));
        std::fs::write(&tmp, &archive).unwrap();
        let limits = DecompressLimits {
            max_decompressed_bytes: 1024 * 1024,
            ..Default::default()
        };
        let err = match open_with(&tmp, &limits) {
            Ok(_) => panic!("a bomb must be refused at open_with"),
            Err(e) => e,
        };
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("max_decompressed_bytes"));
        // The stock `open` refuses it too — at the ratio cap rather than
        // the size cap, since 8 MiB of `'0'` is well under 4 GiB but
        // ~1028x expanded.
        let err = match open(&tmp) {
            Ok(_) => panic!("a bomb must be refused at the default limits too"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("max_expansion_ratio"), "{err}");
        let _ = std::fs::remove_file(&tmp);

        // A genuine archive still opens through the same entry point.
        let step = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/minimal.ifc"),
        )
        .expect("tests/fixtures/minimal.ifc");
        let real =
            std::env::temp_dir().join(format!("ifcfast-real-test-{}.ifczip", std::process::id()));
        std::fs::write(&real, make_zip("minimal.ifc", &step)).unwrap();
        let ok = open(&real).expect("a real ifczip opens at the default limits");
        assert_eq!(ok.as_bytes(), &step[..]);
        let _ = std::fs::remove_file(&real);
    }

    #[test]
    fn defaults_are_the_documented_numbers() {
        let d = DecompressLimits::default();
        #[cfg(not(target_arch = "wasm32"))]
        assert_eq!(d.max_decompressed_bytes, 4 * 1024 * 1024 * 1024);
        #[cfg(target_arch = "wasm32")]
        assert_eq!(d.max_decompressed_bytes, 1024 * 1024 * 1024);
        assert_eq!(d.max_expansion_ratio, 200.0);
        assert_eq!(d.ratio_floor_bytes, 8 * 1024 * 1024);
        assert_eq!(d.max_members, 4096);
        assert_eq!(d.max_name_len, 1024);
    }

    #[test]
    fn open_accepts_terminated_plain_ifc() {
        let whole = b"ISO-10303-21;\nHEADER;\nENDSEC;\nDATA;\nENDSEC;\nEND-ISO-10303-21;\n";
        let tmp =
            std::env::temp_dir().join(format!("ifcfast-whole-test-{}.ifc", std::process::id()));
        std::fs::write(&tmp, whole).unwrap();
        let src = open(&tmp).expect("terminated plain IFC opens");
        assert_eq!(src.as_bytes(), whole);
        let _ = std::fs::remove_file(&tmp);
    }
}
