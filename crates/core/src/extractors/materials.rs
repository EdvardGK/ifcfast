//! Material assignment extraction.
//!
//! Walks `IfcRelAssociatesMaterial` and emits long-format rows. The
//! `RelatingMaterial` ref can point at any of several IFC material types;
//! we resolve each into a flat list of (material_name, layer_thickness,
//! layer_index, category) entries.
//!
//! Output schema:
//!     (guid, role, layer_index, material_name, layer_thickness_mm, category,
//!      fraction, source)
//!
//! `role`:
//!   `"direct"`       — `IfcMaterial` directly assigned
//!   `"list"`         — `IfcMaterialList` (one row per material)
//!   `"layer"`        — `IfcMaterialLayer` inside `IfcMaterialLayerSet` /
//!                      `IfcMaterialLayerSetUsage` (one row per layer)
//!   `"constituent"`  — `IfcMaterialConstituent` inside `IfcMaterialConstituentSet`
//!   `"profile"`      — `IfcMaterialProfile` inside `IfcMaterialProfileSet`
//!
//! Phase 1 covers IfcMaterial / IfcMaterialList / IfcMaterialLayerSet /
//! IfcMaterialLayerSetUsage. Constituent + profile sets are uncommon in
//! Norwegian/Nordic practice; folded in if/when they surface.

use std::collections::HashMap;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

#[derive(Debug, Default)]
pub struct MaterialTable {
    pub guid: Vec<String>,
    pub role: Vec<&'static str>,
    pub layer_index: Vec<i32>, // -1 for non-layered roles
    pub material_name: Vec<Option<String>>,
    pub layer_thickness_mm: Vec<Option<f64>>,
    pub category: Vec<Option<String>>,
    /// Composition fraction for `role = "constituent"` rows — IFC4
    /// `IfcMaterialConstituent.Fraction`, typically a unit-normalised
    /// 0..1 value representing the volume / mass contribution of this
    /// constituent within the parent set. `None` on all other roles
    /// (layers have `layer_thickness_mm` instead; direct / list /
    /// profile bindings don't have a per-row weight in the IFC schema).
    pub fraction: Vec<Option<f64>>,
    /// `"instance"` for a material associated directly with the product
    /// via `IfcRelAssociatesMaterial`, `"type"` for one inherited from
    /// the product's `IfcTypeObject` (`IfcRelDefinesByType` →
    /// RelatingType, itself the RelatedObject of an
    /// `IfcRelAssociatesMaterial`). Same column name and same values as
    /// `psets.source` / `quantities.source`, and the same
    /// instance-wins-on-collision rule: a product with ANY directly
    /// associated material inherits nothing from its type (GH #165).
    pub source: Vec<&'static str>,
}

impl MaterialTable {
    pub fn len(&self) -> usize {
        self.guid.len()
    }

    pub fn is_empty(&self) -> bool {
        self.guid.is_empty()
    }
}

