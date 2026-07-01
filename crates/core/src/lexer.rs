//! Byte-level STEP / ISO-10303-21 tokenizer.
//!
//! The STEP physical file format puts each entity instance on a record
//! that ends with `;`. A record looks like
//!
//! ```text
//! #<id> = <TYPE> ( <args> ) ;
//! ```
//!
//! Whitespace (incl. newlines) can appear anywhere outside string
//! literals. String literals use single quotes; an embedded `'` is
//! escaped by doubling. STEP also allows multi-line records, so we
//! frame on `;` rather than newline.
//!
//! For tier-1 indexing we don't need to interpret args here — we just
//! locate each record and hand the (id, type, args) byte ranges to the
//! indexer, which decides what (if anything) to extract.

use memchr::memchr3;

/// One STEP entity instance located in the source buffer.
#[derive(Debug, Clone, Copy)]
pub struct Record<'a> {
    pub id: u64,
    /// Uppercase type token, e.g. `IFCWALLSTANDARDCASE`.
    pub type_name: &'a [u8],
    /// Raw argument list, between the outer `(` and `)`. Whitespace
    /// preserved; nested parens and quoted strings intact.
    pub args: &'a [u8],
}

/// Position of the `DATA;` section content, exclusive of the marker.
///
/// ISO-10303-21 lets `/* ... */` comments and single-quoted strings
/// appear anywhere whitespace is allowed, so the scan must treat both as
/// inert: a `DATA;` literal sitting inside a HEADER string value (e.g.
/// `FILE_DESCRIPTION(('Bridge DATA; rev2'),'2;1')`) must NOT be mistaken
/// for the real section marker. We therefore walk byte-by-byte, skipping
/// over quoted strings and comments, and only match `DATA` when it is a
/// bare token in the section-control stream.
pub fn data_section_start(buf: &[u8]) -> Option<usize> {
    let needle = b"DATA";
    let len = buf.len();
    let mut i = 0;
    let mut prev: u8 = 0; // last non-skipped byte seen (0 = start of file)
    while i < len {
        match buf[i] {
            b'\'' => {
                // Quoted string: contents are inert. The byte before the
                // string for token-boundary purposes is the quote itself.
                i = skip_quoted_string(buf, i + 1, len);
                prev = b'\'';
                continue;
            }
            b'/' if i + 1 < len && buf[i + 1] == b'*' => {
                i = skip_block_comment(buf, i + 2, len);
                prev = b' '; // a comment is whitespace-equivalent
                continue;
            }
            b'D' | b'd' => {
                // Token-boundary: previous meaningful byte must not be
                // part of a longer identifier.
                let prev_ok = !prev.is_ascii_alphanumeric() && prev != b'_';
                if prev_ok
                    && i + needle.len() <= len
                    && buf[i..i + needle.len()].eq_ignore_ascii_case(needle)
                {
                    // Skip past 'DATA' then whitespace/comments then `;`.
                    let mut j = i + needle.len();
                    j = skip_ws_and_comments(buf, j, len);
                    if j < len && buf[j] == b';' {
                        return Some(j + 1);
                    }
                }
                prev = buf[i];
                i += 1;
            }
            b => {
                prev = b;
                i += 1;
            }
        }
    }
    None
}

/// `ENDSEC;` marks the end of DATA. Returns its position if found, else
/// the buffer length.
///
/// String- and comment-aware: an `ENDSEC` substring inside a quoted
/// value (e.g. a wall named `'SEE ENDSEC FOR DETAILS'`) or inside a
/// `/* */` comment must NOT terminate the section.
///
/// This scans the whole DATA section (hot path), so we keep it SIMD-fast:
/// `memchr3` jumps directly to the next byte that matters — `'` (string
/// start), `/` (possible comment start), or `E` (possible `ENDSEC`). Every
/// run of ordinary bytes between them is skipped by the SIMD scan rather
/// than inspected one at a time. STEP section keywords are uppercase per
/// spec, so the `E`-first fast path matches the prior behaviour exactly.
pub fn endsec_position(buf: &[u8], from: usize) -> usize {
    let needle = b"ENDSEC";
    let len = buf.len();
    let mut i = from;
    while i < len {
        match memchr3(b'\'', b'/', b'E', &buf[i..len]) {
            Some(off) => {
                let abs = i + off;
                match buf[abs] {
                    b'\'' => i = skip_quoted_string(buf, abs + 1, len),
                    b'/' => {
                        if abs + 1 < len && buf[abs + 1] == b'*' {
                            i = skip_block_comment(buf, abs + 2, len);
                        } else {
                            i = abs + 1;
                        }
                    }
                    b'E' => {
                        if abs + needle.len() <= len && &buf[abs..abs + needle.len()] == needle {
                            return abs;
                        }
                        i = abs + 1;
                    }
                    _ => unreachable!(),
                }
            }
            None => return len,
        }
    }
    len
}

