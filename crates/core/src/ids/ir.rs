//! Schema-independent IR of an IDS 1.0 document (design §2.3).
//!
//! The IR is a lossless image of what the XML says, after structural
//! validation: every attribute and value the IDS 1.0 XSD allows is kept
//! (including `instructions`, which the reporter needs). Names are NOT
//! resolved against an IFC schema here — `compile` does that.
//!
//! [`IdsDocument::to_canonical_json`] emits the parse-differential
//! projection (schema `ifcfast.ids.canonical/1`, defined in
//! `tests/oracle/ids_parse_differential.py`). The `Serialize` derives
//! give a plain structural dump of the IR for debugging.

use serde::Serialize;

/// A parsed IDS document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IdsDocument {
    pub info: IdsInfo,
    pub specs: Vec<Spec>,
}

/// `<info>`. Only `title` is required by the XSD.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IdsInfo {
    pub title: String,
    pub copyright: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    /// Validated against the XSD pattern `[^@]+@[^\.]+\..+`.
    pub author: Option<String>,
    /// Validated `xs:date` lexical form, kept verbatim.
    pub date: Option<String>,
    pub purpose: Option<String>,
    pub milestone: Option<String>,
}

/// `<specification>`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Spec {
    /// 0-based position in document order.
    pub idx: u32,
    pub name: String,
    pub identifier: Option<String>,
    pub description: Option<String>,
    pub instructions: Option<String>,
    /// `ifcVersion` list tokens in document order (duplicates kept, as
    /// IfcTester does). Never empty.
    pub ifc_versions: Vec<Schema>,
    /// From `applicability/@minOccurs` + `@maxOccurs` (raw values below).
    pub cardinality: SpecCardinality,
    /// `applicability/@minOccurs`; `xs:occurs` default 1 when absent.
    pub min_occurs: u32,
    /// `applicability/@maxOccurs`; `None` = `unbounded`; default 1.
    pub max_occurs: Option<u32>,
    /// Applicability facets in document order (XSD order: entity,
    /// partOf*, classification*, attribute*, property*, material*).
    pub applicability: Vec<Facet>,
    /// Requirement facets in document order (the XSD allows any order).
    pub requirements: Vec<Requirement>,
    /// `requirements/@description`.
    pub requirements_description: Option<String>,
}

/// One requirement facet with its per-facet attributes.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Requirement {
    pub facet: Facet,
    /// Always `Required` for the entity facet (the XSD gives it no
    /// cardinality attribute); `Optional` is impossible for partOf.
    pub cardinality: FacetCardinality,
    pub instructions: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum Schema {
    #[serde(rename = "IFC2X3")]
    Ifc2x3,
    #[serde(rename = "IFC4")]
    Ifc4,
    /// IDS token `IFC4X3_ADD2`.
    #[serde(rename = "IFC4X3_ADD2")]
    Ifc4x3,
}

impl Schema {
    /// Parse an `ifcVersion` list token (case-sensitive, as the XSD enum).
    pub fn from_ids_token(tok: &str) -> Option<Schema> {
        match tok {
            "IFC2X3" => Some(Schema::Ifc2x3),
            "IFC4" => Some(Schema::Ifc4),
            "IFC4X3_ADD2" => Some(Schema::Ifc4x3),
            _ => None,
        }
    }

    pub fn ids_token(self) -> &'static str {
        match self {
            Schema::Ifc2x3 => "IFC2X3",
            Schema::Ifc4 => "IFC4",
            Schema::Ifc4x3 => "IFC4X3_ADD2",
        }
    }
}

/// Specification usage (applicability min/maxOccurs), IDS 1.0
/// UserManual/specifications.md table: 1/unbounded = required,
/// 0/unbounded = optional, 0/0 = prohibited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SpecCardinality {
    Required,
    Optional,
    Prohibited,
}

/// Requirement facet `cardinality` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum FacetCardinality {
    Required,
    Optional,
    Prohibited,
}

/// The six IDS facets.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "facet", rename_all = "snake_case")]
pub enum Facet {
    Entity(EntityFacet),
    Attribute {
        name: Val,
        value: Option<Val>,
    },
    Property {
        property_set: Val,
        base_name: Val,
        value: Option<Val>,
        /// Uppercase IFC defined-type name (`IFCLENGTHMEASURE`), verbatim.
        /// Resolved against the schema by `compile`.
        data_type: Option<String>,
        /// Requirements only.
        uri: Option<String>,
    },
    Classification {
        /// Required by the IDS 1.0 XSD (`classificationType/system`
        /// minOccurs=1).
        system: Val,
        value: Option<Val>,
        /// Requirements only.
        uri: Option<String>,
    },
    Material {
        value: Option<Val>,
        /// Requirements only.
        uri: Option<String>,
    },
    PartOf {
        entity: Box<EntityFacet>,
        relation: Option<Relation>,
    },
}

