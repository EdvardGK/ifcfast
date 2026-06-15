//! Native IFC → triangle mesh emitter.
//!
//! Feature-gated behind `mesh` so the default `ifcfast` builds (Python
//! extension, bench binary) don't pull in `earcutr` and `glam`.
//!
//! Design stance: **reveal what the file says.** Every representation
//! item is dispatched to a handler that returns either real geometry
//! (tagged by source) or an explicit `Unhandled { ifc_type }` fragment
//! so the consumer knows exactly what the file contained and what we
//! couldn't tessellate yet. We never silently drop a representation.
//!
//! Composite solids (`IfcBooleanResult`, `IfcCsgSolid`) emit BOTH
//! operands as their own visible mesh segments. We don't perform the
//! boolean — that would erase the structural information ("this volume
//! is the host", "this volume is the cut") that downstream tools and
//! human readers need to make decisions and surgically edit the model.

pub mod boolean;
pub mod brep;
pub mod csg_primitive;
#[cfg(feature = "csg")]
pub mod cut_openings;
/// Pure-data outcome + counter types for the cut-openings pipeline.
/// NOT csg-gated — mesh / prism builds carry the counters even when the
/// csg-gated `cut_openings` module is absent (GH #61). `cut_openings`
/// re-exports these when present.
pub mod cut_stats;
#[cfg(feature = "csg")]
pub mod cut_validate;
pub mod curveset;
pub mod extrusion;
pub mod faceset;
pub mod gltf;
pub mod halfspace_clip;
pub mod indexed_curve;
pub mod mapped;
pub mod obj;
pub mod placement;
#[cfg(feature = "prism-csg-fast")]
pub mod polygon_bool;
#[cfg(feature = "prism-csg-fast")]
pub mod prism_csg;
pub mod profile;
pub mod qto;
pub mod revolved;
pub mod sample;
pub mod stats;
pub mod styles;

use std::collections::HashMap;
use std::time::Instant;

use glam::{DMat4, DVec3, Mat4, Vec3, Vec4};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;
use crate::mesh::placement::PlacementResolver;

/// Lossless upcast of an f32 placement matrix to f64. Used to do the
/// per-fragment anchor multiply in f64 — the `world_f64 * instance_f64
/// * rep_origin_f64` chain needs to be precise when `rep_origin` carries
/// the huge bbox-min of a transformed/georeferenced face set. The f32
/// `instance_transform` itself is small (mapped-item composition) so
/// the upcast is exact.
fn mat4_to_dmat4(m: Mat4) -> DMat4 {
    let c = m.to_cols_array();
    DMat4::from_cols_array(&[
        c[0] as f64, c[1] as f64, c[2] as f64, c[3] as f64,
        c[4] as f64, c[5] as f64, c[6] as f64, c[7] as f64,
        c[8] as f64, c[9] as f64, c[10] as f64, c[11] as f64,
        c[12] as f64, c[13] as f64, c[14] as f64, c[15] as f64,
    ])
}

/// One contiguous slice of a `ProductMesh`'s triangle list that came
/// from a single representation item or operand. Lets the consumer
/// know which triangles are "the host wall" vs "the door opening
/// volume" inside a `ProductMesh` built from an `IfcBooleanResult`.
#[derive(Debug, Clone)]
pub struct MeshSegment {
    /// First index in `ProductMesh.indices` that belongs to this segment.
    pub index_start: u32,
    /// Number of indices (always a multiple of 3) in this segment.
    pub index_count: u32,
    /// Provenance tag — see [`MeshFragment::source_tags`] for the set
    /// of known values, plus `"unhandled:IFCXXX"` for items we saw
    /// but couldn't tessellate.
    pub source: String,
}

/// IfcMappedItem dedup cache. Keyed by the inner representation item's
/// `step_id`; entries are the tessellated direct-geometry fragments
/// (`Vec<(LocalMesh, source_tag)>`). DashMap (sharded RwLock) is the
/// concurrency story for parallel per-product tessellation in
/// [`mesh_ifc_streaming_framed`] — multiple worker threads can `get`/
/// `insert` without serialising on a single lock. Single-shard
/// contention is rare in real files (most representations are unique
/// per product); when it happens at startup on a wide-facade model
/// (5000 identical windows sharing one `IfcRepresentationMap`), the
/// worst case is a handful of duplicated tessellations of the same
/// `LocalMesh` before the shard warms.
pub(crate) type ShapeCache = dashmap::DashMap<u64, Vec<(LocalMesh, &'static str)>>;

/// What `mesh_item` returns per representation item it walks. Either a
/// real triangle mesh with its source tag, or an explicit "we saw a
/// representation of this type but don't have a handler for it" marker
/// so the caller can bucket it into stats. Never a silent drop.
///
/// `role` is set by composite handlers (`boolean::boolean_result`,
/// `boolean::csg_solid`) to record the structural position of this
/// fragment inside the parent tree (e.g. `Some("boolean_first_operand")`
/// for the host side of an `IfcBooleanClippingResult`). The leaf
/// `source` keeps the underlying representation type, so a halfspace
/// used as a clip target appears with `source="halfspace_bounded"` AND
/// `role=Some("boolean_second_operand")` — both facts preserved.
///
/// `rep_step_id` is the step_id of the source representation item that
/// produced this fragment — for an `IfcMappedItem` expansion it's the
/// id of the *inner* item inside the `IfcRepresentationMap` (shared by
/// every instance of the same mapped shape), and for direct dispatch
/// it's the item_id itself. Multiple products that resolve to the same
/// `rep_step_id` carry the same local geometry, enabling substrate-
/// level deduplication of representations.
///
/// `instance_transform` is the per-instance composition applied to the
/// untransformed local mesh — `t_target * t_origin` from
/// `IfcMappedItem`, identity for direct geometry. The local `mesh`
/// stays untransformed; the kernel applies `world * instance_transform`
/// to compose the final world-coordinate vertex stream.
#[derive(Debug)]
pub enum MeshFragment {
    Mesh {
        mesh: LocalMesh,
        /// The leaf entity tag (e.g. `"extrusion"`, `"brep"`,
        /// `"csg_cylinder"`). Always exactly one token. See
        /// [`MeshFragment::source_tags`] for the closed set.
        source: &'static str,
        /// Wrapping composite roles, accumulated as we walk back up
        /// the dispatch tree. Innermost-first (the first push is the
        /// role at the deepest containing boolean / csg node); each
        /// outer wrapping retag call appends. At serialization the
        /// chain is rendered outermost-first by reversing this vec.
        ///
        /// Pre-W1 this was a single `Option<&'static str>` with
        /// innermost-wins semantics (`role.unwrap_or(new_role)`),
        /// which silently dropped outer wrapping roles. A fragment
        /// that is a cutter at level N but a host at level N+1 would
        /// lose the cutter annotation on the chain — see
        /// [GH #58 / W1] in `docs/plans/2026-06-05_cut-openings-manifold-replacement.md`
        /// for the design call.
        roles: Vec<&'static str>,
        rep_step_id: u64,
        instance_transform: Mat4,
        /// Parametric `IfcPolygonalBoundedHalfSpace` cutter params, set
        /// ONLY on a bounded-halfspace leaf fragment (W6 / GH #58 F6);
        /// `None` for every other fragment. `retag` carries it up the
        /// boolean tree unchanged so it reaches the [`ProductMesh`].
        /// See [`BoundedHalfspacePayload`].
        bounded_halfspace: Option<BoundedHalfspacePayload>,
    },
    Unhandled {
        ifc_type: String,
    },
}

impl MeshFragment {
    /// Known source tags emitted by the dispatch tree. Useful for
    /// downstream consumers that want to validate the set.
    pub fn source_tags() -> &'static [&'static str] {
        &[
            "extrusion",
            "mapped",
            "polygonal_faceset",
            "triangulated_faceset",
            "brep",
            "advanced_brep_approx",
            "faceset_fbsm",
            "faceset_sbsm",
            "boolean_first_operand",
            "boolean_second_operand",
            "boolean_union_operand",
            "boolean_intersection_operand",
            "csg_branch",
            "halfspace_bounded:agree",
            "halfspace_bounded:disagree",
            "halfspace_plane:agree",
            "halfspace_plane:disagree",
            "curve_set",
            "csg_block",
            "csg_cylinder",
            "csg_cone",
            "csg_sphere",
            "csg_pyramid",
            "revolved",
        ]
    }
}

