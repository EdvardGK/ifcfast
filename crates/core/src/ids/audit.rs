//! Schema-free semantic audit of a parsed IDS.
//!
//! None of the 27 `invalid-*` cases of the buildingSMART conformance
//! suite (pinned a67047736aa93586d723329fce3aab1b9ac056af) fails the
//! XSD: they are audit violations. The rules below need no IFC schema
//! and are always applied by [`crate::ids::parse_ids`]; rules that need
//! the schema (unknown entity/attribute names, derived/inverse
//! attributes, attribute value types, predefined-type enumerations)
//! belong to `compile`.
//!
//! | Rule | Source | Pinned by |
//! |---|---|---|
//! | literal lexical form per base (DataTypes.md "XML base types" regex column) | DataTypes.md | `property/invalid-only_specifically_formatted_numbers_are_allowed_{1,2}_4`, `property/invalid-integer_values_*`, `property/invalid-booleans_must_be_specified_as_lowercase_strings_3_3` |
//! | facets allowed per base (DataTypes.md base-type table) | DataTypes.md | — |
//! | property `dataType` fixes the restriction base (DataTypes.md "Restriction base type") | DataTypes.md | — |
//! | entity names are uppercase STEP tokens | UserManual/entity-facet.md | `entity/invalid-entities_must_be_specified_as_uppercase_strings` |
//! | a requirement entity must be able to match the applicability entity | XSD annotation on `requirements/entity` | `entity/invalid-an_entity_not_matching_the_specified_class_should_fail`, `…as_a_xsd_regex_pattern_1_2`, `…as_an_enumeration_3_3`, `…invalid_entities_always_fail`, `…subclasses_are_not_considered_as_matching` |
//! | prohibited specifications carry no requirements (enforced in `xml.rs`) | UserManual/specifications.md | `ids/invalid-prohibited_specifications_invalid_if_requirements_are_specified` |

use std::sync::OnceLock;

use regex::Regex;

use super::datatypes::datatype_base;
use super::ir::{EntityFacet, Facet, Requirement, Val, XsdBase};
use super::restriction::{Actual, CompiledVal};
use super::IdsError;

/// Is `v` a valid literal of `base`? Implements the "Value string regex
/// constraint" column of DataTypes.md ("XML base types"), with two
/// tightenings: a double needs at least one digit (the doc regex also
/// admits `""` and `"."`), and unsigned `INF` is accepted as in XSD.
/// Surrounding XML whitespace is ignored (XSD whiteSpace=collapse for
/// every non-string base).
///
/// Numbers use a dot as decimal separator and no thousands separator:
/// `42,3` and `123,4.5` are invalid
/// (`attribute|property/invalid-only_specifically_formatted_numbers_are_allowed_{1,2}_4.ids`),
/// `1.2345E3`, `42.`, `0.` are valid (`…_{3,4}_4.ids`, tolerance cases).
/// Integers carry no decimal point or exponent: `42.`, `42.0`, `42.3`
/// are invalid for xs:integer (`property/invalid-integer_values_*`).
/// Booleans are `true|false|0|1`: `FALSE` is invalid
/// (`property/invalid-booleans_must_be_specified_as_lowercase_strings_3_3.ids`).
pub fn lexical_ok(base: XsdBase, v: &str) -> bool {
    let t = v.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r'));
    match base {
        XsdBase::String => true,
        XsdBase::Boolean => matches!(t, "true" | "false" | "0" | "1"),
        XsdBase::Integer => {
            let d = t.strip_prefix(['+', '-']).unwrap_or(t);
            !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
        }
        XsdBase::Double => {
            if matches!(t, "INF" | "+INF" | "-INF" | "NaN") {
                return true;
            }
            let (m, e) = match t.find(['e', 'E']) {
                Some(i) => (&t[..i], Some(&t[i + 1..])),
                None => (t, None),
            };
            let exp_ok = match e {
                None => true,
                Some(e) => {
                    let d = e.strip_prefix(['+', '-']).unwrap_or(e);
                    !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit())
                }
            };
            is_decimal(m) && exp_ok
        }
        XsdBase::Date | XsdBase::DateTime | XsdBase::Time | XsdBase::Duration => {
            temporal_regex(base).is_match(t)
        }
    }
}

