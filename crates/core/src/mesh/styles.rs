//! IfcSurfaceStyle → RGBA colour extraction (GH #3).
//!
//! Two parallel indexes built from the entity table in one pass:
//!
//! * **`item`** — `IfcRepresentationItem step_id → RGBA`. Resolved
//!   through the direct chain
//!   `IfcStyledItem.Item → IfcStyledItem.Styles → IfcPresentationStyleAssignment?
//!   → IfcSurfaceStyle.Styles → IfcSurfaceStyleRendering|Shading.SurfaceColour
//!   → IfcColourRgb`. This is the dominant authoring pattern in IFC2x3
//!   exporters (Revit emits one `IfcStyledItem` per representation item;
//!   Sannergata ARK_E has ≈ 5 200 of them).
//!
//! * **`product`** — `IfcProduct step_id → RGBA`. Resolved through the
//!   material chain `IfcRelAssociatesMaterial.RelatedObjects[i] →
//!   IfcMaterial → (reverse map) IfcMaterialDefinitionRepresentation
//!   where RepresentedMaterial == material → Representations[] of
//!   IfcStyledRepresentation → Items[] of IfcStyledItem → … → RGBA`.
//!   This is the fallback for products whose representation items don't
//!   carry direct `IfcStyledItem` styling. Layered/usage materials
//!   (`IfcMaterialLayerSetUsage` etc.) are NOT walked yet — they fall
//!   through to the entity palette.
//!
//! Alpha is `1 − Transparency` when an `IfcSurfaceStyleRendering`
//! supplies it; for `IfcSurfaceStyleShading` alpha defaults to 1.0.
//! Out-of-range colour components are clamped to `[0, 1]`.
//!
//! Cost: one extra linear pass over the entity table, building a few
//! small HashMaps. Roughly proportional to the number of `IfcStyledItem`
//! entities (≈ a few thousand on a typical Revit export). No geometry
//! parsing, no recursion — index lookups by step_id only.

use std::collections::HashMap;

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

/// Per-mesh-item and per-product colour indexes.
#[derive(Debug, Default, Clone)]
pub struct StyleIndex {
    /// `IfcRepresentationItem step_id → RGBA` in linear-space `[0, 1]`.
    pub item: HashMap<u64, [f32; 4]>,
    /// `IfcProduct step_id → RGBA` (material chain).
    pub product: HashMap<u64, [f32; 4]>,
}