/// Per-fragment instancing info, carried alongside `segments` so the
/// substrate writer can split the product into (representation, instance)
/// rows instead of baking every transformed copy as standalone geometry.
///
/// For a single-fragment product whose source is shared (e.g. an
/// `IfcMappedItem` window pointing into a family library): `rep_step_id`
/// is shared across every instance of that family, `local_vertices` /
/// `local_indices` are the untransformed source mesh (identical across
/// instances → dedupes on `rep_step_id`), `instance_transform` carries
/// the per-instance composition that positions this copy in the product's
/// local frame. The full world-space placement is
/// `world_transform * instance_transform`.
///
/// For multi-fragment products (e.g. a wall whose representation is an
/// `IfcBooleanClippingResult` of two operands), each operand fragment
/// gets its own `InstancePart` with its own rep_step_id + instance
/// transform. The substrate writer chooses whether to dedup the whole
/// composite as one row keyed by the product step_id, or to honour each
/// fragment's rep_step_id individually.
#[derive(Debug, Clone)]
pub struct InstancePart {
    /// step_id of the source representation item — shared across every
    /// instance pointing to the same `IfcRepresentationMap`.
    pub rep_step_id: u64,
    /// `t_target * t_origin` from `IfcMappedItem`, identity for direct
    /// geometry. Column-major 4x4 (glam `Mat4` layout).
    pub instance_transform: [f32; 16],
    /// Untransformed source mesh — `[x, y, z, x, y, z, ...]`. Cache-
    /// hit clones across instances pointing to the same source share
    /// identical bytes here, which is the basis for substrate dedup.
    pub local_vertices: Vec<f32>,
    pub local_indices: Vec<u32>,
    /// Where this fragment's triangles sit inside the world-baked
    /// `ProductMesh.indices` buffer — parallel to `MeshSegment` so
    /// back-compat consumers (stats, gltf, obj) can still iterate by
    /// segment without touching the parts data.
    pub index_start: u32,
    pub index_count: u32,
    /// Compound source tag, mirroring `MeshSegment.source` (e.g.
    /// `"boolean_first_operand|extrusion"` or `"mapped"`).
    pub source: String,
    /// RGBA in linear-space `[0, 1]` derived from the
    /// `IfcStyledItem.Item == rep_step_id` chain (`IfcSurfaceStyle` →
    /// `IfcSurfaceStyleRendering`/`Shading` → `IfcColourRgb` + Transparency).
    /// `None` when no per-item style was authored — consumers fall back
    /// to [`ProductMesh::surface_color`] or a per-entity palette.
    pub surface_color: Option<[f32; 4]>,
}

/// Parametric description of an `IfcPolygonalBoundedHalfSpace` cutter,
/// resolved at tessellation time and carried on the [`ProductMesh`] so
/// the cut-openings pass can perform the *bounded* cut (W6 / GH #58 F6)
/// instead of the over-aggressive infinite-plane clip.
///
/// **Why this lives on `ProductMesh` (audit "approach 2").** The slab
/// triangles `boolean::polygonal_bounded_halfspace` emits do not carry
/// the boundary polygon or the cutter's frames — they are baked into a
/// thin visualisation slab. `cut_openings::apply` runs inside the
/// streaming sink's `on_product`, where no `EntityTable` is in scope, so
/// it cannot re-derive the boundary from the IFC entity. Resolving the
/// payload in `boolean.rs` (where the table IS in scope) and attaching it
/// to the product is the same "carry params, not the kernel" pattern
/// `InstancePart.rep_step_id` already follows.
///
/// `Option<...>` (not cfg-gated) keeps every `ProductMesh` constructor
/// free of feature blocks: it is `None` in default builds and on every
/// non-bounded product. The bounded fast-path only *reads* it under the
/// `prism-csg-fast` feature; without the feature the field is inert.
///
/// **Frame contract (F3).** All vectors / matrices here are in the
/// product's world frame (the same frame the baked `vertices` live in
/// when `apply` runs). `boundary_xform` maps the 2D `boundary` points
/// (in the `Position` arg[2] frame) into that world frame.
#[derive(Debug, Clone)]
pub struct BoundedHalfspacePayload {
    /// The polygonal boundary in its own 2D (`Position` arg[2]) frame.
    pub boundary: crate::mesh::profile::Polygon2D,
    /// Maps `boundary` 2D points (`z = 0`) into the working frame.
    pub boundary_xform: Mat4,
    /// Unit cutting-plane normal, oriented into the **subtracted**
    /// half-space (the side `cut_openings` removes), in the working
    /// frame. Same convention as the slab's first-triangle normal.
    pub plane_normal: Vec3,
    /// A point on the cutting plane (the BaseSurface origin), working
    /// frame.
    pub plane_point: Vec3,
}

/// A finished mesh in world coordinates, keyed back to its IfcProduct.
#[derive(Debug, Clone)]
pub struct ProductMesh {
    pub guid: String,
    pub entity: String,
    /// step_id of the IfcProduct itself — used by substrate writers as
    /// the fallback composite rep_id when a product has multi-fragment
    /// geometry that can't naturally dedup against any one inner item.
    pub ifc_id: u64,
    /// Flat `[x, y, z, x, y, z, ...]` vertex positions in model units (mm).
    pub vertices: Vec<f32>,
    /// Triangle indices into `vertices` (every 3 = one triangle).
    pub indices: Vec<u32>,
    /// Dominant source tag — the first segment's tag, kept for back-
    /// compat with consumers that don't read `segments`. For composite
    /// representations (`IfcBooleanResult` etc.), prefer iterating
    /// `segments` to see all operands.
    pub source: &'static str,
    /// Per-item provenance — one entry per representation item that
    /// contributed triangles. For an `IfcWall` whose representation is
    /// a single `IfcExtrudedAreaSolid`, this is one segment. For one
    /// whose representation is `IfcBooleanClippingResult(wall, door)`,
    /// this is two segments tagged `"boolean_first_operand"` and
    /// `"boolean_second_operand"`.
    pub segments: Vec<MeshSegment>,
    /// World-space position of the product's IfcLocalPlacement origin —
    /// i.e. where the authoring tool thinks the element "is". Used by
    /// the drift analyser to detect placement-vs-geometry mismatches
    /// (a 50mm sensor whose mesh is 100m from its basepoint is an
    /// authoring bug).
    pub placement_origin: [f32; 3],
    /// Per-fragment instancing payload — one entry per `MeshSegment`,
    /// carrying the rep step_id + untransformed local mesh + per-
    /// instance transform. Populated for the substrate writer; safe to
    /// ignore for batch consumers that only read the world-baked
    /// `vertices` / `indices`.
    pub parts: Vec<InstancePart>,
    /// The product's full world placement matrix (4x4 col-major). The
    /// world-baked `vertices` were computed as
    /// `world_transform * part.instance_transform * local_vertex`.
    pub world_transform: [f32; 16],
    /// Precise (f64) world position of the product's placement origin.
    /// Same point as `placement_origin` but resolved through the f64
    /// placement chain, so it stays exact at georeferenced magnitudes
    /// where the f32 `placement_origin` quantises to tens of mm / metres.
    /// This is the *authoring* placement — where the IfcLocalPlacement
    /// chain says the product is.
    pub world_origin: [f64; 3],
    /// Precise (f64) world position of the product's first geometry
    /// fragment's local origin (kernel rebase reference). For typical
    /// authoring this equals `world_origin`; for transformed /
    /// georeferenced files where huge world coords were baked into the
    /// representation geometry (not the placement) it's the geometry's
    /// actual world position, which the placement chain doesn't reach.
    /// This is the anchor a [`BakeFrame::Local`] consumer adds back
    /// (minus a global shift, in f64) to position near-origin shape
    /// geometry in world space — using `world_origin` instead would
    /// re-collapse rebased fragments.
    pub mesh_anchor: [f64; 3],
    /// Product-level fallback colour resolved through the material
    /// chain (`IfcRelAssociatesMaterial` → `IfcMaterial` →
    /// `IfcMaterialDefinitionRepresentation` →
    /// `IfcStyledRepresentation` → `IfcStyledItem`). Used only when no
    /// `InstancePart` carries a `surface_color`. `None` when the
    /// product has no styled material — caller falls back to a
    /// per-entity palette.
    pub surface_color: Option<[f32; 4]>,
    /// Parametric `IfcPolygonalBoundedHalfSpace` cutters carried for the
    /// W6 bounded cut (GH #58 F6). `None` / empty for every product that
    /// has no bounded-halfspace cutter — which is all of them in default
    /// builds, where the field is never read. See
    /// [`BoundedHalfspacePayload`]. Always-present (not cfg-gated) so
    /// constructors stay free of feature blocks.
    pub bounded_halfspaces: Vec<BoundedHalfspacePayload>,
}

#[derive(Debug, Default, Clone)]
pub struct MeshStats {
    pub products_seen: usize,
    pub products_meshed: usize,
    pub products_deferred: usize,
    /// Products emitted to the sink with an empty geometry buffer
    /// (`vertices.is_empty()`) because they have no `Representation`,
    /// no body items, or every item was unhandled. Only nonzero when
    /// the sink returns `wants_geometryless()` == `true`; for legacy
    /// sinks (OBJ/glTF/drift) this stays 0 and the products are silently
    /// deferred as before.
    pub products_emitted_geometryless: usize,
    pub triangles: usize,
    pub by_source: HashMap<String, usize>,
    pub elapsed_ms: f64,
    pub entity_table_build_ms: f64,
}

