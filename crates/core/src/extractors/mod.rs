//! Data-layer extractors.
//!
//! Each extractor walks the shared `EntityTable` once and emits one
//! long-format table (parallel column lists) for the corresponding
//! IFC data layer:
//!
//!   * [`psets`]            — `IfcRelDefinesByProperties` → `IfcPropertySet` → `IfcPropertySingleValue`
//!   * [`quantities`]       — `IfcRelDefinesByProperties` → `IfcElementQuantity` → physical-simple quantities
//!   * [`materials`]        — `IfcRelAssociatesMaterial`  → `IfcMaterial` / layer sets / lists
//!   * [`classifications`]  — `IfcRelAssociatesClassification` → `IfcClassificationReference`
//!
//! All four are independent and can be called in any combination. The
//! `extract_all` PyO3 entry point in [`crate`] shares the entity table
//! and product-GUID map across the four for ~2-3× speedup on large
//! files.

pub mod classifications;
pub mod materials;
pub mod psets;
pub mod quantities;
