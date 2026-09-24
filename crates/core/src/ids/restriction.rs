//! Value matching: an IDS [`Val`] (simple value or `xs:restriction`)
//! against an actual IFC value.
//!
//! Semantics follow the buildingSMART conformance suite first and
//! IfcTester (ifctester 0.8.4, `ifctester/facet.py`) where the suite is
//! silent:
//!
//! * **Simple value vs string** — exact, case-sensitive equality
//!   (facet.py `Attribute.__call__` / `Property.__call__`: `value != self.value`).
//! * **Simple value vs number** — the IDS text is cast with Python
//!   `float()` (facet.py `cast_to_value`: ints are compared as floats so
//!   `"42"` matches `42` and `"42.3"` does not). Reals use the IDS
//!   tolerance (below); integers compare exactly.
//! * **Real tolerance** — `Documentation/ImplementersDocumentation/tolerance.md`:
//!   `v - |v|·ε - ε ≤ x ≤ v + |v|·ε + ε`, `ε = 1e-6`, applied to simple
//!   values and enumerations, **not** to bounds. The comparison is
//!   inclusive: the suite's `pass-comparison_tolerance_for_floating_point_zero_upper_bound`
//!   expects `1e-6` to equal `0.`. IfcTester's `is_x` (facet.py:52-59)
//!   is relative-only (`v·(1±1e-6)`), which fails the zero/near-zero
//!   tolerance cases — we follow the suite, not IfcTester, here.
//! * **Booleans** — IDS writes booleans as XSD lexical `true`/`false`
//!   (also `1`/`0`), facet.py `cast_to_value`. `TRUE`/`FALSE` never match
//!   (the `invalid-booleans_must_be_specified_as_lowercase_strings`
//!   cases; compile rejects them up front where the type is known).
//!   Callers map IFC `.T.`/`.F.` to `Actual::Bool`.
//! * **Restriction** — every facet kind present must hold (AND);
//!   `enumeration` = any listed value equals (cast to the actual's type);
//!   `pattern` = actual must be a string and ANY pattern fully matches
//!   (XSD OR semantics, pinned by `pass-regex_patterns_work_in_OR_*`;
//!   IfcTester 0.8.4 ANDs them — divergence); bounds = Python
//!   `float(actual)` vs bound, exact; lengths = `len(str(actual))` in
//!   code points, with Python `str()` formatting for numbers/booleans.
//! * **Lists** (enumerated / list properties) — pass if ANY element
//!   matches (facet.py `Property.__call__`, list branches).

use regex::Regex;

use super::ir::{Restriction, Val, XsdBase};
use super::xsd_regex::compile_xsd_pattern;
use super::IdsError;

/// IDS equality tolerance ε (tolerance.md).
pub const REAL_TOLERANCE: f64 = 1e-6;

/// An actual IFC value, already unwrapped from its IFC defined type by
/// the caller.
#[derive(Debug, Clone, PartialEq)]
pub enum Actual {
    /// Labels, texts, identifiers, enumeration values (as their
    /// uppercase token), dates/times in their IFC lexical form.
    Str(String),
    /// Reals and measures (in SI, after unit conversion).
    Num(f64),
    /// Integers and counts.
    Int(i64),
    /// IfcBoolean / IfcLogical `.T.`/`.F.`. Callers decide what a
    /// logical `.U.` is (IfcTester treats it as empty for attributes).
    Bool(bool),
    /// List / enumerated / bounded-list values: any element may match.
    List(Vec<Actual>),
}

/// A [`Val`] with its patterns compiled and bounds parsed, for the hot
/// path (one compile per facet, many matches).
#[derive(Debug, Clone)]
pub struct CompiledVal {
    kind: CompiledKind,
}

#[derive(Debug, Clone)]
enum CompiledKind {
    Simple(String),
    Restriction(CompiledRestriction),
}

#[derive(Debug, Clone)]
struct CompiledRestriction {
    enumeration: Option<Vec<String>>,
    patterns: Option<Vec<Regex>>,
    min_inclusive: Option<f64>,
    min_exclusive: Option<f64>,
    max_inclusive: Option<f64>,
    max_exclusive: Option<f64>,
    length: Option<u32>,
    min_length: Option<u32>,
    max_length: Option<u32>,
}

impl CompiledVal {
    /// Compile a value. Fails on an invalid pattern or bound (`InvalidIds`)
    /// or an untranslatable pattern (`Unsupported`); never silently.
    pub fn new(val: &Val) -> Result<CompiledVal, IdsError> {
        let kind = match val {
            Val::Simple(s) => CompiledKind::Simple(s.clone()),
            Val::Restriction(r) => CompiledKind::Restriction(compile_restriction(r)?),
        };
        Ok(CompiledVal { kind })
    }