/// Build the material table. `unit_scale` is the IFC project's
/// linear-unit-to-metres factor as reported by the indexer (0.001 for
/// millimetre files, 1.0 for metre files).
///
/// When the indexer could not resolve a scale it reports `None`, and
/// callers currently pass `1.0`. That is a GUESS, not a safe default:
/// `1.0` claims the file is authored in metres, so on the far more
/// common millimetre file every `layer_thickness_mm` comes out 1000×
/// too large (a 200 mm layer reads as 200 000 mm). The indexer emits a
/// loud warning in exactly that case — see `IndexedFile::warnings` and
/// GH #149 — and consumers should treat the column as unusable rather
/// than trust it silently.
///
/// The `LayerThickness` IFC field carries a raw value in the project's
/// linear unit. The output column is named `layer_thickness_mm` so we
/// scale at parse time: `raw * unit_scale * 1000` gives millimetres
/// regardless of the source unit. Pre-fix this column held the raw
/// value (e.g. `0.003` for a 3 mm layer on a metres-authored file like
/// Duplex), making downstream code that trusted the name silently
/// wrong by 1000×.
pub fn build(
    table: &EntityTable,
    product_step_to_guid: &HashMap<u64, String>,
    unit_scale: f64,
) -> MaterialTable {
    // Pass 1: index every material-related entity by step_id so we can
    //         resolve refs cheaply during the second pass.
    let mut materials: HashMap<u64, MaterialRecord> = HashMap::with_capacity(2048);
    let mut layer_sets: HashMap<u64, Vec<u64>> = HashMap::with_capacity(512);
    let mut layer_set_usages: HashMap<u64, u64> = HashMap::with_capacity(512);
    let mut layers: HashMap<u64, LayerRecord> = HashMap::with_capacity(4096);
    let mut material_lists: HashMap<u64, Vec<u64>> = HashMap::with_capacity(256);
    // IFC4: IfcMaterialConstituentSet → list of IfcMaterialConstituent.
    // Common in MEP / structural exports where a product is built from
    // a mix of materials in defined proportions (concrete + rebar,
    // glass + sealant). Phase 1 stored the raw `Fraction` field on
    // the constituent record; surfacing it as a Parquet column is
    // queued behind a `_CACHE_SCHEMA_VERSION` bump.
    let mut constituent_sets: HashMap<u64, Vec<u64>> = HashMap::with_capacity(256);
    let mut constituents: HashMap<u64, ConstituentRecord> = HashMap::with_capacity(2048);
    // IFC4: IfcMaterialProfileSet → list of IfcMaterialProfile. Used
    // for structural elements (steel beam with HEA200 profile +
    // material grade). The profile shape itself (IfcIShapeProfileDef
    // etc.) is parsed by the mesh module, not here — we only carry
    // the material binding.
    let mut profile_sets: HashMap<u64, Vec<u64>> = HashMap::with_capacity(256);
    let mut profiles: HashMap<u64, ConstituentRecord> = HashMap::with_capacity(1024);

    // Collect rel-pairs: (related_object_step_ids, relating_material_ref)
    let mut rel_pairs: Vec<(u64, u64)> = Vec::with_capacity(8192);
    // Type inheritance (GH #165) — the path psets.rs and quantities.rs
    // have had since GH #36 / #45. `IfcRelAssociatesMaterial` very
    // commonly binds the material to the *type* (IfcWallType) rather
    // than to each occurrence; without this pass every occurrence of
    // such a type reported ZERO materials, silently.
    let mut product_to_type: HashMap<u64, u64> = HashMap::with_capacity(16_384);
    // Step ids of IfcTypeObject / IfcXxxType entities, so a rel pair
    // whose RelatedObject is a type can be routed to the inheritance
    // map instead of being dropped for "not a product".
    let mut type_object_ids: std::collections::HashSet<u64> =
        std::collections::HashSet::with_capacity(512);

    for (step_id, type_name, args) in table.iter() {
        if type_name.eq_ignore_ascii_case(b"IFCMATERIAL") {
            // IFC2X3: (Name)
            // IFC4:   (Name, Description, Category)
            let fields = split_top_level_args(args);
            let name = string_at(&fields, 0);
            let category = string_at(&fields, 2);
            materials.insert(step_id, MaterialRecord { name, category });
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALLAYER") {
            // IFC2X3: (Material, LayerThickness, IsVentilated)
            // IFC4:   (Material, LayerThickness, IsVentilated, Name, Description, Category, Priority)
            let fields = split_top_level_args(args);
            let material_ref = match fields.first().copied().map(parse_field) {
                Some(Field::Ref(id)) => Some(id),
                _ => None,
            };
            // IFC stores LayerThickness in the project's linear unit;
            // normalize to mm via the indexer-derived `unit_scale`
            // (raw-to-metres factor) so the output column matches its name.
            let thickness = number_at(&fields, 1).map(|t| t * unit_scale * 1000.0);
            // IFC4 layer-name overrides Material.Name when present.
            let name_override = string_at(&fields, 3);
            let category_override = string_at(&fields, 5);
            layers.insert(
                step_id,
                LayerRecord {
                    material_ref,
                    thickness_mm: thickness,
                    name_override,
                    category_override,
                },
            );
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALLAYERSET") {
            // (MaterialLayers, LayerSetName, ...)
            let fields = split_top_level_args(args);
            layer_sets.insert(step_id, ref_list_at(&fields, 0));
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALLAYERSETUSAGE") {
            // (ForLayerSet, LayerSetDirection, DirectionSense, OffsetFromReferenceLine)
            let fields = split_top_level_args(args);
            if let Some(Field::Ref(id)) = fields.first().copied().map(parse_field) {
                layer_set_usages.insert(step_id, id);
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALLIST") {
            // (Materials: LIST OF IfcMaterial)
            let fields = split_top_level_args(args);
            material_lists.insert(step_id, ref_list_at(&fields, 0));
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALCONSTITUENTSET") {
            // IFC4: (Name, Description, MaterialConstituents, Category)
            // MaterialConstituents at arg index 2 is a LIST of
            // IfcMaterialConstituent refs.
            let fields = split_top_level_args(args);
            constituent_sets.insert(step_id, ref_list_at(&fields, 2));
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALCONSTITUENT") {
            // IFC4: (Name, Description, Material, Fraction, Category)
            let fields = split_top_level_args(args);
            let name_override = string_at(&fields, 0);
            let material_ref = match fields.get(2).copied().map(parse_field) {
                Some(Field::Ref(id)) => Some(id),
                _ => None,
            };
            let fraction = number_at(&fields, 3);
            let category_override = string_at(&fields, 4);
            constituents.insert(
                step_id,
                ConstituentRecord {
                    material_ref,
                    name_override,
                    category_override,
                    fraction,
                },
            );
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALPROFILESET") {
            // IFC4: (Name, Description, MaterialProfiles, CompositeProfile)
            let fields = split_top_level_args(args);
            profile_sets.insert(step_id, ref_list_at(&fields, 2));
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALPROFILE") {
            // IFC4: (Name, Description, Material, Profile, Priority, Category)
            // The Profile (arg 3) is an IfcProfileDef ref carrying the
            // cross-section shape — parsed by the mesh extruder, not
            // by us. We carry only the material binding plus name and
            // category overrides, same shape as IfcMaterialConstituent.
            // Priority (arg 4) is intentionally dropped — it's a
            // stacking order for composite profiles, not a fraction,
            // and we don't yet have a column for it.
            let fields = split_top_level_args(args);
            let name_override = string_at(&fields, 0);
            let material_ref = match fields.get(2).copied().map(parse_field) {
                Some(Field::Ref(id)) => Some(id),
                _ => None,
            };
            let category_override = string_at(&fields, 5);
            profiles.insert(
                step_id,
                ConstituentRecord {
                    material_ref,
                    name_override,
                    category_override,
                    fraction: None,
                },
            );
        } else if type_name.eq_ignore_ascii_case(b"IFCMATERIALPROFILESETUSAGE") {
            // IFC4: (ForProfileSet, CardinalPoint, ReferenceExtent)
            // Same indirection pattern as IfcMaterialLayerSetUsage —
            // the actual profile set lives at arg 0. Reuse the
            // layer-set-usage map so the second pass's redirect logic
            // doesn't need to special-case this; the usage→set wrap
            // is identical.
            let fields = split_top_level_args(args);
            if let Some(Field::Ref(id)) = fields.first().copied().map(parse_field) {
                layer_set_usages.insert(step_id, id);
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCRELASSOCIATESMATERIAL") {
            // (GlobalId, OwnerHistory, Name, Description, RelatedObjects, RelatingMaterial)
            let fields = split_top_level_args(args);
            let relating = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            let relateds = match fields.get(4).copied().map(parse_field) {
                Some(Field::List(body)) => parse_ref_list(body),
                Some(Field::Ref(id)) => vec![id],
                _ => continue,
            };
            for obj_id in relateds {
                rel_pairs.push((obj_id, relating));
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCRELDEFINESBYTYPE") {
            // (GlobalId, OwnerHistory, Name, Description, RelatedObjects,
            //  RelatingType). Same shape as the psets / quantities
            // readers — fan out N products per relation (GH #165).
            let fields = split_top_level_args(args);
            let type_id = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(id)) => id,
                _ => continue,
            };
            let relateds = match fields.get(4).copied().map(parse_field) {
                Some(Field::List(body)) => parse_ref_list(body),
                Some(Field::Ref(id)) => vec![id],
                _ => continue,
            };
            for obj_id in relateds {
                product_to_type.insert(obj_id, type_id);
            }
        } else if is_type_object(type_name) {
            type_object_ids.insert(step_id);
        }
    }

    let index = MaterialIndex {
        materials,
        layer_sets,
        layer_set_usages,
        layers,
        material_lists,
        constituent_sets,
        constituents,
        profile_sets,
        profiles,
    };

    let mut out = MaterialTable::default();

    // (type_step_id → [relating_material_ref]). Filled from the rel
    // pairs whose RelatedObject is a type rather than a product; those
    // pairs used to fall out of the loop at the `product_step_to_guid`
    // miss and take every occurrence's material with them.
    let mut type_materials: HashMap<u64, Vec<u64>> = HashMap::with_capacity(256);
    // Products that got at least one directly-associated material.
    // Instance wins on collision, exactly as in psets.rs.
    let mut has_instance_material: std::collections::HashSet<u64> =
        std::collections::HashSet::with_capacity(product_step_to_guid.len());

    // Instance pass. The two branches are independent on purpose: some
    // callers hand us a `product_step_to_guid` that also contains type
    // objects (`lib.rs::build_guid_index` keys on "any IFC entity whose
    // first attribute is a 22-char GUID"), so "not a product" is NOT a
    // reliable test for "is a type". Route on the entity class instead,
    // and keep emitting the type's own row where it was emitted before.
    for (obj_step_id, relating_id) in &rel_pairs {
        if type_object_ids.contains(obj_step_id) {
            type_materials
                .entry(*obj_step_id)
                .or_default()
                .push(*relating_id);
        }
        if let Some(guid) = product_step_to_guid.get(obj_step_id) {
            index.emit(&mut out, guid, *relating_id, "instance");
            has_instance_material.insert(*obj_step_id);
        }
        // Anything else (a group, a rel pointing at an entity we don't
        // resolve) stays skipped, as before.
    }

    // Type-inheritance pass. Sorted by product step id so the emitted
    // row order is deterministic — the HashMap-iteration determinism
    // trap of GH #152, avoided here by construction.
    if !type_materials.is_empty() && !product_to_type.is_empty() {
        let mut inherit: Vec<(u64, u64)> = product_to_type
            .iter()
            .map(|(product, type_id)| (*product, *type_id))
            .collect();
        inherit.sort_unstable();
        for (product_step_id, type_step_id) in &inherit {
            if has_instance_material.contains(product_step_id) {
                continue; // instance-declared material shadows the type's
            }
            let guid = match product_step_to_guid.get(product_step_id) {
                Some(g) => g,
                None => continue,
            };
            let relating_ids = match type_materials.get(type_step_id) {
                Some(v) => v,
                None => continue,
            };
            for relating_id in relating_ids {
                index.emit(&mut out, guid, *relating_id, "type");
            }
        }
    }

    out
}

/// Every material-container map, so the resolution of one
/// `RelatingMaterial` ref is written once and used by both the instance
/// pass and the type-inheritance pass (GH #165).
struct MaterialIndex {
    materials: HashMap<u64, MaterialRecord>,
    layer_sets: HashMap<u64, Vec<u64>>,
    layer_set_usages: HashMap<u64, u64>,
    layers: HashMap<u64, LayerRecord>,
    material_lists: HashMap<u64, Vec<u64>>,
    constituent_sets: HashMap<u64, Vec<u64>>,
    constituents: HashMap<u64, ConstituentRecord>,
    profile_sets: HashMap<u64, Vec<u64>>,
    profiles: HashMap<u64, ConstituentRecord>,
}

impl MaterialIndex {
    /// Resolve `relating_id` against each known material container type
    /// and push the resulting row(s). Always emits at least one row —
    /// an unresolvable ref becomes a `role = "unknown"` marker so the
    /// association is visible rather than dropped.
    fn emit(&self, out: &mut MaterialTable, guid: &str, relating_id: u64, source: &'static str) {
        if let Some(mat) = self.materials.get(&relating_id) {
            push_row(
                out,
                guid,
                "direct",
                -1,
                mat.name.clone(),
                None,
                mat.category.clone(),
                None,
                source,
            );
            return;
        }
        if let Some(list) = self.material_lists.get(&relating_id) {
            for (i, mid) in list.iter().enumerate() {
                if let Some(mat) = self.materials.get(mid) {
                    push_row(
                        out,
                        guid,
                        "list",
                        i as i32,
                        mat.name.clone(),
                        None,
                        mat.category.clone(),
                        None,
                        source,
                    );
                }
            }
            return;
        }
        if let Some(constituent_ids) = self.constituent_sets.get(&relating_id) {
            for (i, cid) in constituent_ids.iter().enumerate() {
                if let Some(c) = self.constituents.get(cid) {
                    let mat = c.material_ref.and_then(|mid| self.materials.get(&mid));
                    let name = c
                        .name_override
                        .clone()
                        .or_else(|| mat.and_then(|m| m.name.clone()));
                    let category = c
                        .category_override
                        .clone()
                        .or_else(|| mat.and_then(|m| m.category.clone()));
                    push_row(
                        out,
                        guid,
                        "constituent",
                        i as i32,
                        name,
                        None,
                        category,
                        c.fraction,
                        source,
                    );
                }
            }
            return;
        }
        // Profile-set lookup. The relating ref may point at an
        // IfcMaterialProfileSetUsage; the layer_set_usages map (now
        // doubling as "any *_Usage indirection") resolves through.
        let pset_id = self
            .layer_set_usages
            .get(&relating_id)
            .copied()
            .unwrap_or(relating_id);
        if let Some(profile_ids) = self.profile_sets.get(&pset_id) {
            for (i, pid) in profile_ids.iter().enumerate() {
                if let Some(p) = self.profiles.get(pid) {
                    let mat = p.material_ref.and_then(|mid| self.materials.get(&mid));
                    let name = p
                        .name_override
                        .clone()
                        .or_else(|| mat.and_then(|m| m.name.clone()));
                    let category = p
                        .category_override
                        .clone()
                        .or_else(|| mat.and_then(|m| m.category.clone()));
                    push_row(
                        out, guid, "profile", i as i32, name, None, category, None, source,
                    );
                }
            }
            return;
        }
        // IfcMaterialLayerSetUsage → IfcMaterialLayerSet.
        let lset_id = self
            .layer_set_usages
            .get(&relating_id)
            .copied()
            .unwrap_or(relating_id);
        if let Some(layer_ids) = self.layer_sets.get(&lset_id) {
            for (i, lid) in layer_ids.iter().enumerate() {
                if let Some(layer) = self.layers.get(lid) {
                    let mat = layer.material_ref.and_then(|mid| self.materials.get(&mid));
                    let name = layer
                        .name_override
                        .clone()
                        .or_else(|| mat.and_then(|m| m.name.clone()));
                    let category = layer
                        .category_override
                        .clone()
                        .or_else(|| mat.and_then(|m| m.category.clone()));
                    push_row(
                        out,
                        guid,
                        "layer",
                        i as i32,
                        name,
                        layer.thickness_mm,
                        category,
                        None,
                        source,
                    );
                }
            }
            return;
        }
        // Unknown relating type — record the GUID with a placeholder so
        // the row count reflects reality.
        push_row(out, guid, "unknown", -1, None, None, None, None, source);
    }
}

/// Detect an IfcTypeObject / IfcXxxType subclass by entity-name suffix.
/// Byte-for-byte the rule used by `extractors/psets.rs`,
/// `extractors/quantities.rs` and `indexer::index`, so all four loops
/// agree on what counts as a type.
fn is_type_object(t: &[u8]) -> bool {
    let suffix_ok = t.len() > 7
        && t[..3].eq_ignore_ascii_case(b"IFC")
        && t[t.len() - 4..].eq_ignore_ascii_case(b"TYPE");
    let ifc2x3_style =
        t.eq_ignore_ascii_case(b"IFCDOORSTYLE") || t.eq_ignore_ascii_case(b"IFCWINDOWSTYLE");
    let bare_base =
        t.eq_ignore_ascii_case(b"IFCTYPEPRODUCT") || t.eq_ignore_ascii_case(b"IFCTYPEOBJECT");
    suffix_ok || ifc2x3_style || bare_base
}

#[allow(clippy::too_many_arguments)]
fn push_row(
    out: &mut MaterialTable,
    guid: &str,
    role: &'static str,
    layer_index: i32,
    name: Option<String>,
    thickness: Option<f64>,
    category: Option<String>,
    fraction: Option<f64>,
    source: &'static str,
) {
    out.guid.push(guid.to_string());
    out.role.push(role);
    out.layer_index.push(layer_index);
    out.material_name.push(name);
    out.layer_thickness_mm.push(thickness);
    out.category.push(category);
    out.fraction.push(fraction);
    out.source.push(source);
}

struct MaterialRecord {
    name: Option<String>,
    category: Option<String>,
}

struct LayerRecord {
    material_ref: Option<u64>,
    thickness_mm: Option<f64>,
    name_override: Option<String>,
    category_override: Option<String>,
}

struct ConstituentRecord {
    material_ref: Option<u64>,
    name_override: Option<String>,
    category_override: Option<String>,
    /// IfcMaterialConstituent.Fraction. None on IfcMaterialProfile,
    /// which reuses this struct but has Priority (an integer 0-100)
    /// at a different arg index that we don't currently capture.
    fraction: Option<f64>,
}

fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    match parse_field(fields.get(idx)?) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

fn number_at(fields: &[&[u8]], idx: usize) -> Option<f64> {
    match parse_field(fields.get(idx)?) {
        Field::Number(n) => Some(n),
        _ => None,
    }
}

fn ref_list_at(fields: &[&[u8]], idx: usize) -> Vec<u64> {
    match fields.get(idx).copied().map(parse_field) {
        Some(Field::List(body)) => parse_ref_list(body),
        _ => Vec::new(),
    }
}

fn parse_ref_list(body: &[u8]) -> Vec<u64> {
    split_top_level_args(body)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Ref(id) => Some(id),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build a tiny IFC4 buffer with one wall whose material assignment
    /// is a single-layer IfcMaterialLayerSet. The IFC project's
    /// LENGTHUNIT is parameterised so we can verify the same raw
    /// `LayerThickness` token (`200.`) lands as 200 mm on a mm-authored
    /// file and 200 000 mm on a metre-authored file. The "200" reads
    /// as 200 mm in a mm file and as 200 m in a metre file, so the
    /// 1000× difference is the unit normalisation working correctly.
    fn build_buf(prefix: &str, raw_thickness: f32) -> String {
        format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('mat_unit.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,{prefix},.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);
#20=IFCMATERIAL('Concrete');
#21=IFCMATERIALLAYER(#20,{raw_thickness},$);
#22=IFCMATERIALLAYERSET((#21),'WallSet');
#30=IFCRELASSOCIATESMATERIAL('2Rel00000000000000001',$,$,$,(#10),#22);
ENDSEC;
END-ISO-10303-21;
"#
        )
    }

    fn run(buf: &str, unit_scale: f64) -> Vec<f64> {
        let table = crate::entity_table::EntityTable::build(buf.as_bytes());
        let mut step_to_guid: HashMap<u64, String> = HashMap::new();
        for (sid, _t, args) in table.iter() {
            let fields = split_top_level_args(args);
            if let Some(first) = fields.first() {
                if let Field::String(s) = parse_field(first) {
                    if s.len() == 22 {
                        step_to_guid.insert(sid, s);
                    }
                }
            }
        }
        let mat = build(&table, &step_to_guid, unit_scale);
        mat.layer_thickness_mm.iter().flatten().copied().collect()
    }

    #[test]
    fn layer_thickness_normalised_to_mm_for_mm_file() {
        // Millimetre file: unit_scale = 0.001 m/unit. Raw "200" reads
        // as 200 mm. After scaling, output should be 200 mm.
        let buf = build_buf(".MILLI.", 200.0);
        let v = run(&buf, 0.001);
        assert_eq!(v.len(), 1, "expected one material layer, got {}", v.len());
        assert!(
            (v[0] - 200.0).abs() < 1e-6,
            "expected 200 mm for mm file, got {}",
            v[0]
        );
    }

    #[test]
    fn layer_thickness_normalised_to_mm_for_metres_file() {
        // Metres file: unit_scale = 1.0 m/unit. Raw "0.2" reads as
        // 0.2 m = 200 mm. After scaling, output should be 200 mm.
        let buf = build_buf("$", 0.2);
        let v = run(&buf, 1.0);
        assert_eq!(v.len(), 1);
        assert!(
            (v[0] - 200.0).abs() < 1e-6,
            "expected 200 mm for metres file with 0.2 m raw, got {}",
            v[0]
        );
    }

    #[test]
    fn pre_fix_metres_file_would_have_returned_raw_value() {
        // Sanity check on the test setup: feeding unit_scale = 1.0
        // with raw = 200 (a metres file naming the raw value 200)
        // gives 200 * 1.0 * 1000 = 200 000 mm = 200 m. Reasonable
        // for a typical "huge length" misread in metre files. Pre-fix
        // the column would have stored raw (200) — silently a 1000x
        // off for 0.001 / 1000 unit_scales.
        let buf = build_buf("$", 200.0);
        let v = run(&buf, 1.0);
        assert!(
            (v[0] - 200_000.0).abs() < 1e-3,
            "raw 200 in a metre file scales to 200 000 mm, got {}",
            v[0]
        );
    }

    /// IfcMaterialConstituentSet support — IFC4 schema used heavily on
    /// MEP / composite-element exports. Pre-fix this fell through to
    /// the "unknown" fallback row, so the substrate saw "the product
    /// has materials" but couldn't enumerate them.
    fn run_full_table(buf: &str) -> MaterialTable {
        let table = crate::entity_table::EntityTable::build(buf.as_bytes());
        let mut step_to_guid: HashMap<u64, String> = HashMap::new();
        for (sid, _t, args) in table.iter() {
            let fields = split_top_level_args(args);
            if let Some(first) = fields.first() {
                if let Field::String(s) = parse_field(first) {
                    if s.len() == 22 {
                        step_to_guid.insert(sid, s);
                    }
                }
            }
        }
        build(&table, &step_to_guid, 1.0)
    }

    #[test]
    fn constituent_set_resolves_one_row_per_constituent() {
        let buf = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('cset.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCBEAM('1Beam00000000000000001',$,'B',$,$,$,$,'t',.STANDARD.);
#20=IFCMATERIAL('Concrete C30/37',$,'Structural');
#21=IFCMATERIAL('Rebar B500B',$,'Reinforcement');
#22=IFCMATERIALCONSTITUENT('Concrete body',$,#20,0.97,'Bulk');
#23=IFCMATERIALCONSTITUENT('Rebar mass',$,#21,0.03,'Reinforcement');
#24=IFCMATERIALCONSTITUENTSET('RC Beam C30/37 mix',$,(#22,#23),$);
#25=IFCRELASSOCIATESMATERIAL('2Rel000000000000000001',$,$,$,(#10),#24);
ENDSEC;
END-ISO-10303-21;
"#;
        let t = run_full_table(buf);
        assert_eq!(t.len(), 2, "expected 2 constituent rows, got {}", t.len());

        // All rows attribute to the beam, with role="constituent".
        for i in 0..t.len() {
            assert_eq!(t.guid[i], "1Beam00000000000000001");
            assert_eq!(t.role[i], "constituent");
        }
        // The name override on the constituent (e.g. "Concrete body")
        // takes precedence over the referenced material name, mirroring
        // the IfcMaterialLayer behaviour.
        let names: std::collections::HashSet<&str> = t
            .material_name
            .iter()
            .filter_map(|n| n.as_deref())
            .collect();
        assert!(names.contains("Concrete body"));
        assert!(names.contains("Rebar mass"));
        // Categories also override.
        let cats: std::collections::HashSet<&str> =
            t.category.iter().filter_map(|c| c.as_deref()).collect();
        assert!(cats.contains("Bulk"));
        assert!(cats.contains("Reinforcement"));

        // Fraction column: 0.97 concrete + 0.03 rebar — the whole point
        // of constituent sets vs. plain material lists. Pre-fix this
        // information was discarded.
        let fractions: Vec<f64> = t.fraction.iter().filter_map(|f| *f).collect();
        assert_eq!(fractions.len(), 2);
        assert!(fractions.iter().any(|&f| (f - 0.97).abs() < 1e-6));
        assert!(fractions.iter().any(|&f| (f - 0.03).abs() < 1e-6));
        // Fractions across a constituent set should sum to ~1.0.
        let sum: f64 = fractions.iter().sum();
        assert!(
            (sum - 1.0).abs() < 1e-6,
            "constituent fractions should sum to 1.0, got {sum}"
        );
    }

    #[test]
    fn constituent_with_no_overrides_falls_back_to_material() {
        // Constituent declared without Name / Category overrides ($) —
        // both must fall through to the referenced IfcMaterial's
        // values (same pattern as IfcMaterialLayer).
        let buf = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('cset2.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCBEAM('1Beam00000000000000001',$,'B',$,$,$,$,'t',.STANDARD.);
#20=IFCMATERIAL('Steel S355','High-strength','Structural');
#22=IFCMATERIALCONSTITUENT($,$,#20,1.0,$);
#24=IFCMATERIALCONSTITUENTSET('Single-material set',$,(#22),$);
#25=IFCRELASSOCIATESMATERIAL('2Rel000000000000000001',$,$,$,(#10),#24);
ENDSEC;
END-ISO-10303-21;
"#;
        let t = run_full_table(buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.material_name[0].as_deref(), Some("Steel S355"));
        assert_eq!(t.category[0].as_deref(), Some("Structural"));
    }

    #[test]
    fn profile_set_resolves_via_usage_indirection() {
        // Steel beam with an IfcMaterialProfileSetUsage → IfcMaterialProfileSet
        // → IfcMaterialProfile chain. The most common structural-export
        // pattern (one profile = one material on a structural member).
        let buf = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('profile.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCBEAM('1Beam00000000000000001',$,'B',$,$,$,$,'t',.STANDARD.);
#20=IFCMATERIAL('Steel S355','Hot-rolled','Structural');
#21=IFCISHAPEPROFILEDEF(.AREA.,'HEA200',$,200.,200.,6.5,10.0,18.0);
#22=IFCMATERIALPROFILE('Web+Flanges',$,#20,#21,$,'Section');
#23=IFCMATERIALPROFILESET('HEA200-S355',$,(#22),$);
#24=IFCMATERIALPROFILESETUSAGE(#23,1,$);
#25=IFCRELASSOCIATESMATERIAL('2Rel000000000000000001',$,$,$,(#10),#24);
ENDSEC;
END-ISO-10303-21;
"#;
        let t = run_full_table(buf);
        assert_eq!(t.len(), 1, "expected 1 profile row, got {}", t.len());
        assert_eq!(t.role[0], "profile");
        assert_eq!(t.guid[0], "1Beam00000000000000001");
        // Constituent's `Name` ("Web+Flanges") overrides the material's
        // name, same precedence rule as layers / constituents.
        assert_eq!(t.material_name[0].as_deref(), Some("Web+Flanges"));
        assert_eq!(t.category[0].as_deref(), Some("Section"));
    }

    #[test]
    fn profile_set_resolves_without_usage_indirection() {
        // Some exports skip IfcMaterialProfileSetUsage and reference
        // the profile set directly from IfcRelAssociatesMaterial. Both
        // paths must work.
        let buf = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('profile2.ifc','2026-05-26T00:00:00',('test'),('test'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCCOLUMN('1Col000000000000000001',$,'C',$,$,$,$,'t',.STANDARD.);
#20=IFCMATERIAL('Concrete C40/50',$,'Structural');
#21=IFCCIRCLEPROFILEDEF(.AREA.,'Round-400',$,200.);
#22=IFCMATERIALPROFILE($,$,#20,#21,$,$);
#23=IFCMATERIALPROFILESET('Round Column',$,(#22),$);
#24=IFCRELASSOCIATESMATERIAL('2Rel000000000000000001',$,$,$,(#10),#23);
ENDSEC;
END-ISO-10303-21;
"#;
        let t = run_full_table(buf);
        assert_eq!(t.len(), 1);
        assert_eq!(t.role[0], "profile");
        // No override on the IfcMaterialProfile → falls through to the
        // referenced IfcMaterial's Name + Category.
        assert_eq!(t.material_name[0].as_deref(), Some("Concrete C40/50"));
        assert_eq!(t.category[0].as_deref(), Some("Structural"));
    }
}

#[cfg(test)]
mod inheritance_tests {
    use super::*;
    use std::collections::HashMap;

    /// Only OCCURRENCES are products. A type object is not, which is
    /// exactly why a type-bound material used to vanish (GH #165).
    fn products_only(table: &crate::entity_table::EntityTable) -> HashMap<u64, String> {
        let mut m = HashMap::new();
        for (sid, t, args) in table.iter() {
            if !t.eq_ignore_ascii_case(b"IFCWALL") {
                continue;
            }
            let fields = split_top_level_args(args);
            if let Some(f) = fields.first() {
                if let Field::String(s) = parse_field(f) {
                    m.insert(sid, s);
                }
            }
        }
        m
    }

    const HEAD: &str = "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\n\
FILE_SCHEMA(('IFC4'));\nENDSEC;\nDATA;\n";

    fn run(data: &str) -> MaterialTable {
        let buf = format!("{HEAD}{data}ENDSEC;\nEND-ISO-10303-21;\n");
        let table = crate::entity_table::EntityTable::build(buf.as_bytes());
        let products = products_only(&table);
        build(&table, &products, 0.001)
    }

    /// `IFCRELASSOCIATESMATERIAL(…,(#40),#22)` with #40 an IfcWallType:
    /// every occurrence of that type must get the layer rows, tagged
    /// `source = "type"`.
    #[test]
    fn type_bound_material_reaches_occurrences() {
        let t = run(
            "#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);\n\
             #11=IFCWALL('1Wall00000000000000002',$,'W',$,$,$,$,'t',.STANDARD.);\n\
             #40=IFCWALLTYPE('3Type00000000000000001',$,'WT',$,$,$,$,$,$,.STANDARD.);\n\
             #41=IFCRELDEFINESBYTYPE('4Rel000000000000000001',$,$,$,(#10,#11),#40);\n\
             #20=IFCMATERIAL('Concrete');\n\
             #21=IFCMATERIALLAYER(#20,200.,$);\n\
             #22=IFCMATERIALLAYERSET((#21),'WallSet');\n\
             #30=IFCRELASSOCIATESMATERIAL('2Rel00000000000000001',$,$,$,(#40),#22);\n",
        );
        assert_eq!(t.len(), 2, "both occurrences inherit the type's material");
        assert!(t.source.iter().all(|s| *s == "type"));
        assert!(t
            .material_name
            .iter()
            .all(|n| n.as_deref() == Some("Concrete")));
        assert!((t.layer_thickness_mm[0].unwrap() - 200.0).abs() < 1e-9);
    }

    /// A directly associated material shadows the type's entirely.
    #[test]
    fn instance_material_shadows_type() {
        let t = run(
            "#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);\n\
             #40=IFCWALLTYPE('3Type00000000000000001',$,'WT',$,$,$,$,$,$,.STANDARD.);\n\
             #41=IFCRELDEFINESBYTYPE('4Rel000000000000000001',$,$,$,(#10),#40);\n\
             #20=IFCMATERIAL('Concrete');\n\
             #23=IFCMATERIAL('Brick');\n\
             #30=IFCRELASSOCIATESMATERIAL('2Rel00000000000000001',$,$,$,(#40),#20);\n\
             #31=IFCRELASSOCIATESMATERIAL('2Rel00000000000000002',$,$,$,(#10),#23);\n",
        );
        assert_eq!(t.len(), 1, "instance wins; no type row emitted");
        assert_eq!(t.material_name[0].as_deref(), Some("Brick"));
        assert_eq!(t.source[0], "instance");
    }

    /// Occurrence-only files must be untouched by the new pass.
    #[test]
    fn instance_only_file_unchanged() {
        let t = run(
            "#10=IFCWALL('1Wall00000000000000001',$,'W',$,$,$,$,'t',.STANDARD.);\n\
             #20=IFCMATERIAL('Concrete');\n\
             #30=IFCRELASSOCIATESMATERIAL('2Rel00000000000000001',$,$,$,(#10),#20);\n",
        );
        assert_eq!(t.len(), 1);
        assert_eq!(t.role[0], "direct");
        assert_eq!(t.source[0], "instance");
    }
}
