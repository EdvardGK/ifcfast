//! Mesh hotswap — replace a product's body geometry with a new triangle
//! mesh (GH #124 Phase 3, the north-star write payload).
//!
//! The use-case in the user's words: *"hot-swapping bad meshes with
//! decimated or different meshes."* Given a product GlobalId and a new
//! `(vertices, triangles)` mesh, this repoints the product's **Body**
//! `IfcShapeRepresentation` at a freshly minted `IfcTriangulatedFaceSet`
//! (backed by an `IfcCartesianPointList3D`), rewrites the representation
//! type to `Tessellation`, and garbage-collects the geometry the old
//! items uniquely owned. Everything else in the file is emitted verbatim.
//!
//! ## Coordinate frame — the caller's contract
//!
//! An `IfcTriangulatedFaceSet`'s coordinates live in the representation's
//! **local** frame: the product's `ObjectPlacement` is applied on top of
//! them by any consumer. So the supplied `vertices` MUST be in that same
//! object-local frame (the frame the *original* body items used), NOT
//! world coordinates. For a decimate-in-place round-trip the caller
//! extracts the element's local-frame mesh, simplifies it, and swaps it
//! back — the placement is never touched, so the element stays put.
//!
//! ## What the swap does, precisely
//!
//! 1. Resolve `guid` → product; follow `Representation`@6 →
//!    `IfcProductDefinitionShape.Representations`@2 → the shape rep whose
//!    `RepresentationIdentifier`@1 is `Body`.
//! 2. Mint `#(max_id+1)` = point list, `#(max_id+2)` = faceset.
//! 3. Override the body rep: `Items`@3 → `(#faceset)`,
//!    `RepresentationType`@2 → `'Tessellation'`. Every other byte of that
//!    record (context, identifier, separators) is preserved.
//! 4. **Orphan GC** (refcount to a fixpoint): the old items and their
//!    forward closure are removed *iff* nothing else still points at them
//!    after the repoint. A shared `IfcRepresentationMap` referenced by
//!    other instances survives automatically — only the geometry this
//!    product uniquely owned is reclaimed, so the file actually shrinks.
//!
//! ## Guarantee
//!
//! The emitted bytes re-open (ifcfast **or** ifcopenshell) with zero
//! dangling references: the repointed rep names only records that remain,
//! the new faceset/point-list are appended, and GC removes a record only
//! when its post-swap inbound refcount is zero.

use std::collections::{HashMap, HashSet};

use super::rel_rules::{field_refs, field_span, RelField};
use super::refs::{forward_refs, reachable_closure};
use super::Doc;

/// Summary of a hotswap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotswapStats {
    /// Product whose body geometry was replaced.
    pub product: u64,
    /// The `IfcShapeRepresentation` that was repointed.
    pub shape_rep: u64,
    /// New root geometric item the body rep now points at (an
    /// `IfcTriangulatedFaceSet` on IFC4+, an `IfcShellBasedSurfaceModel`
    /// on IFC2x3).
    pub new_geometry: u64,
    /// Records appended for the new geometry (compact on IFC4+, a
    /// point/loop/face graph on IFC2x3).
    pub new_records: usize,
    /// Old geometric-item ids the body rep dropped.
    pub old_items: usize,
    /// Records reclaimed by orphan GC.
    pub records_gc: usize,
    /// Records in the emitted document.
    pub records_out: usize,
}

/// The tessellation dialect a document's schema supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tessellation {
    /// IFC4 / IFC4x3: `IfcTriangulatedFaceSet` + `IfcCartesianPointList3D`.
    FaceSet,
    /// IFC2x3 (and earlier): `IfcShellBasedSurfaceModel` over an
    /// `IfcOpenShell` of `IfcFace`/`IfcPolyLoop` — the compact facesets
    /// don't exist there.
    SurfaceModel,
}

/// Why a hotswap could not be performed. Every variant is a *loud*
/// failure — the swap never silently no-ops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HotswapError {
    /// The GlobalId is not present in the document.
    UnknownGuid(String),
    /// The product has no `Representation` (field 6 is `$`).
    NoRepresentation,
    /// No `Body` `IfcShapeRepresentation` was found under the product's
    /// `IfcProductDefinitionShape`.
    NoBodyRepresentation,
    /// The mesh is empty or a triangle indexes a vertex out of range.
    BadMesh(String),
    /// A record referenced by the traversal was malformed / unparseable.
    Malformed(String),
}