fn is_decimal(t: &str) -> bool {
    let d = t.strip_prefix(['+', '-']).unwrap_or(t);
    let (int, frac) = match d.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (d, None),
    };
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    match frac {
        None => !int.is_empty() && digits(int),
        Some(f) => (!int.is_empty() || !f.is_empty()) && digits(int) && digits(f),
    }
}

fn temporal_regex(base: XsdBase) -> &'static Regex {
    static DATE: OnceLock<Regex> = OnceLock::new();
    static DATETIME: OnceLock<Regex> = OnceLock::new();
    static TIME: OnceLock<Regex> = OnceLock::new();
    static DURATION: OnceLock<Regex> = OnceLock::new();
    const TZ: &str = r"(Z|([+-][0-9]{2}:[0-9]{2}))?";
    // Patterns are constants; a failure here is a programming error
    // caught by the unit tests, not a user-data condition.
    let build = |p: String| Regex::new(&p).expect("constant temporal regex compiles");
    match base {
        XsdBase::Date => DATE.get_or_init(|| build(format!(r"^[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}{TZ}$"))),
        XsdBase::DateTime => DATETIME.get_or_init(|| {
            build(format!(
                r"^[0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}}T[0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}(\.[0-9]+)?{TZ}$"
            ))
        }),
        XsdBase::Time => TIME.get_or_init(|| build(format!(r"^[0-9]{{2}}:[0-9]{{2}}:[0-9]{{2}}(\.[0-9]+)?{TZ}$"))),
        _ => DURATION.get_or_init(|| {
            build(r"^[-+]?P([0-9]+Y)?([0-9]+M)?([0-9]+D)?(T([0-9]+H)?([0-9]+M)?([0-9]+S)?)?$".to_string())
        }),
    }
}

/// Is restriction facet `local` (`enumeration`, `pattern`, `minInclusive`,
/// `length`, …) allowed on `base`? DataTypes.md base-type table:
/// string → pattern, enumeration, lengths; boolean → pattern only;
/// every other base → pattern, enumeration, bounds.
pub fn facet_allowed(base: XsdBase, local: &str) -> bool {
    let bound = matches!(local, "minInclusive" | "minExclusive" | "maxInclusive" | "maxExclusive");
    let length = matches!(local, "length" | "minLength" | "maxLength");
    match (base, local) {
        (_, "pattern") => true,
        (XsdBase::Boolean, _) => false,
        (_, "enumeration") => true,
        (XsdBase::String, _) => length,
        _ => bound,
    }
}

/// Facet-level rules. Returns the violation message.
pub fn audit_facet(f: &Facet) -> Result<(), String> {
    match f {
        Facet::Entity(e) => entity_name_uppercase(e),
        Facet::PartOf { entity, .. } => entity_name_uppercase(entity),
        Facet::Property {
            data_type: Some(dt),
            value: Some(v),
            ..
        } => match datatype_base(dt) {
            Some(Some(base)) => match v {
                Val::Simple(s) => {
                    if lexical_ok(base, s) {
                        Ok(())
                    } else {
                        Err(format!(
                            "property value '{s}' is not a valid {dt} literal (dataType {dt} requires xs:{})",
                            base.local_name()
                        ))
                    }
                }
                Val::Restriction(r) => {
                    if r.base == base {
                        Ok(())
                    } else {
                        Err(format!(
                            "restriction base xs:{} does not match dataType {dt}, which requires xs:{}",
                            r.base.local_name(),
                            base.local_name()
                        ))
                    }
                }
            },
            // IFCBINARY (no base) or a name DataTypes.md does not list:
            // `compile` validates the name against the schema.
            _ => Ok(()),
        },
        _ => Ok(()),
    }
}

/// Entity names are uppercase STEP tokens (`IFCWALL`, never `IfcWall`).
/// Checked on simple values and enumeration literals; patterns are the
/// author's business.
fn entity_name_uppercase(e: &EntityFacet) -> Result<(), String> {
    let bad = |s: &str| s.chars().any(char::is_lowercase);
    match &e.name {
        Val::Simple(s) if bad(s) => Err(format!(
            "entity name '{s}' must be an uppercase IFC class name (e.g. '{}')",
            s.to_uppercase()
        )),
        Val::Restriction(r) => match r.enumeration.as_ref().and_then(|en| en.iter().find(|s| bad(s))) {
            Some(s) => Err(format!(
                "entity name enumeration value '{s}' must be an uppercase IFC class name"
            )),
            None => Ok(()),
        },
        _ => Ok(()),
    }
}