/// Given that we're now inside a `/* ... */` block comment starting at
/// `i` (the byte AFTER the opening `/*`), return the index of the byte
/// immediately AFTER the closing `*/`. If unterminated, returns `end`.
fn skip_block_comment(buf: &[u8], mut i: usize, end: usize) -> usize {
    while i < end {
        let off = match memchr::memchr(b'*', &buf[i..end]) {
            Some(o) => o,
            None => return end,
        };
        let star = i + off;
        if star + 1 < end && buf[star + 1] == b'/' {
            return star + 2;
        }
        i = star + 1;
    }
    end
}

/// Skip the run of whitespace and `/* */` comments starting at `i`,
/// returning the index of the first byte that is neither. Per
/// ISO-10303-21, a comment is whitespace-equivalent and may appear
/// anywhere whitespace can.
#[inline]
fn skip_ws_and_comments(buf: &[u8], mut i: usize, end: usize) -> usize {
    loop {
        while i < end && is_ws(buf[i]) {
            i += 1;
        }
        if i + 1 < end && buf[i] == b'/' && buf[i + 1] == b'*' {
            i = skip_block_comment(buf, i + 2, end);
            continue;
        }
        return i;
    }
}

/// Iterate over every entity record in `buf[start..end]`.
///
/// `callback` receives one [`Record`] per instance. The iterator is
/// resilient to malformed records: it logs nothing and just skips
/// anything between two `;` that doesn't match the `#<id> = TYPE(...)`
/// shape.
pub fn for_each_record<'a, F>(buf: &'a [u8], start: usize, end: usize, mut callback: F)
where
    F: FnMut(Record<'a>),
{
    let mut pos = start;
    while pos < end {
        // Skip leading whitespace/control chars and `/* */` comments.
        // Comments may legally sit between records (e.g. an exporter
        // banner), so skipping them — rather than bailing — is what keeps
        // every record after a comment visible.
        pos = skip_ws_and_comments(buf, pos, end);
        if pos >= end {
            break;
        }
        // Records must begin with `#`.
        if buf[pos] != b'#' {
            // Could be ENDSEC; etc. Bail out of the DATA loop entirely
            // — a non-`#`, non-comment start indicates we've left the
            // entity stream.
            break;
        }
        let record_start = pos;
        let term = match find_record_end(buf, pos, end) {
            Some(t) => t,
            None => break,
        };
        // Parse the record header: #<digits> = TYPE
        if let Some(rec) = parse_record(&buf[record_start..term]) {
            callback(Record {
                id: rec.0,
                type_name: rec.1,
                args: rec.2,
            });
        }
        pos = term + 1; // step past the `;`
    }
}

/// Like [`for_each_record`], but reports each record's absolute byte
/// span `[start, end)` in `buf` instead of its parsed slices. `start` is
/// the `#`; `end` is one byte past the terminating `;`. This is the
/// primitive the `doc` module's serialiser uses to re-emit kept records
/// verbatim (byte-identical round-trip) — it deliberately exposes the
/// record boundaries that [`for_each_record`] keeps internal.
///
/// Only well-formed `#<id> = TYPE(...)` records invoke the callback;
/// inter-record whitespace and comments are not reported, so a caller
/// re-emitting spans should attribute the gap *after* a record to that
/// record (extend its emit span to the next record's `start`) to
/// reproduce the original separators.
pub fn for_each_record_span<F>(buf: &[u8], start: usize, end: usize, mut callback: F)
where
    F: FnMut(u64, usize, usize),
{
    let mut pos = start;
    while pos < end {
        pos = skip_ws_and_comments(buf, pos, end);
        if pos >= end {
            break;
        }
        if buf[pos] != b'#' {
            break;
        }
        let record_start = pos;
        let term = match find_record_end(buf, pos, end) {
            Some(t) => t,
            None => break,
        };
        if let Some(rec) = parse_record(&buf[record_start..term]) {
            callback(rec.0, record_start, term + 1);
        }
        pos = term + 1;
    }
}

/// Every `#<digits>` entity-reference token in `bytes`, in order,
/// skipping single-quoted string literals and `/* */` comments (a `#`
/// inside `'a #4 name'` or a comment is not a reference). Schema-free:
/// it doesn't care which attribute position a ref sits in, only that
/// it's a real reference token — which is all the `doc` module's
/// reachability graph needs.
///
/// NOTE: scanning a whole record span (`#id = TYPE(...)`) yields the
/// record's OWN id as the first token. Callers wanting only *outbound*
/// references drop it (see `doc::refs::forward_refs`).
pub fn scan_ref_tokens(bytes: &[u8]) -> Vec<u64> {
    let n = bytes.len();
    let mut out: Vec<u64> = Vec::with_capacity(4);
    let mut i = 0;
    while i < n {
        match bytes[i] {
            b'\'' => i = skip_quoted_string(bytes, i + 1, n),
            b'/' if i + 1 < n && bytes[i + 1] == b'*' => {
                i = skip_block_comment(bytes, i + 2, n)
            }
            b'#' => {
                let s = i + 1;
                let mut j = s;
                while j < n && bytes[j].is_ascii_digit() {
                    j += 1;
                }
                if j > s {
                    let mut id: u64 = 0;
                    for &b in &bytes[s..j] {
                        id = id.wrapping_mul(10).wrapping_add((b - b'0') as u64);
                    }
                    out.push(id);
                    i = j;
                } else {
                    i += 1;
                }
            }
            _ => i += 1,
        }
    }
    out
}

#[inline]
fn is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | 0x0B | 0x0C)
}

