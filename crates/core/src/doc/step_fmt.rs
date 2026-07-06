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

/// Encode a Rust string as a quoted STEP string in the *canonical
/// escaped form* of ISO-10303-21 §7.3 (GH #142).
///
/// `decode_string` is deliberately lenient — it accepts raw UTF-8 on top
/// of the standard escapes (GH #77). Its inverse must NOT mirror that
/// leniency: a string body may only use `SPACE..TILDE` (0x20–0x7E), and
/// strict readers (ifcopenshell, Solibri, Revit) silently drop raw
/// UTF-8 bytes — silent data loss on æøå. So every code point outside
/// the basic alphabet is emitted as a control directive: BMP characters
/// as a `\X2\<UTF-16BE hex>\X0\` run, non-BMP as a `\X4\<UTF-32 hex>\X0\`
/// run (both of which `decode_string` reads back). `'` doubles to `''`
/// and `\` doubles to `\\`.
///
/// Control characters (`< U+0020`, `U+007F`) still return `Err` (fail
/// loud): they'd be *encodable* via `\X2\`, but a control character in a
/// Name/Description is upstream garbage, not data to smuggle through.
pub fn encode_string(s: &str) -> Result<String, String> {
    fn is_control(u: u32) -> bool {
        u < 0x20 || u == 0x7F
    }
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        let u = c as u32;
        match c {
            '\'' => out.push_str("''"),
            '\\' => out.push_str("\\\\"),
            _ if is_control(u) => {
                return Err(format!(
                    "control character U+{u:04X} is not representable in a STEP string"
                ));
            }
            _ if u <= 0x7E => out.push(c),
            _ => {
                // Maximal homogeneous run of escaped characters: \X2\ for
                // BMP (4 hex digits each), \X4\ for the rest (8 digits).
                use std::fmt::Write as _;
                let bmp = u <= 0xFFFF;
                out.push_str(if bmp { "\\X2\\" } else { "\\X4\\" });
                if bmp {
                    write!(out, "{u:04X}").unwrap();
                } else {
                    write!(out, "{u:08X}").unwrap();
                }
                while let Some(&n) = chars.peek() {
                    let nu = n as u32;
                    if nu <= 0x7E || is_control(nu) || (nu <= 0xFFFF) != bmp {
                        break;
                    }
                    if bmp {
                        write!(out, "{nu:04X}").unwrap();
                    } else {
                        write!(out, "{nu:08X}").unwrap();
                    }
                    chars.next();
                }
                out.push_str("\\X0\\");
            }
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
        // Control chars break a run mid-string too.
        assert!(encode_string("æ\nø").is_err());
    }

    // GH #142: the emitted form must be the canonical ISO-10303-21 §7.3
    // escape encoding — strict readers (ifcopenshell 0.8.5 verified)
    // silently drop raw UTF-8 bytes in string literals.
    #[test]
    fn non_ascii_is_escaped_not_raw() {
        let enc = encode_string("Vegg-Ø-æøå").unwrap();
        assert_eq!(enc, r"'Vegg-\X2\00D8\X0\-\X2\00E600F800E5\X0\'");
        assert!(enc.is_ascii(), "no raw bytes outside 0x20..=0x7E: {enc:?}");
        assert_eq!(decode_string(enc.as_bytes()).unwrap(), "Vegg-Ø-æøå");
    }

    #[test]
    fn non_bmp_uses_x4_runs() {
        assert_eq!(encode_string("😀").unwrap(), r"'\X4\0001F600\X0\'");
        assert_eq!(encode_string("😀👍").unwrap(), r"'\X4\0001F6000001F44D\X0\'");
        // Adjacent BMP / non-BMP switch runs instead of mixing widths.
        let enc = encode_string("æ😀å").unwrap();
        assert_eq!(enc, r"'\X2\00E6\X0\\X4\0001F600\X0\\X2\00E5\X0\'");
        assert_eq!(decode_string(enc.as_bytes()).unwrap(), "æ😀å");
    }

    #[test]
    fn every_emitted_string_is_basic_alphabet() {
        for s in ["Kjøkken-æøå", "混凝土", "ÆØÅ 😀 blandet", "plain"] {
            let enc = encode_string(s).unwrap();
            assert!(
                enc.bytes().all(|b| (0x20..=0x7E).contains(&b)),
                "raw byte escaped the basic alphabet in {enc:?}"
            );
            assert_eq!(decode_string(enc.as_bytes()).unwrap(), s);
        }
    }
}
