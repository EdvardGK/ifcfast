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

use super::refs::{forward_refs, reachable_closure};
use super::rel_rules::{field_refs, field_span, RelField};
use super::step_fmt::fmt_tuple;
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
    /// How many OTHER records (products, typically) reference the same
    /// `IfcProductDefinitionShape` — when nonzero, this swap changed the
    /// visible geometry of every one of them, not just `product`
    /// (GH #132 item 6). Legal IFC; the caller decides if it's intended.
    pub pds_shared_with: usize,
    /// `Body` shape representations found under the PDS. Only the first
    /// is swapped; a value > 1 means further body reps were left as-is.
    pub body_reps: usize,
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
    let (shape_rep, body_reps) = find_body_rep(doc, pds)?;

    let old_items = list_refs(doc, shape_rep, 3);

    // 2. Build the new geometry in the dialect the schema supports, minting
    //    ids above the source max. `root` is the item the body rep points at.
    let dialect = detect_tessellation(doc);
    let (appended, root, rep_type, new_records) =
        build_geometry(dialect, doc.max_id(), verts, tris);

    // 3. Override the body rep: Items → (#root), RepType → the dialect's tag.
    let rep_override = rewrite_body_rep(doc, shape_rep, root, rep_type)?;

    // 4. Orphan GC: remove the old items' closure where nothing else keeps
    //    it alive after the repoint (weak-referrer cleanup included).
    let (removed, gc_overrides) = gc_orphans(doc, shape_rep, &old_items);

    // 5. Emit the mutated document.
    let mut overrides: HashMap<u64, Vec<u8>> = gc_overrides;
    overrides.insert(shape_rep, rep_override);
    let (bytes, records_out) = emit(doc, &removed, &overrides, &appended);

    // Sharing telemetry (GH #132 item 6): a PDS referenced by several
    // products means this swap changed the geometry of ALL of them —
    // legal IFC, but the caller must know it wasn't a single-element edit.
    let pds_shared_with = doc
        .ids()
        .iter()
        .filter(|&&id| id != product && forward_refs(doc, id).contains(&pds))
        .count();

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
            pds_shared_with,
            body_reps,
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
    let refs = field_refs_at(
        doc,
        id,
        RelField {
            index,
            is_set: false,
        },
    );
    refs.into_iter().next()
}

