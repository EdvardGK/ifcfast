//! Serde-friendly IDS specification IR (compiled from IfcTester or Rust XML).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledIds {
    pub ids_path: Option<String>,
    pub specifications: Vec<CompiledSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledSpec {
    pub name: String,
    #[serde(default)]
    pub ifc_versions: Vec<String>,
    #[serde(default)]
    pub min_occurs: u32,
    /// `"unbounded"`, a number, or omitted (= unbounded).
    #[serde(default = "default_max_occurs")]
    pub max_occurs: MaxOccurs,
    #[serde(default)]
    pub applicability: Vec<CompiledFacet>,
    #[serde(default)]
    pub requirements: Vec<CompiledFacet>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MaxOccurs {
    Unbounded(String),
    Count(u32),
}

impl MaxOccurs {
    pub fn is_prohibited(&self) -> bool {
        matches!(self, MaxOccurs::Count(0))
    }

    pub fn allows_requirements(&self) -> bool {
        !self.is_prohibited()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FacetKind {
    Entity,
    Attribute,
    Property,
    Classification,
    Material,
    PartOf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompiledFacet {
    pub kind: FacetKind,
    #[serde(default = "default_cardinality")]
    pub cardinality: Cardinality,
    /// Entity: IFC class name(s) via `entity_names`.
    #[serde(default)]
    pub entity_names: Vec<String>,
    #[serde(default)]
    pub predefined_type: Option<String>,
    #[serde(default)]
    pub predefined_type_constraint: Option<ValueConstraint>,
    /// Attribute: IDS attribute name (e.g. Name, Tag).
    #[serde(default)]
    pub attribute_name: Option<String>,
    /// Attribute: when ``name`` is an enumeration, any listed column may satisfy the facet.
    #[serde(default)]
    pub attribute_names: Vec<String>,
    #[serde(default)]
    pub attribute_name_constraint: Option<ValueConstraint>,
    #[serde(default)]
    pub property_set: Option<String>,
    #[serde(default)]
    pub property_sets: Vec<String>,
    #[serde(default)]
    pub property_set_constraint: Option<ValueConstraint>,
    #[serde(default)]
    pub base_name: Option<String>,
    #[serde(default)]
    pub base_names: Vec<String>,
    /// When ``baseName`` is an xs:restriction (pattern, bounds, …), not a simple name.
    #[serde(default)]
    pub base_name_constraint: Option<ValueConstraint>,
    #[serde(default)]
    pub data_type: Option<String>,
    #[serde(default)]
    pub value: Option<ValueConstraint>,
    #[serde(default)]
    pub partof_relation: Option<String>,
    #[serde(default)]
    pub classification_system: Option<ValueConstraint>,
    /// Legacy compiled IR; prefer [`CompiledFacet::value`] for material facets.
    #[serde(default)]
    pub material_value: Option<String>,
}

fn default_cardinality() -> Cardinality {
    Cardinality::Required
}

fn default_max_occurs() -> MaxOccurs {
    MaxOccurs::Unbounded("unbounded".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cardinality {
    Required,
    Optional,
    Prohibited,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ValueConstraint {
    Simple { text: String },
    Enumeration { values: Vec<String> },
    Pattern { patterns: Vec<String> },
    Bounds {
        min_inclusive: Option<f64>,
        max_inclusive: Option<f64>,
        min_exclusive: Option<f64>,
        max_exclusive: Option<f64>,
    },
    Length {
        length: usize,
    },
    MinLength {
        min: usize,
    },
    MaxLength {
        max: usize,
    },
    /// Combined XSD length facets (minLength + maxLength + length).
    LengthBounds {
        #[serde(default)]
        length: Option<usize>,
        #[serde(default)]
        min: Option<usize>,
        #[serde(default)]
        max: Option<usize>,
    },
}