/// Streaming sink for product meshes. Implementors decide whether to
/// accumulate (legacy glTF/OBJ batch writers) or emit + drop (the
/// streaming Parquet/bundle writer that bounds RAM at one product).
///
/// Bounded-RAM analysis lives or dies on the sink end consuming and
/// releasing each `ProductMesh` immediately — otherwise the per-product
/// `Vec<f32>` vertex buffers accumulate exactly like the old global
/// `Vec<ProductMesh>` did, and we're back at OOM on 1 GB files.
pub trait ProductSink {
    fn on_product(&mut self, mesh: ProductMesh);

    /// Whether this sink also wants emissions for products that produce
    /// no body geometry (no `Representation`, no body items, or every
    /// item was unhandled). Default: `false` — preserves the legacy
    /// contract for batch OBJ/glTF/drift consumers that assume every
    /// `ProductMesh` carries real triangles.
    ///
    /// The substrate [`crate::bundle::parquet_sink::ParquetSink`]
    /// overrides this to `true`: products without geometry still carry
    /// identity, placement, psets, materials, classifications, and a
    /// type binding — dropping them violates the reveal-all stance and
    /// hides 10–20% of the products in a typical AEC file from
    /// substrate consumers.
    fn wants_geometryless(&self) -> bool {
        false
    }
}

/// In-memory sink — used by callers that genuinely need every product
/// in a `Vec` (the batch glTF writer, the drift analyser). For new
/// pipelines prefer a streaming sink.
#[derive(Default)]
pub struct VecSink {
    pub products: Vec<ProductMesh>,
}

impl ProductSink for VecSink {
    fn on_product(&mut self, mesh: ProductMesh) {
        self.products.push(mesh);
    }
}

/// Mesh every product in the IFC and return them keyed by GUID order.
///
/// Reveal-all stance: opening elements, void volumes, intersecting
/// halfspaces, both operands of a boolean tree — all of it is emitted
/// as visible geometry. Anything we can't tessellate yet is reported
/// as `stats.by_source["unhandled:IFCXXX"]` so the consumer knows
/// exactly what's in the file that we haven't surfaced.
///
/// Batch entry point — accumulates every product into a `Vec`. Scales
/// linearly in host RAM with file size (~2-3× working-set ratio); OOMs
/// around 1 GB IFC on 16 GB hosts. For bounded-RAM analysis use
/// [`mesh_ifc_streaming`] with a streaming sink (e.g. Parquet writer).
pub fn mesh_ifc(buf: &[u8]) -> (Vec<ProductMesh>, MeshStats) {
    let mut sink = VecSink::default();
    let stats = mesh_ifc_streaming(buf, &mut sink);
    (sink.products, stats)
}

/// Streaming entry point. Walks every product once, hands each finished
/// `ProductMesh` to `sink`, then drops it. Working-set RAM is bounded
/// by the topology caches (`PlacementResolver`, `shape_cache` for
/// `IfcMappedItem` dedup) — both keyed by reusable subgraph ids, not
/// Coordinate frame the mesher bakes vertices into.
///
/// - `World`: vertices are full world coordinates (`world * local`). The
///   default, used by OBJ/glTF/drift/substrate consumers that want
///   absolute placement.
/// - `Local`: vertices carry the object's *shape* in a near-origin
///   frame — the linear (rotation/scale) part of the placement is
///   applied but the large world *translation* is dropped, with each
///   fragment's small intra-product offset preserved. This is what
///   keeps far-from-origin objects (georeferenced MEP, large site
///   coordinates) from collapsing into a single f32-quantised point.
///   QTO is translation-invariant, so it computes correct volume /
///   area / orientation on the Local frame and is immune to the
///   precision cliff. The product's world origin is still on
///   `ProductMesh.placement_origin`, so position is never lost — and
///   the GUID keeps the link to the spatial graph for relative-
///   placement queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BakeFrame {
    World,
    Local,
}

/// World-frame streaming mesh — the back-compatible entry point.
/// See [`mesh_ifc_streaming_framed`] for the frame-selectable form.
pub fn mesh_ifc_streaming<S: ProductSink>(buf: &[u8], sink: &mut S) -> MeshStats {
    mesh_ifc_streaming_framed(buf, sink, BakeFrame::World)
}