impl std::fmt::Display for HotswapError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HotswapError::UnknownGuid(g) => write!(f, "unknown GlobalId: {g}"),
            HotswapError::NoRepresentation => write!(f, "product has no Representation"),
            HotswapError::NoBodyRepresentation => {
                write!(f, "product has no 'Body' shape representation")
            }
            HotswapError::BadMesh(m) => write!(f, "bad mesh: {m}"),
            HotswapError::Malformed(m) => write!(f, "malformed record: {m}"),
        }
    }
}

impl std::error::Error for HotswapError {}

/// Replace the body geometry of the product identified by `guid` with the
/// triangle mesh `(verts, tris)` (both in the element's local frame; see
/// the module docs). Returns the emitted STEP bytes and a [`HotswapStats`].
pub fn hotswap(
    doc: &Doc,
    guid: &str,
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
) -> Result<(Vec<u8>, HotswapStats), HotswapError> {
    if verts.is_empty() {
        return Err(HotswapError::BadMesh("no vertices".into()));
    }
    if tris.is_empty() {
        return Err(HotswapError::BadMesh("no triangles".into()));
    }
    for (i, v) in verts.iter().enumerate() {
        if !v.iter().all(|c| c.is_finite()) {
            return Err(HotswapError::BadMesh(format!(
                "vertex {i} has a non-finite coordinate ({:?},{:?},{:?})",
                v[0], v[1], v[2]
            )));
        }
    }
    let nv = verts.len() as u32;
    for (t, tri) in tris.iter().enumerate() {
        for &v in tri {
            if v >= nv {
                return Err(HotswapError::BadMesh(format!(
                    "triangle {t} indexes vertex {v} of {nv}"
                )));
            }
        }
    }

    // 1. Resolve the product and locate its body shape representation.
    let (found, _missing) = doc.resolve_guids(&[guid.to_string()]);
    let product = *found
        .first()
        .ok_or_else(|| HotswapError::UnknownGuid(guid.to_string()))?;

    let pds = single_ref(doc, product, 6).ok_or(HotswapError::NoRepresentation)?;
    let shape_rep = find_body_rep(doc, pds)?;

    let old_items = list_refs(doc, shape_rep, 3);

    // 2. Build the new geometry in the dialect the schema supports, minting
    //    ids above the source max. `root` is the item the body rep points at.
    let dialect = detect_tessellation(doc);
    let (appended, root, rep_type, new_records) =
        build_geometry(dialect, doc.max_id(), verts, tris);

    // 3. Override the body rep: Items → (#root), RepType → the dialect's tag.
    let rep_override = rewrite_body_rep(doc, shape_rep, root, rep_type)?;

    // 4. Orphan GC: remove the old items' closure where nothing else keeps
    //    it alive after the repoint.
    let removed = gc_orphans(doc, shape_rep, &old_items);

    // 5. Emit the mutated document.
    let mut overrides: HashMap<u64, Vec<u8>> = HashMap::new();
    overrides.insert(shape_rep, rep_override);
    let (bytes, records_out) = emit(doc, &removed, &overrides, &appended);

    Ok((
        bytes,
        HotswapStats {
            product,
            shape_rep,
            new_geometry: root,
            new_records,
            old_items: old_items.len(),
            records_gc: removed.len(),
            records_out,
        },
    ))
}

