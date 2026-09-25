//! Unit resolution: every `IfcUnitAssignment` entry to an SI factor.
//!
//! The indexer only ever needed the length unit (`unit_scale`, metres per
//! file unit). IDS property checks (GH #192, design §2.5) need every unit
//! type: an `IfcPressureMeasure` authored in kPa, an area in mm², a mass in
//! tonnes. [`UnitTable`] resolves them all, from one walk over the unit
//! entities:
//!
//! | Unit entity                          | SI factor                                         |
//! |--------------------------------------|---------------------------------------------------|
//! | `IfcSIUnit`                          | prefix^dim × base (`SQUARE_METRE` dim 2, `CUBIC_METRE` dim 3, `GRAM` base 1e-3 kg) |
//! | `IfcConversionBasedUnit`             | `ConversionFactor` → `IfcMeasureWithUnit` value × factor of its `UnitComponent` (recursive) |
//! | `IfcConversionBasedUnitWithOffset`   | as above, plus `ConversionOffset` (see [`ResolvedUnit`]) |
//! | `IfcDerivedUnit`                     | Π factor(element unit) ^ exponent                  |
//! | `IfcMonetaryUnit`                    | no scale ([`UnitError::NoScale`])                  |
//! | `IfcContextDependentUnit`            | no scale ([`UnitError::NoScale`])                  |
//!
//! **Never assume SI** (GH #149, indexer.rs): anything that does not
//! resolve is an `Err` / `None`, never `1.0`. The IDS side turns that into
//! `IdsError::UnresolvedUnit`.
//!
//! Which assignment counts: the first `IfcUnitAssignment` in file order
//! whose `Units` list is non-empty. That is the rule the indexer has always
//! used for `unit_scale`, and what `ifcopenshell.util.unit` reads in
//! practice (`IfcProject.UnitsInContext`, which is that same entity in
//! every file seen so far). Within it, the first entry of a unit type that
//! resolves wins; a second entry of the same type is an IFC rule violation
//! (IfcUnitAssignment WR01) and is not consulted.
//!
//! The indexer's `unit_scale` is computed through [`UnitTable::length_scale`],
//! which keeps the pre-UnitTable resolution rules for LENGTHUNIT (SI units
//! and single-level conversion-based units with an SI length base) and the
//! exact same warnings, so `unit_scale` did not move by a bit when it was
//! routed through here. The general resolver is strictly more capable
//! (nested conversion bases, offsets); where the two could differ is listed
//! on [`UnitTable::length_scale`].

use std::collections::HashMap;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, parse_ref_list, split_top_level_args, Field};

/// Nesting cap for conversion-based / derived unit chains. Real files use
/// one or two levels; a cycle stops here with [`UnitError::Cycle`].
const MAX_UNIT_DEPTH: usize = 8;

/// Why a unit did not resolve to an SI factor. Every variant names the
/// STEP id involved so a caller's error message can point at the file.
#[derive(Debug, Clone, PartialEq)]
pub enum UnitError {
    /// The assignment has no entry of this unit type.
    NotDeclared { unit_type: String },
    /// A ref points at nothing, or at an entity that is not a unit.
    Dangling { step: u64 },
    /// `IfcSIUnit` with an `IfcSIPrefix` we do not know.
    UnknownPrefix { step: u64, prefix: String },
    /// `IfcSIUnit` with an `IfcSIUnitName` we do not know.
    UnknownSiName { step: u64, name: String },
    /// A unit whose declared UnitType disagrees with what it measures
    /// (`LENGTHUNIT` named `RADIAN`, a conversion-based LENGTHUNIT whose
    /// factor is in square metres).
    Inconsistent {
        step: u64,
        unit_type: String,
        detail: String,
    },
    /// A conversion-based unit whose `ConversionFactor` chain is missing
    /// or malformed, or a derived-unit element without unit / exponent.
    Malformed { step: u64, detail: String },
    /// `IfcMonetaryUnit` / `IfcContextDependentUnit`: a unit with no SI
    /// factor by definition.
    NoScale { step: u64, entity: &'static str },
    /// An offset unit (°C, `IfcConversionBasedUnitWithOffset`) used where
    /// only a pure factor makes sense (inside an `IfcDerivedUnit`).
    OffsetInDerived { step: u64 },
    /// Chain deeper than [`MAX_UNIT_DEPTH`] — almost certainly a cycle.
    Cycle { step: u64 },
}

impl std::fmt::Display for UnitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnitError::NotDeclared { unit_type } => {
                write!(f, "the IfcUnitAssignment declares no {unit_type}")
            }
            UnitError::Dangling { step } => write!(f, "#{step} is not a unit"),
            UnitError::UnknownPrefix { step, prefix } => {
                write!(f, "IfcSIUnit #{step}: unknown IfcSIPrefix {prefix:?}")
            }
            UnitError::UnknownSiName { step, name } => {
                write!(f, "IfcSIUnit #{step}: unknown IfcSIUnitName {name:?}")
            }
            UnitError::Inconsistent {
                step,
                unit_type,
                detail,
            } => write!(f, "unit #{step} ({unit_type}): {detail}"),
            UnitError::Malformed { step, detail } => write!(f, "unit #{step}: {detail}"),
            UnitError::NoScale { step, entity } => {
                write!(f, "{entity} #{step} has no SI conversion factor")
            }
            UnitError::OffsetInDerived { step } => write!(
                f,
                "unit #{step} carries an offset and cannot be an IfcDerivedUnit element"
            ),
            UnitError::Cycle { step } => {
                write!(f, "unit chain through #{step} is cyclic or too deep")
            }
        }
    }
}