    /// Does `actual` satisfy this value?
    pub fn matches(&self, actual: &Actual) -> bool {
        if let Actual::List(items) = actual {
            return items.iter().any(|a| self.matches(a));
        }
        match &self.kind {
            CompiledKind::Simple(s) => simple_eq(s, actual),
            CompiledKind::Restriction(r) => r.matches_scalar(actual),
        }
    }
}

/// One-shot convenience: compile `val` and match. Use [`CompiledVal`]
/// when matching many values against one facet.
pub fn matches(val: &Val, actual: &Actual) -> Result<bool, IdsError> {
    Ok(CompiledVal::new(val)?.matches(actual))
}

fn compile_restriction(r: &Restriction) -> Result<CompiledRestriction, IdsError> {
    let patterns = match &r.patterns {
        Some(ps) => Some(
            ps.iter()
                .map(|p| compile_xsd_pattern(p))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        None => None,
    };
    let bound = |name: &str, v: &Option<String>| -> Result<Option<f64>, IdsError> {
        match v {
            None => Ok(None),
            Some(s) => parse_bound(r.base, name, s).map(Some),
        }
    };
    Ok(CompiledRestriction {
        enumeration: r.enumeration.clone(),
        patterns,
        min_inclusive: bound("minInclusive", &r.min_inclusive)?,
        min_exclusive: bound("minExclusive", &r.min_exclusive)?,
        max_inclusive: bound("maxInclusive", &r.max_inclusive)?,
        max_exclusive: bound("maxExclusive", &r.max_exclusive)?,
        length: r.length,
        min_length: r.min_length,
        max_length: r.max_length,
    })
}

/// Bounds are numeric comparisons (IfcTester: `float(value)`). A bound
/// on a temporal base (xs:date …) is valid XSD we do not implement; any
/// other unparseable bound is an invalid IDS.
pub(crate) fn parse_bound(base: XsdBase, facet: &str, lexical: &str) -> Result<f64, IdsError> {
    if base.is_temporal() {
        return Err(IdsError::unsupported(format!(
            "xsd-restriction:{facet}-on-temporal-base"
        )));
    }
    match py_float(lexical) {
        Some(v) if !v.is_nan() => Ok(v),
        _ => Err(IdsError::invalid(format!(
            "xs:{facet} value '{lexical}' is not a number"
        ))),
    }
}

impl CompiledRestriction {
    fn matches_scalar(&self, actual: &Actual) -> bool {
        if let Some(en) = &self.enumeration {
            if !en.iter().any(|v| simple_eq(v, actual)) {
                return false;
            }
        }
        if let Some(ps) = &self.patterns {
            let Actual::Str(s) = actual else {
                return false;
            };
            if !ps.iter().any(|re| re.is_match(s)) {
                return false;
            }
        }
        let has_bounds = self.min_inclusive.is_some()
            || self.min_exclusive.is_some()
            || self.max_inclusive.is_some()
            || self.max_exclusive.is_some();
        if has_bounds {
            let Some(x) = actual_as_float(actual) else {
                return false;
            };
            if x.is_nan() {
                return false;
            }
            if self.min_inclusive.is_some_and(|b| x < b)
                || self.min_exclusive.is_some_and(|b| x <= b)
                || self.max_inclusive.is_some_and(|b| x > b)
                || self.max_exclusive.is_some_and(|b| x >= b)
            {
                return false;
            }
        }
        if self.length.is_some() || self.min_length.is_some() || self.max_length.is_some() {
            let Some(s) = py_str(actual) else {
                return false;
            };
            let n = s.chars().count() as u64;
            if self.length.is_some_and(|l| n != l as u64)
                || self.min_length.is_some_and(|l| n < l as u64)
                || self.max_length.is_some_and(|l| n > l as u64)
            {
                return false;
            }
        }
        true
    }
}

/// Simple IDS value (lexical) vs a scalar actual.
fn simple_eq(ids: &str, actual: &Actual) -> bool {
    match actual {
        Actual::Str(s) => s == ids,
        Actual::Num(x) => match py_float(ids) {
            Some(v) => real_eq(*x, v),
            None => false,
        },
        // facet.py cast_to_value: int targets are compared via float so
        // "42" == 42 and "42.3" != 42.
        Actual::Int(i) => match py_float(ids) {
            Some(v) => v == *i as f64,
            None => false,
        },
        Actual::Bool(b) => match xsd_bool(ids) {
            Some(v) => v == *b,
            None => false,
        },
        Actual::List(items) => items.iter().any(|a| simple_eq(ids, a)),
    }
}

/// IDS tolerance equality (tolerance.md), inclusive.
pub fn real_eq(actual: f64, expected: f64) -> bool {
    if !actual.is_finite() || !expected.is_finite() {
        return actual == expected;
    }
    let tol = expected.abs() * REAL_TOLERANCE + REAL_TOLERANCE;
    actual >= expected - tol && actual <= expected + tol
}

/// XSD `xs:boolean` lexical space: `true`, `false`, `1`, `0`.
fn xsd_bool(s: &str) -> Option<bool> {
    match s {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// Python `float(str)`: surrounding whitespace allowed; decimal,
/// exponent, `inf`/`infinity`/`nan` (any case, signed). Underscore digit
/// grouping (`1_000`) is not accepted (never seen in IDS; rejecting it
/// makes the value non-numeric rather than silently different).
pub(crate) fn py_float(s: &str) -> Option<f64> {
    let t = s.trim_matches(|c: char| c.is_whitespace());
    if t.is_empty() || t.contains('_') {
        return None;
    }
    t.parse::<f64>().ok()
}

fn actual_as_float(a: &Actual) -> Option<f64> {
    match a {
        Actual::Num(x) => Some(*x),
        Actual::Int(i) => Some(*i as f64),
        // Python float(True) == 1.0
        Actual::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Actual::Str(s) => py_float(s),
        Actual::List(_) => None,
    }
}

/// Python `str(value)` for length facets.
fn py_str(a: &Actual) -> Option<String> {
    match a {
        Actual::Str(s) => Some(s.clone()),
        Actual::Int(i) => Some(i.to_string()),
        Actual::Bool(b) => Some(if *b { "True" } else { "False" }.to_string()),
        Actual::Num(x) => Some(py_float_repr(*x)),
        Actual::List(_) => None,
    }
}

/// Python `repr(float)`: shortest round-trip digits; scientific when the
/// decimal exponent is < -4 or >= 16; always a `.0` on integral values.
pub(crate) fn py_float_repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    if x == 0.0 {
        return if x.is_sign_negative() {
            "-0.0".into()
        } else {
            "0.0".into()
        };
    }
    // Rust `{:e}` gives shortest round-trip digits: "-1.2345e-7".
    let sci = format!("{x:e}");
    let (mant, exp) = match sci.split_once('e') {
        Some((m, e)) => (m, e.parse::<i32>().unwrap_or(0)),
        None => (sci.as_str(), 0),
    };
    let (neg, mant) = match mant.strip_prefix('-') {
        Some(m) => (true, m),
        None => (false, mant),
    };
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let sign = if neg { "-" } else { "" };
    if !(-4..16).contains(&exp) {
        let m = if digits.len() == 1 {
            digits.clone()
        } else {
            format!("{}.{}", &digits[..1], &digits[1..])
        };
        let esign = if exp < 0 { '-' } else { '+' };
        return format!("{sign}{m}e{esign}{:02}", exp.abs());
    }
    let n = digits.len() as i32;
    let body = if exp >= n - 1 {
        // integral: digits followed by zeros, then ".0"
        format!("{digits}{}.0", "0".repeat((exp - (n - 1)) as usize))
    } else if exp >= 0 {
        let cut = (exp + 1) as usize;
        format!("{}.{}", &digits[..cut], &digits[cut..])
    } else {
        format!("0.{}{digits}", "0".repeat((-exp - 1) as usize))
    };
    format!("{sign}{body}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ir::Restriction;

    fn s(v: &str) -> Val {
        Val::Simple(v.into())
    }

    fn m(v: &Val, a: Actual) -> bool {
        matches(v, &a).unwrap_or_else(|e| panic!("{e}"))
    }

    fn r() -> Restriction {
        Restriction::default()
    }

    #[test]
    fn ids_simple_string_exact_case_sensitive() {
        assert!(m(&s("Waldo"), Actual::Str("Waldo".into())));
        assert!(!m(&s("Waldo"), Actual::Str("waldo".into())));
        assert!(!m(&s("Waldo"), Actual::Str("Waldo ".into())));
        assert!(!m(&s("42"), Actual::Str("42.0".into())));
    }

    #[test]
    fn ids_simple_numbers_and_tolerance_table() {
        // tolerance.md table rows (inclusive at the edges).
        let rows: &[(&str, f64, bool)] = &[
            ("0.", 0.000001, true),
            ("0.", 0.0000011, false),
            ("0.", -0.000001, true),
            ("0.", -0.0000011, false),
            ("1.", 1.000002, true),
            ("1.", 1.0000021, false),
            ("1.", 0.999998, true),
            ("1.", 0.9999979, false),
            ("-1.", -1.000002, true),
            ("-1.", -1.0000021, false),
            ("100000.", 100000.100001, true),
            ("100000.", 100000.1000011, false),
            ("-100000.", -99999.899999, true),
            ("-100000.", -99999.8999989, false),
            ("42", 42.0, true),
            ("4.2e1", 42.0, true),
            ("42,3", 42.3, false),
            ("abc", 1.0, false),
        ];
        for (ids, x, want) in rows {
            assert_eq!(m(&s(ids), Actual::Num(*x)), *want, "{ids} vs {x}");
        }
    }

    #[test]
    fn ids_simple_integer_cast_via_float() {
        assert!(m(&s("42"), Actual::Int(42)));
        assert!(m(&s("42.0"), Actual::Int(42)));
        assert!(m(&s("4.2e1"), Actual::Int(42)));
        assert!(!m(&s("42.3"), Actual::Int(42)));
        assert!(!m(&s("42.000001"), Actual::Int(42))); // no tolerance on ints
    }

    #[test]
    fn ids_simple_booleans_lowercase_only() {
        assert!(m(&s("true"), Actual::Bool(true)));
        assert!(m(&s("false"), Actual::Bool(false)));
        assert!(m(&s("1"), Actual::Bool(true)));
        assert!(m(&s("0"), Actual::Bool(false)));
        assert!(!m(&s("TRUE"), Actual::Bool(true)));
        assert!(!m(&s("FALSE"), Actual::Bool(false)));
        assert!(!m(&s("true"), Actual::Bool(false)));
    }

    #[test]
    fn ids_simple_list_any() {
        let l = Actual::List(vec![Actual::Str("A".into()), Actual::Str("B".into())]);
        assert!(m(&s("B"), l.clone()));
        assert!(!m(&s("C"), l));
        let nums = Actual::List(vec![Actual::Num(1.0), Actual::Num(2.0000001)]);
        assert!(m(&s("2"), nums));
    }

    #[test]
    fn ids_restriction_enumeration() {
        let mut e = r();
        e.enumeration = Some(vec!["IFCWALL".into(), "IFCSLAB".into()]);
        let v = Val::Restriction(e);
        assert!(m(&v, Actual::Str("IFCSLAB".into())));
        assert!(!m(&v, Actual::Str("IfcSlab".into())));
        let mut n = r();
        n.base = XsdBase::Double;
        n.enumeration = Some(vec!["25".into(), "30".into()]);
        let n = Val::Restriction(n);
        assert!(m(&n, Actual::Num(30.0000001)));
        assert!(!m(&n, Actual::Num(31.0)));
        assert!(m(&n, Actual::Int(25)));
        let list = Actual::List(vec![Actual::Num(1.0), Actual::Num(25.0)]);
        assert!(m(&n, list));
    }

    #[test]
    fn ids_restriction_patterns_or_and_strings_only() {
        let mut p = r();
        p.patterns = Some(vec!["[A-Z]{2}[0-9]{2}".into(), "[a-z]{2}[0-9]{2}".into()]);
        let v = Val::Restriction(p);
        assert!(m(&v, Actual::Str("AB12".into())));
        assert!(m(&v, Actual::Str("ab12".into())));
        assert!(!m(&v, Actual::Str("aB12".into())));
        let mut any = r();
        any.patterns = Some(vec![".*".into()]);
        let any = Val::Restriction(any);
        assert!(!m(&any, Actual::Num(1.0)));
        assert!(!m(&any, Actual::Int(1)));
        assert!(!m(&any, Actual::Bool(true)));
    }

    #[test]
    fn ids_restriction_bounds_exact_no_tolerance() {
        let mut b = r();
        b.base = XsdBase::Double;
        b.min_exclusive = Some("1.0".into());
        let gt1 = Val::Restriction(b.clone());
        assert!(!m(&gt1, Actual::Num(0.99999999)));
        assert!(!m(&gt1, Actual::Num(1.0)));
        assert!(m(&gt1, Actual::Num(1.00000001)));
        b.min_exclusive = None;
        b.min_inclusive = Some("1.0".into());
        let ge1 = Val::Restriction(b.clone());
        assert!(m(&ge1, Actual::Num(1.0)));
        assert!(!m(&ge1, Actual::Num(0.99999999)));
        b.min_inclusive = None;
        b.max_exclusive = Some("1.0".into());
        let lt1 = Val::Restriction(b.clone());
        assert!(m(&lt1, Actual::Num(0.99999999)));
        assert!(!m(&lt1, Actual::Num(1.0)));
        b.max_exclusive = None;
        b.max_inclusive = Some("10".into());
        b.min_exclusive = Some("3".into());
        let range = Val::Restriction(b);
        assert!(m(&range, Actual::Int(10)));
        assert!(!m(&range, Actual::Int(3)));
        assert!(m(&range, Actual::Str("5".into()))); // float("5")
        assert!(!m(&range, Actual::Str("five".into())));
        assert!(!m(&range, Actual::Bool(true))); // float(True) == 1.0 <= 3
    }

    #[test]
    fn ids_restriction_lengths_python_str() {
        let mut l = r();
        l.length = Some(3);
        let v = Val::Restriction(l);
        assert!(m(&v, Actual::Str("ABC".into())));
        assert!(m(&v, Actual::Str("æøå".into()))); // code points, not bytes
        assert!(!m(&v, Actual::Str("AB".into())));
        assert!(m(&v, Actual::Int(123)));
        assert!(m(&v, Actual::Num(1.5))); // str(1.5) == "1.5"
        assert!(m(&v, Actual::Num(1.0))); // str(1.0) == "1.0"
        assert!(!m(&v, Actual::Num(10.0))); // "10.0"
        let mut mm = r();
        mm.min_length = Some(2);
        mm.max_length = Some(4);
        let v = Val::Restriction(mm);
        assert!(m(&v, Actual::Str("AB".into())));
        assert!(m(&v, Actual::Str("ABCD".into())));
        assert!(!m(&v, Actual::Str("A".into())));
        assert!(!m(&v, Actual::Str("ABCDE".into())));
        assert!(m(&v, Actual::Bool(true))); // "True"
        assert!(!m(&v, Actual::Bool(false))); // "False" = 5
    }

    #[test]
    fn ids_restriction_all_facets_and() {
        let mut c = r();
        c.patterns = Some(vec!["[A-Z]+".into()]);
        c.max_length = Some(3);
        let v = Val::Restriction(c);
        assert!(m(&v, Actual::Str("ABC".into())));
        assert!(!m(&v, Actual::Str("ABCD".into())));
        assert!(!m(&v, Actual::Str("abc".into())));
    }

    #[test]
    fn ids_restriction_compile_errors_are_loud() {
        let mut bad = r();
        bad.base = XsdBase::Double;
        bad.min_inclusive = Some("abc".into());
        assert!(matches!(
            matches(&Val::Restriction(bad), &Actual::Num(1.0)),
            Err(IdsError::InvalidIds { .. })
        ));
        let mut date = r();
        date.base = XsdBase::Date;
        date.min_inclusive = Some("2020-01-01".into());
        assert!(matches!(
            matches(&Val::Restriction(date), &Actual::Str("2021-01-01".into())),
            Err(IdsError::Unsupported { .. })
        ));
        let mut pat = r();
        pat.patterns = Some(vec!["a*?".into()]);
        assert!(matches!(
            matches(&Val::Restriction(pat), &Actual::Str("a".into())),
            Err(IdsError::InvalidIds { .. })
        ));
    }

    #[test]
    fn ids_py_float_repr_matches_python() {
        let rows: &[(f64, &str)] = &[
            (1.0, "1.0"),
            (1.5, "1.5"),
            (42.0, "42.0"),
            (0.1, "0.1"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e16, "1e+16"),
            (1234567890123456.0, "1234567890123456.0"),
            (-2.5e-7, "-2.5e-07"),
            (123.456, "123.456"),
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1e22, "1e+22"),
            (1.2345e20, "1.2345e+20"),
        ];
        for (x, want) in rows {
            assert_eq!(py_float_repr(*x), *want, "{x}");
        }
    }

    #[test]
    fn ids_py_float_parsing() {
        assert_eq!(py_float("0."), Some(0.0));
        assert_eq!(py_float(" 42 "), Some(42.0));
        assert_eq!(py_float(".5"), Some(0.5));
        assert_eq!(py_float("1e3"), Some(1000.0));
        assert_eq!(py_float("42,3"), None);
        assert_eq!(py_float("1_000"), None);
        assert_eq!(py_float(""), None);
        assert!(py_float("INF").is_some_and(|v| v.is_infinite()));
    }
}