/// Which tessellation dialect the document's `FILE_SCHEMA` supports. IFC4
/// and later carry the compact `IfcTriangulatedFaceSet`; IFC2x3 and earlier
/// have only the shell/face model.
fn detect_tessellation(doc: &Doc) -> Tessellation {
    let prefix = &doc.buf()[..doc.prefix_end()];
    // Find FILE_SCHEMA((' ... ')) and read the first schema token.
    let needle = b"FILE_SCHEMA";
    let name = prefix
        .windows(needle.len())
        .position(|w| w.eq_ignore_ascii_case(needle))
        .map(|p| {
            let tail = &prefix[p..];
            let start = tail.iter().position(|&b| b == b'\'').map(|i| i + 1);
            match start {
                Some(s) => {
                    let end = tail[s..].iter().position(|&b| b == b'\'').unwrap_or(0);
                    &tail[s..s + end]
                }
                None => &[][..],
            }
        })
        .unwrap_or(&[][..]);
    // "IFC4", "IFC4X3", … → facesets; "IFC2X3", "IFC2X2" → surface model.
    if name.len() >= 4 && name[..4].eq_ignore_ascii_case(b"IFC4") {
        Tessellation::FaceSet
    } else {
        Tessellation::SurfaceModel
    }
}

/// The single `#ref` held by field `index` of record `id`, if any.
fn single_ref(doc: &Doc, id: u64, index: usize) -> Option<u64> {
    let refs = field_refs_at(doc, id, RelField { index, is_set: false });
    refs.into_iter().next()
}

/// The refs held by field `index` (as a SET) of record `id`.
fn list_refs(doc: &Doc, id: u64, index: usize) -> Vec<u64> {
    field_refs_at(doc, id, RelField { index, is_set: true })
}

/// Split record `id` and read `field` as anchor/pull-style refs.
fn field_refs_at(doc: &Doc, id: u64, field: RelField) -> Vec<u64> {
    let Some(span) = doc.record_bytes(id) else {
        return Vec::new();
    };
    let Some((_id, _ty, args)) = crate::lexer::parse_record_span(span) else {
        return Vec::new();
    };
    let split = crate::lexer::split_top_level_args(args);
    field_refs(&split, field)
}

/// Among the shape representations under `pds`
/// (`IfcProductDefinitionShape.Representations`@2), the id of the one whose
/// `RepresentationIdentifier`@1 decodes to `Body`.
fn find_body_rep(doc: &Doc, pds: u64) -> Result<u64, HotswapError> {
    let reps = list_refs(doc, pds, 2);
    if reps.is_empty() {
        return Err(HotswapError::NoRepresentation);
    }
    for rep in reps {
        let Some(span) = doc.record_bytes(rep) else {
            continue;
        };
        let Some((_id, _ty, args)) = crate::lexer::parse_record_span(span) else {
            continue;
        };
        let split = crate::lexer::split_top_level_args(args);
        if let Some(ident) = split.get(1).and_then(|f| crate::lexer::decode_string(f)) {
            if ident.eq_ignore_ascii_case("Body") {
                return Ok(rep);
            }
        }
    }
    Err(HotswapError::NoBodyRepresentation)
}

/// Rebuild the body rep's bytes with `Items`@3 pointing at `(#root)` and
/// `RepresentationType`@2 set to `rep_type`, preserving every other byte.
/// Splices the higher-index field first so the lower field's byte offsets
/// stay valid.
fn rewrite_body_rep(
    doc: &Doc,
    shape_rep: u64,
    root: u64,
    rep_type: &str,
) -> Result<Vec<u8>, HotswapError> {
    let span = doc
        .record_bytes(shape_rep)
        .ok_or_else(|| HotswapError::Malformed(format!("#{shape_rep} absent")))?;

    let items_range = field_span(span, RelField { index: 3, is_set: true })
        .ok_or_else(|| HotswapError::Malformed(format!("#{shape_rep} has no Items field")))?;
    let type_range = field_span(span, RelField { index: 2, is_set: false })
        .ok_or_else(|| HotswapError::Malformed(format!("#{shape_rep} has no RepType field")))?;

    let items_repl = format!("(#{root})").into_bytes();
    let type_repl = format!("'{rep_type}'").into_bytes();

    // Apply the later (Items@3) splice first, then the earlier (Type@2).
    let mut out = span.to_vec();
    out.splice(items_range.clone(), items_repl);
    out.splice(type_range.clone(), type_repl);
    Ok(out)
}