/// Locate the `;` that terminates the record beginning at `start`,
/// honouring STEP single-quoted string escapes (`''` = literal `'`).
fn find_record_end(buf: &[u8], start: usize, end: usize) -> Option<usize> {
    let mut i = start;
    let limit = end.min(buf.len());
    while i < limit {
        // memchr3 jumps to the next byte that matters for framing: `;`
        // (record terminator), `'` (string start), or `/` (possible
        // `/* */` comment start). memchr is SIMD-accelerated on x86.
        match memchr3(b';', b'\'', b'/', &buf[i..limit]) {
            Some(off) => {
                let abs = i + off;
                match buf[abs] {
                    b';' => return Some(abs),
                    b'\'' => {
                        i = skip_quoted_string(buf, abs + 1, limit);
                    }
                    b'/' => {
                        // Only `/*` opens a comment; a lone `/` (e.g. a
                        // path or division in a value) is an ordinary byte.
                        if abs + 1 < limit && buf[abs + 1] == b'*' {
                            i = skip_block_comment(buf, abs + 2, limit);
                        } else {
                            i = abs + 1;
                        }
                    }
                    _ => unreachable!(),
                }
            }
            None => return None,
        }
    }
    None
}

/// Given that we're now inside a single-quoted string starting at
/// `i` (the byte AFTER the opening `'`), return the index of the byte
/// immediately AFTER the closing `'`. Handles `''` escape sequences.
fn skip_quoted_string(buf: &[u8], mut i: usize, end: usize) -> usize {
    while i < end {
        let off = match memchr::memchr(b'\'', &buf[i..end]) {
            Some(o) => o,
            None => return end,
        };
        let q = i + off;
        // A doubled quote `''` is an escape — stay inside the string.
        if q + 1 < end && buf[q + 1] == b'\'' {
            i = q + 2;
            continue;
        }
        return q + 1;
    }
    end
}

/// Split a record (`#42 = TYPE ( args ) ;` — the trailing `;` is NOT
/// in `record`) into id, type, args byte slices.
fn parse_record(record: &[u8]) -> Option<(u64, &[u8], &[u8])> {
    // Expect leading `#`. We already verified that in the caller, but
    // be defensive.
    let mut i = 0;
    if record.first() != Some(&b'#') {
        return None;
    }
    i += 1;
    // Read digits → entity id.
    let id_start = i;
    while i < record.len() && record[i].is_ascii_digit() {
        i += 1;
    }
    if i == id_start {
        return None;
    }
    // Fast u64 parse. The digit slice is guaranteed-ASCII-digit by the
    // scan above, so we skip std's UTF-8 validation and per-digit
    // checked-overflow path. Step ids are at most ~10 digits so u64
    // wrap is impossible. Saves ~25 ns/record × 14M records on ST28.
    let mut id: u64 = 0;
    for &b in &record[id_start..i] {
        id = id.wrapping_mul(10).wrapping_add((b - b'0') as u64);
    }
    // Skip whitespace then `=`.
    while i < record.len() && is_ws(record[i]) {
        i += 1;
    }
    if i >= record.len() || record[i] != b'=' {
        return None;
    }
    i += 1;
    while i < record.len() && is_ws(record[i]) {
        i += 1;
    }
    // Read type token: alphanumeric / underscore.
    let type_start = i;
    while i < record.len()
        && (record[i].is_ascii_alphanumeric() || record[i] == b'_')
    {
        i += 1;
    }
    if i == type_start {
        return None;
    }
    let type_name = &record[type_start..i];
    // Skip whitespace then `(`.
    while i < record.len() && is_ws(record[i]) {
        i += 1;
    }
    if i >= record.len() || record[i] != b'(' {
        return None;
    }
    let args_start = i + 1;
    // Args end at the last byte of `record` that is `)`, trimming
    // trailing whitespace and `/* */` comments (a comment may legally
    // sit between the close paren and the `;`). We can't naively search
    // for `)` because the matching one might be embedded in a quoted
    // string — but the record's trailing `)` is, by the format, the
    // close paren of the outer argument list. So trim from the right.
    let mut j = record.len();
    loop {
        while j > args_start && is_ws(record[j - 1]) {
            j -= 1;
        }
        // Strip a trailing `*/ ... /*` comment if present.
        if j > args_start + 1 && record[j - 1] == b'/' && record[j - 2] == b'*' {
            // Find the matching `/*` opener scanning left.
            let mut k = j - 2;
            let mut found = false;
            while k >= args_start + 1 {
                if record[k - 1] == b'/' && record[k] == b'*' {
                    j = k - 1;
                    found = true;
                    break;
                }
                if k == args_start + 1 {
                    break;
                }
                k -= 1;
            }
            if found {
                continue;
            }
        }
        break;
    }
    if j == args_start || record[j - 1] != b')' {
        return None;
    }
    let args = &record[args_start..j - 1];
    Some((id, type_name, args))
}