impl Facet {
    /// Stable lowercase facet tag (`entity`, `attribute`, `property`,
    /// `classification`, `material`, `part_of`).
    pub fn kind(&self) -> &'static str {
        match self {
            Facet::Entity(_) => "entity",
            Facet::Attribute { .. } => "attribute",
            Facet::Property { .. } => "property",
            Facet::Classification { .. } => "classification",
            Facet::Material { .. } => "material",
            Facet::PartOf { .. } => "part_of",
        }
    }
}

/// `entityType`: used by the entity facet and inside partOf.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntityFacet {
    pub name: Val,
    pub predefined_type: Option<Val>,
}

/// `partOf/@relation`, the IDS 1.0 `relations` enumeration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Relation {
    #[serde(rename = "IFCRELAGGREGATES")]
    Aggregates,
    #[serde(rename = "IFCRELASSIGNSTOGROUP")]
    AssignsToGroup,
    #[serde(rename = "IFCRELCONTAINEDINSPATIALSTRUCTURE")]
    ContainedInSpatialStructure,
    #[serde(rename = "IFCRELNESTS")]
    Nests,
    /// The compound token `IFCRELVOIDSELEMENT IFCRELFILLSELEMENT`
    /// (element fills an opening that voids the whole).
    #[serde(rename = "IFCRELVOIDSELEMENT IFCRELFILLSELEMENT")]
    VoidsElementFillsElement,
}

impl Relation {
    /// Exact match against the XSD enumeration. The value is an
    /// `xs:string` restriction, so no whitespace normalisation applies.
    pub fn from_ids_token(tok: &str) -> Option<Relation> {
        match tok {
            "IFCRELAGGREGATES" => Some(Relation::Aggregates),
            "IFCRELASSIGNSTOGROUP" => Some(Relation::AssignsToGroup),
            "IFCRELCONTAINEDINSPATIALSTRUCTURE" => Some(Relation::ContainedInSpatialStructure),
            "IFCRELNESTS" => Some(Relation::Nests),
            "IFCRELVOIDSELEMENT IFCRELFILLSELEMENT" => Some(Relation::VoidsElementFillsElement),
            _ => None,
        }
    }

    pub fn ids_token(self) -> &'static str {
        match self {
            Relation::Aggregates => "IFCRELAGGREGATES",
            Relation::AssignsToGroup => "IFCRELASSIGNSTOGROUP",
            Relation::ContainedInSpatialStructure => "IFCRELCONTAINEDINSPATIALSTRUCTURE",
            Relation::Nests => "IFCRELNESTS",
            Relation::VoidsElementFillsElement => "IFCRELVOIDSELEMENT IFCRELFILLSELEMENT",
        }
    }
}

/// `idsValue`: a simple value or an `xs:restriction`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Val {
    /// `<simpleValue>`, verbatim (`xs:string`: whitespace preserved).
    Simple(String),
    Restriction(Restriction),
}

/// `xs:restriction` with the IDS 1.0 facet subset. Numeric bounds are
/// kept in their lexical form (validated at parse; converted by
/// [`crate::ids::restriction::CompiledVal`]).
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
pub struct Restriction {
    pub base: XsdBase,
    pub enumeration: Option<Vec<String>>,
    /// One or more `xs:pattern`; a value matches if ANY pattern matches
    /// (XSD semantics; pinned by the `regex_patterns_work_in_OR` cases).
    pub patterns: Option<Vec<String>>,
    pub min_inclusive: Option<String>,
    pub min_exclusive: Option<String>,
    pub max_inclusive: Option<String>,
    pub max_exclusive: Option<String>,
    pub length: Option<u32>,
    pub min_length: Option<u32>,
    pub max_length: Option<u32>,
}

/// The `base` of an `xs:restriction`: exactly the eight base types
/// IDS 1.0 allows (ImplementersDocumentation/DataTypes.md, "XML base
/// types"). Any other `xs:` type is an invalid IDS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum XsdBase {
    #[default]
    #[serde(rename = "xs:string")]
    String,
    #[serde(rename = "xs:boolean")]
    Boolean,
    #[serde(rename = "xs:integer")]
    Integer,
    #[serde(rename = "xs:double")]
    Double,
    #[serde(rename = "xs:date")]
    Date,
    #[serde(rename = "xs:dateTime")]
    DateTime,
    #[serde(rename = "xs:time")]
    Time,
    #[serde(rename = "xs:duration")]
    Duration,
}

