//! Value matching for IDS constraints (aligned with IfcTester / XSD restrictions).

use regex::Regex;

use super::compiled::ValueConstraint;

/// IfcTester-style coercion for IDS simple values vs IFC attribute/property values.
pub fn cast_match(actual: &str, required: &str) -> bool {
    if actual == required {
        return true;
    }
    let req_lower = required.to_ascii_lowercase();
    if req_lower == "true" && (actual == "True" || actual == "T" || actual == "1") {
        return true;
    }
    if req_lower == "false" && (actual == "False" || actual == "F" || actual == "0") {
        return true;
    }
    if let (Ok(a), Ok(b)) = (actual.parse::<f64>(), required.parse::<f64>()) {
        if a.is_finite() && b.is_finite() {
            return float_eq_ifctester(a, b);
        }
    }
    false
}

/// IfcTester `facet.is_x` — relative tolerance for float comparisons (bug 4716).
pub fn float_eq_ifctester(value: f64, cast_value: f64) -> bool {
    if cast_value >= 0.0 {
        value >= cast_value * (1.0 - 1e-6) && value <= cast_value * (1.0 + 1e-6)
    } else {
        value <= cast_value * (1.0 - 1e-6) && value >= cast_value * (1.0 + 1e-6)
    }
}

pub fn value_matches(actual: Option<&str>, constraint: Option<&ValueConstraint>) -> bool {
    let Some(c) = constraint else {
        return true;
    };
    let Some(actual) = actual else {
        return false;
    };
    if actual.is_empty() || actual == "UNKNOWN" {
        return false;
    }
    match c {
        ValueConstraint::Simple { text } => {
            if actual == text.as_str() {
                true
            } else if let (Ok(a), Ok(b)) = (actual.parse::<f64>(), text.parse::<f64>()) {
                a.is_finite() && b.is_finite() && float_eq_ifctester(a, b)
            } else {
                cast_match(actual, text.as_str())
            }
        }
        ValueConstraint::Enumeration { values } => enumeration_matches(actual, values),
        ValueConstraint::Pattern { patterns } => patterns
            .iter()
            .all(|p| pattern_fullmatch(actual, p)),
        ValueConstraint::Bounds {
            min_inclusive,
            max_inclusive,
            min_exclusive,
            max_exclusive,
        } => bounds_match(actual, *min_inclusive, *max_inclusive, *min_exclusive, *max_exclusive),
        ValueConstraint::Length { length } => actual.len() == *length,
        ValueConstraint::MinLength { min } => actual.len() >= *min,
        ValueConstraint::MaxLength { max } => actual.len() <= *max,
        ValueConstraint::LengthBounds { length, min, max } => {
            let n = actual.len();
            if let Some(exact) = length {
                if n != *exact {
                    return false;
                }
            }
            if let Some(mn) = min {
                if n < *mn {
                    return false;
                }
            }
            if let Some(mx) = max {
                if n > *mx {
                    return false;
                }
            }
            true
        }
    }
}

fn enumeration_matches(actual: &str, values: &[String]) -> bool {
    for v in values {
        if actual == v.as_str() {
            return true;
        }
        if let (Ok(a), Ok(b)) = (actual.parse::<f64>(), v.parse::<f64>()) {
            if a.is_finite() && b.is_finite() && float_eq_ifctester(a, b) {
                return true;
            }
        }
    }
    false
}

fn pattern_fullmatch(actual: &str, pattern: &str) -> bool {
    let Ok(re) = Regex::new(pattern) else {
        return false;
    };
    re.find(actual).is_some_and(|m| m.start() == 0 && m.end() == actual.len())
}

fn bounds_match(
    actual: &str,
    min_inclusive: Option<f64>,
    max_inclusive: Option<f64>,
    min_exclusive: Option<f64>,
    max_exclusive: Option<f64>,
) -> bool {
    let Ok(n) = actual.parse::<f64>() else {
        return false;
    };
    if let Some(v) = min_inclusive {
        if n < v {
            return false;
        }
    }
    if let Some(v) = max_inclusive {
        if n > v {
            return false;
        }
    }
    if let Some(v) = min_exclusive {
        if n <= v {
            return false;
        }
    }
    if let Some(v) = max_exclusive {
        if n >= v {
            return false;
        }
    }
    true
}

/// IDS `dataType` vs IFC nominal type (IfcTester compares case-insensitively).
pub fn ids_datatype_matches(value_type: Option<&str>, ids_datatype: &str) -> bool {
    let Some(vt) = value_type else {
        return false;
    };
    let normalized = normalize_ids_datatype(vt);
    let want = normalize_ids_datatype(ids_datatype);
    normalized == want
}

pub fn normalize_ids_datatype(dt: &str) -> String {
    let u = dt.to_uppercase();
    if let Some(rest) = u.strip_prefix("IFC") {
        format!("IFC{rest}")
    } else {
        u
    }
}