/// Remove the old items' forward closure where nothing outside the removed
/// set still references it once the body rep points at the new faceset.
///
/// Refcount fixpoint: build inbound counts over the *post-swap* graph (the
/// body rep contributes its new refs, not its old items), then peel any old
/// item with zero inbound refs, decrementing its children and cascading.
/// This reclaims per-instance items (swept solids, mapped items) while a
/// shared `IfcRepresentationMap` — still pointed at by other instances —
/// keeps a positive count and survives.
fn gc_orphans(doc: &Doc, shape_rep: u64, old_items: &[u64]) -> HashSet<u64> {
    // The candidate universe: everything the old items could reach. Removal
    // is confined to this set, so a stray zero-count elsewhere is untouched.
    let closure = reachable_closure(doc, old_items);

    // Inbound refcount over the post-swap graph.
    let old_set: HashSet<u64> = old_items.iter().copied().collect();
    let mut refcount: HashMap<u64, usize> = HashMap::new();
    for &id in doc.ids() {
        for r in forward_refs(doc, id) {
            // Post-swap the body rep's Items are the new geometry (not yet
            // in the doc), so its old-item edges vanish — but every OTHER
            // field it carries (notably ContextOfItems) survives the swap
            // and must keep its target alive, e.g. a subcontext whose only
            // referrers are shape representations (GH #130).
            if id == shape_rep && old_set.contains(&r) {
                continue;
            }
            if closure.contains(&r) {
                *refcount.entry(r).or_insert(0) += 1;
            }
        }
    }

    let mut removed: HashSet<u64> = HashSet::new();
    let mut work: Vec<u64> = old_items.to_vec();
    while let Some(c) = work.pop() {
        if removed.contains(&c) || !closure.contains(&c) {
            continue;
        }
        if refcount.get(&c).copied().unwrap_or(0) != 0 {
            continue; // still referenced by a surviving record
        }
        removed.insert(c);
        for child in forward_refs(doc, c) {
            if closure.contains(&child) {
                let e = refcount.entry(child).or_insert(0);
                *e = e.saturating_sub(1);
                work.push(child);
            }
        }
    }
    removed
}

/// STEP bytes for the appended geometry, in the schema's dialect. Returns
/// `(bytes, root_id, rep_type, n_records)` where `root_id` is the item the
/// body rep must point at and `rep_type` its `RepresentationType` tag. Ids
/// are minted from `base + 1` upward.
fn build_geometry(
    dialect: Tessellation,
    base: u64,
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
) -> (Vec<u8>, u64, &'static str, usize) {
    match dialect {
        Tessellation::FaceSet => build_faceset(base, verts, tris),
        Tessellation::SurfaceModel => build_surface_model(base, verts, tris),
    }
}

/// IFC4+ compact tessellation: one `IfcCartesianPointList3D` + one
/// `IfcTriangulatedFaceSet`. `CoordIndex` is 1-based; `tris` are 0-based.
fn build_faceset(base: u64, verts: &[[f64; 3]], tris: &[[u32; 3]]) -> (Vec<u8>, u64, &'static str, usize) {
    let point_list = base + 1;
    let faceset = base + 2;
    let mut s = String::with_capacity(verts.len() * 40 + tris.len() * 20 + 64);

    s.push_str(&format!("#{point_list}=IFCCARTESIANPOINTLIST3D(("));
    for (i, v) in verts.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&fmt_tuple(v));
    }
    s.push_str("));\n");

    s.push_str(&format!("#{faceset}=IFCTRIANGULATEDFACESET(#{point_list},$,$,("));
    for (i, t) in tris.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("({},{},{})", t[0] + 1, t[1] + 1, t[2] + 1));
    }
    s.push_str("),$);\n");

    (s.into_bytes(), faceset, "Tessellation", 2)
}

