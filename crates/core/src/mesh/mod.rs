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
pub mod curveset;
pub mod extrusion;
pub mod faceset;
pub mod gltf;
pub mod halfspace_clip;
pub mod indexed_curve;
pub mod mapped;
pub mod obj;
pub mod placement;
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
        source: &'static str,
        role: Option<&'static str>,
        rep_step_id: u64,
        instance_transform: Mat4,
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

    for item_id in items {
        let fragments = mesh_item(table, item_id, shape_cache);
        for frag in fragments {
            match frag {
                MeshFragment::Mesh {
                    mesh: local,
                    source,
                    role,
                    rep_step_id,
                    instance_transform,
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
                    let seg_index_count = combined_i.len() as u32 - seg_index_start;
                    if seg_index_count > 0 {
                        let tag = match role {
                            Some(r) => format!("{}|{}", r, source),
                            None => source.to_string(),
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
        },
        triangle_count,
        segment_tags,
        unhandled_types,
    }
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
                role: None,
                // Cache contents are non-composite direct geometry —
                // their natural rep_step_id IS the lookup key. No per-
                // instance transform: any IfcMappedItem composition is
                // captured by the caller (mapped::expand) on top.
                rep_step_id: item_id,
                instance_transform: Mat4::IDENTITY,
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
                role: None,
                rep_step_id: item_id,
                instance_transform: Mat4::IDENTITY,
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
                Some((m, agreement)) => single(
                    Some(m),
                    if agreement { "halfspace_bounded:agree" } else { "halfspace_bounded:disagree" },
                ),
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
    let is_composite = type_name.eq_ignore_ascii_case(b"IFCMAPPEDITEM")
        || type_name.eq_ignore_ascii_case(b"IFCBOOLEANRESULT")
        || type_name.eq_ignore_ascii_case(b"IFCBOOLEANCLIPPINGRESULT")
        || type_name.eq_ignore_ascii_case(b"IFCCSGSOLID");
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

