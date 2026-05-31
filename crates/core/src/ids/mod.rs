//! Native IDS 1.0 validation on ifcfast columnar indexes.

mod attribute_read;
mod attribute_schema;
mod classification;
mod compiled;
mod context;
mod engine;
mod extract_needs;
mod tier1_validate;
mod validation_plan;
mod ifc_validation_base;
mod entity_schema;
mod facets;
mod material;
mod native_expand;
mod predefined_type;
mod report;
mod restrictions;

#[cfg(test)]
mod tests;

pub mod xml;

pub use compiled::{
    Cardinality, CompiledFacet, CompiledIds, CompiledSpec, FacetKind, MaxOccurs, ValueConstraint,
};
pub use extract_needs::ExtractNeeds;
pub use validation_plan::ValidationPlan;
pub use tier1_validate::validate_tier1;
pub use engine::validate;
pub use report::{SpecResult, ValidationReport};

pub(crate) use context::ValidationContext;
pub(crate) use engine::validate_spec;
pub(crate) use ifc_validation_base::IfcValidationBase;