impl StyleIndex {
    /// Walk the entity table once; build both indexes.
    pub fn build(table: &EntityTable) -> Self {
        let mut idx = StyleIndex::default();

        // Phase 1 — resolve every `IfcSurfaceStyle` to a single RGBA.
        // Done by step_id so later phases can index into it cheaply.
        let mut style_color: HashMap<u64, [f32; 4]> = HashMap::new();
        for &id in table.order() {
            let Some((type_name, _args)) = table.get(id) else { continue };
            if type_name.eq_ignore_ascii_case(b"IFCSURFACESTYLE") {
                if let Some(c) = resolve_surface_style(table, id) {
                    style_color.insert(id, c);
                }
            }
        }
        if style_color.is_empty() {
            return idx;
        }

        // Phase 2 — for each `IfcStyledItem`, find its first reachable
        // `IfcSurfaceStyle`, and pin its colour against the Item ref.
        // Walks two indirections (PSA → SurfaceStyle, or SurfaceStyle
        // directly per IFC4).
        for &id in table.order() {
            let Some((type_name, args)) = table.get(id) else { continue };
            if !type_name.eq_ignore_ascii_case(b"IFCSTYLEDITEM") {
                continue;
            }
            let fields = split_top_level_args(args);
            // IfcStyledItem(Item, Styles[], Name)
            let item_id = match fields.first().copied().map(parse_field) {
                Some(Field::Ref(rid)) => rid,
                _ => continue,
            };
            let styles_body = match fields.get(1).copied().map(parse_field) {
                Some(Field::List(b)) => b,
                _ => continue,
            };
            let colour = first_surface_style_colour(table, styles_body, &style_color);
            if let Some(c) = colour {
                // Bind the colour to every leaf step_id reachable from
                // `item_id`. IFC2x3 MagiCAD-style exports (and any
                // tool that styles at the instance level) write
                // `IfcStyledItem(IfcMappedItem(...), Styles, Name)`,
                // but the mesh pipeline carries the INNER leaf item's
                // step_id on `InstancePart.rep_step_id`. Binding only
                // to `item_id` would miss everything in those files —
                // GH #55. So we follow IfcMappedItem indirection
                // (recursively) to all reachable leaf items and bind
                // the same colour to each.
                expand_styled_target(table, item_id, c, &mut idx.item);
            }
        }

        // Phase 3 — build the material → colour map from
        // `IfcMaterialDefinitionRepresentation` (reverse-keyed by
        // RepresentedMaterial, walks Representations → IfcStyledRepresentation
        // → Items[] → first IfcStyledItem → colour).
        let mut material_color: HashMap<u64, [f32; 4]> = HashMap::new();
        for &id in table.order() {
            let Some((type_name, args)) = table.get(id) else { continue };
            if !type_name.eq_ignore_ascii_case(b"IFCMATERIALDEFINITIONREPRESENTATION") {
                continue;
            }
            // IfcMaterialDefinitionRepresentation(Name, Description, Representations[], RepresentedMaterial)
            let fields = split_top_level_args(args);
            let reps_body = match fields.get(2).copied().map(parse_field) {
                Some(Field::List(b)) => b,
                _ => continue,
            };
            let mat_id = match fields.get(3).copied().map(parse_field) {
                Some(Field::Ref(rid)) => rid,
                _ => continue,
            };
            let colour = first_styled_representation_colour(table, reps_body, &idx.item);
            if let Some(c) = colour {
                material_color.insert(mat_id, c);
            }
        }

        // Phase 4 — for each `IfcRelAssociatesMaterial`, fan the
        // RelatingMaterial's colour out to every RelatedObject (the
        // product step_ids). Only handles direct `IfcMaterial`; layered
        // / profile / usage materials are skipped here.
        for &id in table.order() {
            let Some((type_name, args)) = table.get(id) else { continue };
            if !type_name.eq_ignore_ascii_case(b"IFCRELASSOCIATESMATERIAL") {
                continue;
            }
            let fields = split_top_level_args(args);
            // IfcRelAssociatesMaterial(GlobalId, OwnerHistory, Name,
            //   Description, RelatedObjects[], RelatingMaterial)
            let related_body = match fields.get(4).copied().map(parse_field) {
                Some(Field::List(b)) => b,
                _ => continue,
            };
            let relating = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(rid)) => rid,
                _ => continue,
            };
            let Some(&colour) = material_color.get(&relating) else { continue };
            for f in split_top_level_args(related_body) {
                if let Field::Ref(prod_id) = parse_field(f) {
                    idx.product.insert(prod_id, colour);
                }
            }
        }

        idx
    }

    /// Lookup priority: per-item style → product material style → None.
    /// Caller is expected to fall back to a per-entity palette on None.
    pub fn lookup(&self, item_id: u64, product_id: u64) -> Option<[f32; 4]> {
        self.item
            .get(&item_id)
            .copied()
            .or_else(|| self.product.get(&product_id).copied())
    }
}