impl std::error::Error for UnitError {}

/// A unit resolved to SI: `si_value = file_value * scale + offset`.
///
/// `offset` is non-zero only for temperature-like units (`DEGREE_CELSIUS`,
/// `IfcConversionBasedUnitWithOffset`). The `scale_for_*` helpers refuse
/// such units (they return `None`), because a bare factor would silently
/// drop the offset; call the `resolve_*` methods to handle it explicitly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedUnit {
    pub scale: f64,
    pub offset: f64,
}

/// One unit-related entity, parsed. Strings are kept exactly as written
/// (no case folding): the SI name / prefix match is case-sensitive, the
/// same as the indexer's length resolution has always been.
#[derive(Debug, Clone)]
enum UnitRecord {
    /// `IfcSIUnit(Dimensions, UnitType, Prefix, Name)`.
    Si {
        unit_type: String,
        prefix: String,
        name: String,
    },
    /// `IfcConversionBasedUnit(Dimensions, UnitType, Name, ConversionFactor)`
    /// and `…WithOffset(…, ConversionOffset)`.
    Conversion {
        unit_type: String,
        name: String,
        factor: Option<u64>,
        /// `Some` only for `IfcConversionBasedUnitWithOffset`.
        offset: Option<f64>,
    },
    /// `IfcDerivedUnit(Elements, UnitType, UserDefinedType[, Name])`.
    Derived {
        unit_type: String,
        elements: Vec<u64>,
    },
    /// `IfcDerivedUnitElement(Unit, Exponent)`.
    DerivedElement {
        unit: Option<u64>,
        exponent: Option<i32>,
    },
    /// `IfcMonetaryUnit(Currency)`.
    Monetary,
    /// `IfcContextDependentUnit(Dimensions, UnitType, Name)`.
    ContextDependent { unit_type: String },
    /// `IfcMeasureWithUnit(ValueComponent, UnitComponent)`.
    Measure {
        value: Option<f64>,
        unit: Option<u64>,
    },
}

/// Streaming collector: feed it `(step, type, fields)` for every entity
/// (non-unit entities are ignored) and call [`UnitCollector::finish`].
/// Lets the indexer's single streaming pass build a [`UnitTable`] without
/// an [`EntityTable`].
#[derive(Debug, Default)]
pub(crate) struct UnitCollector {
    records: HashMap<u64, UnitRecord>,
    assigned: Vec<u64>,
}

impl UnitCollector {
    /// `fields` are the entity's top-level args (`split_top_level_args`).
    /// Returns true when the entity was a unit-related one.
    pub(crate) fn feed(&mut self, step: u64, type_name: &[u8], fields: &[&[u8]]) -> bool {
        let t = type_name;
        if t.eq_ignore_ascii_case(b"IFCSIUNIT") {
            self.records.insert(
                step,
                UnitRecord::Si {
                    unit_type: enum_at(fields, 1).unwrap_or_default(),
                    prefix: enum_at(fields, 2).unwrap_or_default(),
                    name: enum_at(fields, 3).unwrap_or_default(),
                },
            );
        } else if t.eq_ignore_ascii_case(b"IFCCONVERSIONBASEDUNIT") {
            self.records.insert(
                step,
                UnitRecord::Conversion {
                    unit_type: enum_at(fields, 1).unwrap_or_default(),
                    name: string_at(fields, 2).unwrap_or_default(),
                    factor: ref_at(fields, 3),
                    offset: None,
                },
            );
        } else if t.eq_ignore_ascii_case(b"IFCCONVERSIONBASEDUNITWITHOFFSET") {
            self.records.insert(
                step,
                UnitRecord::Conversion {
                    unit_type: enum_at(fields, 1).unwrap_or_default(),
                    name: string_at(fields, 2).unwrap_or_default(),
                    factor: ref_at(fields, 3),
                    // A `$` offset on a WithOffset unit is malformed; keep
                    // it as NaN so resolution fails instead of reading 0.
                    offset: Some(
                        fields
                            .get(4)
                            .and_then(|f| measure_number(f))
                            .unwrap_or(f64::NAN),
                    ),
                },
            );
        } else if t.eq_ignore_ascii_case(b"IFCDERIVEDUNIT") {
            let elements = match fields.first().map(|f| parse_field(f)) {
                Some(Field::List(body)) => parse_ref_list(body),
                _ => Vec::new(),
            };
            self.records.insert(
                step,
                UnitRecord::Derived {
                    unit_type: enum_at(fields, 1).unwrap_or_default(),
                    elements,
                },
            );
        } else if t.eq_ignore_ascii_case(b"IFCDERIVEDUNITELEMENT") {
            let exponent = match fields.get(1).map(|f| parse_field(f)) {
                Some(Field::Number(n)) if n.fract() == 0.0 && n.abs() < 1e6 => Some(n as i32),
                _ => None,
            };
            self.records.insert(
                step,
                UnitRecord::DerivedElement {
                    unit: ref_at(fields, 0),
                    exponent,
                },
            );
        } else if t.eq_ignore_ascii_case(b"IFCMONETARYUNIT") {
            self.records.insert(step, UnitRecord::Monetary);
        } else if t.eq_ignore_ascii_case(b"IFCCONTEXTDEPENDENTUNIT") {
            self.records.insert(
                step,
                UnitRecord::ContextDependent {
                    unit_type: enum_at(fields, 1).unwrap_or_default(),
                },
            );
        } else if t.eq_ignore_ascii_case(b"IFCMEASUREWITHUNIT") {
            self.records.insert(
                step,
                UnitRecord::Measure {
                    value: fields.first().and_then(|f| measure_number(f)),
                    unit: ref_at(fields, 1),
                },
            );
        } else if t.eq_ignore_ascii_case(b"IFCUNITASSIGNMENT") {
            // First assignment with a non-empty Units list wins — the
            // indexer's rule since GH #73.
            if self.assigned.is_empty() {
                if let Some(Field::List(body)) = fields.first().map(|f| parse_field(f)) {
                    self.assigned = parse_ref_list(body);
                }
            }
        } else {
            return false;
        }
        true
    }