/// When the applicability names a single entity class, every requirement
/// entity facet must be able to match it (XSD annotation on
/// `requirements/entity`: "Make sure 'Name' value of requirements entity
/// is the same as the 'applicability' node, or a wildcard"). Returns the
/// offending requirement's index and the message.
pub fn audit_requirement_entities(
    applicability: &[Facet],
    requirements: &[Requirement],
) -> Result<(), (usize, String)> {
    let Some(app_name) = applicability.iter().find_map(|f| match f {
        Facet::Entity(EntityFacet {
            name: Val::Simple(n),
            ..
        }) => Some(n.as_str()),
        _ => None,
    }) else {
        return Ok(());
    };
    for (i, r) in requirements.iter().enumerate() {
        let Facet::Entity(e) = &r.facet else { continue };
        let cv = match CompiledVal::new(&e.name) {
            Ok(cv) => cv,
            // Untranslatable pattern: `compile` reports it with the spec index.
            Err(IdsError::Unsupported { .. }) => continue,
            Err(other) => return Err((i, other.to_string())),
        };
        if !cv.matches(&Actual::Str(app_name.to_string())) {
            return Err((
                i,
                format!(
                    "requirement entity {} can never match the applicability entity '{app_name}' \
                     (a requirement entity must equal the applicability entity or be a pattern/enumeration that includes it)",
                    describe_val(&e.name)
                ),
            ));
        }
    }
    Ok(())
}