// ----------------------------------------------------------------------
// Argument parsing
// ----------------------------------------------------------------------

/// Split an argument list at top-level commas, respecting string and
/// paren nesting. Returns one byte slice per positional argument.
pub fn split_top_level_args(args: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::with_capacity(12);
    split_top_level_args_into(args, &mut out);
    out
}

/// Same as [`split_top_level_args`] but writes into a caller-supplied
/// buffer (cleared first). Lets the hot indexer/extractor loop reuse a
/// single allocation instead of one `Vec` per record — saves a malloc
/// per STEP entity on big files (600K+ on ST28_RIV).
pub fn split_top_level_args_into<'a>(args: &'a [u8], out: &mut Vec<&'a [u8]>) {
    out.clear();
    let mut depth: i32 = 0;
    let mut i = 0;
    let mut field_start = 0;
    while i < args.len() {
        match args[i] {
            b'\'' => {
                i = skip_quoted_string(args, i + 1, args.len());
                continue;
            }
            b'(' => depth += 1,
            b')' => depth -= 1,
            b',' if depth == 0 => {
                out.push(trim_ws(&args[field_start..i]));
                field_start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    if field_start <= args.len() {
        out.push(trim_ws(&args[field_start..]));
    }
}

pub(crate) fn trim_ws(s: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < s.len() && is_ws(s[start]) {
        start += 1;
    }
    let mut end = s.len();
    while end > start && is_ws(s[end - 1]) {
        end -= 1;
    }
    &s[start..end]
}

/// Decode a STEP single-quoted string. `bytes` must include the
/// surrounding quotes. Returns the unescaped UTF-8 string (with
/// `\X\xx\` and `\X2\xxxx\` unicode escapes resolved best-effort).
///
/// Raw (un-escaped) high bytes are decoded as UTF-8 first — ISO-10303-21
/// ed.3 streams are UTF-8 and many exporters write æøå/CJK directly — and
/// only fall back to per-byte Latin-1 when the byte run is not valid UTF-8.
pub fn decode_string(bytes: &[u8]) -> Option<String> {
    if bytes.len() < 2 || bytes[0] != b'\'' || *bytes.last()? != b'\'' {
        return None;
    }
    let inner = &bytes[1..bytes.len() - 1];
    let mut out = String::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        let b = inner[i];
        if b == b'\'' && i + 1 < inner.len() && inner[i + 1] == b'\'' {
            out.push('\'');
            i += 2;
            continue;
        }
        // ISO-10303-21 `\\` — an encoded literal backslash collapses to a
        // single `\`. This MUST run before the `\S\` / `\X\` escape probes
        // below: without it, `'C:\\path'` decodes to `C:\\path` (the
        // doubled backslash survives), and worse, a literal backslash that
        // happens to precede `X2`/`S`/`X` text (e.g. `\\X2 drawing`) gets
        // misread as the start of a Unicode escape. Consuming the pair here
        // emits exactly one `\` and skips both bytes so neither is treated
        // as an escape introducer.
        if b == b'\\' && i + 1 < inner.len() && inner[i + 1] == b'\\' {
            out.push('\\');
            i += 2;
            continue;
        }
        // ISO-10303-21 `\S\C` short form: `C` is an ASCII char and the
        // intended Latin-1 byte is `C | 0x80`. Common in Tekla/Revit
        // exports: `\S\E` -> Å, `\S\X` -> Ø, `\S\F` -> Æ. (`'\\` excluded
        // because the run-of-`\` doubled-quote case wins above.)
        if b == b'\\' && i + 3 < inner.len() && inner[i + 1] == b'S' && inner[i + 2] == b'\\' {
            let c = inner[i + 3];
            if c.is_ascii() && c >= 0x20 {
                let high = (c | 0x80) as u32;
                if let Some(ch) = char::from_u32(high) {
                    out.push(ch);
                    i += 4;
                    continue;
                }
            }
        }

        // STEP encoded-char escapes. Best-effort: handle the two most
        // common forms; pass anything else through verbatim.
        if b == b'\\' && i + 2 < inner.len() && inner[i + 1] == b'X' {
            // \X\HH           ISO 8859-1 byte (5 chars total, no closing
            // backslash — that's the ISO-10303-21 short form actually used
            // by Revit / MagiCAD / Archicad for Norwegian/Swedish chars).
            if inner[i + 2] == b'\\' && i + 4 < inner.len() {
                if let Ok(hex) = std::str::from_utf8(&inner[i + 3..i + 5]) {
                    if let Ok(v) = u32::from_str_radix(hex, 16) {
                        if let Some(c) = char::from_u32(v) {
                            out.push(c);
                            i += 5;
                            continue;
                        }
                    }
                }
            }
            // \X2\HHHH...\X0\   UTF-16BE sequence (variable length)
            if inner[i + 2] == b'2' && i + 3 < inner.len() && inner[i + 3] == b'\\' {
                let body_start = i + 4;
                // Find terminator `\X0\`.
                let mut k = body_start;
                while k + 3 < inner.len()
                    && !(inner[k] == b'\\'
                        && inner[k + 1] == b'X'
                        && inner[k + 2] == b'0'
                        && inner[k + 3] == b'\\')
                {
                    k += 1;
                }
                let body = &inner[body_start..k];
                let mut units: Vec<u16> = Vec::with_capacity(body.len() / 4);
                let mut p = 0;
                while p + 4 <= body.len() {
                    if let Ok(hex) = std::str::from_utf8(&body[p..p + 4]) {
                        if let Ok(v) = u16::from_str_radix(hex, 16) {
                            units.push(v);
                        }
                    }
                    p += 4;
                }
                // `from_utf16_lossy` (not `from_utf16`): an unpaired
                // surrogate in a malformed export must not nuke the whole
                // body. Erroring out pushed *nothing* — the entire `\X2\`
                // run silently vanished. Lossy decode substitutes U+FFFD
                // for the bad unit and keeps the surrounding text, which
                // matches ifcopenshell's best-effort behaviour.
                out.push_str(&String::from_utf16_lossy(&units));
                // If the terminator `\X0\` was never found (truncated /
                // malformed export), `k` walked to the end and the four
                // trailing bytes can't be a real terminator — clamp so
                // `i` doesn't run past `inner.len()`.
                i = (k + 4).min(inner.len());
                continue;
            }
            // \X4\HHHHHHHH...\X0\   full Unicode code points, 8 hex digits
            // each (ISO-10303-21 ed.3 escape for non-BMP characters; what
            // some exporters emit for emoji / supplementary-plane CJK
            // instead of a `\X2\` surrogate pair). ifcopenshell decodes
            // these; we previously passed the literal escape text through.
            if inner[i + 2] == b'4' && i + 3 < inner.len() && inner[i + 3] == b'\\' {
                let body_start = i + 4;
                let mut k = body_start;
                while k + 3 < inner.len()
                    && !(inner[k] == b'\\'
                        && inner[k + 1] == b'X'
                        && inner[k + 2] == b'0'
                        && inner[k + 3] == b'\\')
                {
                    k += 1;
                }
                let body = &inner[body_start..k];
                let mut p = 0;
                while p + 8 <= body.len() {
                    if let Ok(hex) = std::str::from_utf8(&body[p..p + 8]) {
                        if let Ok(v) = u32::from_str_radix(hex, 16) {
                            // Invalid scalar values (surrogates, >U+10FFFF)
                            // become U+FFFD, mirroring the lossy `\X2\`
                            // path rather than dropping silently.
                            out.push(char::from_u32(v).unwrap_or('\u{FFFD}'));
                        }
                    }
                    p += 8;
                }
                i = (k + 4).min(inner.len());
                continue;
            }
        }
        // Plain ASCII (and the low half of Latin-1) maps straight through.
        if b < 0x80 {
            out.push(b as char);
            i += 1;
            continue;
        }
        // Raw high byte that is not part of a STEP escape. ISO-10303-21
        // ed.3 streams are UTF-8, and several exporters (Bonsai/BlenderBIM,
        // some ArchiCAD/Tekla configs) write raw UTF-8 æøå/CJK directly
        // instead of `\X2\` escapes. Decode the maximal run of non-escape
        // bytes as UTF-8 first; fall back to per-byte Latin-1 only when the
        // run is not valid UTF-8 (legacy Latin-1 high bytes are rarely
        // valid UTF-8, so this disambiguates the two encodings cleanly).
        //
        // The run stops at the next byte that could begin an escape or a
        // doubled quote (`\` or `'`, both < 0x80), so we never swallow the
        // escape handling above.
        let run_start = i;
        let mut j = i + 1;
        while j < inner.len() && inner[j] != b'\\' && inner[j] != b'\'' {
            j += 1;
        }
        let run = &inner[run_start..j];
        match std::str::from_utf8(run) {
            Ok(s) => {
                out.push_str(s);
                i = j;
            }
            Err(_) => {
                // Deterministic Latin-1 fallback for this byte; the rest of
                // the run is reconsidered on the next iteration (it may
                // itself contain a valid UTF-8 tail).
                out.push(b as char); // Latin-1 → Unicode same codepoint
                i += 1;
            }
        }
    }
    Some(out)
}

/// Try to parse the `args[idx]` field. Returns:
///
/// - `Field::Null` for `$`
/// - `Field::Star` for `*`
/// - `Field::String(s)` for a quoted string
/// - `Field::Ref(id)` for `#<id>`
/// - `Field::Number(f)` for numeric
/// - `Field::Enum(name)` for `.NAME.`
/// - `Field::List(raw)` for `(...)` — the raw inner bytes
/// - `Field::Other` for anything else
#[derive(Debug)]
pub enum Field<'a> {
    Null,
    Star,
    String(String),
    Ref(u64),
    Number(f64),
    Enum(&'a [u8]),
    List(&'a [u8]),
    // Bytes preserved so a caller debugging an unrecognised field can see
    // exactly what didn't match. Not read on the hot path.
    #[allow(dead_code)]
    Other(&'a [u8]),
}

pub fn parse_field(raw: &[u8]) -> Field<'_> {
    let raw = trim_ws(raw);
    if raw.is_empty() {
        return Field::Null;
    }
    match raw[0] {
        b'$' if raw.len() == 1 => Field::Null,
        b'*' if raw.len() == 1 => Field::Star,
        b'\'' => match decode_string(raw) {
            Some(s) => Field::String(s),
            None => Field::Other(raw),
        },
        b'#' => {
            let digits = &raw[1..];
            match std::str::from_utf8(digits).ok().and_then(|s| s.parse().ok()) {
                Some(n) => Field::Ref(n),
                None => Field::Other(raw),
            }
        }
        b'.' => {
            // `.ENUM.`
            if raw.len() >= 3 && *raw.last().unwrap() == b'.' {
                Field::Enum(&raw[1..raw.len() - 1])
            } else {
                Field::Other(raw)
            }
        }
        b'(' => {
            // `(...)`
            if *raw.last().unwrap() == b')' {
                Field::List(&raw[1..raw.len() - 1])
            } else {
                Field::Other(raw)
            }
        }
        b'-' | b'+' | b'0'..=b'9' => {
            match std::str::from_utf8(raw).ok().and_then(|s| s.parse().ok()) {
                Some(n) => Field::Number(n),
                None => Field::Other(raw),
            }
        }
        _ => Field::Other(raw),
    }
}

/// Extract every `#id` reference from a list field's body
/// (the bytes between the outer `(` and `)`). Used for relationship
/// `RelatedObjects` / `RelatedElements`.
pub fn parse_ref_list(body: &[u8]) -> Vec<u64> {
    let mut out = Vec::with_capacity(8);
    for f in split_top_level_args(body) {
        if let Field::Ref(id) = parse_field(f) {
            out.push(id);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{data_section_start, decode_string, endsec_position, for_each_record};

    fn dec(quoted: &[u8]) -> String {
        decode_string(quoted).expect("decode_string returned None")
    }

    // ---- Section / record framing (GH #72) -------------------------------

    /// Collect every record's (id, TYPE) the way the indexer walk does:
    /// frame the DATA section, then iterate records inside it.
    fn collect(src: &str) -> Vec<(u64, String)> {
        let buf = src.as_bytes();
        let start = data_section_start(buf).unwrap_or(0);
        let end = endsec_position(buf, start);
        let mut out = Vec::new();
        for_each_record(buf, start, end, |rec| {
            out.push((
                rec.id,
                String::from_utf8_lossy(rec.type_name).into_owned(),
            ));
        });
        out
    }

    const HEADER: &str =
        "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nENDSEC;\n";

    #[test]
    fn framing_normal_file_no_regression() {
        let src = format!(
            "{HEADER}DATA;\n\
             #1=IFCWALL('g1',$,$);\n\
             #2=IFCSLAB('g2',$,$);\n\
             ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let recs = collect(&src);
        assert_eq!(
            recs,
            vec![(1, "IFCWALL".into()), (2, "IFCSLAB".into())],
            "normal two-record file must parse both records"
        );
    }

    #[test]
    fn framing_comment_between_records_keeps_following() {
        // GH #72 bug 1: a `/* */` comment between records dropped every
        // record after it. All three records must survive.
        let src = format!(
            "{HEADER}DATA;\n\
             #1=IFCWALL('7WALL1',$,$);\n\
             /* exported by FooCAD */\n\
             #2=IFCWALL('7WALL2',$,$);\n\
             /* multi\nline\ncomment */ #3=IFCSLAB('s',$,$);\n\
             ENDSEC;\n"
        );
        let recs = collect(&src);
        assert_eq!(
            recs,
            vec![
                (1, "IFCWALL".into()),
                (2, "IFCWALL".into()),
                (3, "IFCSLAB".into())
            ],
            "records after a /* */ comment must still parse"
        );
    }

    #[test]
    fn framing_comment_inside_record_does_not_desync() {
        // A comment containing `;` and `'` inside the arg list must not be
        // treated as a record terminator or string.
        let src = format!(
            "{HEADER}DATA;\n\
             #1=IFCWALL('a' /* ; ' note */ ,$,$);\n\
             #2=IFCSLAB('b',$,$);\n\
             ENDSEC;\n"
        );
        let recs = collect(&src);
        assert_eq!(recs.len(), 2, "comment with ; and ' must not split the record");
        assert_eq!(recs[0].0, 1);
        assert_eq!(recs[1].0, 2);
    }

    #[test]
    fn framing_endsec_inside_string_does_not_truncate() {
        // GH #72 bug 2: literal `ENDSEC` inside a quoted value truncated
        // the section, dropping the record and everything after it.
        let src = format!(
            "{HEADER}DATA;\n\
             #1=IFCWALL('SEE ENDSEC FOR DETAILS',$,$);\n\
             #2=IFCSLAB('after',$,$);\n\
             ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let recs = collect(&src);
        assert_eq!(
            recs,
            vec![(1, "IFCWALL".into()), (2, "IFCSLAB".into())],
            "ENDSEC inside a string must not end the section"
        );
    }

    #[test]
    fn framing_endsec_inside_comment_does_not_truncate() {
        let src = format!(
            "{HEADER}DATA;\n\
             /* TODO before ENDSEC review */\n\
             #1=IFCWALL('w',$,$);\n\
             ENDSEC;\n"
        );
        let recs = collect(&src);
        assert_eq!(recs, vec![(1, "IFCWALL".into())]);
    }

    #[test]
    fn framing_data_inside_header_string_not_section_start() {
        // GH #72 bug 3: `DATA;` inside a HEADER string value started the
        // section early, emptying the parse.
        let src = "ISO-10303-21;\nHEADER;\n\
             FILE_DESCRIPTION(('Bridge DATA; rev2'),'2;1');\n\
             ENDSEC;\n\
             DATA;\n\
             #1=IFCWALL('real',$,$);\n\
             ENDSEC;\n";
        let buf = src.as_bytes();
        let start = data_section_start(buf).expect("real DATA; must be found");
        // The real DATA; sits after the header ENDSEC, so the first record
        // must be the genuine IFCWALL, not garbage parsed from the header.
        let recs = collect(src);
        assert_eq!(
            recs,
            vec![(1, "IFCWALL".into())],
            "DATA; inside a header string must not be the section start"
        );
        // And start must point past the real marker, not the header one.
        assert!(start > src.find("ENDSEC").unwrap());
    }

    #[test]
    fn framing_data_token_boundary() {
        // `_DATA;` or `METADATA;` must not be mistaken for `DATA;`.
        let src = "HEADER;\nFILE_NAME('METADATA;',$);\nENDSEC;\n\
             DATA;\n#1=IFCWALL('x',$,$);\nENDSEC;\n";
        let recs = collect(src);
        assert_eq!(recs, vec![(1, "IFCWALL".into())]);
    }

    #[test]
    fn ascii_unchanged() {
        assert_eq!(dec(b"'Basic Wall'"), "Basic Wall");
        assert_eq!(dec(b"''"), "");
        // Doubled-quote escape still works.
        assert_eq!(dec(b"'it''s'"), "it's");
    }

    #[test]
    fn raw_utf8_norwegian() {
        // Raw UTF-8 bytes for "Dør-æå" (the c5_utf8.ifc repro case).
        let mut q = vec![b'\''];
        q.extend_from_slice("Dør-æå".as_bytes());
        q.push(b'\'');
        assert_eq!(dec(&q), "Dør-æå");
    }

    #[test]
    fn raw_utf8_multibyte_non_latin() {
        // CJK (3-byte UTF-8) plus an emoji (4-byte) — must round-trip.
        let mut q = vec![b'\''];
        q.extend_from_slice("壁体🧱".as_bytes());
        q.push(b'\'');
        assert_eq!(dec(&q), "壁体🧱");
    }

    #[test]
    fn raw_utf8_mixed_with_ascii_tail() {
        let mut q = vec![b'\''];
        q.extend_from_slice("Vegg-Ø 200mm".as_bytes());
        q.push(b'\'');
        assert_eq!(dec(&q), "Vegg-Ø 200mm");
    }

    #[test]
    fn legacy_latin1_fallback() {
        // 0xD8 = 'Ø' in Latin-1 but an invalid lone UTF-8 lead byte.
        // Must fall back to the Latin-1 codepoint, not corrupt the rest.
        let q = b"'\xD8 200'";
        assert_eq!(dec(q), "Ø 200");
    }

    #[test]
    fn step_x_short_escape() {
        // \X\C5 -> Å (Latin-1 0xC5).
        assert_eq!(dec(b"'\\X\\C5'"), "Å");
    }

    #[test]
    fn step_x2_utf16_escape() {
        // \X2\00C5\X0\ -> Å (UTF-16BE).
        assert_eq!(dec(b"'\\X2\\00C5\\X0\\'"), "Å");
        // Multi-unit: "ÆØÅ".
        assert_eq!(dec(b"'\\X2\\00C600D800C5\\X0\\'"), "ÆØÅ");
    }

    #[test]
    fn step_s_short_escape() {
        // \S\E -> Å (E|0x80 = 0xC5).
        assert_eq!(dec(b"'\\S\\E'"), "Å");
    }

    #[test]
    fn g55_real_escaped_strings() {
        // Real strings from G55_ARK.ifc (an escape-using Revit export):
        // these MUST keep decoding correctly after the raw-UTF-8 fix.
        assert_eq!(dec(b"'Gr\\X\\F8nland 55'"), "Grønland 55");
        assert_eq!(
            dec(b"'S\\X\\F8yle Betong Sirkul\\X\\E6r Eksisterende:500'"),
            "Søyle Betong Sirkulær Eksisterende:500"
        );
    }

    #[test]
    fn escape_adjacent_to_raw_utf8() {
        // Raw UTF-8 run must stop at the backslash so the escape after it
        // is still decoded correctly.
        let mut q = vec![b'\''];
        q.extend_from_slice("æ".as_bytes());
        q.extend_from_slice(b"\\X\\C5"); // -> Å
        q.push(b'\'');
        assert_eq!(dec(&q), "æÅ");
    }

    // ---- GH #76 item 1: encoded literal backslash `\\` ------------------

    #[test]
    fn encoded_backslash_collapses() {
        // `\\` is the ISO-10303-21 encoding of one literal backslash.
        // Pre-fix `'C:\\path'` decoded to `C:\\path` (doubled).
        assert_eq!(dec(br"'C:\\path'"), r"C:\path");
        // A lone trailing `\\` collapses too.
        assert_eq!(dec(br"'a\\'"), r"a\");
    }

    #[test]
    fn encoded_backslash_before_escape_text_not_misread() {
        // A literal backslash immediately followed by what *looks* like an
        // escape introducer (`X2`, `S`, `X`) must NOT be parsed as an
        // escape. `\\X2 drawing` is a literal `\` then the text `X2 ...`.
        assert_eq!(dec(br"'\\X2 drawing'"), r"\X2 drawing");
        assert_eq!(dec(br"'\\S note'"), r"\S note");
        assert_eq!(dec(br"'\\X note'"), r"\X note");
    }

    // ---- GH #76 item 2: `\X4\` non-BMP, 8-hex code points --------------

    #[test]
    fn step_x4_non_bmp_escape() {
        // `\X4\0001F600\X0\` -> 😀 (U+1F600). ifcopenshell decodes this;
        // pre-fix we passed the literal escape text through.
        assert_eq!(dec(br"'A\X4\0001F600\X0\B'"), "A😀B");
        // Multiple code points in one run.
        assert_eq!(dec(br"'\X4\0001F6000001F44D\X0\'"), "😀👍");
        // BMP code point expressed in the 8-hex form still works.
        assert_eq!(dec(br"'\X4\000000C5\X0\'"), "Å");
    }

    // ---- GH #76 item 3: `\X2\` unpaired surrogate is lossy, not dropped -

    #[test]
    fn x2_unpaired_surrogate_keeps_surrounding_text() {
        // D800 is a high surrogate with no following low surrogate. Pre-fix
        // `String::from_utf16` Err'd and pushed NOTHING for the whole body,
        // dropping the valid `0041` ('A') too. Lossy decode keeps 'A' and
        // substitutes U+FFFD for the bad unit.
        let out = dec(br"'\X2\0041D800\X0\Z'");
        assert!(out.starts_with('A'), "valid leading unit must survive: {out:?}");
        assert!(out.ends_with('Z'), "trailing text after \\X0\\ must survive: {out:?}");
        assert!(out.contains('\u{FFFD}'), "bad surrogate -> U+FFFD: {out:?}");
        // A well-formed surrogate pair still decodes to the astral char.
        assert_eq!(dec(br"'\X2\D83DDE00\X0\'"), "😀");
    }
}
