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
pub mod curveset;
pub mod extrusion;
pub mod faceset;
pub mod gltf;
pub mod mapped;
pub mod obj;
pub mod placement;
pub mod profile;
pub mod qto;
pub mod revolved;
pub mod sample;
pub mod stats;

use std::collections::HashMap;
use std::time::Instant;

use glam::{DMat4, DVec3, Mat4, Vec3, Vec4};

use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};
use crate::mesh::extrusion::LocalMesh;
use crate::mesh::placement::PlacementResolver;

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
            "halfspace_bounded",
            "halfspace_plane",
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
    /// This is the anchor a [`BakeFrame::Local`] consumer adds back (in
    /// f64, minus a global shift) to position near-origin shape geometry
    /// in world space without the f32 collapse.
    pub world_origin: [f64; 3],
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
    let mut stats = MeshStats::default();

    let t0 = Instant::now();
    let table = EntityTable::build(buf);
    stats.entity_table_build_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let _ = table.len();

    let t_mesh = Instant::now();
    let mut resolver = PlacementResolver::new(&table);
    let mut shape_cache: HashMap<u64, Vec<(LocalMesh, &'static str)>> = HashMap::new();

    for (step_id, type_name, args) in table.iter() {
        // Skip anything we know isn't a product (rels, primitives, etc.).
        if !is_product_type(type_name) {
            continue;
        }
        stats.products_seen += 1;

        let fields = split_top_level_args(args);
        let guid = string_at(&fields, 0).unwrap_or_default();
        // IfcProduct: arg[5] = ObjectPlacement, arg[6] = Representation
        let placement_id = match fields.get(5).copied().map(parse_field) {
            Some(Field::Ref(id)) => Some(id),
            _ => None,
        };
        let repr_id_opt = match fields.get(6).copied().map(parse_field) {
            Some(Field::Ref(id)) => Some(id),
            _ => None,
        };

        // World placement is computable independent of geometry — we
        // need it hoisted so geometryless emits still carry the
        // authoring tool's `placement_origin` (where the element "is"
        // even when it has no body).
        // Resolve the chain in f64 (precise origin at georeferenced
        // magnitudes), then downcast to f32 for the existing local-frame
        // bake math (rotation is f32-safe; only the translation needed
        // f64, and that's preserved separately in `world_origin`).
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
        let entity_name = crate::indexer::type_name_uppercase_with_proper_case(type_name);

        let repr_id = match repr_id_opt {
            Some(id) => id,
            None => {
                emit_geometryless(
                    sink,
                    &mut stats,
                    "no_representation",
                    guid,
                    entity_name,
                    step_id,
                    placement_origin,
                    world,
                    world_origin,
                );
                continue;
            }
        };

        // Find a body/facetation Items list.
        let items = body_items(&table, repr_id);
        if items.is_empty() {
            emit_geometryless(
                sink,
                &mut stats,
                "no_body_items",
                guid,
                entity_name,
                step_id,
                placement_origin,
                world,
                world_origin,
            );
            continue;
        }
        let mut combined_v: Vec<f32> = Vec::new();
        let mut combined_i: Vec<u32> = Vec::new();
        let mut segments: Vec<MeshSegment> = Vec::new();
        let mut parts: Vec<InstancePart> = Vec::new();

        for item_id in items {
            let fragments = mesh_item(&table, item_id, &mut shape_cache);
            for frag in fragments {
                match frag {
                    MeshFragment::Mesh { mesh: local, source, role, rep_step_id, instance_transform } => {
                        let seg_index_start = combined_i.len() as u32;
                        let base = (combined_v.len() / 3) as u32;
                        // `effective = world * instance_transform` (the
                        // IfcMappedItem composition; identity for direct
                        // geometry).
                        let effective = world * instance_transform;
                        match frame {
                            BakeFrame::World => {
                                // Full world coordinates: `effective * v`.
                                for chunk in local.vertices.chunks_exact(3) {
                                    let p = Vec3::new(chunk[0], chunk[1], chunk[2]);
                                    let w = effective * Vec4::new(p.x, p.y, p.z, 1.0);
                                    combined_v.push(w.x);
                                    combined_v.push(w.y);
                                    combined_v.push(w.z);
                                }
                            }
                            BakeFrame::Local => {
                                // Shape only: apply the linear part
                                // (rotation/scale) via transform_vector3 —
                                // which NEVER adds the world translation,
                                // so a small profile far from origin keeps
                                // full f32 precision instead of collapsing.
                                // Preserve each fragment's small offset
                                // relative to the product origin so
                                // multi-operand products (boolean walls)
                                // keep their internal layout for a correct
                                // union; that offset is a difference of two
                                // large values (≈ tens of mm position error
                                // at extreme coords) but never collapses the
                                // shape.
                                let po = Vec3::new(
                                    placement_origin[0],
                                    placement_origin[1],
                                    placement_origin[2],
                                );
                                let frag_off =
                                    effective.transform_point3(Vec3::ZERO) - po;
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
                            // Compound tag preserves BOTH the structural
                            // role (if any) and the leaf representation
                            // type, so a polygonal halfspace used as a
                            // boolean cut reads as
                            // "boolean_second_operand|halfspace_bounded".
                            let tag = match role {
                                Some(r) => format!("{}|{}", r, source),
                                None => source.to_string(),
                            };
                            segments.push(MeshSegment {
                                index_start: seg_index_start,
                                index_count: seg_index_count,
                                source: tag.clone(),
                            });
                            parts.push(InstancePart {
                                rep_step_id,
                                instance_transform: instance_transform.to_cols_array(),
                                local_vertices: local.vertices,
                                local_indices: local.indices,
                                index_start: seg_index_start,
                                index_count: seg_index_count,
                                source: tag,
                            });
                        }
                    }
                    MeshFragment::Unhandled { ifc_type } => {
                        // Explicit "we saw this representation but
                        // don't tessellate it yet" — the whole point
                        // of the reveal-all stance.
                        *stats
                            .by_source
                            .entry(format!("unhandled:{}", ifc_type))
                            .or_insert(0) += 1;
                    }
                }
            }
        }

        if combined_i.is_empty() {
            // Already credited to unhandled:IFCXXX above; record the
            // outer-level miss too so the consumer can correlate.
            emit_geometryless(
                sink,
                &mut stats,
                "item_unhandled",
                guid,
                entity_name,
                step_id,
                placement_origin,
                world,
                world_origin,
            );
            continue;
        }

        // `placement_origin` was computed above; reused here so substrate
        // consumers and the drift analyser see the same value regardless
        // of which code path emitted the product. Compare against the
        // mesh centroid downstream to detect placement-vs-geometry drift.

        // Dominant source = leaf tag of the first segment. Compound
        // tags ("boolean_first_operand|extrusion") collapse to their
        // leaf (`"extrusion"`) for the legacy `.source` field — full
        // detail still available via `.segments`. Keeps back-compat for
        // consumers (stats.rs, gltf.rs) that read `.source` directly.
        let source_tag: &'static str = segments
            .first()
            .and_then(|s| {
                let leaf = s.source.rsplit('|').next().unwrap_or(s.source.as_str());
                MeshFragment::source_tags().iter().find(|t| **t == leaf).copied()
            })
            .unwrap_or("composite");

        stats.products_meshed += 1;
        stats.triangles += combined_i.len() / 3;
        for seg in &segments {
            *stats.by_source.entry(seg.source.clone()).or_insert(0) += 1;
        }
        sink.on_product(ProductMesh {
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
        });
    }

    stats.elapsed_ms = t_mesh.elapsed().as_secs_f64() * 1000.0;
    stats
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
    shape_cache: &mut HashMap<u64, Vec<(LocalMesh, &'static str)>>,
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
            // Recurse into operands. We must avoid borrow-checker pain
            // when passing &mut shape_cache through a callback closure;
            // boolean::boolean_result takes a function pointer to
            // `mesh_item` so the recursion happens on this exact frame.
            boolean::boolean_result(table, item_id, shape_cache, &mesh_item)
        } else if type_name.eq_ignore_ascii_case(b"IFCCSGSOLID") {
            boolean::csg_solid(table, item_id, shape_cache, &mesh_item)
        } else if type_name.eq_ignore_ascii_case(b"IFCPOLYGONALBOUNDEDHALFSPACE") {
            single(boolean::polygonal_bounded_halfspace(table, item_id), "halfspace_bounded")
        } else if type_name.eq_ignore_ascii_case(b"IFCHALFSPACESOLID") {
            single(boolean::halfspace_solid(table, item_id), "halfspace_plane")
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