/// Bind `colour` to every **leaf representation item** step_id
/// reachable from `target_id`, recursing through `IfcMappedItem` and
/// `IfcShapeRepresentation` so a style attached at the instance level
/// still lands on the inner geometry that `mesh::tessellate_one`
/// surfaces as `InstancePart.rep_step_id`. GH #55.
///
/// Cycle-guarded with a HashSet — pathological self-referencing
/// `IfcRepresentationMap` chains can't be expressed in the schema but
/// the guard keeps us from looping if a malformed file ever ships one.
fn expand_styled_target(
    table: &EntityTable,
    target_id: u64,
    colour: [f32; 4],
    out: &mut HashMap<u64, [f32; 4]>,
) {
    fn recurse(
        table: &EntityTable,
        id: u64,
        colour: [f32; 4],
        out: &mut HashMap<u64, [f32; 4]>,
        visited: &mut std::collections::HashSet<u64>,
    ) {
        if !visited.insert(id) {
            return;
        }
        let Some((t, args)) = table.get(id) else {
            return;
        };
        if t.eq_ignore_ascii_case(b"IFCMAPPEDITEM") {
            // IfcMappedItem(MappingSource, MappingTarget)
            let fields = split_top_level_args(args);
            let map_id = match fields.first().copied().map(parse_field) {
                Some(Field::Ref(rid)) => rid,
                _ => return,
            };
            // IfcRepresentationMap(MappingOrigin, MappedRepresentation)
            let Some((mt, margs)) = table.get(map_id) else {
                return;
            };
            if !mt.eq_ignore_ascii_case(b"IFCREPRESENTATIONMAP") {
                return;
            }
            let mfields = split_top_level_args(margs);
            let rep_id = match mfields.get(1).copied().map(parse_field) {
                Some(Field::Ref(rid)) => rid,
                _ => return,
            };
            recurse(table, rep_id, colour, out, visited);
            return;
        }
        if t.eq_ignore_ascii_case(b"IFCSHAPEREPRESENTATION")
            || t.eq_ignore_ascii_case(b"IFCREPRESENTATION")
        {
            // (ContextOfItems, RepresentationIdentifier, RepresentationType, Items[])
            let fields = split_top_level_args(args);
            let items_body = match fields.get(3).copied().map(parse_field) {
                Some(Field::List(b)) => b,
                _ => return,
            };
            for f in split_top_level_args(items_body) {
                if let Field::Ref(inner) = parse_field(f) {
                    recurse(table, inner, colour, out, visited);
                }
            }
            return;
        }
        // Leaf — bind the colour here. Don't overwrite if a more
        // specific styled-item already wrote one for this same id
        // (the earlier write was a closer-to-the-geometry style; this
        // expansion was at one or more wrapping levels above).
        out.entry(id).or_insert(colour);
    }

    let mut visited = std::collections::HashSet::new();
    recurse(table, target_id, colour, out, &mut visited);
}

/// Walk the styles list on an `IfcStyledItem` (or the styles list on an
/// `IfcPresentationStyleAssignment`) and return the first RGBA reachable
/// through an `IfcSurfaceStyle`.
fn first_surface_style_colour(
    table: &EntityTable,
    list_body: &[u8],
    style_color: &HashMap<u64, [f32; 4]>,
) -> Option<[f32; 4]> {
    for f in split_top_level_args(list_body) {
        let Field::Ref(rid) = parse_field(f) else { continue };
        if let Some(c) = style_color.get(&rid).copied() {
            return Some(c);
        }
        // `IfcPresentationStyleAssignment(Styles[])` — IFC2x3 wrapper.
        if let Some((t, args)) = table.get(rid) {
            if t.eq_ignore_ascii_case(b"IFCPRESENTATIONSTYLEASSIGNMENT") {
                let inner = split_top_level_args(args);
                if let Some(Field::List(body)) =
                    inner.first().copied().map(parse_field)
                {
                    if let Some(c) =
                        first_surface_style_colour(table, body, style_color)
                    {
                        return Some(c);
                    }
                }
            }
        }
    }
    None
}