impl XsdBase {
    /// Map the local part of an XSD built-in type name.
    pub fn from_local_name(local: &str) -> Option<XsdBase> {
        Some(match local {
            "string" => XsdBase::String,
            "boolean" => XsdBase::Boolean,
            "integer" => XsdBase::Integer,
            "double" => XsdBase::Double,
            "date" => XsdBase::Date,
            "dateTime" => XsdBase::DateTime,
            "time" => XsdBase::Time,
            "duration" => XsdBase::Duration,
            _ => return None,
        })
    }

    /// Local name without the `xs:` prefix (`"double"`), as IfcTester
    /// reports `Restriction.base`.
    pub fn local_name(self) -> &'static str {
        match self {
            XsdBase::String => "string",
            XsdBase::Boolean => "boolean",
            XsdBase::Integer => "integer",
            XsdBase::Double => "double",
            XsdBase::Date => "date",
            XsdBase::DateTime => "dateTime",
            XsdBase::Time => "time",
            XsdBase::Duration => "duration",
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(self, XsdBase::Integer | XsdBase::Double)
    }

    pub fn is_temporal(self) -> bool {
        matches!(
            self,
            XsdBase::Date | XsdBase::DateTime | XsdBase::Time | XsdBase::Duration
        )
    }
}

/// Canonical-JSON schema id (tests/oracle/ids_parse_differential.py).
pub const CANONICAL_SCHEMA: &str = "ifcfast.ids.canonical/1";

impl IdsDocument {
    /// The parse-differential projection, schema v1, serialised exactly
    /// like Python `json.dumps(obj, sort_keys=True, separators=(",", ":"),
    /// ensure_ascii=False)`. The spec lives in the module docstring of
    /// `tests/oracle/ids_parse_differential.py`; the IfcTester-side
    /// normalisations it lists are reproduced here:
    ///
    /// * empty strings become `null` (IfcTester drops falsy values), and
    ///   an empty `<title>` becomes `"Untitled"` (ifctester/ids.py
    ///   `Ids.__init__` default, not overwritten by a falsy title);
    /// * an empty `<simpleValue/>` becomes `null` (IfcTester parses it as
    ///   "no value constraint"; see the ambiguity register);
    /// * `minOccurs`/`maxOccurs` are the raw values with the XSD defaults
    ///   (1/1) filled in, as xmlschema does for IfcTester;
    /// * requirements are stably sorted by facet type (entity, partOf,
    ///   classification, attribute, property, material);
    /// * restriction `base` loses its `xs:` prefix; enumeration and
    ///   pattern lists are sorted; bounds keep their lexical form.
    pub fn to_canonical_json(&self) -> String {
        let v = self.canonical_value();
        let mut out = String::new();
        write_sorted(&v, &mut out);
        out
    }

    /// The canonical projection as a JSON value (unsorted map; use
    /// [`IdsDocument::to_canonical_json`] for the byte-exact form).
    pub fn canonical_value(&self) -> serde_json::Value {
        use serde_json::{json, Value};
        let opt = |s: &Option<String>| -> Value {
            match s.as_deref() {
                None | Some("") => Value::Null,
                Some(v) => Value::String(v.to_string()),
            }
        };
        let info = &self.info;
        let title = if info.title.is_empty() {
            "Untitled".to_string()
        } else {
            info.title.clone()
        };
        let info_v = json!({
            "title": title,
            "copyright": opt(&info.copyright),
            "version": opt(&info.version),
            "description": opt(&info.description),
            "author": opt(&info.author),
            "date": opt(&info.date),
            "purpose": opt(&info.purpose),
            "milestone": opt(&info.milestone),
        });
        let specs: Vec<Value> = self
            .specs
            .iter()
            .map(|sp| {
                let mut versions: Vec<&str> = sp.ifc_versions.iter().map(|s| s.ids_token()).collect();
                versions.sort_unstable();
                let min = sp.min_occurs;
                let max: Value = match sp.max_occurs {
                    None => json!("unbounded"),
                    Some(m) => json!(m),
                };
                let applicability: Vec<Value> = sp
                    .applicability
                    .iter()
                    .map(|f| canonical_facet(f, None, None))
                    .collect();
                let mut reqs: Vec<&Requirement> = sp.requirements.iter().collect();
                reqs.sort_by_key(|r| facet_order(&r.facet));
                let requirements: Vec<Value> = reqs
                    .iter()
                    .map(|r| canonical_facet(&r.facet, Some(r.cardinality), Some(&r.instructions)))
                    .collect();
                json!({
                    "name": sp.name,
                    "identifier": opt(&sp.identifier),
                    "description": opt(&sp.description),
                    "instructions": opt(&sp.instructions),
                    "ifcVersion": versions,
                    "minOccurs": min,
                    "maxOccurs": max,
                    "cardinality": match sp.cardinality {
                        SpecCardinality::Required => "required",
                        SpecCardinality::Optional => "optional",
                        SpecCardinality::Prohibited => "prohibited",
                    },
                    "applicability": applicability,
                    "requirements": requirements,
                })
            })
            .collect();
        json!({
            "schema": CANONICAL_SCHEMA,
            "info": info_v,
            "specifications": specs,
        })
    }
}

