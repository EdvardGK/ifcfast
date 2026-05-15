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

use memchr::memchr2;

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
pub fn data_section_start(buf: &[u8]) -> Option<usize> {
    // Be tolerant to `DATA ;` and case. The marker only appears in the
    // section-control area, which is structurally before any record, so
    // a literal byte search is fine.
    let needle = b"DATA";
    let mut i = 0;
    while let Some(pos) = find_subslice(&buf[i..], needle) {
        let abs = i + pos;
        // Verify token boundary: previous byte must not be alnum.
        let prev_ok = abs == 0
            || !buf[abs - 1].is_ascii_alphanumeric() && buf[abs - 1] != b'_';
        if !prev_ok {
            i = abs + needle.len();
            continue;
        }
        // Skip past 'DATA' then whitespace then expect ';'.
        let mut j = abs + needle.len();
        while j < buf.len() && (buf[j] as char).is_whitespace() {
            j += 1;
        }
        if j < buf.len() && buf[j] == b';' {
            return Some(j + 1);
        }
        i = abs + needle.len();
    }
    None
}

/// `ENDSEC;` marks the end of DATA. Returns its position if found, else
/// the buffer length.
pub fn endsec_position(buf: &[u8], from: usize) -> usize {
    if let Some(p) = find_subslice(&buf[from..], b"ENDSEC") {
        from + p
    } else {
        buf.len()
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
        // Skip leading whitespace/control chars.
        while pos < end && is_ws(buf[pos]) {
            pos += 1;
        }
        if pos >= end {
            break;
        }
        // Records must begin with `#`.
        if buf[pos] != b'#' {
            // Could be ENDSEC; etc. Bail out of the DATA loop entirely
            // — anything beyond a non-`#` start indicates we've left
            // the entity stream.
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
        // memchr2 jumps to the next `;` or `'` — the only two bytes that
        // matter for framing. memchr is SIMD-accelerated on x86.
        match memchr2(b';', b'\'', &buf[i..limit]) {
            Some(off) => {
                let abs = i + off;
                match buf[abs] {
                    b';' => return Some(abs),
                    b'\'' => {
                        i = skip_quoted_string(buf, abs + 1, limit);
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
    // trailing whitespace. We can't naively search for `)` because the
    // matching one might be embedded in a quoted string — but the
    // record's trailing `)` is, by the format, the close paren of the
    // outer argument list. So trim from the right.
    let mut j = record.len();
    while j > args_start && is_ws(record[j - 1]) {
        j -= 1;
    }
    if j == args_start || record[j - 1] != b')' {
        return None;
    }
    let args = &record[args_start..j - 1];
    Some((id, type_name, args))
}

/// Tiny substring search used only for section markers (rare path).
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let first = needle[0];
    let mut i = 0;
    while i + needle.len() <= haystack.len() {
        match memchr::memchr(first, &haystack[i..]) {
            Some(off) => {
                let abs = i + off;
                if abs + needle.len() > haystack.len() {
                    return None;
                }
                if &haystack[abs..abs + needle.len()] == needle {
                    return Some(abs);
                }
                i = abs + 1;
            }
            None => return None,
        }
    }
    None
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

fn trim_ws(s: &[u8]) -> &[u8] {
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
                if let Ok(decoded) = String::from_utf16(&units) {
                    out.push_str(&decoded);
                }
                i = k + 4;
                continue;
            }
        }
        // Fast path: assume the byte is valid UTF-8 (Latin-1 codepoint
        // 0x00..0x7F is identical). Outside of that we fall back to
        // pushing the byte as Latin-1.
        if b < 0x80 {
            out.push(b as char);
        } else {
            out.push(b as char); // Latin-1 → Unicode same codepoint
        }
        i += 1;
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