/// For an `IfcMaterialDefinitionRepresentation.Representations[]` list,
/// pick the first `IfcStyledRepresentation` and follow its first
/// `IfcStyledItem` to a colour. Returns `None` if no styled rep is
/// present or its item didn't index a colour above.
fn first_styled_representation_colour(
    table: &EntityTable,
    list_body: &[u8],
    item_color: &HashMap<u64, [f32; 4]>,
) -> Option<[f32; 4]> {
    for f in split_top_level_args(list_body) {
        let Field::Ref(rid) = parse_field(f) else { continue };
        let Some((t, args)) = table.get(rid) else { continue };
        if !t.eq_ignore_ascii_case(b"IFCSTYLEDREPRESENTATION") {
            continue;
        }
        // IfcStyledRepresentation(ContextOfItems, RepresentationIdentifier,
        //   RepresentationType, Items[])
        let inner = split_top_level_args(args);
        let items_body = match inner.get(3).copied().map(parse_field) {
            Some(Field::List(b)) => b,
            _ => continue,
        };
        for itf in split_top_level_args(items_body) {
            let Field::Ref(styled_id) = parse_field(itf) else { continue };
            // `IfcStyledItem.Item` was the keying ref in phase 2 — but
            // here the styled-item is _under_ the material's styled
            // rep, and its "Item" field is typically `$` (no inner
            // representation item to bind to). The styled-item's Styles
            // list still carries the colour. Re-resolve directly.
            if let Some((it_type, it_args)) = table.get(styled_id) {
                if it_type.eq_ignore_ascii_case(b"IFCSTYLEDITEM") {
                    let it_fields = split_top_level_args(it_args);
                    let styles_body = match it_fields.get(1).copied().map(parse_field) {
                        Some(Field::List(b)) => b,
                        _ => continue,
                    };
                    // Resolve via the same surface-style mechanism. The
                    // colours are already populated into `item_color`
                    // when `IfcStyledItem.Item` was non-null, but for
                    // material-bound styled-items we re-resolve.
                    let mut style_color: HashMap<u64, [f32; 4]> = HashMap::new();
                    for sf in split_top_level_args(styles_body) {
                        if let Field::Ref(sid) = parse_field(sf) {
                            if let Some(c) = resolve_surface_style(table, sid) {
                                style_color.insert(sid, c);
                            }
                            // PSA wrap.
                            if let Some((t2, a2)) = table.get(sid) {
                                if t2.eq_ignore_ascii_case(b"IFCPRESENTATIONSTYLEASSIGNMENT") {
                                    let inner = split_top_level_args(a2);
                                    if let Some(Field::List(body)) =
                                        inner.first().copied().map(parse_field)
                                    {
                                        for psaf in split_top_level_args(body) {
                                            if let Field::Ref(ssid) = parse_field(psaf) {
                                                if let Some(c) =
                                                    resolve_surface_style(table, ssid)
                                                {
                                                    style_color.insert(ssid, c);
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if let Some(c) =
                        first_surface_style_colour(table, styles_body, &style_color)
                    {
                        return Some(c);
                    }
                }
            }
            // Suppress unused warning when the inner block returns nothing.
            let _ = item_color;
        }
    }
    None
}

/// Resolve an `IfcSurfaceStyle` to a single RGBA.
///
/// `IfcSurfaceStyle(Name, Side, Styles[])` — Styles is a SET of 1..5 of
/// `IfcSurfaceStyleElementSelect` (Shading, Rendering, WithTextures,
/// Lighting, Refraction). We pick the first Rendering (richer — has
/// Transparency) or Shading we encounter and use its `SurfaceColour`.
fn resolve_surface_style(table: &EntityTable, id: u64) -> Option<[f32; 4]> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCSURFACESTYLE") {
        return None;
    }
    let fields = split_top_level_args(args);
    let elements_body = match fields.get(2).copied().map(parse_field) {
        Some(Field::List(b)) => b,
        _ => return None,
    };
    // First Rendering wins; else first Shading.
    let mut shading_color: Option<[f32; 4]> = None;
    for f in split_top_level_args(elements_body) {
        let Field::Ref(rid) = parse_field(f) else { continue };
        let Some((t, _)) = table.get(rid) else { continue };
        if t.eq_ignore_ascii_case(b"IFCSURFACESTYLERENDERING") {
            if let Some(c) = resolve_surface_style_rendering(table, rid) {
                return Some(c);
            }
        } else if t.eq_ignore_ascii_case(b"IFCSURFACESTYLESHADING") {
            if shading_color.is_none() {
                shading_color = resolve_surface_style_shading(table, rid);
            }
        }
    }
    shading_color
}

/// `IfcSurfaceStyleRendering(SurfaceColour, Transparency, DiffuseColour?,
/// TransmissionColour?, DiffuseTransmissionColour?, ReflectionColour?,
/// SpecularColour?, SpecularHighlight?, ReflectanceMethod)` — alpha is
/// `1 - Transparency`. Out-of-range values are clamped.
fn resolve_surface_style_rendering(
    table: &EntityTable,
    id: u64,
) -> Option<[f32; 4]> {
    let (_, args) = table.get(id)?;
    let fields = split_top_level_args(args);
    let colour_id = match fields.first().copied().map(parse_field) {
        Some(Field::Ref(rid)) => rid,
        _ => return None,
    };
    let rgb = resolve_colour_rgb(table, colour_id)?;
    let transparency = match fields.get(1).copied().map(parse_field) {
        Some(Field::Number(n)) => (n as f32).clamp(0.0, 1.0),
        _ => 0.0,
    };
    Some([rgb[0], rgb[1], rgb[2], (1.0 - transparency).clamp(0.0, 1.0)])
}

/// `IfcSurfaceStyleShading(SurfaceColour, Transparency?)` — IFC4 added
/// the optional Transparency field; IFC2x3 stops at one arg. Treat
/// missing Transparency as opaque.
fn resolve_surface_style_shading(table: &EntityTable, id: u64) -> Option<[f32; 4]> {
    let (_, args) = table.get(id)?;
    let fields = split_top_level_args(args);
    let colour_id = match fields.first().copied().map(parse_field) {
        Some(Field::Ref(rid)) => rid,
        _ => return None,
    };
    let rgb = resolve_colour_rgb(table, colour_id)?;
    let transparency = match fields.get(1).copied().map(parse_field) {
        Some(Field::Number(n)) => (n as f32).clamp(0.0, 1.0),
        _ => 0.0,
    };
    Some([rgb[0], rgb[1], rgb[2], (1.0 - transparency).clamp(0.0, 1.0)])
}

/// `IfcColourRgb(Name, Red, Green, Blue)` — values in `[0, 1]`. Clamps
/// out-of-range components defensively (some exporters write 0..255).
fn resolve_colour_rgb(table: &EntityTable, id: u64) -> Option<[f32; 3]> {
    let (type_name, args) = table.get(id)?;
    if !type_name.eq_ignore_ascii_case(b"IFCCOLOURRGB") {
        return None;
    }
    let fields = split_top_level_args(args);
    let mut comps = [0.0_f32; 3];
    for i in 0..3 {
        comps[i] = match fields.get(1 + i).copied().map(parse_field) {
            Some(Field::Number(n)) => {
                let v = n as f32;
                if v > 1.5 {
                    // Some authoring tools write 0..255 instead of 0..1.
                    (v / 255.0).clamp(0.0, 1.0)
                } else {
                    v.clamp(0.0, 1.0)
                }
            }
            _ => return None,
        };
    }
    Some(comps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLED_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('styled.ifc','2026-06-04T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#100=IFCCOLOURRGB('TestRed',0.8,0.2,0.1);
#101=IFCSURFACESTYLERENDERING(#100,0.3,$,$,$,$,$,$,.NOTDEFINED.);
#102=IFCSURFACESTYLE('RedStyle',.BOTH.,(#101));
#103=IFCPRESENTATIONSTYLEASSIGNMENT((#102));
#104=IFCSTYLEDITEM(#33,(#103),$);
ENDSEC;
END-ISO-10303-21;
"#;

    #[test]
    fn item_to_color_resolves_through_psa() {
        let table = EntityTable::build(STYLED_IFC.as_bytes());
        let idx = StyleIndex::build(&table);
        // Item #33 should resolve to (0.8, 0.2, 0.1, 0.7) — alpha is
        // 1 - 0.3.
        let got = idx.item.get(&33).copied().expect("item #33 styled");
        let want = [0.8_f32, 0.2, 0.1, 0.7];
        for i in 0..4 {
            assert!(
                (got[i] - want[i]).abs() < 1e-5,
                "channel {} mismatch: got {got:?}, want {want:?}",
                i
            );
        }
    }

    #[test]
    fn empty_file_yields_empty_index() {
        let buf = b"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('x'),'2;1');
FILE_NAME('x.ifc','x',('x'),('x'),'x','x','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0',$,'p',$,$,$,$,$,$);
ENDSEC;
END-ISO-10303-21;
";
        let table = EntityTable::build(buf);
        let idx = StyleIndex::build(&table);
        assert!(idx.item.is_empty());
        assert!(idx.product.is_empty());
    }

    /// GH #55: `IfcStyledItem.Item` points at an `IfcMappedItem` (the
    /// MagiCAD pattern). The colour must propagate to the INNER
    /// representation item that the mesh pipeline keys
    /// `InstancePart.rep_step_id` on, not just the outer mapped-item
    /// step_id.
    const STYLED_MAPPED_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('styled_mapped.ifc','2026-06-04T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,.MILLI.,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#40=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#33));
#41=IFCREPRESENTATIONMAP(#6,#40);
#42=IFCCARTESIANTRANSFORMATIONOPERATOR3D($,$,#7,1.,$);
#43=IFCMAPPEDITEM(#41,#42);
#100=IFCCOLOURRGB('Green',0.1,0.7,0.2);
#101=IFCSURFACESTYLERENDERING(#100,0.0,$,$,$,$,$,$,.NOTDEFINED.);
#102=IFCSURFACESTYLE('GreenStyle',.BOTH.,(#101));
#103=IFCPRESENTATIONSTYLEASSIGNMENT((#102));
#104=IFCSTYLEDITEM(#43,(#103),$);
ENDSEC;
END-ISO-10303-21;
"#;

    #[test]
    fn styled_mapped_item_propagates_to_inner_extrusion() {
        // IfcStyledItem #104 styles IfcMappedItem #43, NOT the inner
        // #33 extrusion. Pre-#55, idx.item[33] would be None and the
        // glTF emitter would fall back to the per-entity palette.
        // Post-#55, idx.item[33] = the green RGBA.
        let table = EntityTable::build(STYLED_MAPPED_IFC.as_bytes());
        let idx = StyleIndex::build(&table);
        let got = idx.item.get(&33).copied().expect(
            "GH #55: colour authored on IfcMappedItem must reach inner extrusion #33",
        );
        let want = [0.1_f32, 0.7, 0.2, 1.0];
        for i in 0..4 {
            assert!(
                (got[i] - want[i]).abs() < 1e-5,
                "channel {} mismatch: got {got:?}, want {want:?}",
                i
            );
        }
    }

    #[test]
    fn colour_rgb_clamps_oversized() {
        let buf = format!(
            r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('x'),'2;1');
FILE_NAME('x.ifc','x',('x'),('x'),'x','x','');
FILE_SCHEMA(('IFC4'));
ENDSEC;
DATA;
#1=IFCPROJECT('0',$,'p',$,$,$,$,$,$);
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'r',#31,1.,1.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#32=IFCDIRECTION((0.,0.,1.));
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,1.);
#100=IFCCOLOURRGB('Big',204.,102.,51.);
#101=IFCSURFACESTYLESHADING(#100);
#102=IFCSURFACESTYLE('S',.BOTH.,(#101));
#104=IFCSTYLEDITEM(#33,(#102),$);
ENDSEC;
END-ISO-10303-21;
"#,
        );
        let table = EntityTable::build(buf.as_bytes());
        let idx = StyleIndex::build(&table);
        let got = idx.item.get(&33).copied().expect("item #33 styled");
        // 204/255 = 0.8, 102/255 = 0.4, 51/255 = 0.2.
        assert!((got[0] - 0.8).abs() < 1e-2);
        assert!((got[1] - 0.4).abs() < 1e-2);
        assert!((got[2] - 0.2).abs() < 1e-2);
        assert!((got[3] - 1.0).abs() < 1e-5);
    }
}
