//! IDS 1.0 validation in the browser (GH #192 slice 4), behind the
//! crate's opt-in `ids` feature (`IFCFAST_WASM_FEATURES=ids
//! crates/wasm/build.sh`): it adds roxmltree, regex and the per-schema
//! IDS tables, +1.74 MB raw / +367 KB brotli over the base module, so the
//! default (site) bundle is built without it.
//!
//! The report is the IfcTester-shaped JSON the wheel's
//! `IdsReport.to_ifctester_json()` returns — the same Rust builder
//! (`ifcfast_core::ids::ifctester_json`), not a port.

use ifcfast_core::entity_table::EntityTable;
use ifcfast_core::ids::ifctester_json::JsonMeta;
use ifcfast_core::ids::{
    schema_from_header, schema_identifier_from_header, validate_ifctester_json, IdsError,
    OnUnsupported, ValidateOptions,
};
use wasm_bindgen::prelude::*;

use crate::IfcModel;

/// IfcTester's `date` (`%Y-%m-%d %H:%M:%S`), local time from JS `Date`.
fn local_date() -> String {
    let d = js_sys::Date::new_0();
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        d.get_full_year(),
        d.get_month() + 1,
        d.get_date(),
        d.get_hours(),
        d.get_minutes(),
        d.get_seconds()
    )
}

/// The Python exception name as a message prefix, so JS can branch on
/// `err.message.startsWith("IdsInvalidError")`.
fn js_err(e: IdsError) -> JsError {
    let kind = match e {
        IdsError::InvalidIds { .. } => "IdsInvalidError",
        IdsError::Unsupported { .. } => "IdsUnsupportedError",
        IdsError::UnresolvedUnit { .. } => "IdsUnitError",
        IdsError::IfcInput { .. } => "IfcfastError",
    };
    JsError::new(&format!("{kind}: {e}"))
}

#[wasm_bindgen]
impl IfcModel {
    /// Validate this model against one IDS 1.0 document; returns the
    /// IfcTester-shaped JSON report (the wheel's
    /// `IdsReport.to_ifctester_json()`, byte-for-byte the same builder).
    ///
    /// `onUnsupported`: `"raise"` (default) or `"mark"`;
    /// `filterIfcVersion`: skip specs whose `ifcVersion` excludes the
    /// file's schema (default `false`, like IfcTester). `filepath` /
    /// `filename` carry the name given to `fromBytes`.
    ///
    /// Throws `Error("IdsInvalidError: …")`, `"IdsUnsupportedError: …"`,
    /// `"IdsUnitError: …"` or `"IfcfastError: …"`. Mesh-free: runs on
    /// the retained STEP bytes.
    #[wasm_bindgen(js_name = validateIds)]
    pub fn validate_ids(
        &self,
        ids_xml: &str,
        on_unsupported: Option<String>,
        filter_ifc_version: Option<bool>,
    ) -> Result<String, JsError> {
        let name = on_unsupported.as_deref().unwrap_or("raise");
        let mode = OnUnsupported::from_name(name).ok_or_else(|| {
            JsError::new(&format!(
                "validateIds: onUnsupported must be 'raise' or 'mark', got {name:?}"
            ))
        })?;
        let buf = self.inner.source.as_bytes();
        let schema = schema_from_header(buf).map_err(js_err)?;
        let table = EntityTable::build(buf);
        let ident = schema_identifier_from_header(buf);
        let date = local_date();
        let meta = JsonMeta {
            date: &date,
            filepath: Some(&self.inner.name),
            schema_identifier: &ident,
        };
        let opts = ValidateOptions {
            on_unsupported: mode,
            filter_ifc_version: filter_ifc_version.unwrap_or(false),
        };
        let (_, mut json) =
            validate_ifctester_json(&[ids_xml.as_bytes()], &table, schema, opts, &meta)
                .map_err(js_err)?;
        Ok(json.pop().unwrap_or_default())
    }
}