/// Streaming mesh with an explicit [`BakeFrame`]. `Local` keeps small
/// geometry precise far from the origin (see the enum docs); QTO/point-
/// sampling consumers use it, world-coordinate consumers pass `World`.
pub fn mesh_ifc_streaming_framed<S: ProductSink>(
    buf: &[u8],
    sink: &mut S,
    frame: BakeFrame,
) -> MeshStats {
    use rayon::prelude::*;

    let mut stats = MeshStats::default();

    let t0 = Instant::now();
    let table = EntityTable::build(buf);
    stats.entity_table_build_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let _ = table.len();

    // Resolve IfcStyledItem + IfcRelAssociatesMaterial chains once up
    // front so each `tessellate_one` call can look up per-fragment +
    // per-product colour by integer step_id. Cost: one extra linear
    // pass; cheap relative to placement-resolve / tessellation.
    // GH #3.
    let style_index = styles::StyleIndex::build(&table);

    let t_mesh = Instant::now();

    // Phase 1: build `Vec<Work>` for every product. Three sub-phases,
    // two of which run in parallel (GH #26):
    //
    //   1a (parallel): shard the entity table's order vec across rayon
    //      workers. Each worker filters for products and parses each
    //      product's args far enough to extract guid, entity_name,
    //      placement_id, repr_id. Placement matrix is NOT computed yet
    //      because the resolver is share-heavy + recursive.
    //   1b (serial): warm a single `PlacementResolver` against every
    //      placement_id seen in 1a, then freeze its cache into an
    //      `Arc<HashMap>`. The resolver's chain-caching makes this
    //      cheap (placement chains share long tails — every product
    //      under a building reuses the same IfcLocalPlacement parent
    //      chain).
    //   1c (parallel): each partial-work entry from 1a looks up its
    //      placement matrix in the frozen Arc map and finishes the
    //      Work entry (world, world_origin, placement_origin).
    //
    // Why split 1a/1c rather than merge into one parallel pass: 1a
    // collects the unique placement set so 1b knows what to resolve.
    // Merging would force every worker to either run the resolver
    // itself (impossible — `&mut self`) or do duplicate placement
    // lookups against a thread-safe cache (DashMap), which contends
    // on shared parent chains. Two passes with a frozen Arc map in
    // between is share-nothing and matches the contract that the
    // resolver only walks each chain once.

    /// Intermediate stub between phases 1a and 1c. Owns the parsed
    /// string fields so 1c can move them into the final `Work`.
    struct PartialWork {
        step_id: u64,
        guid: String,
        entity_name: String,
        repr_id_opt: Option<u64>,
        placement_id: Option<u64>,
    }

    let partial: Vec<PartialWork> = table
        .order()
        .par_iter()
        .filter_map(|&step_id| {
            let (type_name, args) = table.get(step_id)?;
            if !is_product_type(type_name) {
                return None;
            }
            let fields = split_top_level_args(args);
            let guid = string_at(&fields, 0).unwrap_or_default();
            let placement_id = match fields.get(5).copied().map(parse_field) {
                Some(Field::Ref(id)) => Some(id),
                _ => None,
            };
            let repr_id_opt = match fields.get(6).copied().map(parse_field) {
                Some(Field::Ref(id)) => Some(id),
                _ => None,
            };
            let entity_name =
                crate::indexer::type_name_uppercase_with_proper_case(type_name);
            Some(PartialWork {
                step_id,
                guid,
                entity_name,
                repr_id_opt,
                placement_id,
            })
        })
        .collect();
    stats.products_seen = partial.len();

    // Phase 1b: warm the placement cache against every referenced
    // placement_id. The resolver's recursive resolve() walks each
    // chain once and caches every intermediate, so resolving N
    // placements is amortized over the shared chain prefixes.
    let mut resolver = PlacementResolver::new(&table);
    for pw in &partial {
        if let Some(pid) = pw.placement_id {
            let _ = resolver.world(pid);
        }
    }
    let placement_cache = std::sync::Arc::new(resolver.into_cache());

    // Phase 1c: parallel finalize. Look up placement matrix from the
    // frozen cache, derive origin + placement origin, build Work.
    let work: Vec<Work> = partial
        .into_par_iter()
        .map(|pw| {
            let world_f64 = pw
                .placement_id
                .and_then(|pid| placement_cache.get(&pid).copied())
                .unwrap_or(DMat4::IDENTITY);
            let world = world_f64.as_mat4();
            let world_origin = {
                let p = world_f64.transform_point3(DVec3::ZERO);
                [p.x, p.y, p.z]
            };
            let placement_origin = {
                let p = world * Vec4::new(0.0, 0.0, 0.0, 1.0);
                [p.x, p.y, p.z]
            };
            Work {
                step_id: pw.step_id,
                guid: pw.guid,
                entity_name: pw.entity_name,
                repr_id_opt: pw.repr_id_opt,
                world_f64,
                world,
                world_origin,
                placement_origin,
            }
        })
        .collect();
    // `placement_cache` Arc drops when phase 1c finishes — no longer
    // needed once every Work has its baked world matrix.
    drop(placement_cache);

    // Phase 2 + 3 (interleaved): bounded ordered channel between
    // parallel tessellation workers and the serial drain. Workers send
    // `(seq, ProductOutcome)` over a bounded `sync_channel`; the main
    // thread receives, reorders out-of-order arrivals in a small
    // `BTreeMap`, and forwards to `sink.on_product` as soon as the
    // next-expected seq is available.
    //
    // Why this beats the previous `Vec<ProductOutcome>` collect (GH
    // #25):
    //   1. RAM bounded by `channel_cap + reorder_buffer_size` products
    //      in flight, not the total product count — restores the "few
    //      products in flight" contract the substrate writer relies on
    //      for 1 GB IFCs.
    //   2. T=1 matches pre-rayon baseline: no Vec scaffolding overhead,
    //      drain runs on the calling thread same as the original
    //      streaming sink.
    //   3. T>1 lets the drain consume products as soon as they're
    //      tessellated; workers don't sit on completed work waiting
    //      for an aggregate collect.
    //
    // Worker panics: if `tessellate_one` panics, the unwinding worker
    // drops its `tx` clone via rayon's panic-catching machinery, other
    // workers complete normally, the channel closes when the last
    // clone drops, and the drain exits. The panic is re-raised from
    // `std::thread::scope` once the spawn joins, propagating up to the
    // PyO3 `catch_panic` wrapper as before.
    let shape_cache: ShapeCache = ShapeCache::new();
    let num_threads = rayon::current_num_threads().max(1);

    // T=1 fast path. With one rayon worker the channel + thread::scope
    // overhead is pure cost — no parallelism to amortise. Drop
    // straight back to the pre-rayon serial path so single-threaded
    // hosts (or `RAYON_NUM_THREADS=1` consumers) match the original
    // baseline timing exactly: walk work, tessellate, drain to sink.
    if num_threads == 1 {
        for w in work {
            let outcome = tessellate_one(&table, &shape_cache, &style_index, frame, w);
            apply_outcome(outcome, sink, &mut stats);
        }
        stats.elapsed_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;
        return stats;
    }

    // Cap is large enough that workers rarely block on `tx.send`
    // even when the drain is mid-heavy-product. Each in-flight
    // ProductOutcome carries the full per-product mesh, so RAM bound
    // = cap × max_product_mesh — sized to a few MB peak for typical
    // AEC files (avg ~3 KB per product mesh on the LBK 41 MB benchmark,
    // ~10 KB on the 179 MB ARK).
    let cap = (num_threads * 16).max(64);
    // crossbeam_channel is lock-free; std mpsc's mutex+condvar
    // SyncSender measured 13–24% slower than the previous
    // Vec<ProductOutcome>::collect on real files (32k–34k products
    // at T=8). Crossbeam recovers the Vec-collect speed while
    // keeping the bounded RAM contract.
    let (tx, rx) = crossbeam_channel::bounded::<(usize, ProductOutcome)>(cap);

    std::thread::scope(|s| {
        let table_ref = &table;
        let shape_cache_ref = &shape_cache;
        let style_index_ref = &style_index;
        s.spawn(move || {
            // Rayon drives the parallel tessellation. Per-worker `tx`
            // clones via `for_each_with`; the seed `tx` is consumed
            // (and dropped at the end of `for_each_with`) so the
            // channel closes once every worker finishes its chunk.
            work.into_par_iter()
                .enumerate()
                .for_each_with(tx, |tx, (seq, w)| {
                    let outcome = tessellate_one(
                        table_ref,
                        shape_cache_ref,
                        style_index_ref,
                        frame,
                        w,
                    );
                    let _ = tx.send((seq, outcome));
                });
        });

        // Main-thread drain. Receives outcomes (possibly out of order
        // due to rayon's work-stealing), buffers in a HashMap, and
        // forwards to `sink` in seq order so the existing emission
        // contract (substrate / OBJ / glTF / cut_openings wrapper)
        // holds. HashMap (O(1)) rather than BTreeMap (O(log N))
        // because we only ever look up by `next_seq` — the ordered
        // iteration BTreeMap gives us is wasted. `&mut sink` and
        // `&mut stats` are borrowed from the caller's frame — no
        // Send bound needed on S because the sink never crosses a
        // thread boundary.
        let mut next_seq: usize = 0;
        let mut buffer: HashMap<usize, ProductOutcome> = HashMap::new();
        while let Ok((seq, outcome)) = rx.recv() {
            if seq == next_seq {
                apply_outcome(outcome, sink, &mut stats);
                next_seq += 1;
                while let Some(o) = buffer.remove(&next_seq) {
                    apply_outcome(o, sink, &mut stats);
                    next_seq += 1;
                }
            } else {
                buffer.insert(seq, outcome);
            }
        }
        // Channel closed. A non-empty buffer here means some workers
        // never sent (panic); drain in seq order anyway so the sink
        // sees the surviving outcomes. The panic will re-raise from
        // `std::thread::scope` once the spawn joins.
        let mut leftover: Vec<(usize, ProductOutcome)> =
            buffer.into_iter().collect();
        leftover.sort_by_key(|(seq, _)| *seq);
        for (_, outcome) in leftover {
            apply_outcome(outcome, sink, &mut stats);
        }
    });

    stats.elapsed_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;
    stats
}

/// Per-product work item emitted by phase 1 of
/// [`mesh_ifc_streaming_framed`]. Owns everything the parallel
/// tessellation phase needs so the worker closure doesn't borrow
/// per-item table slices across thread boundaries.
struct Work {
    step_id: u64,
    guid: String,
    entity_name: String,
    repr_id_opt: Option<u64>,
    world_f64: DMat4,
    world: Mat4,
    world_origin: [f64; 3],
    placement_origin: [f32; 3],
}

/// Result of tessellating one product. Carries enough context for the
/// serial drain in phase 3 to:
///   - bump stats counters (products_meshed, triangles, by_source);
///   - emit the right `ProductMesh` (or invoke the geometryless path)
///     via the sink, in IFC iteration order.
/// Stats live here (as `Vec<String>` and counters) rather than being
/// updated from the parallel closures, so we avoid a hot lock on
/// `MeshStats.by_source`.
enum ProductOutcome {
    Mesh {
        product: ProductMesh,
        triangle_count: usize,
        segment_tags: Vec<String>,
        unhandled_types: Vec<String>,
    },
    Geometryless {
        reason: &'static str,
        guid: String,
        entity_name: String,
        ifc_id: u64,
        placement_origin: [f32; 3],
        world: Mat4,
        world_origin: [f64; 3],
        unhandled_types: Vec<String>,
    },
}

