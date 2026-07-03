//! STEP value formatting shared by the write axis (hotswap, mutate).
//!
//! Everything the writer mints must parse back through the same lexer
//! that reads authored files, so these helpers are the single place the
//! *inverse* encodings live: REAL formatting that satisfies the
//! ISO-10303-21 grammar, and string encoding that is the true inverse of
//! [`crate::lexer::decode_string`].

/// Format an `f64` as a STEP REAL. `{:?}` gives the shortest
/// round-tripping form and a `.` for whole values (`1.0`), but drops the
/// point in exponent form (`5e-5`) — the ISO-10303-21 REAL grammar requires
/// a decimal point even with an exponent, so re-insert it. Non-finite
/// values must be rejected upstream (the write entry points validate);
/// debug-assert here as the backstop.
pub fn fmt_real(x: f64) -> String {
    debug_assert!(x.is_finite(), "fmt_real: non-finite {x}");
    let s = format!("{x:?}");
    match s.find(['e', 'E']) {
        Some(epos) if !s[..epos].contains('.') => {
            format!("{}.0{}", &s[..epos], &s[epos..])
        }
        _ => s,
    }
}

/// A STEP coordinate tuple `(x,y,z)` — the shared inner list for an
/// `IfcCartesianPointList3D` item, an `IfcCartesianPoint`'s
/// `Coordinates`, or an `IfcDirection`'s `DirectionRatios`.
pub fn fmt_tuple(v: &[f64; 3]) -> String {
    format!("({},{},{})", fmt_real(v[0]), fmt_real(v[1]), fmt_real(v[2]))
}

/// Encode a Rust string as a quoted STEP string — the inverse of
/// [`crate::lexer::decode_string`].
///
/// ISO-10303-21 ed.3 streams are UTF-8, and `decode_string` prefers raw
/// UTF-8 over `\X2\` escapes, so the encoder writes non-ASCII (æøå, CJK)
/// as raw UTF-8 bytes rather than legacy escapes: `encode(decode(x))`
/// then reproduces modern exporters' output and never mangles Norwegian
/// characters. Only the two characters the grammar reserves are escaped:
/// `'` doubles to `''` and `\` doubles to `\\`.
///
/// Control characters (`< U+0020`, `U+007F`) are not representable in a
/// STEP string; they return `Err` (fail loud) rather than being dropped
/// or smuggled through to corrupt the record framing.
pub fn encode_string(s: &str) -> Result<String, String> {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        match c {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 || c as u32 == 0x7F => {
                return Err(format!(
                    "control character U+{:04X} is not representable in a STEP string",
                    c as u32
                ));
            }
            c => out.push(c),
        }
    }
    out.push('\'');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::decode_string;

    #[test]
    fn real_grammar() {
        assert_eq!(fmt_real(1.0), "1.0");
        assert_eq!(fmt_real(-2.5), "-2.5");
        // Exponent forms keep a decimal point.
        let tiny = fmt_real(5e-323);
        assert!(tiny.contains('.'), "exponent REAL needs a point: {tiny}");
    }

    #[test]
    fn string_roundtrip() {
        for s in [
            "plain",
            "it's",
            "back\\slash",
            "Vegg Æ-Ø-Å blåbærsyltetøy",
            "混凝土",
            "",
        ] {
            let enc = encode_string(s).unwrap();
            let dec = decode_string(enc.as_bytes()).unwrap();
            assert_eq!(dec, s, "round-trip failed for {s:?} via {enc:?}");
        }
    }

    #[test]
    fn control_chars_fail_loud() {
        assert!(encode_string("line\nbreak").is_err());
        assert!(encode_string("\t").is_err());
    }
}