    pub(crate) fn finish(self) -> UnitTable {
        UnitTable {
            records: self.records,
            assigned: self.assigned,
        }
    }
}

/// Every unit entity in a file plus the project's unit assignment,
/// resolvable to SI factors. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct UnitTable {
    records: HashMap<u64, UnitRecord>,
    assigned: Vec<u64>,
}

impl UnitTable {
    /// One walk over the entity table. Never fails as a whole: a unit that
    /// cannot be resolved is reported per unit by `resolve_*`, so one bad
    /// declaration (a typo'd prefix on the THERMODYNAMICTEMPERATUREUNIT)
    /// does not take the length unit down with it.
    pub fn from_table(table: &EntityTable) -> UnitTable {
        let mut c = UnitCollector::default();
        for (step, type_name, args) in table.iter() {
            if is_unit_entity(type_name) {
                let fields = split_top_level_args(args);
                c.feed(step, type_name, &fields);
            }
        }
        c.finish()
    }

    /// The `Units` refs of the assignment in use, in file order.
    pub fn assigned_units(&self) -> &[u64] {
        &self.assigned
    }

    /// The UnitType of a unit entity, uppercase-as-written
    /// (`LENGTHUNIT`, `THERMALTRANSMITTANCEUNIT`). `None` for monetary
    /// units, non-units and dangling refs.
    pub fn unit_type_of(&self, step: u64) -> Option<&str> {
        match self.records.get(&step)? {
            UnitRecord::Si { unit_type, .. }
            | UnitRecord::Conversion { unit_type, .. }
            | UnitRecord::Derived { unit_type, .. }
            | UnitRecord::ContextDependent { unit_type } => Some(unit_type.as_str()),
            _ => None,
        }
    }

    /// Resolve any unit entity (a property's own `Unit`, an assignment
    /// entry, a conversion base) to SI.
    pub fn resolve_unit_step(&self, step: u64) -> Result<ResolvedUnit, UnitError> {
        self.resolve(step, 0)
    }