/// Tessellate a single product into a [`ProductOutcome`]. Called from
/// the parallel phase 2 of [`mesh_ifc_streaming_framed`]; reads-only
/// against `&table`, share-by-reference against the DashMap
/// `shape_cache`. No mutation of caller-visible state.
fn tessellate_one(
    table: &EntityTable,
    shape_cache: &ShapeCache,
    style_index: &styles::StyleIndex,
    frame: BakeFrame,
    w: Work,
) -> ProductOutcome {
    let Work {
        step_id,
        guid,
        entity_name,
        repr_id_opt,
        world_f64,
        world,
        world_origin,
        placement_origin,
    } = w;

    let repr_id = match repr_id_opt {
        Some(id) => id,
        None => {
            return ProductOutcome::Geometryless {
                reason: "no_representation",
                guid,
                entity_name,
                ifc_id: step_id,
                placement_origin,
                world,
                world_origin,
                unhandled_types: Vec::new(),
            };
        }
    };

    let items = body_items(table, repr_id);
    if items.is_empty() {
        return ProductOutcome::Geometryless {
            reason: "no_body_items",
            guid,
            entity_name,
            ifc_id: step_id,
            placement_origin,
            world,
            world_origin,
            unhandled_types: Vec::new(),
        };
    }

    let mut combined_v: Vec<f32> = Vec::new();
    let mut combined_i: Vec<u32> = Vec::new();
    let mut segments: Vec<MeshSegment> = Vec::new();
    let mut parts: Vec<InstancePart> = Vec::new();
    let mut mesh_anchor_f64: Option<DVec3> = None;
    let mut unhandled_types: Vec<String> = Vec::new();
    // Only mutated under `prism-csg-fast` (the rebake loop below); stays an
    // empty, never-pushed Vec on default builds.
    #[cfg_attr(not(feature = "prism-csg-fast"), allow(unused_mut))]
    let mut bounded_halfspaces: Vec<BoundedHalfspacePayload> = Vec::new();

    for item_id in items {
        let fragments = mesh_item(table, item_id, shape_cache);
        for frag in fragments {
            match frag {
                MeshFragment::Mesh {
                    mesh: local,
                    source,
                    roles,
                    rep_step_id,
                    instance_transform,
                    bounded_halfspace,
                } => {
                    let seg_index_start = combined_i.len() as u32;
                    let base = (combined_v.len() / 3) as u32;
                    let effective = world * instance_transform;
                    let instance_f64 = mat4_to_dmat4(instance_transform);
                    let effective_f64 = world_f64 * instance_f64;
                    let rep_origin_f64 = DVec3::new(
                        local.rep_origin[0],
                        local.rep_origin[1],
                        local.rep_origin[2],
                    );
                    let precise_anchor_f64 = effective_f64.transform_point3(rep_origin_f64);
                    let _ = mesh_anchor_f64.get_or_insert(precise_anchor_f64);
                    let anchor_f32 = Vec3::new(
                        precise_anchor_f64.x as f32,
                        precise_anchor_f64.y as f32,
                        precise_anchor_f64.z as f32,
                    );
                    match frame {
                        BakeFrame::World => {
                            for chunk in local.vertices.chunks_exact(3) {
                                let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
                                let w = effective.transform_vector3(p) + anchor_f32;
                                combined_v.push(w.x);
                                combined_v.push(w.y);
                                combined_v.push(w.z);
                            }
                        }
                        BakeFrame::Local => {
                            let anchor = mesh_anchor_f64
                                .expect("pinned above for both bake frames");
                            let frag_off_f64 = precise_anchor_f64 - anchor;
                            let frag_off = Vec3::new(
                                frag_off_f64.x as f32,
                                frag_off_f64.y as f32,
                                frag_off_f64.z as f32,
                            );
                            for chunk in local.vertices.chunks_exact(3) {
                                let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
                                let v = effective.transform_vector3(p) + frag_off;
                                combined_v.push(v.x);
                                combined_v.push(v.y);
                                combined_v.push(v.z);
                            }
                        }
                    }
                    for &idx in &local.indices {
                        combined_i.push(base + idx);
                    }

                    // W6: bake the bounded-halfspace payload into the SAME
                    // frame as this fragment's vertices so `cut_openings`
                    // can use it against the baked mesh. The fragment's
                    // bake map is affine: linear = `effective` (rotation +
                    // any instance scale), translation = `anchor_f32`
                    // (World) or the per-fragment offset (Local). Apply
                    // that exact map to the payload's points + xform and
                    // the linear part to its normal.
                    // Only the `prism-csg-fast` bounded fast-path consumes
                    // `bounded_halfspaces`; default builds skip the per-
                    // product re-bake entirely (the payload is dropped).
                    #[cfg(not(feature = "prism-csg-fast"))]
                    let _ = bounded_halfspace;
                    #[cfg(feature = "prism-csg-fast")]
                    if let Some(p) = bounded_halfspace {
                        let bake_translation = match frame {
                            BakeFrame::World => anchor_f32,
                            BakeFrame::Local => {
                                let anchor = mesh_anchor_f64
                                    .expect("pinned above for both bake frames");
                                let off = precise_anchor_f64 - anchor;
                                Vec3::new(off.x as f32, off.y as f32, off.z as f32)
                            }
                        };
                        let bake = {
                            let mut m = effective;
                            m.w_axis = Vec4::new(
                                bake_translation.x,
                                bake_translation.y,
                                bake_translation.z,
                                1.0,
                            );
                            m
                        };
                        // Normal: inverse-transpose of the linear part, so
                        // mirrored / non-uniformly-scaled placements
                        // (negative-determinant Revit families) transform
                        // the plane normal correctly instead of skewing /
                        // flipping it (GH #64 #6). For a pure rotation
                        // (R⁻¹)ᵀ = R, identical to the linear part.
                        let normal_xform =
                            glam::Mat3::from_mat4(effective).inverse().transpose();
                        bounded_halfspaces.push(BoundedHalfspacePayload {
                            boundary: p.boundary,
                            boundary_xform: bake * p.boundary_xform,
                            plane_normal: normal_xform
                                .mul_vec3(p.plane_normal)
                                .normalize_or_zero(),
                            plane_point: bake.transform_point3(p.plane_point),
                        });
                    }
                    let seg_index_count = combined_i.len() as u32 - seg_index_start;
                    if seg_index_count > 0 {
                        // Serialise the chain outermost-first: `roles`
                        // is stored innermost-first (the deepest retag
                        // pushed first as we walked back up the tree),
                        // so iterate in reverse, then append the leaf
                        // `source`. A 3-deep boolean cutter-of-cutter
                        // produces `boolean_second_operand|boolean_second_operand|extrusion`;
                        // a cutter-side fragment that is a host at the
                        // innermost level produces
                        // `boolean_second_operand|boolean_first_operand|extrusion`.
                        // Readers split on `|` and scan tokens — see
                        // `cut_openings::chain_contains` / `chain_count`.
                        let tag = if roles.is_empty() {
                            source.to_string()
                        } else {
                            let mut buf =
                                String::with_capacity(roles.iter().map(|r| r.len() + 1).sum::<usize>() + source.len());
                            for r in roles.iter().rev() {
                                buf.push_str(r);
                                buf.push('|');
                            }
                            buf.push_str(source);
                            buf
                        };
                        segments.push(MeshSegment {
                            index_start: seg_index_start,
                            index_count: seg_index_count,
                            source: tag.clone(),
                        });
                        let surface_color = style_index.item.get(&rep_step_id).copied();
                        parts.push(InstancePart {
                            rep_step_id,
                            instance_transform: instance_transform.to_cols_array(),
                            local_vertices: local.vertices,
                            local_indices: local.indices,
                            index_start: seg_index_start,
                            index_count: seg_index_count,
                            source: tag,
                            surface_color,
                        });
                    }
                }
                MeshFragment::Unhandled { ifc_type } => {
                    unhandled_types.push(format!("unhandled:{}", ifc_type));
                }
            }
        }
    }

    if combined_i.is_empty() {
        return ProductOutcome::Geometryless {
            reason: "item_unhandled",
            guid,
            entity_name,
            ifc_id: step_id,
            placement_origin,
            world,
            world_origin,
            unhandled_types,
        };
    }

    let source_tag: &'static str = segments
        .first()
        .and_then(|s| {
            let leaf = s.source.rsplit('|').next().unwrap_or(s.source.as_str());
            MeshFragment::source_tags().iter().find(|t| **t == leaf).copied()
        })
        .unwrap_or("composite");

    let triangle_count = combined_i.len() / 3;
    let segment_tags: Vec<String> = segments.iter().map(|s| s.source.clone()).collect();
    let mesh_anchor = match mesh_anchor_f64 {
        Some(a) => [a.x, a.y, a.z],
        None => world_origin,
    };

    let product_surface_color = style_index.product.get(&step_id).copied();
    ProductOutcome::Mesh {
        product: ProductMesh {
            guid,
            entity: entity_name,
            ifc_id: step_id,
            vertices: combined_v,
            indices: combined_i,
            source: source_tag,
            segments,
            placement_origin,
            parts,
            world_transform: world.to_cols_array(),
            world_origin,
            mesh_anchor,
            surface_color: product_surface_color,
            bounded_halfspaces,
        },
        triangle_count,
        segment_tags,
        unhandled_types,
    }
}

/// Tessellate a specific subset of products, identified by step id, into
/// their [`ProductMesh`]es in the requested [`BakeFrame`]. Geometryless
/// products yield no entry; output order follows `step_ids` (skipping any
/// step that isn't a product or isn't found).
///
/// This is the single-product fast path behind `m.mesh(guid=…)` (GH #47).
/// It builds the entity table + style index (one linear pass each) and
/// resolves only the requested products' placement + representation
/// chains, then runs the same [`tessellate_one`] the batch streaming pass
/// uses — but skips tessellating the rest of the model. For one product
/// (plus, in cut mode, the handful of openings voiding it) that turns an
/// O(model) tessellation into an O(target) one.
///
/// No cross-product opening cutting is applied here — the caller owns
/// that (it already holds the `IfcRelVoidsElement` index from the
/// indexer) and routes the returned meshes through
/// [`cut_openings::CrossProductCut`] exactly as the batch path does.
pub fn mesh_products_by_step(
    buf: &[u8],
    step_ids: &[u64],
    frame: BakeFrame,
) -> Vec<ProductMesh> {
    let table = EntityTable::build(buf);
    let style_index = styles::StyleIndex::build(&table);
    // One resolver shared across the (few) requested products — its
    // chain caching makes resolving the host + its openings cheap, since
    // an opening's placement chain typically shares the host's tail.
    let mut resolver = PlacementResolver::new(&table);
    let shape_cache: ShapeCache = ShapeCache::new();

    let mut out = Vec::with_capacity(step_ids.len());
    for &step_id in step_ids {
        let (type_name, args) = match table.get(step_id) {
            Some(x) => x,
            None => continue,
        };
        if !is_product_type(type_name) {
            continue;
        }
        // Same arg-positions the batch phase 1a parses: guid (0),
        // ObjectPlacement (5), Representation (6).
        let fields = split_top_level_args(args);
        let guid = string_at(&fields, 0).unwrap_or_default();
        let placement_id = match fields.get(5).copied().map(parse_field) {
            Some(Field::Ref(id)) => Some(id),
            _ => None,
        };
        let repr_id_opt = match fields.get(6).copied().map(parse_field) {
            Some(Field::Ref(id)) => Some(id),
            _ => None,
        };
        let entity_name = crate::indexer::type_name_uppercase_with_proper_case(type_name);
        let world_f64 = placement_id
            .map(|pid| resolver.world(pid))
            .unwrap_or(DMat4::IDENTITY);
        let world = world_f64.as_mat4();
        let world_origin = {
            let p = world_f64.transform_point3(DVec3::ZERO);
            [p.x, p.y, p.z]
        };
        let placement_origin = {
            let p = world * Vec4::new(0.0, 0.0, 0.0, 1.0);
            [p.x, p.y, p.z]
        };
        let w = Work {
            step_id,
            guid,
            entity_name,
            repr_id_opt,
            world_f64,
            world,
            world_origin,
            placement_origin,
        };
        if let ProductOutcome::Mesh { product, .. } =
            tessellate_one(&table, &shape_cache, &style_index, frame, w)
        {
            out.push(product);
        }
    }
    out
}