fn facet_order(f: &Facet) -> u8 {
    match f {
        Facet::Entity(_) => 0,
        Facet::PartOf { .. } => 1,
        Facet::Classification { .. } => 2,
        Facet::Attribute { .. } => 3,
        Facet::Property { .. } => 4,
        Facet::Material { .. } => 5,
    }
}

/// `card`/`instructions` are `None` for applicability facets.
fn canonical_facet(
    f: &Facet,
    card: Option<FacetCardinality>,
    instructions: Option<&Option<String>>,
) -> serde_json::Value {
    use serde_json::{json, Value};
    let opt = |s: &Option<String>| -> Value {
        match s.as_deref() {
            None | Some("") => Value::Null,
            Some(v) => Value::String(v.to_string()),
        }
    };
    let (kind, fields) = match f {
        Facet::Entity(e) => (
            "entity",
            json!({"name": canonical_val(Some(&e.name)), "predefinedType": canonical_val(e.predefined_type.as_ref())}),
        ),
        Facet::PartOf { entity, relation } => (
            "partOf",
            json!({
                "name": canonical_val(Some(&entity.name)),
                "predefinedType": canonical_val(entity.predefined_type.as_ref()),
                "relation": relation.map(|r| r.ids_token()),
            }),
        ),
        Facet::Classification { system, value, uri } => (
            "classification",
            json!({"value": canonical_val(value.as_ref()), "system": canonical_val(Some(system)), "uri": opt(uri)}),
        ),
        Facet::Attribute { name, value } => (
            "attribute",
            json!({"name": canonical_val(Some(name)), "value": canonical_val(value.as_ref())}),
        ),
        Facet::Property {
            property_set,
            base_name,
            value,
            data_type,
            uri,
        } => (
            "property",
            json!({
                "propertySet": canonical_val(Some(property_set)),
                "baseName": canonical_val(Some(base_name)),
                "value": canonical_val(value.as_ref()),
                "dataType": opt(data_type),
                "uri": opt(uri),
            }),
        ),
        Facet::Material { value, uri } => (
            "material",
            json!({"value": canonical_val(value.as_ref()), "uri": opt(uri)}),
        ),
    };
    let cardinality = match (card, f) {
        (None, _) => Value::Null,
        (Some(_), Facet::Entity(_)) => json!("required"),
        (Some(c), _) => json!(match c {
            FacetCardinality::Required => "required",
            FacetCardinality::Optional => "optional",
            FacetCardinality::Prohibited => "prohibited",
        }),
    };
    json!({
        "facet": kind,
        "cardinality": cardinality,
        "instructions": instructions.map_or(Value::Null, opt),
        "fields": fields,
    })
}

fn canonical_val(v: Option<&Val>) -> serde_json::Value {
    use serde_json::{json, Map, Value};
    match v {
        None => Value::Null,
        Some(Val::Simple(s)) if s.is_empty() => Value::Null,
        Some(Val::Simple(s)) => json!({ "simpleValue": s }),
        Some(Val::Restriction(r)) => {
            let mut m = Map::new();
            m.insert("base".into(), json!(r.base.local_name()));
            if let Some(en) = &r.enumeration {
                let mut e = en.clone();
                e.sort();
                m.insert("enumeration".into(), json!(e));
            }
            if let Some(ps) = &r.patterns {
                let mut p = ps.clone();
                p.sort();
                m.insert("pattern".into(), json!(p));
            }
            let lex = [
                ("minInclusive", &r.min_inclusive),
                ("minExclusive", &r.min_exclusive),
                ("maxInclusive", &r.max_inclusive),
                ("maxExclusive", &r.max_exclusive),
            ];
            for (k, v) in lex {
                if let Some(v) = v {
                    m.insert(k.into(), json!(v));
                }
            }
            let lens = [("length", r.length), ("minLength", r.min_length), ("maxLength", r.max_length)];
            for (k, v) in lens {
                if let Some(v) = v {
                    m.insert(k.into(), json!(v.to_string()));
                }
            }
            json!({ "restriction": Value::Object(m) })
        }
    }
}