fn describe_val(v: &Val) -> String {
    match v {
        Val::Simple(s) => format!("'{s}'"),
        Val::Restriction(r) => {
            let mut parts = Vec::new();
            if let Some(en) = &r.enumeration {
                parts.push(format!("enumeration {en:?}"));
            }
            if let Some(ps) = &r.patterns {
                parts.push(format!("pattern {ps:?}"));
            }
            format!("restriction ({})", parts.join(", "))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::ir::{FacetCardinality, Restriction};

    #[test]
    fn ids_audit_lexical_table() {
        let rows: &[(XsdBase, &str, bool)] = &[
            (XsdBase::Double, "42", true),
            (XsdBase::Double, "42.", true),
            (XsdBase::Double, "0.", true),
            (XsdBase::Double, ".5", true),
            (XsdBase::Double, "-0.0000001", true),
            (XsdBase::Double, "1.2345E3", true),
            (XsdBase::Double, "1.2345e3", true),
            (XsdBase::Double, "-INF", true),
            (XsdBase::Double, "42,3", false),
            (XsdBase::Double, "123,4.5", false),
            (XsdBase::Double, "", false),
            (XsdBase::Double, ".", false),
            (XsdBase::Double, "1e", false),
            (XsdBase::Double, "inf", false),
            (XsdBase::Integer, "42", true),
            (XsdBase::Integer, "-7", true),
            (XsdBase::Integer, "42.", false),
            (XsdBase::Integer, "42.0", false),
            (XsdBase::Integer, "42.3", false),
            (XsdBase::Integer, "4e1", false),
            (XsdBase::Boolean, "true", true),
            (XsdBase::Boolean, "false", true),
            (XsdBase::Boolean, "0", true),
            (XsdBase::Boolean, "1", true),
            (XsdBase::Boolean, "FALSE", false),
            (XsdBase::Boolean, "True", false),
            (XsdBase::Date, "2022-01-01", true),
            (XsdBase::Date, "2022-01-01Z", true),
            (XsdBase::Date, "2022-01-01+01:00", true),
            (XsdBase::Date, "01.01.2022", false),
            (XsdBase::DateTime, "2022-01-01T12:00:00.5Z", true),
            (XsdBase::DateTime, "2022-01-01 12:00:00", false),
            (XsdBase::Time, "12:00:00", true),
            (XsdBase::Time, "12:00", false),
            (XsdBase::Duration, "PT16H", true),
            (XsdBase::Duration, "P1Y2M3DT4H5M6S", true),
            (XsdBase::Duration, "16H", false),
            (XsdBase::String, "anything, really", true),
        ];
        for (b, v, want) in rows {
            assert_eq!(lexical_ok(*b, v), *want, "{b:?} {v:?}");
        }
    }

    #[test]
    fn ids_audit_facets_allowed_per_base() {
        assert!(facet_allowed(XsdBase::String, "length"));
        assert!(!facet_allowed(XsdBase::String, "minInclusive"));
        assert!(facet_allowed(XsdBase::Double, "minInclusive"));
        assert!(!facet_allowed(XsdBase::Double, "maxLength"));
        assert!(facet_allowed(XsdBase::Double, "pattern"));
        assert!(facet_allowed(XsdBase::Boolean, "pattern"));
        assert!(!facet_allowed(XsdBase::Boolean, "enumeration"));
        assert!(facet_allowed(XsdBase::Date, "maxExclusive"));
    }

    fn prop(dt: &str, v: Val) -> Facet {
        Facet::Property {
            property_set: Val::Simple("Foo_Bar".into()),
            base_name: Val::Simple("Foo".into()),
            value: Some(v),
            data_type: Some(dt.into()),
            uri: None,
        }
    }

    #[test]
    fn ids_audit_property_values_follow_datatype() {
        assert!(audit_facet(&prop("IFCINTEGER", Val::Simple("42".into()))).is_ok());
        assert!(audit_facet(&prop("IFCINTEGER", Val::Simple("42.0".into()))).is_err());
        assert!(audit_facet(&prop("IFCCOUNTMEASURE", Val::Simple("3.5".into()))).is_err());
        assert!(audit_facet(&prop("IFCREAL", Val::Simple("42,3".into()))).is_err());
        assert!(audit_facet(&prop("IFCBOOLEAN", Val::Simple("FALSE".into()))).is_err());
        assert!(audit_facet(&prop("IFCBOOLEAN", Val::Simple("false".into()))).is_ok());
        assert!(audit_facet(&prop("IFCLABEL", Val::Simple("42,3".into()))).is_ok());
        assert!(audit_facet(&prop("IFCUNKNOWNTHING", Val::Simple("x".into()))).is_ok());
        let dbl = Restriction {
            base: XsdBase::Double,
            min_inclusive: Some("0".into()),
            ..Restriction::default()
        };
        assert!(audit_facet(&prop("IFCREAL", Val::Restriction(dbl.clone()))).is_ok());
        assert!(audit_facet(&prop("IFCLABEL", Val::Restriction(dbl))).is_err());
    }

    fn ent(name: Val) -> Facet {
        Facet::Entity(EntityFacet {
            name,
            predefined_type: None,
        })
    }

    fn req(f: Facet) -> Requirement {
        Requirement {
            facet: f,
            cardinality: FacetCardinality::Required,
            instructions: None,
        }
    }

    #[test]
    fn ids_audit_entity_names_uppercase() {
        assert!(audit_facet(&ent(Val::Simple("IFCWALL".into()))).is_ok());
        assert!(audit_facet(&ent(Val::Simple("IfcWall".into()))).is_err());
        let en = Restriction {
            enumeration: Some(vec!["IFCWALL".into(), "IfcSlab".into()]),
            ..Restriction::default()
        };
        assert!(audit_facet(&ent(Val::Restriction(en))).is_err());
    }

    #[test]
    fn ids_audit_requirement_entity_must_match_applicability() {
        let app = vec![ent(Val::Simple("IFCWALL".into()))];
        let same = vec![req(ent(Val::Simple("IFCWALL".into())))];
        assert!(audit_requirement_entities(&app, &same).is_ok());
        let other = vec![req(ent(Val::Simple("IFCSLAB".into())))];
        assert_eq!(audit_requirement_entities(&app, &other).map_err(|e| e.0), Err(0));
        let pat = Restriction {
            patterns: Some(vec!["IFC.*TYPE".into()]),
            ..Restriction::default()
        };
        assert!(audit_requirement_entities(&app, &[req(ent(Val::Restriction(pat.clone())))]).is_err());
        let app_type = vec![ent(Val::Simple("IFCWALLTYPE".into()))];
        assert!(audit_requirement_entities(&app_type, &[req(ent(Val::Restriction(pat)))]).is_ok());
        // Applicability without a simple entity: nothing to check.
        assert!(audit_requirement_entities(&[], &other).is_ok());
    }
}