/// Apply one [`ProductOutcome`] to stats + sink. Called serially from
/// phase 3 of [`mesh_ifc_streaming_framed`] so emission order matches
/// the IFC entity-table iteration order and stats counters need no
/// synchronisation.
fn apply_outcome<S: ProductSink>(
    outcome: ProductOutcome,
    sink: &mut S,
    stats: &mut MeshStats,
) {
    match outcome {
        ProductOutcome::Mesh {
            product,
            triangle_count,
            segment_tags,
            unhandled_types,
        } => {
            stats.products_meshed += 1;
            stats.triangles += triangle_count;
            for tag in segment_tags {
                *stats.by_source.entry(tag).or_insert(0) += 1;
            }
            for tag in unhandled_types {
                *stats.by_source.entry(tag).or_insert(0) += 1;
            }
            sink.on_product(product);
        }
        ProductOutcome::Geometryless {
            reason,
            guid,
            entity_name,
            ifc_id,
            placement_origin,
            world,
            world_origin,
            unhandled_types,
        } => {
            for tag in unhandled_types {
                *stats.by_source.entry(tag).or_insert(0) += 1;
            }
            emit_geometryless(
                sink,
                stats,
                reason,
                guid,
                entity_name,
                ifc_id,
                placement_origin,
                world,
                world_origin,
            );
        }
    }
}

/// Emit a `ProductMesh` with an empty geometry buffer when the sink
/// opted in via [`ProductSink::wants_geometryless`]. Always bumps
/// `products_deferred` and the per-reason `by_source` counter so the
/// reveal-all stats stay accurate whether or not the sink wanted the
/// emission. `reason` is one of `"no_representation"`, `"no_body_items"`,
/// or `"item_unhandled"`.
#[allow(clippy::too_many_arguments)]
fn emit_geometryless<S: ProductSink>(
    sink: &mut S,
    stats: &mut MeshStats,
    reason: &str,
    guid: String,
    entity_name: String,
    ifc_id: u64,
    placement_origin: [f32; 3],
    world: Mat4,
    world_origin: [f64; 3],
) {
    stats.products_deferred += 1;
    *stats.by_source.entry(reason.to_string()).or_insert(0) += 1;
    if sink.wants_geometryless() {
        stats.products_emitted_geometryless += 1;
        sink.on_product(ProductMesh {
            guid,
            entity: entity_name,
            ifc_id,
            vertices: Vec::new(),
            indices: Vec::new(),
            source: "none",
            segments: Vec::new(),
            placement_origin,
            parts: Vec::new(),
            world_transform: world.to_cols_array(),
            world_origin,
            // Geometryless products have no geometry to anchor; default
            // mesh_anchor to the placement origin so Stage 2 sinks see a
            // consistent f64 anchor.
            mesh_anchor: world_origin,
            surface_color: None,
            bounded_halfspaces: Vec::new(),
        });
    }
}