/// The refs held by field `index` (as a SET) of record `id`.
fn list_refs(doc: &Doc, id: u64, index: usize) -> Vec<u64> {
    field_refs_at(
        doc,
        id,
        RelField {
            index,
            is_set: true,
        },
    )
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
/// (`IfcProductDefinitionShape.Representations`@2), the id of the FIRST
/// one whose `RepresentationIdentifier`@1 decodes to `Body`, plus how many
/// `Body` reps exist in total (only the first is swapped — the count goes
/// to stats so a multi-body product isn't silently half-swapped).
fn find_body_rep(doc: &Doc, pds: u64) -> Result<(u64, usize), HotswapError> {
    let reps = list_refs(doc, pds, 2);
    if reps.is_empty() {
        return Err(HotswapError::NoRepresentation);
    }
    let mut first: Option<u64> = None;
    let mut count = 0usize;
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
                count += 1;
                if first.is_none() {
                    first = Some(rep);
                }
            }
        }
    }
    match first {
        Some(rep) => Ok((rep, count)),
        None => Err(HotswapError::NoBodyRepresentation),
    }
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

    let items_range = field_span(
        span,
        RelField {
            index: 3,
            is_set: true,
        },
    )
    .ok_or_else(|| HotswapError::Malformed(format!("#{shape_rep} has no Items field")))?;
    let type_range = field_span(
        span,
        RelField {
            index: 2,
            is_set: false,
        },
    )
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
/// Returns the removed set plus overrides for records whose ref SETs had
/// to be pruned (layer assignments that named removed geometry).
///
/// Refcount fixpoint: build inbound counts over the *post-swap* graph (the
/// body rep contributes its new refs, not its old items), then peel any old
/// item with zero inbound refs, decrementing its children and cascading.
/// This reclaims per-instance items (swept solids, mapped items) while a
/// shared `IfcRepresentationMap` — still pointed at by other instances —
/// keeps a positive count and survives.
///
/// ## Weak referrers (GH #132 item 5)
///
/// `IfcStyledItem` and `IfcPresentationLayerAssignment` point AT geometry
/// items but are decorations, not consumers: counting their inbound edges
/// as ownership kept every styled (Revit-coloured) element's dead body
/// alive forever — hotswap never actually shrank real files. Their edges
/// are excluded from the refcount, and they're reconciled after the peel:
/// a styled item whose `Item` was removed is removed with it; a layer
/// assignment names survivors only (or is removed when none remain).
fn gc_orphans(
    doc: &Doc,
    shape_rep: u64,
    old_items: &[u64],
) -> (HashSet<u64>, HashMap<u64, Vec<u8>>) {
    // The candidate universe: everything the old items could reach. Removal
    // is confined to this set, so a stray zero-count elsewhere is untouched.
    let closure = reachable_closure(doc, old_items);

    // Weak referrers: styled items keyed by the item they decorate, layer
    // assignments with their member sets. Only ones touching the closure
    // matter.
    let mut styled_by_item: HashMap<u64, Vec<u64>> = HashMap::new();
    let mut layers: Vec<(u64, Vec<u64>)> = Vec::new();
    let mut weak: HashSet<u64> = HashSet::new();
    for &id in doc.ids() {
        let Some(span) = doc.record_bytes(id) else {
            continue;
        };
        let Some((_id, ty, args)) = crate::lexer::parse_record_span(span) else {
            continue;
        };
        let ty = ty.to_ascii_uppercase();
        if ty == b"IFCSTYLEDITEM" {
            let split = crate::lexer::split_top_level_args(args);
            let item = field_refs(
                &split,
                RelField {
                    index: 0,
                    is_set: false,
                },
            );
            if let Some(&item) = item.first() {
                if closure.contains(&item) {
                    styled_by_item.entry(item).or_default().push(id);
                    weak.insert(id);
                }
            }
        } else if ty == b"IFCPRESENTATIONLAYERASSIGNMENT" || ty == b"IFCPRESENTATIONLAYERWITHSTYLE"
        {
            let split = crate::lexer::split_top_level_args(args);
            let items = field_refs(
                &split,
                RelField {
                    index: 2,
                    is_set: true,
                },
            );
            if items.iter().any(|i| closure.contains(i)) {
                layers.push((id, items));
                weak.insert(id);
            }
        }
    }

    // Inbound refcount over the post-swap graph.
    let old_set: HashSet<u64> = old_items.iter().copied().collect();
    let mut refcount: HashMap<u64, usize> = HashMap::new();
    for &id in doc.ids() {
        // A weak referrer's edges don't confer ownership.
        if weak.contains(&id) {
            continue;
        }
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

    // Reconcile the weak referrers with what the peel decided.
    for (item, sis) in &styled_by_item {
        if removed.contains(item) {
            removed.extend(sis.iter().copied());
        }
    }
    let mut overrides: HashMap<u64, Vec<u8>> = HashMap::new();
    for (lid, items) in &layers {
        let survivors: Vec<u64> = items
            .iter()
            .copied()
            .filter(|i| !removed.contains(i))
            .collect();
        if survivors.len() == items.len() {
            continue; // nothing it named was removed
        }
        if survivors.is_empty() {
            removed.insert(*lid);
        } else if let Some(bytes) = splice_ref_set(doc, *lid, 2, &survivors) {
            overrides.insert(*lid, bytes);
        }
    }
    (removed, overrides)
}

/// Rebuild record `id`'s bytes with SET field `index` rewritten to
/// `survivors` (order preserved), every other byte verbatim.
fn splice_ref_set(doc: &Doc, id: u64, index: usize, survivors: &[u64]) -> Option<Vec<u8>> {
    let span = doc.record_bytes(id)?;
    let range = field_span(
        span,
        RelField {
            index,
            is_set: true,
        },
    )?;
    let mut field = Vec::with_capacity(survivors.len() * 8 + 2);
    field.push(b'(');
    for (i, s) in survivors.iter().enumerate() {
        if i > 0 {
            field.push(b',');
        }
        field.push(b'#');
        field.extend_from_slice(s.to_string().as_bytes());
    }
    field.push(b')');
    let mut out = Vec::with_capacity(span.len());
    out.extend_from_slice(&span[..range.start]);
    out.extend_from_slice(&field);
    out.extend_from_slice(&span[range.end..]);
    Some(out)
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
fn build_faceset(
    base: u64,
    verts: &[[f64; 3]],
    tris: &[[u32; 3]],
) -> (Vec<u8>, u64, &'static str, usize) {
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

    s.push_str(&format!(
        "#{faceset}=IFCTRIANGULATEDFACESET(#{point_list},$,$,("
    ));
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
