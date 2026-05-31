//! Validation report types.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidationReport {
    pub ids_path: String,
    pub ifc_path: String,
    pub schema: String,
    pub engine: String,
    #[serde(default)]
    pub open_ms: f64,
    pub index_ms: f64,
    pub pset_extract_ms: f64,
    pub validate_ms: f64,
    pub specifications: Vec<SpecResult>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SpecResult {
    pub name: String,
    pub status: bool,
    pub ifc_version_ok: bool,
    pub applicable_count: usize,
    pub passed_count: usize,
    pub failed_count: usize,
    pub failed_guids: Vec<String>,
}