/// Mesh a single `IfcRepresentationItem`. Returns one or more fragments
/// — each either a real mesh tagged with its source, or an explicit
/// `Unhandled` marker carrying the IFC type name so the caller can
/// bucket it into stats. Recurses for `IfcMappedItem`, `IfcBooleanResult`,
/// `IfcCsgSolid`.
pub(crate) fn mesh_item(
    table: &EntityTable,
    item_id: u64,
    shape_cache: &ShapeCache,
) -> Vec<MeshFragment> {
    if let Some(cached) = shape_cache.get(&item_id) {
        return cached
            .iter()
            .map(|(m, s)| MeshFragment::Mesh {
                mesh: clone_local(m),
                source: s,
                roles: Vec::new(),
                // Cache contents are non-composite direct geometry —
                // their natural rep_step_id IS the lookup key. No per-
                // instance transform: any IfcMappedItem composition is
                // captured by the caller (mapped::expand) on top.
                rep_step_id: item_id,
                instance_transform: Mat4::IDENTITY,
                // Bounded-halfspace fragments are excluded from the cache
                // (see the insert site below), so a cache hit never needs
                // to reconstruct a payload — it is always `None` here.
                bounded_halfspace: None,
            })
            .collect();
    }

    let (type_name, _args) = match table.get(item_id) {
        Some(x) => x,
        None => return Vec::new(),
    };

    let single = |maybe: Option<LocalMesh>, tag: &'static str| -> Vec<MeshFragment> {
        match maybe {
            Some(m) => vec![MeshFragment::Mesh {
                mesh: m,
                source: tag,
                roles: Vec::new(),
                rep_step_id: item_id,
                instance_transform: Mat4::IDENTITY,
                bounded_halfspace: None,
            }],
            None => Vec::new(),
        }
    };

    let result: Vec<MeshFragment> =
        if type_name.eq_ignore_ascii_case(b"IFCEXTRUDEDAREASOLID") {
            single(extrusion::extrude(table, item_id), "extrusion")
        } else if type_name.eq_ignore_ascii_case(b"IFCMAPPEDITEM") {
            mapped::expand(table, item_id, shape_cache)
        } else if type_name.eq_ignore_ascii_case(b"IFCPOLYGONALFACESET") {
            single(faceset::polygonal_face_set(table, item_id), "polygonal_faceset")
        } else if type_name.eq_ignore_ascii_case(b"IFCTRIANGULATEDFACESET") {
            single(
                faceset::triangulated_face_set(table, item_id),
                "triangulated_faceset",
            )
        } else if type_name.eq_ignore_ascii_case(b"IFCFACETEDBREP")
            || type_name.eq_ignore_ascii_case(b"IFCMANIFOLDSOLIDBREP")
        {
            single(brep::faceted_brep(table, item_id), "brep")
        } else if type_name.eq_ignore_ascii_case(b"IFCADVANCEDBREP") {
            single(brep::faceted_brep(table, item_id), "advanced_brep_approx")
        } else if type_name.eq_ignore_ascii_case(b"IFCFACEBASEDSURFACEMODEL") {
            single(brep::face_based_surface_model(table, item_id), "faceset_fbsm")
        } else if type_name.eq_ignore_ascii_case(b"IFCSHELLBASEDSURFACEMODEL") {
            single(brep::shell_based_surface_model(table, item_id), "faceset_sbsm")
        } else if type_name.eq_ignore_ascii_case(b"IFCBOOLEANRESULT")
            || type_name.eq_ignore_ascii_case(b"IFCBOOLEANCLIPPINGRESULT")
        {
            // The recurse callback into mesh_item threads the shared
            // `&ShapeCache` through — boolean::boolean_result takes a
            // function pointer so the recursion happens on this exact
            // frame (still useful for clarity; the borrow argument
            // is now moot since DashMap is share-by-reference).
            boolean::boolean_result(table, item_id, shape_cache, &mesh_item)
        } else if type_name.eq_ignore_ascii_case(b"IFCCSGSOLID") {
            boolean::csg_solid(table, item_id, shape_cache, &mesh_item)
        } else if type_name.eq_ignore_ascii_case(b"IFCPOLYGONALBOUNDEDHALFSPACE") {
            match boolean::polygonal_bounded_halfspace(table, item_id) {
                // Construct the fragment directly (not via `single`) so we
                // can attach the W6 bounded-halfspace payload. The payload
                // rides up the boolean tree via `retag` and lands on the
                // product's `bounded_halfspaces` list in `tessellate_one`.
                Some((m, agreement, payload)) => vec![MeshFragment::Mesh {
                    mesh: m,
                    source: if agreement {
                        "halfspace_bounded:agree"
                    } else {
                        "halfspace_bounded:disagree"
                    },
                    roles: Vec::new(),
                    rep_step_id: item_id,
                    instance_transform: Mat4::IDENTITY,
                    bounded_halfspace: Some(payload),
                }],
                None => Vec::new(),
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCHALFSPACESOLID") {
            match boolean::halfspace_solid(table, item_id) {
                Some((m, agreement)) => single(
                    Some(m),
                    if agreement { "halfspace_plane:agree" } else { "halfspace_plane:disagree" },
                ),
                None => Vec::new(),
            }
        } else if type_name.eq_ignore_ascii_case(b"IFCGEOMETRICCURVESET")
            || type_name.eq_ignore_ascii_case(b"IFCGEOMETRICSET")
        {
            single(curveset::geometric_curve_set(table, item_id), "curve_set")
        } else if type_name.eq_ignore_ascii_case(b"IFCBLOCK") {
            single(csg_primitive::block(table, item_id), "csg_block")
        } else if type_name.eq_ignore_ascii_case(b"IFCRIGHTCIRCULARCYLINDER") {
            single(csg_primitive::right_circular_cylinder(table, item_id), "csg_cylinder")
        } else if type_name.eq_ignore_ascii_case(b"IFCRIGHTCIRCULARCONE") {
            single(csg_primitive::right_circular_cone(table, item_id), "csg_cone")
        } else if type_name.eq_ignore_ascii_case(b"IFCSPHERE") {
            single(csg_primitive::sphere(table, item_id), "csg_sphere")
        } else if type_name.eq_ignore_ascii_case(b"IFCRECTANGULARPYRAMID") {
            single(csg_primitive::rectangular_pyramid(table, item_id), "csg_pyramid")
        } else if type_name.eq_ignore_ascii_case(b"IFCREVOLVEDAREASOLID") {
            single(revolved::revolved_area_solid(table, item_id), "revolved")
        } else {
            // Reveal-all stance: name the type explicitly so the
            // consumer sees exactly what's in the file we can't yet
            // tessellate, instead of a silent black hole.
            vec![MeshFragment::Unhandled {
                ifc_type: bytes_to_string(type_name),
            }]
        };

    // Cache only the real mesh fragments — unhandled markers are cheap
    // to re-derive. Composite handlers (boolean / csg) don't cache the
    // outer node either; their operand caches do the work.
    //
    // `IfcPolygonalBoundedHalfSpace` is excluded ONLY under
    // `prism-csg-fast`: there its fragment carries a `bounded_halfspace`
    // payload that the cache tuple `(LocalMesh, &str)` cannot hold, and a
    // cache hit would reconstruct the fragment with
    // `bounded_halfspace: None`, silently dropping the bounded-cut params.
    // Default builds never read the payload, so they keep caching this
    // leaf (re-extruding the slab per repeated instance would be pure
    // waste on facade-heavy IfcRepresentationMap files).
    let is_composite = type_name.eq_ignore_ascii_case(b"IFCMAPPEDITEM")
        || type_name.eq_ignore_ascii_case(b"IFCBOOLEANRESULT")
        || type_name.eq_ignore_ascii_case(b"IFCBOOLEANCLIPPINGRESULT")
        || type_name.eq_ignore_ascii_case(b"IFCCSGSOLID")
        || (cfg!(feature = "prism-csg-fast")
            && type_name.eq_ignore_ascii_case(b"IFCPOLYGONALBOUNDEDHALFSPACE"));
    if !is_composite {
        let cacheable: Vec<(LocalMesh, &'static str)> = result
            .iter()
            .filter_map(|f| match f {
                MeshFragment::Mesh { mesh, source, .. } => {
                    Some((clone_local(mesh), *source))
                }
                MeshFragment::Unhandled { .. } => None,
            })
            .collect();
        shape_cache.insert(item_id, cacheable);
    }
    result
}

fn clone_local(m: &LocalMesh) -> LocalMesh {
    LocalMesh {
        vertices: m.vertices.clone(),
        indices: m.indices.clone(),
        rep_origin: m.rep_origin,
    }
}

/// Collect the top-level Items list from a representation, preferring
/// Body / Facetation contexts.
fn body_items(table: &EntityTable, repr_id: u64) -> Vec<u64> {
    let (type_name, args) = match table.get(repr_id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    // IfcProductDefinitionShape(Name, Description, Representations: LIST OF IfcRepresentation)
    if type_name.eq_ignore_ascii_case(b"IFCPRODUCTDEFINITIONSHAPE") {
        let fields = split_top_level_args(args);
        let body = match parse_field(fields.get(2).unwrap_or(&&[][..])) {
            Field::List(b) => b,
            _ => return Vec::new(),
        };
        // Try every representation; prefer Body / Facetation context.
        let mut body_id: Option<u64> = None;
        let mut any_id: Option<u64> = None;
        for f in split_top_level_args(body) {
            if let Field::Ref(rid) = parse_field(f) {
                if is_body_or_facetation(table, rid) {
                    body_id = Some(rid);
                    break;
                }
                if any_id.is_none() {
                    any_id = Some(rid);
                }
            }
        }
        let chosen = body_id.or(any_id);
        return chosen.map(|id| representation_items(table, id)).unwrap_or_default();
    }
    // IfcShapeRepresentation directly (rare top-level).
    representation_items(table, repr_id)
}

fn is_body_or_facetation(table: &EntityTable, repr_id: u64) -> bool {
    let (type_name, args) = match table.get(repr_id) {
        Some(x) => x,
        None => return false,
    };
    if !type_name.eq_ignore_ascii_case(b"IFCSHAPEREPRESENTATION") {
        return false;
    }
    let fields = split_top_level_args(args);
    // IfcShapeRepresentation: (ContextOfItems, RepresentationIdentifier,
    //                          RepresentationType, Items)
    // RepresentationIdentifier at arg[1].
    let ident = match parse_field(fields.get(1).unwrap_or(&&[][..])) {
        Field::String(s) => s.to_lowercase(),
        _ => return false,
    };
    matches!(ident.as_str(), "body" | "facetation")
}

pub(crate) fn representation_items(table: &EntityTable, repr_id: u64) -> Vec<u64> {
    let (type_name, args) = match table.get(repr_id) {
        Some(x) => x,
        None => return Vec::new(),
    };
    if !type_name.eq_ignore_ascii_case(b"IFCSHAPEREPRESENTATION") {
        return Vec::new();
    }
    let fields = split_top_level_args(args);
    let items = match parse_field(fields.get(3).unwrap_or(&&[][..])) {
        Field::List(b) => b,
        _ => return Vec::new(),
    };
    split_top_level_args(items)
        .into_iter()
        .filter_map(|f| match parse_field(f) {
            Field::Ref(id) => Some(id),
            _ => None,
        })
        .collect()
}

/// Delegate to the indexer's canonical product-type set
/// ([`crate::indexer::is_meshable_product`]). Pre-fix this module
/// maintained its own permissive "starts with IFC and not in this
/// blacklist" filter — a leaky proxy that let representation primitives
/// (IfcPolyloop, IfcFaceOuterBound, IfcSphericalSurface, ...) through
/// to the streaming loop. With the silent-drop fix, that leakage would
/// have written every such primitive as a junk instance row.
fn is_product_type(type_name: &[u8]) -> bool {
    crate::indexer::is_meshable_product(type_name)
}

fn string_at(fields: &[&[u8]], idx: usize) -> Option<String> {
    let f = fields.get(idx)?;
    match parse_field(f) {
        Field::String(s) => Some(s),
        _ => None,
    }
}

fn bytes_to_string(b: &[u8]) -> String {
    std::str::from_utf8(b)
        .map(|s| s.to_ascii_uppercase())
        .unwrap_or_else(|_| String::from_utf8_lossy(b).into_owned())
}

// ----------------------------------------------------------------------
// Synthetic-cutter stripping (GH #66)
// ----------------------------------------------------------------------

/// True when a compound segment tag names a **synthetic half-space
/// visualisation fragment on the subtracted side of a boolean** — the
/// `±HALFSPACE_PLANE_EXTENT` stand-in slab `boolean.rs` emits so an
/// *infinite* `IfcHalfSpaceSolid` has something visible. That slab is
/// tool geometry, not element geometry: it has foreign extent (a 40 m
/// square inside a 7 m floor strip, GH #66), and when it leaks into a
/// consumer it poisons meshes, AABBs, point-cloud sampling, drift
/// stats, the clash substrate and glTF output.
///
/// Deliberately narrow: an *authored* solid on the subtracted side
/// (`boolean_second_operand|extrusion`, a void shape modelled as a real
/// solid) is bounded, local, authored geometry and stays revealed. Only
/// the synthetic `halfspace_plane*` / `halfspace_bounded*` leaves under
/// a `boolean_second_operand` role match (including their
/// `:agree`/`:disagree` orientation-suffixed forms).
pub fn is_synthetic_cutter_tag(tag: &str) -> bool {
    let mut second_operand = false;
    let mut halfspace_leaf = false;
    for link in tag.split('|') {
        if link == "boolean_second_operand" {
            second_operand = true;
        } else if link.starts_with("halfspace_plane")
            || link.starts_with("halfspace_bounded")
        {
            halfspace_leaf = true;
        }
    }
    second_operand && halfspace_leaf
}

/// Remove synthetic half-space cutter fragments from a tessellated
/// product (GH #66). Runs **instead of** `cut_openings::apply` — apply
/// consumes the cutter fragments as cut payloads and removes them
/// itself; this pass is for every path where the cut does NOT run
/// (`cut_openings=False`, builds without the `csg` feature, the bundle
/// substrate, point clouds, drift). Without it the product mesh ships
/// the `±20 000`-unit stand-in slabs as if they were element geometry.
///
/// Rewrites `indices`, `segments` and `parts` (kept in lockstep — they
/// are pushed pairwise in `tessellate_one`) and **compacts `vertices`**
/// to the referenced set, because downstream AABBs (`stats.rs`,
/// `bundle::record`) iterate the raw vertex buffer and orphaned cutter
/// vertices would still poison them. `bounded_halfspaces` payloads are
/// left in place — they are inert unless `apply` runs. The legacy
/// `source` field (dominant first-segment tag) is not rewritten; it is
/// documented as advisory.
///
/// Returns the number of segments removed (0 = untouched).
pub fn strip_synthetic_cutters(mesh: &mut ProductMesh) -> u32 {
    let removed = mesh
        .segments
        .iter()
        .filter(|s| is_synthetic_cutter_tag(&s.source))
        .count() as u32;
    if removed == 0 {
        return 0;
    }

    let old_indices = std::mem::take(&mut mesh.indices);
    let mut new_indices: Vec<u32> = Vec::with_capacity(old_indices.len());
    let mut new_segments: Vec<MeshSegment> = Vec::with_capacity(mesh.segments.len());
    let mut kept_ranges: Vec<(u32, u32)> = Vec::new();

    for seg in mesh.segments.drain(..) {
        if is_synthetic_cutter_tag(&seg.source) {
            continue;
        }
        let start = seg.index_start as usize;
        let end = start + seg.index_count as usize;
        let new_start = new_indices.len() as u32;
        if end <= old_indices.len() {
            new_indices.extend_from_slice(&old_indices[start..end]);
        }
        kept_ranges.push((seg.index_start, new_start));
        new_segments.push(MeshSegment {
            index_start: new_start,
            index_count: seg.index_count,
            source: seg.source,
        });
    }

    // Parts are pushed in lockstep with segments and share index_start;
    // keep the ones whose old range survived, retargeted to the new one.
    let retarget: std::collections::HashMap<u32, u32> =
        kept_ranges.into_iter().collect();
    mesh.parts.retain(|p| retarget.contains_key(&p.index_start));
    for p in &mut mesh.parts {
        if let Some(&ns) = retarget.get(&p.index_start) {
            p.index_start = ns;
        }
    }

    // Vertex compaction: downstream AABBs iterate the raw buffer.
    let vert_count = mesh.vertices.len() / 3;
    let mut remap: Vec<u32> = vec![u32::MAX; vert_count];
    let mut new_vertices: Vec<f32> = Vec::with_capacity(mesh.vertices.len());
    let mut next: u32 = 0;
    for idx in &mut new_indices {
        let old = *idx as usize;
        if old >= vert_count {
            continue; // malformed index — leave as-is rather than panic
        }
        if remap[old] == u32::MAX {
            remap[old] = next;
            new_vertices.extend_from_slice(&mesh.vertices[old * 3..old * 3 + 3]);
            next += 1;
        }
        *idx = remap[old];
    }

    mesh.indices = new_indices;
    mesh.segments = new_segments;
    mesh.vertices = new_vertices;
    removed
}

#[cfg(test)]
mod strip_cutter_tests {
    use super::*;

    fn box_at(ox: f32, size: f32, vertices: &mut Vec<f32>, indices: &mut Vec<u32>) -> (u32, u32) {
        let base = (vertices.len() / 3) as u32;
        let start = indices.len() as u32;
        let s = size;
        let corners = [
            [ox, 0.0, 0.0], [ox + s, 0.0, 0.0], [ox + s, s, 0.0], [ox, s, 0.0],
            [ox, 0.0, s], [ox + s, 0.0, s], [ox + s, s, s], [ox, s, s],
        ];
        for c in corners { vertices.extend_from_slice(&c); }
        const QUADS: [[u32; 4]; 6] = [
            [0, 1, 2, 3], [4, 5, 6, 7], [0, 1, 5, 4],
            [2, 3, 7, 6], [1, 2, 6, 5], [0, 3, 7, 4],
        ];
        for q in QUADS {
            indices.extend_from_slice(&[base + q[0], base + q[1], base + q[2]]);
            indices.extend_from_slice(&[base + q[0], base + q[2], base + q[3]]);
        }
        (start, indices.len() as u32 - start)
    }

    fn product_with(boxes: &[(&str, f32, f32)]) -> ProductMesh {
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut segments = Vec::new();
        for (tag, ox, size) in boxes {
            let (start, count) = box_at(*ox, *size, &mut vertices, &mut indices);
            segments.push(MeshSegment {
                index_start: start,
                index_count: count,
                source: tag.to_string(),
            });
        }
        ProductMesh {
            guid: "G".into(),
            entity: "IfcSlab".into(),
            ifc_id: 1,
            vertices,
            indices,
            source: "extrusion",
            segments,
            placement_origin: [0.0; 3],
            parts: Vec::new(),
            world_transform: glam::Mat4::IDENTITY.to_cols_array(),
            world_origin: [0.0; 3],
            mesh_anchor: [0.0; 3],
            surface_color: None,
            bounded_halfspaces: Vec::new(),
        }
    }

    #[test]
    fn tag_predicate_matches_synthetic_cutters_only() {
        assert!(is_synthetic_cutter_tag("boolean_second_operand|halfspace_plane"));
        assert!(is_synthetic_cutter_tag("boolean_second_operand|halfspace_bounded"));
        assert!(is_synthetic_cutter_tag("boolean_second_operand|halfspace_plane:agree"));
        assert!(is_synthetic_cutter_tag(
            "boolean_second_operand|boolean_second_operand|halfspace_bounded:disagree"
        ));
        // authored solid subtractor: revealed, NOT stripped
        assert!(!is_synthetic_cutter_tag("boolean_second_operand|extrusion"));
        // host-side geometry never stripped
        assert!(!is_synthetic_cutter_tag("boolean_first_operand|extrusion"));
        // union operands are additive geometry
        assert!(!is_synthetic_cutter_tag("boolean_union_operand|halfspace_plane"));
        assert!(!is_synthetic_cutter_tag("extrusion"));
        // halfspace leaf without a boolean role (bare clip target) stays
        assert!(!is_synthetic_cutter_tag("halfspace_bounded"));
    }

    #[test]
    fn strip_removes_cutters_and_compacts() {
        // GH #66 anatomy: host extrusion + two giant halfspace caps.
        let mut mesh = product_with(&[
            ("boolean_first_operand|extrusion", 0.0, 1.0),
            ("boolean_second_operand|halfspace_plane", 100.0, 40.0),
            ("boolean_second_operand|halfspace_bounded", 200.0, 40.0),
        ]);
        let before_tris = mesh.indices.len() / 3;
        let removed = strip_synthetic_cutters(&mut mesh);
        assert_eq!(removed, 2);
        assert_eq!(mesh.segments.len(), 1);
        assert_eq!(mesh.indices.len() / 3, before_tris / 3); // 36 -> 12
        assert_eq!(mesh.vertices.len() / 3, 8); // compacted to host verts
        // AABB over the raw vertex buffer no longer sees the 40-unit caps
        let max_x = mesh
            .vertices
            .chunks_exact(3)
            .map(|c| c[0])
            .fold(f32::MIN, f32::max);
        assert!(max_x <= 1.0 + 1e-6, "max_x={max_x}");
        // indices all reference the compacted buffer
        let vc = (mesh.vertices.len() / 3) as u32;
        assert!(mesh.indices.iter().all(|&i| i < vc));
        assert_eq!(mesh.segments[0].index_start, 0);
    }

    #[test]
    fn strip_noop_without_cutters() {
        let mut mesh = product_with(&[
            ("extrusion", 0.0, 1.0),
            ("boolean_second_operand|extrusion", 5.0, 1.0),
        ]);
        let v = mesh.vertices.clone();
        let i = mesh.indices.clone();
        assert_eq!(strip_synthetic_cutters(&mut mesh), 0);
        assert_eq!(mesh.vertices, v);
        assert_eq!(mesh.indices, i);
        assert_eq!(mesh.segments.len(), 2);
    }

    #[test]
    fn strip_keeps_interleaved_host_segments_in_order() {
        let mut mesh = product_with(&[
            ("boolean_first_operand|extrusion", 0.0, 1.0),
            ("boolean_second_operand|halfspace_plane:agree", 100.0, 40.0),
            ("boolean_first_operand|brep", 2.0, 1.0),
        ]);
        let removed = strip_synthetic_cutters(&mut mesh);
        assert_eq!(removed, 1);
        assert_eq!(mesh.segments.len(), 2);
        assert_eq!(mesh.segments[0].source, "boolean_first_operand|extrusion");
        assert_eq!(mesh.segments[1].source, "boolean_first_operand|brep");
        // contiguity: second segment starts where the first ends
        assert_eq!(
            mesh.segments[1].index_start,
            mesh.segments[0].index_start + mesh.segments[0].index_count
        );
        assert_eq!(mesh.vertices.len() / 3, 16);
    }
}