/// Python `json.dumps(..., sort_keys=True, separators=(",", ":"),
/// ensure_ascii=False)`. Python sorts keys by code point; for the ASCII
/// keys of schema v1 that equals Rust's byte order. String escaping:
/// serde_json and Python agree for `ensure_ascii=False` (`\"`, `\\`,
/// `\n` `\r` `\t` `\b` `\f`, other C0 controls as lowercase `\u00xx`).
fn write_sorted(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value;
    match v {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String((*k).clone()).to_string());
                out.push(':');
                write_sorted(&map[k.as_str()], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, it) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_sorted(it, out);
            }
            out.push(']');
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_schema_and_relation_tokens_roundtrip() {
        for s in [Schema::Ifc2x3, Schema::Ifc4, Schema::Ifc4x3] {
            assert_eq!(Schema::from_ids_token(s.ids_token()), Some(s));
        }
        assert_eq!(Schema::from_ids_token("IFC4X3"), None);
        assert_eq!(Schema::from_ids_token("ifc4"), None);
        for r in [
            Relation::Aggregates,
            Relation::AssignsToGroup,
            Relation::ContainedInSpatialStructure,
            Relation::Nests,
            Relation::VoidsElementFillsElement,
        ] {
            assert_eq!(Relation::from_ids_token(r.ids_token()), Some(r));
        }
        assert_eq!(Relation::from_ids_token("IFCRELVOIDSELEMENT"), None);
    }

    #[test]
    fn ids_canonical_json_schema_v1() {
        let doc = IdsDocument {
            info: IdsInfo {
                title: String::new(),
                copyright: None,
                version: None,
                description: Some("d".into()),
                author: None,
                date: None,
                purpose: None,
                milestone: None,
            },
            specs: vec![Spec {
                idx: 0,
                name: "s".into(),
                identifier: None,
                description: Some(String::new()),
                instructions: None,
                ifc_versions: vec![Schema::Ifc4, Schema::Ifc2x3],
                cardinality: SpecCardinality::Required,
                min_occurs: 1,
                max_occurs: None,
                applicability: vec![Facet::Entity(EntityFacet {
                    name: Val::Simple("IFCWALL".into()),
                    predefined_type: None,
                })],
                requirements: vec![
                    Requirement {
                        facet: Facet::Attribute {
                            name: Val::Simple("Name".into()),
                            value: Some(Val::Restriction(Restriction {
                                base: XsdBase::String,
                                patterns: Some(vec!["b".into(), "a".into()]),
                                max_length: Some(3),
                                ..Restriction::default()
                            })),
                        },
                        cardinality: FacetCardinality::Optional,
                        instructions: Some("do it".into()),
                    },
                    Requirement {
                        facet: Facet::Entity(EntityFacet {
                            name: Val::Simple("IFCWALL".into()),
                            predefined_type: Some(Val::Simple("ÆØÅ".into())),
                        }),
                        cardinality: FacetCardinality::Required,
                        instructions: None,
                    },
                ],
                requirements_description: None,
            }],
        };
        let want = concat!(
            r#"{"info":{"author":null,"copyright":null,"date":null,"description":"d","milestone":null,"purpose":null,"title":"Untitled","version":null},"#,
            r#""schema":"ifcfast.ids.canonical/1","specifications":[{"applicability":[{"cardinality":null,"facet":"entity","fields":{"name":{"simpleValue":"IFCWALL"},"predefinedType":null},"instructions":null}],"#,
            r#""cardinality":"required","description":null,"identifier":null,"ifcVersion":["IFC2X3","IFC4"],"instructions":null,"maxOccurs":"unbounded","minOccurs":1,"name":"s","#,
            r#""requirements":[{"cardinality":"required","facet":"entity","fields":{"name":{"simpleValue":"IFCWALL"},"predefinedType":{"simpleValue":"ÆØÅ"}},"instructions":null},"#,
            r#"{"cardinality":"optional","facet":"attribute","fields":{"name":{"simpleValue":"Name"},"value":{"restriction":{"base":"string","maxLength":"3","pattern":["a","b"]}}},"instructions":"do it"}]}]}"#
        );
        assert_eq!(doc.to_canonical_json(), want);
    }
}