    /// Resolve the project unit of `unit_type` (`"LENGTHUNIT"`,
    /// `"PRESSUREUNIT"`, …; compared case-insensitively). The first entry
    /// of that type in the assignment that resolves wins; if none
    /// resolves, the first entry's error is returned.
    pub fn resolve_unit_type(&self, unit_type: &str) -> Result<ResolvedUnit, UnitError> {
        let mut first_err: Option<UnitError> = None;
        for &step in &self.assigned {
            let matches = self
                .unit_type_of(step)
                .is_some_and(|t| t.eq_ignore_ascii_case(unit_type));
            if !matches {
                continue;
            }
            match self.resolve(step, 0) {
                Ok(r) => return Ok(r),
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        Err(first_err.unwrap_or_else(|| UnitError::NotDeclared {
            unit_type: unit_type.to_ascii_uppercase(),
        }))
    }

    /// SI factor of the project unit of `unit_type`. `None` when it is not
    /// declared, does not resolve, or carries an offset (use
    /// [`UnitTable::resolve_unit_type`] for those).
    pub fn scale_for_unit_type(&self, unit_type: &str) -> Option<f64> {
        pure_scale(self.resolve_unit_type(unit_type))
    }

    /// SI factor of a specific unit entity (e.g. an
    /// `IfcPropertySingleValue.Unit`). Same `None` rules as
    /// [`UnitTable::scale_for_unit_type`].
    pub fn scale_for_unit_step(&self, step: u64) -> Option<f64> {
        pure_scale(self.resolve_unit_step(step))
    }

    /// SI factor for a value of IFC measure type `measure` (e.g.
    /// `"IFCLENGTHMEASURE"`, any case) in the project's units:
    ///
    /// - measure unknown to the schema → `None`;
    /// - measure with no unit type (`IFCLABEL`, `IFCCOUNTMEASURE`,
    ///   `IFCREAL`) → `Some(1.0)`: there is nothing to convert, this is not
    ///   an SI assumption;
    /// - otherwise the project unit of its unit type, `None` if that does
    ///   not resolve.
    ///
    /// A property with its own `Unit` must use
    /// [`UnitTable::scale_for_unit_step`] instead.
    #[cfg(feature = "ids")]
    pub fn si_scale_for_measure(
        &self,
        measure: &str,
        tables: &crate::ids::schema_tables::SchemaTables,
    ) -> Option<f64> {
        match tables.unit_type_for_measure(measure)? {
            None => Some(1.0),
            Some(unit_type) => self.scale_for_unit_type(unit_type),
        }
    }

    fn resolve(&self, step: u64, depth: usize) -> Result<ResolvedUnit, UnitError> {
        if depth > MAX_UNIT_DEPTH {
            return Err(UnitError::Cycle { step });
        }
        let rec = self
            .records
            .get(&step)
            .ok_or(UnitError::Dangling { step })?;
        match rec {
            UnitRecord::Si {
                unit_type,
                prefix,
                name,
            } => resolve_si(step, unit_type, prefix, name),
            UnitRecord::Conversion {
                unit_type,
                name,
                factor,
                offset,
            } => {
                let malformed = |detail: &str| UnitError::Malformed {
                    step,
                    detail: format!("IfcConversionBasedUnit {name:?}: {detail}"),
                };
                let factor = factor.ok_or_else(|| malformed("no ConversionFactor"))?;
                let (value, base) = match self.records.get(&factor) {
                    Some(UnitRecord::Measure { value, unit }) => (*value, *unit),
                    _ => return Err(malformed("ConversionFactor is not an IfcMeasureWithUnit")),
                };
                let value = value.ok_or_else(|| malformed("ValueComponent is not a number"))?;
                let base = base.ok_or_else(|| malformed("UnitComponent is not a unit ref"))?;
                // A named base unit must measure the same thing.
                if let (Some(bt), false) = (
                    self.unit_type_of(base),
                    matches!(self.records.get(&base), Some(UnitRecord::Derived { .. })),
                ) {
                    if !bt.eq_ignore_ascii_case(unit_type) {
                        return Err(UnitError::Inconsistent {
                            step,
                            unit_type: unit_type.clone(),
                            detail: format!("ConversionFactor unit #{base} is a {bt}"),
                        });
                    }
                }
                let b = self.resolve(base, depth + 1)?;
                let conv_offset = match offset {
                    None => 0.0,
                    Some(o) if o.is_finite() => *o,
                    Some(_) => return Err(malformed("ConversionOffset is not a number")),
                };
                // file → base: v*value + conv_offset; base → SI: *b.scale + b.offset.
                Ok(ResolvedUnit {
                    scale: value * b.scale,
                    offset: conv_offset * b.scale + b.offset,
                })
            }
            UnitRecord::Derived { elements, .. } => {
                if elements.is_empty() {
                    return Err(UnitError::Malformed {
                        step,
                        detail: "IfcDerivedUnit without elements".to_string(),
                    });
                }
                let mut scale = 1.0_f64;
                for el in elements {
                    let (unit, exponent) = match self.records.get(el) {
                        Some(UnitRecord::DerivedElement { unit, exponent }) => (*unit, *exponent),
                        _ => return Err(UnitError::Dangling { step: *el }),
                    };
                    let (unit, exponent) = match (unit, exponent) {
                        (Some(u), Some(e)) => (u, e),
                        _ => {
                            return Err(UnitError::Malformed {
                                step: *el,
                                detail: "IfcDerivedUnitElement without Unit or Exponent"
                                    .to_string(),
                            })
                        }
                    };
                    let r = self.resolve(unit, depth + 1)?;
                    if r.offset != 0.0 {
                        return Err(UnitError::OffsetInDerived { step: unit });
                    }
                    scale *= r.scale.powi(exponent);
                }
                Ok(ResolvedUnit { scale, offset: 0.0 })
            }
            UnitRecord::Monetary => Err(UnitError::NoScale {
                step,
                entity: "IfcMonetaryUnit",
            }),
            UnitRecord::ContextDependent { .. } => Err(UnitError::NoScale {
                step,
                entity: "IfcContextDependentUnit",
            }),
            UnitRecord::DerivedElement { .. } | UnitRecord::Measure { .. } => {
                Err(UnitError::Dangling { step })
            }
        }
    }

    /// Metres per file length unit: the indexer's `unit_scale`, with its
    /// warnings appended to `warnings`.
    ///
    /// Deliberately the pre-UnitTable rules, so `unit_scale` is bitwise
    /// unchanged (`tests::length_scale_matches_legacy_*`):
    /// - `IfcSIUnit` LENGTHUNIT: name `METRE` (or `METER`) × prefix;
    /// - `IfcConversionBasedUnit` LENGTHUNIT: `IfcMeasureWithUnit` value ×
    ///   an `IfcSIUnit` LENGTHUNIT base, one level only;
    /// - everything else in the assignment is skipped.
    ///
    /// Where [`UnitTable::resolve_unit_type`]`("LENGTHUNIT")` is more
    /// capable (and so could differ): a conversion base that is itself
    /// conversion-based, and `IfcConversionBasedUnitWithOffset`. Neither
    /// occurs in any file we have; aligning them would move `unit_scale`
    /// for such files and is left as a follow-up.
    pub(crate) fn length_scale(&self, warnings: &mut Vec<String>) -> Option<f64> {
        for unit_ref in &self.assigned {
            match self.records.get(unit_ref) {
                Some(UnitRecord::Si {
                    unit_type,
                    prefix,
                    name,
                }) => {
                    if unit_type.eq_ignore_ascii_case("LENGTHUNIT") {
                        match si_length_scale_checked(prefix, name) {
                            Ok(scale) => return Some(scale),
                            Err(SiScaleError::UnknownPrefix) => warnings.push(format!(
                                "IfcSIUnit #{unit_ref} declares LENGTHUNIT with an \
                                 unrecognised IfcSIPrefix {prefix:?} (name {name:?}); \
                                 it cannot be converted to metres and is IGNORED \
                                 rather than treated as the un-prefixed base unit \
                                 (which would be wrong by a power of ten)."
                            )),
                            Err(SiScaleError::NotLength) => warnings.push(format!(
                                "IfcSIUnit #{unit_ref} declares UnitType LENGTHUNIT but \
                                 an IfcSIUnitName of {name:?}, which is not a length \
                                 unit; the declaration is inconsistent and is IGNORED."
                            )),
                        }
                    }
                }
                Some(UnitRecord::Conversion {
                    unit_type,
                    name: conv_name,
                    factor,
                    offset: None,
                }) => {
                    if !unit_type.eq_ignore_ascii_case("LENGTHUNIT") {
                        continue;
                    }
                    let resolved = factor
                        .and_then(|fr| match self.records.get(&fr) {
                            Some(UnitRecord::Measure { value, unit }) => Some((*value, *unit)),
                            _ => None,
                        })
                        .and_then(|(value, base_ref)| {
                            let v = value?;
                            let base_ref = base_ref?;
                            let (base_ut, base_prefix, base_name) =
                                match self.records.get(&base_ref)? {
                                    UnitRecord::Si {
                                        unit_type,
                                        prefix,
                                        name,
                                    } => (unit_type, prefix, name),
                                    _ => return None,
                                };
                            if !base_ut.eq_ignore_ascii_case("LENGTHUNIT") {
                                return None;
                            }
                            let base_scale =
                                si_length_scale_checked(base_prefix, base_name).ok()?;
                            Some(v * base_scale)
                        });
                    match resolved {
                        Some(scale) => return Some(scale),
                        None => warnings.push(format!(
                            "IfcConversionBasedUnit (LENGTHUNIT, name={conv_name:?}, \
                             #{unit_ref}) could not be resolved to a metres-per-unit \
                             scale; its ConversionFactor → IfcMeasureWithUnit → \
                             IfcSIUnit chain is missing or malformed. unit_scale is \
                             left unset (consumers default to metres, which is WRONG \
                             for this file)."
                        )),
                    }
                }
                _ => {}
            }
        }
        if self.assigned.is_empty() {
            warnings.push(
                "no IfcUnitAssignment (or an empty one) was found in this file: \
                 the project's length unit is UNDECLARED. unit_scale is left \
                 unset; consumers that default to metres will be wrong by 1000× \
                 on a millimetre-authored file."
                    .to_string(),
            );
        } else {
            warnings.push(format!(
                "the IfcUnitAssignment lists {} unit(s) but none of them \
                 resolved to a LENGTHUNIT metres-per-unit scale. unit_scale is \
                 left unset; consumers that default to metres may be wrong.",
                self.assigned.len()
            ));
        }
        None
    }
}

fn pure_scale(r: Result<ResolvedUnit, UnitError>) -> Option<f64> {
    match r {
        Ok(ResolvedUnit { scale, offset }) if offset == 0.0 && scale.is_finite() => Some(scale),
        _ => None,
    }
}

/// Entity types [`UnitCollector::feed`] accepts; lets
/// [`UnitTable::from_table`] skip the arg split for everything else.
fn is_unit_entity(t: &[u8]) -> bool {
    const NAMES: &[&[u8]] = &[
        b"IFCSIUNIT",
        b"IFCCONVERSIONBASEDUNIT",
        b"IFCCONVERSIONBASEDUNITWITHOFFSET",
        b"IFCDERIVEDUNIT",
        b"IFCDERIVEDUNITELEMENT",
        b"IFCMONETARYUNIT",
        b"IFCCONTEXTDEPENDENTUNIT",
        b"IFCMEASUREWITHUNIT",
        b"IFCUNITASSIGNMENT",
    ];
    NAMES.iter().any(|n| t.eq_ignore_ascii_case(n))
}

/// `IfcSIPrefix` → multiplier. `""` (a `$` prefix) is 1. `None` for an
/// unknown prefix: that is a defect, never the base unit (GH #149).
pub(crate) fn si_prefix_multiplier(prefix: &str) -> Option<f64> {
    Some(match prefix {
        "" => 1.0,
        "EXA" => 1e18,
        "PETA" => 1e15,
        "TERA" => 1e12,
        "GIGA" => 1e9,
        "MEGA" => 1e6,
        "KILO" => 1e3,
        "HECTO" => 1e2,
        "DECA" => 10.0,
        "DECI" => 1e-1,
        "CENTI" => 1e-2,
        "MILLI" => 1e-3,
        "MICRO" => 1e-6,
        "NANO" => 1e-9,
        "PICO" => 1e-12,
        "FEMTO" => 1e-15,
        "ATTO" => 1e-18,
        _ => return None,
    })
}

/// Why an `IfcSIUnit` could not be turned into a metres-per-unit factor
/// (GH #149): a non-length name and an unknown prefix need different
/// warnings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SiScaleError {
    /// Not a length unit at all (`RADIAN`, `SQUARE_METRE`, …).
    NotLength,
    /// A length unit whose `IfcSIPrefix` value we don't recognise.
    UnknownPrefix,
}

/// SI prefix + name → metres-per-unit. FOOT / INCH are not legal
/// `IfcSIUnitName`s; imperial length is an `IfcConversionBasedUnit`.
pub(crate) fn si_length_scale_checked(prefix: &str, name: &str) -> Result<f64, SiScaleError> {
    let base = match name {
        "METRE" | "METER" => 1.0,
        _ => return Err(SiScaleError::NotLength),
    };
    let multiplier = si_prefix_multiplier(prefix).ok_or(SiScaleError::UnknownPrefix)?;
    Ok(base * multiplier)
}

/// `IfcSIUnitName` → (UnitType it measures, factor to the coherent SI
/// unit, dimension the prefix is raised to, offset to SI).
fn si_name_info(name: &str) -> Option<(&'static str, f64, i32, f64)> {
    Some(match name {
        "METRE" | "METER" => ("LENGTHUNIT", 1.0, 1, 0.0),
        "SQUARE_METRE" => ("AREAUNIT", 1.0, 2, 0.0),
        "CUBIC_METRE" => ("VOLUMEUNIT", 1.0, 3, 0.0),
        // The coherent SI mass unit is the kilogram.
        "GRAM" => ("MASSUNIT", 1e-3, 1, 0.0),
        "SECOND" => ("TIMEUNIT", 1.0, 1, 0.0),
        "AMPERE" => ("ELECTRICCURRENTUNIT", 1.0, 1, 0.0),
        "KELVIN" => ("THERMODYNAMICTEMPERATUREUNIT", 1.0, 1, 0.0),
        "DEGREE_CELSIUS" => ("THERMODYNAMICTEMPERATUREUNIT", 1.0, 1, 273.15),
        "MOLE" => ("AMOUNTOFSUBSTANCEUNIT", 1.0, 1, 0.0),
        "CANDELA" => ("LUMINOUSINTENSITYUNIT", 1.0, 1, 0.0),
        "RADIAN" => ("PLANEANGLEUNIT", 1.0, 1, 0.0),
        "STERADIAN" => ("SOLIDANGLEUNIT", 1.0, 1, 0.0),
        "HERTZ" => ("FREQUENCYUNIT", 1.0, 1, 0.0),
        "NEWTON" => ("FORCEUNIT", 1.0, 1, 0.0),
        "PASCAL" => ("PRESSUREUNIT", 1.0, 1, 0.0),
        "JOULE" => ("ENERGYUNIT", 1.0, 1, 0.0),
        "WATT" => ("POWERUNIT", 1.0, 1, 0.0),
        "COULOMB" => ("ELECTRICCHARGEUNIT", 1.0, 1, 0.0),
        "VOLT" => ("ELECTRICVOLTAGEUNIT", 1.0, 1, 0.0),
        "FARAD" => ("ELECTRICCAPACITANCEUNIT", 1.0, 1, 0.0),
        "OHM" => ("ELECTRICRESISTANCEUNIT", 1.0, 1, 0.0),
        "SIEMENS" => ("ELECTRICCONDUCTANCEUNIT", 1.0, 1, 0.0),
        "WEBER" => ("MAGNETICFLUXUNIT", 1.0, 1, 0.0),
        "TESLA" => ("MAGNETICFLUXDENSITYUNIT", 1.0, 1, 0.0),
        "HENRY" => ("INDUCTANCEUNIT", 1.0, 1, 0.0),
        "LUMEN" => ("LUMINOUSFLUXUNIT", 1.0, 1, 0.0),
        "LUX" => ("ILLUMINANCEUNIT", 1.0, 1, 0.0),
        "BECQUEREL" => ("RADIOACTIVITYUNIT", 1.0, 1, 0.0),
        "GRAY" => ("ABSORBEDDOSEUNIT", 1.0, 1, 0.0),
        "SIEVERT" => ("DOSEEQUIVALENTUNIT", 1.0, 1, 0.0),
        _ => return None,
    })
}

fn resolve_si(
    step: u64,
    unit_type: &str,
    prefix: &str,
    name: &str,
) -> Result<ResolvedUnit, UnitError> {
    let (measures, base, dim, offset) = si_name_info(name).ok_or(UnitError::UnknownSiName {
        step,
        name: name.to_string(),
    })?;
    if !unit_type.eq_ignore_ascii_case(measures) {
        return Err(UnitError::Inconsistent {
            step,
            unit_type: unit_type.to_string(),
            detail: format!("IfcSIUnitName {name} measures {measures}"),
        });
    }
    let p = si_prefix_multiplier(prefix).ok_or(UnitError::UnknownPrefix {
        step,
        prefix: prefix.to_string(),
    })?;
    // IFC: the prefix applies to the base unit before the exponent
    // (MILLI SQUARE_METRE = mm² = 1e-6 m²), as ifcopenshell's convert().
    Ok(ResolvedUnit {
        scale: base * p.powi(dim),
        offset,
    })
}

/// A number that may be wrapped in a defined-type constructor, e.g.
/// `IFCLENGTHMEASURE(0.3048)` or a bare `0.3048` (an
/// `IfcMeasureWithUnit.ValueComponent`). `None` if no number is there.
pub(crate) fn measure_number(raw: &[u8]) -> Option<f64> {
    match parse_field(raw) {
        Field::Number(n) => Some(n),
        _ => {
            let open = raw.iter().position(|&b| b == b'(')?;
            let close = raw.iter().rposition(|&b| b == b')')?;
            if close <= open + 1 {
                return None;
            }
            let inner = &raw[open + 1..close];
            std::str::from_utf8(inner).ok()?.trim().parse().ok()
        }
    }
}

fn enum_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    match parse_field(fields.get(idx)?) {
        Field::Enum(e) => std::str::from_utf8(e).ok().map(|s| s.to_string()),
        _ => None,
    }
}

fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    match parse_field(fields.get(idx)?) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

fn ref_at(fields: &[&[u8]], idx: usize) -> Option<u64> {
    match parse_field(fields.get(idx)?) {
        Field::Ref(id) => Some(id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn props_units() -> UnitTable {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/ids/props_units.ifc");
        let buf = std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let table = EntityTable::build(&buf);
        UnitTable::from_table(&table)
    }

    fn close(a: Option<f64>, b: f64) -> bool {
        a.is_some_and(|a| ((a - b) / b).abs() < 1e-12)
    }

    #[test]
    fn every_assignment_entry_resolves() {
        let u = props_units();
        assert_eq!(u.assigned_units(), &[3, 4, 8, 11, 12, 16, 17, 18]);
        assert_eq!(u.scale_for_unit_type("LENGTHUNIT"), Some(1e-3));
        assert_eq!(u.scale_for_unit_type("lengthunit"), Some(1e-3));
        assert_eq!(u.scale_for_unit_type("AREAUNIT"), Some(1.0));
        assert_eq!(u.scale_for_unit_type("VOLUMEUNIT"), Some(1.0));
        // KILO GRAM is the coherent SI mass unit.
        assert_eq!(u.scale_for_unit_type("MASSUNIT"), Some(1.0));
        // Conversion-based DEGREE → radians.
        assert_eq!(
            u.scale_for_unit_type("PLANEANGLEUNIT"),
            Some(0.0174532925199433)
        );
        // Derived W·m⁻²·K⁻¹.
        assert_eq!(u.scale_for_unit_type("THERMALTRANSMITTANCEUNIT"), Some(1.0));
    }

    #[test]
    fn unit_steps_prefix_and_exponent() {
        let u = props_units();
        // kPa: a property's own Unit, not in the assignment.
        assert_eq!(u.scale_for_unit_step(23), Some(1e3));
        // MILLI SQUARE_METRE is mm² (prefix squared), not 1e-3 m².
        assert!(close(u.scale_for_unit_step(24), 1e-6));
        assert_eq!(u.unit_type_of(24), Some("AREAUNIT"));
    }

    #[test]
    fn offsets_and_no_scale_units_are_not_a_bare_factor() {
        let u = props_units();
        // °C: resolvable, but only with its offset.
        assert_eq!(u.scale_for_unit_type("THERMODYNAMICTEMPERATUREUNIT"), None);
        assert_eq!(
            u.resolve_unit_type("THERMODYNAMICTEMPERATUREUNIT"),
            Ok(ResolvedUnit {
                scale: 1.0,
                offset: 273.15
            })
        );
        assert_eq!(
            u.resolve_unit_step(17),
            Err(UnitError::NoScale {
                step: 17,
                entity: "IfcMonetaryUnit"
            })
        );
        assert_eq!(
            u.resolve_unit_type("FORCEUNIT"),
            Err(UnitError::NotDeclared {
                unit_type: "FORCEUNIT".into()
            })
        );
        assert_eq!(u.scale_for_unit_step(9999), None);
        assert_eq!(u.resolve_unit_step(9), Err(UnitError::Dangling { step: 9 }));
    }

    fn table_from(data: &str) -> UnitTable {
        let src = format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_NAME('t.ifc','2026-09-25T00:00:00',(''),(''),'ifcfast','ifcfast','');\n\
FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n{data}ENDSEC;\nEND-ISO-10303-21;\n"
        );
        let table = EntityTable::build(src.as_bytes());
        UnitTable::from_table(&table)
    }

    #[test]
    fn imperial_and_nested_conversion() {
        let u = table_from(
            "#1=IFCUNITASSIGNMENT((#2));\n\
             #2=IFCCONVERSIONBASEDUNIT(#9,.LENGTHUNIT.,'yard',#3);\n\
             #3=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(3.),#4);\n\
             #4=IFCCONVERSIONBASEDUNIT(#9,.LENGTHUNIT.,'foot',#5);\n\
             #5=IFCMEASUREWITHUNIT(IFCLENGTHMEASURE(0.3048),#6);\n\
             #6=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);\n\
             #9=IFCDIMENSIONALEXPONENTS(1,0,0,0,0,0,0);\n",
        );
        // The general resolver follows the chain…
        assert!(close(u.scale_for_unit_type("LENGTHUNIT"), 0.9144));
        assert!(close(u.scale_for_unit_step(4), 0.3048));
        // …the legacy length path does not (one level only), and says so.
        let mut w = Vec::new();
        assert_eq!(u.length_scale(&mut w), None);
        assert_eq!(w.len(), 2, "{w:?}");
    }

    #[test]
    fn broken_units_are_errors_never_si() {
        let u = table_from(
            "#1=IFCUNITASSIGNMENT((#2,#3,#4,#5));\n\
             #2=IFCSIUNIT(*,.LENGTHUNIT.,.KILLO.,.METRE.);\n\
             #3=IFCSIUNIT(*,.AREAUNIT.,$,.METRE.);\n\
             #4=IFCCONVERSIONBASEDUNIT($,.PLANEANGLEUNIT.,'DEGREE',$);\n\
             #5=IFCCONTEXTDEPENDENTUNIT($,.USERDEFINED.,'piece');\n\
             #6=IFCSIUNIT(*,.LENGTHUNIT.,$,.FURLONG.);\n\
             #7=IFCDERIVEDUNIT((#8),.USERDEFINED.,'x');\n\
             #8=IFCDERIVEDUNITELEMENT(#7,1);\n",
        );
        assert!(matches!(
            u.resolve_unit_type("LENGTHUNIT"),
            Err(UnitError::UnknownPrefix { step: 2, .. })
        ));
        assert!(matches!(
            u.resolve_unit_type("AREAUNIT"),
            Err(UnitError::Inconsistent { step: 3, .. })
        ));
        assert!(matches!(
            u.resolve_unit_type("PLANEANGLEUNIT"),
            Err(UnitError::Malformed { step: 4, .. })
        ));
        assert!(matches!(
            u.resolve_unit_type("USERDEFINED"),
            Err(UnitError::NoScale { step: 5, .. })
        ));
        assert!(matches!(
            u.resolve_unit_step(6),
            Err(UnitError::UnknownSiName { step: 6, .. })
        ));
        assert!(matches!(
            u.resolve_unit_step(7),
            Err(UnitError::Cycle { .. })
        ));
        for t in ["LENGTHUNIT", "AREAUNIT", "PLANEANGLEUNIT", "USERDEFINED"] {
            assert_eq!(u.scale_for_unit_type(t), None, "{t}");
        }
    }

    #[test]
    fn first_nonempty_assignment_wins() {
        let u = table_from(
            "#1=IFCUNITASSIGNMENT(());\n\
             #2=IFCUNITASSIGNMENT((#4));\n\
             #3=IFCUNITASSIGNMENT((#5));\n\
             #4=IFCSIUNIT(*,.LENGTHUNIT.,.CENTI.,.METRE.);\n\
             #5=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);\n",
        );
        assert_eq!(u.assigned_units(), &[4]);
        assert_eq!(u.scale_for_unit_type("LENGTHUNIT"), Some(1e-2));
    }

    #[cfg(feature = "ids")]
    #[test]
    fn si_scale_for_measure_composes_the_schema_table() {
        use crate::ids::schema_tables::tables;
        use crate::ids::Schema;
        let u = props_units();
        let t = tables(Schema::Ifc2x3);
        assert_eq!(u.si_scale_for_measure("IFCLENGTHMEASURE", t), Some(1e-3));
        assert_eq!(
            u.si_scale_for_measure("IfcPositiveLengthMeasure", t),
            Some(1e-3)
        );
        assert_eq!(
            u.si_scale_for_measure("IFCPLANEANGLEMEASURE", t),
            Some(0.0174532925199433)
        );
        // No unit type: nothing to convert.
        assert_eq!(u.si_scale_for_measure("IFCLABEL", t), Some(1.0));
        // Unit type not declared in this file: unresolved, not SI.
        assert_eq!(u.si_scale_for_measure("IFCPRESSUREMEASURE", t), None);
        // Not a measure at all.
        assert_eq!(u.si_scale_for_measure("IFCNOSUCHMEASURE", t), None);
    }
}