/// IFC2x3 shell/face model: one `IfcCartesianPoint` per vertex, a
/// `IfcPolyLoop`/`IfcFaceOuterBound`/`IfcFace` triple per triangle, then one
/// `IfcOpenShell` and one `IfcShellBasedSurfaceModel` (which handles open
/// meshes, unlike `IfcClosedShell`). Ids are minted densely so the appended
/// block is self-contained.
fn build_surface_model(
    base: u64,
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
) -> (Vec<u8>, u64, &'static str, usize) {
    let n = verts.len() as u64;
    let point_base = base; // vertex i → point_base + 1 + i
    let tri_base = base + n; // triangle k → three ids at tri_base + 3k + {1,2,3}
    let shell = base + n + 3 * tris.len() as u64 + 1;
    let sbsm = shell + 1;

    let mut s = String::with_capacity((verts.len() + tris.len() * 3) * 48 + 64);

    for (i, v) in verts.iter().enumerate() {
        let id = point_base + 1 + i as u64;
        s.push_str(&format!("#{id}=IFCCARTESIANPOINT({});\n", fmt_tuple(v)));
    }

    let mut face_ids: Vec<u64> = Vec::with_capacity(tris.len());
    for (k, t) in tris.iter().enumerate() {
        let loop_id = tri_base + 3 * k as u64 + 1;
        let bound_id = loop_id + 1;
        let face_id = loop_id + 2;
        let p0 = point_base + 1 + t[0] as u64;
        let p1 = point_base + 1 + t[1] as u64;
        let p2 = point_base + 1 + t[2] as u64;
        s.push_str(&format!("#{loop_id}=IFCPOLYLOOP((#{p0},#{p1},#{p2}));\n"));
        s.push_str(&format!("#{bound_id}=IFCFACEOUTERBOUND(#{loop_id},.T.);\n"));
        s.push_str(&format!("#{face_id}=IFCFACE((#{bound_id}));\n"));
        face_ids.push(face_id);
    }

    s.push_str(&format!("#{shell}=IFCOPENSHELL(("));
    for (i, f) in face_ids.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("#{f}"));
    }
    s.push_str("));\n");
    s.push_str(&format!("#{sbsm}=IFCSHELLBASEDSURFACEMODEL((#{shell}));\n"));

    let n_records = verts.len() + tris.len() * 3 + 2;
    (s.into_bytes(), sbsm, "SurfaceModel", n_records)
}

/// A STEP coordinate tuple `(x,y,z)` with STEP-real formatting — the shared
/// inner list for both an `IfcCartesianPointList3D` item and an
/// `IfcCartesianPoint`'s `Coordinates`.
fn fmt_tuple(v: &[f64; 3]) -> String {
    format!("({},{},{})", fmt_real(v[0]), fmt_real(v[1]), fmt_real(v[2]))
}

/// Format an `f64` as a STEP REAL. `{:?}` gives the shortest
/// round-tripping form and a `.` for whole values (`1.0`), but drops the
/// point in exponent form (`5e-5`) — the ISO-10303-21 REAL grammar requires
/// a decimal point even with an exponent, so re-insert it. Non-finite
/// values must be rejected upstream (`hotswap` validates); debug-assert
/// here as the backstop.
fn fmt_real(x: f64) -> String {
    debug_assert!(x.is_finite(), "fmt_real: non-finite {x}");
    let s = format!("{x:?}");
    match s.find(['e', 'E']) {
        Some(epos) if !s[..epos].contains('.') => {
            format!("{}.0{}", &s[..epos], &s[epos..])
        }
        _ => s,
    }
}

/// Emit the mutated document: header + every record (skipping `removed`,
/// substituting `overrides`) + the appended geometry + trailer. Returns the
/// bytes and the record count written (kept records + appended records).
fn emit(
    doc: &Doc,
    removed: &HashSet<u64>,
    overrides: &HashMap<u64, Vec<u8>>,
    appended: &[u8],
) -> (Vec<u8>, usize) {
    let buf = doc.buf();
    let mut out = Vec::with_capacity(buf.len() + appended.len());
    out.extend_from_slice(&buf[..doc.prefix_end()]);

    let mut records_out = 0usize;
    for (id, i) in doc.records() {
        if removed.contains(&id) {
            continue;
        }
        match overrides.get(&id) {
            Some(bytes) => out.extend_from_slice(bytes),
            None => out.extend_from_slice(&buf[doc.record_span(i)]),
        }
        records_out += 1;
    }

    // Appended new records go after the last kept record, before ENDSEC.
    out.extend_from_slice(appended);
    records_out += appended.iter().filter(|&&b| b == b';').count();

    out.extend_from_slice(&buf[doc.endsec()..]);
    (out, records_out)
}
