//! Minimal glTF 2.0 binary (.glb) writer.
//!
//! Layout per the spec (https://registry.khronos.org/glTF/specs/2.0/glTF-2.0.html):
//!
//! ```text
//! glb file
//! ├── header (12 bytes)        magic 'glTF' (0x46546C67) + version 2 + total length
//! ├── chunk 0 (JSON)           length + 'JSON' type tag + JSON content (4-byte padded)
//! └── chunk 1 (BIN)            length + 'BIN ' type tag + binary buffer (4-byte padded)
//! ```
//!
//! Output strategy for IFC product meshes:
//!
//!   * **Baked path**: products with multiple representation fragments
//!     (e.g. `IfcBooleanResult`, `IfcCsgSolid`) or unique single-fragment
//!     reps emit one node + mesh per product, baked in world coords.
//!     Identical to pre-v0.4.23 behaviour.
//!
//!   * **Instanced path** (v0.4.23+): products whose single-fragment
//!     representation is shared across ≥2 products are grouped by
//!     `rep_step_id`. Each group emits ONE shared mesh (in LOCAL coords)
//!     and ONE node with `EXT_mesh_gpu_instancing`, carrying per-instance
//!     translation / rotation / scale derived from each product's
//!     `world_transform * instance_transform`. Per-instance identity
//!     (guid + entity + segments) is preserved in the node's `extras`
//!     as a parallel array so the viewer can map a picked instance index
//!     back to the BIM model.
//!
//! Per-node `extras` carries `guid` + `entity` (baked path) or per-instance
//! `instances: [{guid, entity, segments}, ...]` (instanced path) so the
//! viewer can pick by GUID either way.
//!
//! No materials beyond the per-entity-type palette (richer
//! `IfcSurfaceStyle` colour extraction tracked separately in ifcfast#3),
//! no textures, no animations. The viewer computes face normals on load
//! (Three.js / Babylon / xeokit all do this for normal-less meshes).

use std::collections::HashMap;
use std::io::{self, BufWriter, Write};

use glam::Mat4;

use crate::mesh::ProductMesh;

// glTF component types
const F32: u32 = 5126;
const U32_T: u32 = 5125;
const U16_T: u32 = 5123;

// glTF accessor types (string in JSON)
const VEC3: &str = "VEC3";
const VEC4: &str = "VEC4";
const SCALAR: &str = "SCALAR";

// glTF buffer view targets
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;

/// Minimum group size that triggers EXT_mesh_gpu_instancing. Below this
/// the rep is emitted as baked individual meshes (no extension
/// overhead) — at 1 instance there are zero bytes saved and the
/// extension's JSON overhead is pure loss. At 2 the break-even is
/// near-immediate (one duplicated mesh saved vs ~3 short TRS arrays).
const INSTANCE_THRESHOLD: usize = 2;

/// Knobs for [`write_with_options`]. Defaulted to viewer-optimal:
/// instancing on. The single one-arg `write()` continues to use these
/// defaults so existing call sites (the `ifcfast-mesh` binary, tests)
/// keep their behaviour.
#[derive(Debug, Clone, Copy)]
pub struct WriteOptions {
    /// When true, products sharing a single-fragment `rep_step_id`
    /// are emitted with `EXT_mesh_gpu_instancing` (one shared mesh,
    /// per-instance TRS). When false, every product gets a baked
    /// node — required when `cut_openings` has been applied because
    /// the cut changes per-product geometry away from the shared
    /// local mesh, breaking the "all members share `parts[0]`'s
    /// local geometry" assumption the instancer relies on.
    pub instancing: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        Self { instancing: true }
    }
}

/// Write `meshes` as a glTF 2.0 binary file with default options
/// (instancing enabled). See [`write_with_options`] for the knob-
/// configurable form.
pub fn write<W: Write>(meshes: &[ProductMesh], out: &mut W) -> io::Result<()> {
    write_with_options(meshes, &WriteOptions::default(), out)
}

/// Like [`write`] but with caller-supplied options. `m.to_gltf` calls
/// this directly with `instancing = !cut_openings` so cut-applied
/// glTFs render correctly (every wall keeps its own cut geometry
/// rather than being snapped back to the shared pre-cut rep).
pub fn write_with_options<W: Write>(
    meshes: &[ProductMesh],
    options: &WriteOptions,
    out: &mut W,
) -> io::Result<()> {
    let plan = Plan::classify(meshes, options.instancing);

    // 1. Build the binary buffer.
    let (binary, layout) = pack_binary(meshes, &plan);

    // 2. Build the JSON.
    let json = build_json(meshes, &plan, &layout, binary.len() as u32);

    // 3. Pad both chunks to 4-byte multiples (spec requires).
    let json_bytes = pad_json(json.into_bytes());
    let bin_bytes = pad_bin(binary);

    let json_chunk_len = json_bytes.len() as u32;
    let bin_chunk_len = bin_bytes.len() as u32;
    let total_len = 12 + (8 + json_chunk_len) + (8 + bin_chunk_len);

    let mut w = BufWriter::with_capacity(1 << 20, out);

    // Header
    w.write_all(&0x46546C67u32.to_le_bytes())?; // 'glTF'
    w.write_all(&2u32.to_le_bytes())?; // version
    w.write_all(&total_len.to_le_bytes())?;

    // JSON chunk
    w.write_all(&json_chunk_len.to_le_bytes())?;
    w.write_all(b"JSON")?;
    w.write_all(&json_bytes)?;

    // BIN chunk
    w.write_all(&bin_chunk_len.to_le_bytes())?;
    w.write_all(b"BIN\0")?;
    w.write_all(&bin_bytes)?;

    w.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Plan: classification of products into baked + instanced groups.
// ---------------------------------------------------------------------------

/// Which output mode each product gets and what shared shapes look like.
/// Built once up-front so the binary packer and JSON builder agree on
/// indices.
struct Plan {
    /// Indices into the input `meshes` slice that get the baked-mesh
    /// treatment: one mesh + one node per product, vertices in world
    /// coords. Multi-fragment products + rep-unique singletons.
    baked: Vec<usize>,
    /// Groups of products that share a single-fragment representation.
    /// Each entry is one shared mesh + one node with
    /// `EXT_mesh_gpu_instancing`.
    instanced: Vec<InstanceGroup>,
}

/// One instance group. `rep_step_id` is the shared representation;
/// `member_indices` lists every product (by input slice index) that
/// uses it. Members are stored in input order so per-instance metadata
/// arrays stay aligned with how the viewer's pick-by-instance-index
/// reads the TRS attributes.
struct InstanceGroup {
    rep_step_id: u64,
    /// Members in input order; the first member's `parts[0]` supplies
    /// the shared local geometry.
    member_indices: Vec<usize>,
}

impl Plan {
    /// `instancing_enabled = false` forces every mesh down the baked
    /// path — used by `m.to_gltf(cut_openings=True)` because the cut
    /// modifies per-product geometry and breaks the "all members
    /// share `parts[0]`'s local mesh" assumption.
    fn classify(meshes: &[ProductMesh], instancing_enabled: bool) -> Self {
        if !instancing_enabled {
            let baked: Vec<usize> = (0..meshes.len())
                .filter(|&i| !meshes[i].vertices.is_empty() && !meshes[i].indices.is_empty())
                .collect();
            return Plan {
                baked,
                instanced: Vec::new(),
            };
        }

        // Bucket single-fragment products by rep_step_id; multi-fragment
        // products and geometryless products go straight to baked.
        let mut by_rep: HashMap<u64, Vec<usize>> = HashMap::new();
        let mut forced_baked: Vec<usize> = Vec::new();
        for (i, mesh) in meshes.iter().enumerate() {
            if mesh.parts.len() == 1 && !mesh.vertices.is_empty() && !mesh.indices.is_empty() {
                by_rep
                    .entry(mesh.parts[0].rep_step_id)
                    .or_default()
                    .push(i);
            } else {
                forced_baked.push(i);
            }
        }

        let mut baked: Vec<usize> = forced_baked;
        let mut instanced: Vec<InstanceGroup> = Vec::new();
        for (rep_step_id, mut member_indices) in by_rep {
            if member_indices.len() >= INSTANCE_THRESHOLD {
                member_indices.sort_unstable();
                instanced.push(InstanceGroup {
                    rep_step_id,
                    member_indices,
                });
            } else {
                baked.extend(member_indices);
            }
        }
        baked.sort_unstable();
        // Order instance groups by their first member's input index so
        // the resulting glTF nodes have a stable, replay-friendly order.
        instanced.sort_by_key(|g| g.member_indices.first().copied().unwrap_or(u64::MAX as usize));
        Plan { baked, instanced }
    }
}

// ---------------------------------------------------------------------------
// Binary layout.
// ---------------------------------------------------------------------------

/// Where every emitted accessor's bytes live in the binary buffer +
/// the AABB / count metadata the JSON builder needs to write the
/// accessor headers without re-walking the meshes.
struct BinaryLayout {
    /// Per baked product (parallel to `plan.baked`): one positions
    /// view + one indices view.
    baked: Vec<BakedViews>,
    /// Per instance group (parallel to `plan.instanced`): one shared
    /// positions view, one shared indices view, plus the three TRS
    /// attribute views for the EXT_mesh_gpu_instancing extension.
    instanced: Vec<InstancedViews>,
}

#[derive(Clone, Copy)]
struct View {
    /// Byte offset into the single concatenated binary buffer.
    byte_offset: u32,
    /// Length in bytes (unpadded).
    byte_length: u32,
    /// glTF bufferView.target — `ARRAY_BUFFER` for positions/TRS,
    /// `ELEMENT_ARRAY_BUFFER` for indices, or 0 (omitted) for
    /// instance attribute buffers (which the spec says SHOULD NOT
    /// carry a target).
    target: u32,
}

struct BakedViews {
    positions: View,
    indices: View,
    n_verts: u32,
    n_indices: u32,
    /// `U16_T` (when verts < 65536) or `U32_T`.
    idx_component: u32,
    /// Quantization metadata for KHR_mesh_quantization. The position
    /// bufferView holds U16 triples in `[0, 65535]`; the runtime
    /// reconstructs world coords as `translation + scale * u16` via
    /// the node TRS. Per-axis scale is 0 when the AABB collapses
    /// along that axis (a planar mesh / single vertex); the u16
    /// value is always 0 in that case, so 0 * 0 + translation gives
    /// the right answer.
    quant_translation: [f32; 3],
    quant_scale: [f32; 3],
    /// Per-axis quantized max — `0` for collapsed axes, `65535`
    /// otherwise. Drives the accessor's `min`/`max` in the spec
    /// (which must be in encoded units when quantization is in use).
    quant_u16_max: [u32; 3],
}

struct InstancedViews {
    positions: View,
    indices: View,
    translation: View,
    rotation: View,
    scale: View,
    n_verts: u32,
    n_indices: u32,
    n_instances: u32,
    min: [f32; 3],
    max: [f32; 3],
    idx_component: u32,
}

/// Concatenate every product's positions + indices (and per-group TRS
/// arrays) into one binary blob, aligned to 4 bytes between regions.
fn pack_binary(meshes: &[ProductMesh], plan: &Plan) -> (Vec<u8>, BinaryLayout) {
    // Conservatively reserve. Over-estimates are cheap; re-allocs are not.
    let baked_verts: usize = plan
        .baked
        .iter()
        .map(|&i| meshes[i].vertices.len())
        .sum();
    let baked_idx: usize = plan.baked.iter().map(|&i| meshes[i].indices.len()).sum();
    let inst_verts: usize = plan
        .instanced
        .iter()
        .map(|g| meshes[g.member_indices[0]].parts[0].local_vertices.len())
        .sum();
    let inst_idx: usize = plan
        .instanced
        .iter()
        .map(|g| meshes[g.member_indices[0]].parts[0].local_indices.len())
        .sum();
    let inst_trs: usize = plan
        .instanced
        .iter()
        .map(|g| g.member_indices.len() * (3 + 4 + 3) * 4)
        .sum();
    // baked positions are quantized u16 (2 bytes per coord, 3 per vert);
    // instanced shared positions still f32; indices are f32 capacity
    // upper-bound regardless of u16 vs u32 narrowing.
    let mut bin = Vec::with_capacity(
        baked_verts * 2 + baked_idx * 4 + inst_verts * 4 + inst_idx * 4 + inst_trs,
    );

    let mut layout = BinaryLayout {
        baked: Vec::with_capacity(plan.baked.len()),
        instanced: Vec::with_capacity(plan.instanced.len()),
    };

    // Baked products first — quantized positions + indices.
    for &i in &plan.baked {
        layout.baked.push(pack_one_baked(&meshes[i], &mut bin));
    }

    // Instanced groups next. The shared mesh comes from the first
    // member's `parts[0]` (local geometry — all members share the same
    // local mesh by definition of being in this group).
    for group in &plan.instanced {
        let first = &meshes[group.member_indices[0]];
        let part = &first.parts[0];
        let (positions, indices, n_verts, n_indices, min, max, idx_component) =
            pack_one_local(&part.local_vertices, &part.local_indices, &mut bin);

        // Per-instance TRS arrays. Each instance's transform is
        // `world_transform * instance_transform`; decompose into a
        // (translation, rotation, scale) tuple via glam — the standard
        // glTF instance attribute shape per the extension spec.
        let n_instances = group.member_indices.len() as u32;
        let (translation, rotation, scale) =
            pack_instance_trs(meshes, group, &mut bin);

        layout.instanced.push(InstancedViews {
            positions,
            indices,
            translation,
            rotation,
            scale,
            n_verts,
            n_indices,
            n_instances,
            min,
            max,
            idx_component,
        });
    }

    (bin, layout)
}

/// Pack a single baked product's positions (KHR_mesh_quantization
/// U16-quantized) + indices into `bin`. Halves the position-buffer
/// footprint vs the pre-v0.4.24 f32 layout. The denorm transform
/// (per-axis translation + scale) is recorded on the returned
/// `BakedViews` so the JSON builder can attach it as `node.translation`
/// / `node.scale` per the extension contract.
fn pack_one_baked(mesh: &ProductMesh, bin: &mut Vec<u8>) -> BakedViews {
    let (min, max) = bbox(&mesh.vertices);
    // Per-axis range. Floor at f32::MIN_POSITIVE-equivalent (any
    // positive value) so the divisor isn't 0 — when range is 0 the
    // quantized value is forced to 0 anyway, so the scale never
    // actually multiplies a non-zero u16.
    let range = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
    // Quantized max per axis: 65535 normally, 0 when the AABB is
    // collapsed along this axis (every vert quantizes to 0).
    let q_max: [u32; 3] = [
        if range[0] > 0.0 { 65535 } else { 0 },
        if range[1] > 0.0 { 65535 } else { 0 },
        if range[2] > 0.0 { 65535 } else { 0 },
    ];
    // Denorm scale per axis. Zero range → zero scale (the u16 is
    // always 0, so 0*0 + translation = translation = the only
    // world value along that axis).
    let q_scale: [f32; 3] = [
        if range[0] > 0.0 { range[0] / 65535.0 } else { 0.0 },
        if range[1] > 0.0 { range[1] / 65535.0 } else { 0.0 },
        if range[2] > 0.0 { range[2] / 65535.0 } else { 0.0 },
    ];

    let pos_offset = bin.len() as u32;
    for chunk in mesh.vertices.chunks_exact(3) {
        for k in 0..3 {
            let q: u16 = if range[k] > 0.0 {
                // (v - min) / range * 65535, clamped to u16.
                let f = ((chunk[k] - min[k]) / range[k] * 65535.0).round();
                f.clamp(0.0, 65535.0) as u16
            } else {
                0
            };
            bin.extend_from_slice(&q.to_le_bytes());
        }
    }
    let pos_len = bin.len() as u32 - pos_offset;
    pad4(bin);

    let n_verts = (mesh.vertices.len() / 3) as u32;
    let (idx_offset, idx_component) = pack_indices(&mesh.indices, n_verts, bin);
    let idx_len = bin.len() as u32 - idx_offset;
    pad4(bin);

    BakedViews {
        positions: View {
            byte_offset: pos_offset,
            byte_length: pos_len,
            target: ARRAY_BUFFER,
        },
        indices: View {
            byte_offset: idx_offset,
            byte_length: idx_len,
            target: ELEMENT_ARRAY_BUFFER,
        },
        n_verts,
        n_indices: mesh.indices.len() as u32,
        idx_component,
        quant_translation: min,
        quant_scale: q_scale,
        quant_u16_max: q_max,
    }
}

/// Pack a local (untransformed) mesh into `bin`. Same body as the
/// baked packer; split so the per-instance writer doesn't depend on
/// the `ProductMesh` shape.
fn pack_one_local(
    vertices: &[f32],
    indices: &[u32],
    bin: &mut Vec<u8>,
) -> (View, View, u32, u32, [f32; 3], [f32; 3], u32) {
    let pos_offset = bin.len() as u32;
    let (min, max) = bbox(vertices);
    for v in vertices {
        bin.extend_from_slice(&v.to_le_bytes());
    }
    let pos_len = bin.len() as u32 - pos_offset;
    pad4(bin);

    let n_verts = (vertices.len() / 3) as u32;
    let (idx_offset, idx_component) = pack_indices(indices, n_verts, bin);
    let idx_len = bin.len() as u32 - idx_offset;
    pad4(bin);

    (
        View {
            byte_offset: pos_offset,
            byte_length: pos_len,
            target: ARRAY_BUFFER,
        },
        View {
            byte_offset: idx_offset,
            byte_length: idx_len,
            target: ELEMENT_ARRAY_BUFFER,
        },
        n_verts,
        indices.len() as u32,
        min,
        max,
        idx_component,
    )
}

fn pack_indices(indices: &[u32], n_verts: u32, bin: &mut Vec<u8>) -> (u32, u32) {
    let idx_offset = bin.len() as u32;
    let component = if n_verts <= u16::MAX as u32 + 1 {
        for &i in indices {
            bin.extend_from_slice(&(i as u16).to_le_bytes());
        }
        U16_T
    } else {
        for &i in indices {
            bin.extend_from_slice(&i.to_le_bytes());
        }
        U32_T
    };
    (idx_offset, component)
}

/// Compute the per-coord AABB of a flat `[x, y, z, x, y, z, ...]` buffer.
fn bbox(vertices: &[f32]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for chunk in vertices.chunks_exact(3) {
        for k in 0..3 {
            let v = chunk[k];
            if v < min[k] {
                min[k] = v;
            }
            if v > max[k] {
                max[k] = v;
            }
        }
    }
    // Empty mesh sanity — caller should have filtered upstream but be
    // defensive: collapse to origin so the JSON doesn't carry ±inf.
    if !min[0].is_finite() {
        min = [0.0; 3];
        max = [0.0; 3];
    }
    (min, max)
}

/// Pack three per-instance attribute arrays — TRANSLATION (VEC3),
/// ROTATION (VEC4 quaternion xyzw), SCALE (VEC3) — into `bin`. One
/// triple per instance, in `member_indices` order. The per-instance
/// 4×4 is `world_transform * instance_transform`; glam's
/// `to_scale_rotation_translation` decomposes it.
fn pack_instance_trs(
    meshes: &[ProductMesh],
    group: &InstanceGroup,
    bin: &mut Vec<u8>,
) -> (View, View, View) {
    let n = group.member_indices.len();

    // Translation
    let t_offset = bin.len() as u32;
    let mut t_scratch: Vec<(f32, f32, f32)> = Vec::with_capacity(n);
    let mut r_scratch: Vec<(f32, f32, f32, f32)> = Vec::with_capacity(n);
    let mut s_scratch: Vec<(f32, f32, f32)> = Vec::with_capacity(n);
    for &i in &group.member_indices {
        let mesh = &meshes[i];
        let world = Mat4::from_cols_array(&mesh.world_transform);
        let inst = Mat4::from_cols_array(&mesh.parts[0].instance_transform);
        let m = world * inst;
        let (scale, rot, trans) = m.to_scale_rotation_translation();
        t_scratch.push((trans.x, trans.y, trans.z));
        r_scratch.push((rot.x, rot.y, rot.z, rot.w));
        s_scratch.push((scale.x, scale.y, scale.z));
    }
    for &(x, y, z) in &t_scratch {
        bin.extend_from_slice(&x.to_le_bytes());
        bin.extend_from_slice(&y.to_le_bytes());
        bin.extend_from_slice(&z.to_le_bytes());
    }
    let t_len = bin.len() as u32 - t_offset;
    pad4(bin);

    let r_offset = bin.len() as u32;
    for &(x, y, z, w) in &r_scratch {
        bin.extend_from_slice(&x.to_le_bytes());
        bin.extend_from_slice(&y.to_le_bytes());
        bin.extend_from_slice(&z.to_le_bytes());
        bin.extend_from_slice(&w.to_le_bytes());
    }
    let r_len = bin.len() as u32 - r_offset;
    pad4(bin);

    let s_offset = bin.len() as u32;
    for &(x, y, z) in &s_scratch {
        bin.extend_from_slice(&x.to_le_bytes());
        bin.extend_from_slice(&y.to_le_bytes());
        bin.extend_from_slice(&z.to_le_bytes());
    }
    let s_len = bin.len() as u32 - s_offset;
    pad4(bin);

    // Instance attribute buffer views SHOULD NOT have a target per
    // the EXT_mesh_gpu_instancing spec (they're not vertex or index
    // buffers as far as the renderer is concerned).
    let trs_target = 0u32;
    (
        View {
            byte_offset: t_offset,
            byte_length: t_len,
            target: trs_target,
        },
        View {
            byte_offset: r_offset,
            byte_length: r_len,
            target: trs_target,
        },
        View {
            byte_offset: s_offset,
            byte_length: s_len,
            target: trs_target,
        },
    )
}

fn pad4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

fn pad_json(mut json: Vec<u8>) -> Vec<u8> {
    // JSON chunk padded with ASCII spaces per spec.
    while !json.len().is_multiple_of(4) {
        json.push(b' ');
    }
    json
}

fn pad_bin(mut bin: Vec<u8>) -> Vec<u8> {
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    bin
}

// ---------------------------------------------------------------------------
// JSON builder.
// ---------------------------------------------------------------------------

/// Build the entire glTF JSON chunk. Layout, top-down:
///   asset → buffers → bufferViews → accessors → materials → meshes →
///   nodes → scenes.
///
/// Indices are issued in deterministic order so the binary layout (which
/// the bufferViews reference) and the JSON references stay in sync.
fn build_json(
    meshes: &[ProductMesh],
    plan: &Plan,
    layout: &BinaryLayout,
    binary_len: u32,
) -> String {
    // Pre-compute the global index ranges so we can cross-reference
    // accessors / meshes / nodes / materials without re-walking the
    // plan multiple times.
    let n_baked = plan.baked.len();
    let n_groups = plan.instanced.len();

    // bufferViews + accessors layout:
    //   - 2 per baked product (positions, indices) → [0, 2*n_baked)
    //   - 5 per instance group (positions, indices, T, R, S) → next.
    // meshes layout:
    //   - 1 per baked product → [0, n_baked)
    //   - 1 per instance group → [n_baked, n_baked + n_groups)
    let mesh_baked_base = 0;
    let mesh_inst_base = n_baked;

    // materials: per-entity-type palette, reused across instances of
    // the same entity to avoid an N-entry material array per glb.
    // Baked products carry a per-product GUID material (kept for
    // pick-to-BIM-by-material backwards-compat); instanced groups
    // share one material keyed by the first member's entity.
    let mut entity_palette: Vec<(String, [f32; 4])> = Vec::new();
    let mut entity_index: HashMap<String, usize> = HashMap::new();
    let mut baked_material_idx: Vec<usize> = Vec::with_capacity(n_baked);
    for &i in &plan.baked {
        let mesh = &meshes[i];
        // Per-product material name = GUID (legacy pick-to-BIM hook),
        // colour = entity palette lookup.
        let color = default_color_for_entity(&mesh.entity);
        let idx = baked_material_idx.len();
        baked_material_idx.push(idx);
        entity_palette.push((mesh.guid.clone(), color));
    }
    // Groups: one material per group, named by the rep_step_id-derived
    // tag `instanced:<entity>` so a viewer sees these aren't
    // individually addressable by GUID (the per-instance GUIDs live
    // in node.extras).
    let mut group_material_idx: Vec<usize> = Vec::with_capacity(n_groups);
    for group in &plan.instanced {
        let first = &meshes[group.member_indices[0]];
        let key = first.entity.clone();
        let idx = if let Some(&existing) = entity_index.get(&key) {
            existing
        } else {
            let color = default_color_for_entity(&first.entity);
            let new_idx = entity_palette.len();
            entity_palette.push((format!("instanced:{}", first.entity), color));
            entity_index.insert(key, new_idx);
            new_idx
        };
        group_material_idx.push(idx);
    }

    // ----- begin JSON -----
    let mut s = String::with_capacity(meshes.len() * 400 + 8192);
    s.push_str(r#"{"asset":{"version":"2.0","generator":"ifcfast-mesh"},"#);

    // Extensions used / required.
    //   - EXT_mesh_gpu_instancing: declared whenever ≥1 instance group
    //     exists. Required because without it the viewer would
    //     silently render each shared mesh at its local origin.
    //   - KHR_mesh_quantization: declared whenever ≥1 baked mesh
    //     exists (always, on real files). Required because the
    //     position accessors are u16 — a viewer that doesn't know
    //     to apply node.translation + node.scale would render
    //     geometry in [0, 65535]³, several km from origin.
    let want_quant = n_baked > 0;
    let want_instancing = n_groups > 0;
    if want_quant || want_instancing {
        s.push_str(r#""extensionsUsed":["#);
        let mut first_ext = true;
        if want_quant {
            s.push_str(r#""KHR_mesh_quantization""#);
            first_ext = false;
        }
        if want_instancing {
            if !first_ext {
                s.push(',');
            }
            s.push_str(r#""EXT_mesh_gpu_instancing""#);
        }
        s.push_str("],");
        s.push_str(r#""extensionsRequired":["#);
        let mut first_ext = true;
        if want_quant {
            s.push_str(r#""KHR_mesh_quantization""#);
            first_ext = false;
        }
        if want_instancing {
            if !first_ext {
                s.push(',');
            }
            s.push_str(r#""EXT_mesh_gpu_instancing""#);
        }
        s.push_str("],");
    }

    // 1. Single buffer
    s.push_str(r#""buffers":[{"byteLength":"#);
    s.push_str(&binary_len.to_string());
    s.push_str("}],");

    // 2. bufferViews
    s.push_str(r#""bufferViews":["#);
    let mut first = true;
    for v in &layout.baked {
        push_view(&mut s, &mut first, &v.positions);
        push_view(&mut s, &mut first, &v.indices);
    }
    for v in &layout.instanced {
        push_view(&mut s, &mut first, &v.positions);
        push_view(&mut s, &mut first, &v.indices);
        push_view(&mut s, &mut first, &v.translation);
        push_view(&mut s, &mut first, &v.rotation);
        push_view(&mut s, &mut first, &v.scale);
    }
    s.push_str("],");

    // 3. Accessors. bufferView indices are issued in the same order
    //    the bufferViews block uses (baked × 2 then instanced × 5).
    s.push_str(r#""accessors":["#);
    first = true;
    let mut bv_idx: u32 = 0;
    for v in &layout.baked {
        // Quantized positions: UNSIGNED_SHORT, accessor min/max in
        // *encoded* (u16) units per the KHR_mesh_quantization spec.
        push_accessor_positions_quantized(&mut s, &mut first, bv_idx, v.n_verts, v.quant_u16_max);
        bv_idx += 1;
        push_accessor_indices(&mut s, &mut first, bv_idx, v.n_indices, v.idx_component);
        bv_idx += 1;
    }
    for v in &layout.instanced {
        // Instanced shared mesh stays f32 — the per-instance TRS
        // composes with the node's TRS, so naive quantization would
        // multiply the per-instance transform by the (pre-decode)
        // raw u16 vertex. Future: bake the quant denorm into the
        // shared mesh + adjust instance TRS, or use a different
        // primitive layout. For now: float positions, full precision.
        push_accessor_positions(&mut s, &mut first, bv_idx, v.n_verts, v.min, v.max);
        bv_idx += 1;
        push_accessor_indices(&mut s, &mut first, bv_idx, v.n_indices, v.idx_component);
        bv_idx += 1;
        // TRANSLATION: VEC3 floats, one per instance.
        push_accessor_vec(&mut s, &mut first, bv_idx, v.n_instances, VEC3);
        bv_idx += 1;
        // ROTATION: VEC4 floats (quaternion xyzw), one per instance.
        push_accessor_vec(&mut s, &mut first, bv_idx, v.n_instances, VEC4);
        bv_idx += 1;
        // SCALE: VEC3 floats, one per instance.
        push_accessor_vec(&mut s, &mut first, bv_idx, v.n_instances, VEC3);
        bv_idx += 1;
    }
    s.push_str("],");

    // 4. Materials — palette deduped by entity (instanced) plus per-
    //    product (baked).
    s.push_str(r#""materials":["#);
    first = true;
    for (name, color) in &entity_palette {
        if !first {
            s.push(',');
        }
        first = false;
        let (r, g, b, a) = (color[0], color[1], color[2], color[3]);
        let translucent = a < 1.0;
        s.push_str(r#"{"name":""#);
        push_json_string(&mut s, name);
        s.push_str(r#"","pbrMetallicRoughness":{"baseColorFactor":["#);
        s.push_str(&format_f32(r));
        s.push(',');
        s.push_str(&format_f32(g));
        s.push(',');
        s.push_str(&format_f32(b));
        s.push(',');
        s.push_str(&format_f32(a));
        s.push_str(r#"],"metallicFactor":0,"roughnessFactor":0.85}"#);
        if translucent {
            s.push_str(r#","alphaMode":"BLEND""#);
        }
        s.push_str(r#","doubleSided":true}"#);
    }
    s.push_str("],");

    // 5. Meshes — one per baked product, one per instance group.
    s.push_str(r#""meshes":["#);
    first = true;
    let mut acc_base: u32 = 0;
    for (i, _) in plan.baked.iter().enumerate() {
        if !first {
            s.push(',');
        }
        first = false;
        let pos_acc = acc_base;
        let idx_acc = acc_base + 1;
        s.push_str(r#"{"primitives":[{"attributes":{"POSITION":"#);
        s.push_str(&pos_acc.to_string());
        s.push_str(r#"},"indices":"#);
        s.push_str(&idx_acc.to_string());
        s.push_str(r#","material":"#);
        s.push_str(&baked_material_idx[i].to_string());
        s.push_str(r#","mode":4}]}"#);
        acc_base += 2;
    }
    for (g, _group) in plan.instanced.iter().enumerate() {
        if !first {
            s.push(',');
        }
        first = false;
        let pos_acc = acc_base;
        let idx_acc = acc_base + 1;
        s.push_str(r#"{"primitives":[{"attributes":{"POSITION":"#);
        s.push_str(&pos_acc.to_string());
        s.push_str(r#"},"indices":"#);
        s.push_str(&idx_acc.to_string());
        s.push_str(r#","material":"#);
        s.push_str(&group_material_idx[g].to_string());
        s.push_str(r#","mode":4}]}"#);
        acc_base += 5; // positions + indices + T + R + S
    }
    s.push_str("],");

    // 6. Nodes — baked products + instance group nodes + a root
    //    rotator that maps the Z-up BIM frame into glTF's Y-up frame.
    s.push_str(r#""nodes":["#);
    first = true;
    // 6a. Baked nodes — one per product, mesh idx = baked position.
    //     Each node carries `translation` + `scale` to dequantize its
    //     u16 vertex buffer per KHR_mesh_quantization. The root
    //     rotator is applied AFTER the per-node TRS in the standard
    //     glTF chain (`world = root.matrix * node.matrix * vertex`),
    //     so the Z-up→Y-up rotation lands correctly on the
    //     dequantized world coords.
    for (bi, &orig_i) in plan.baked.iter().enumerate() {
        if !first {
            s.push(',');
        }
        first = false;
        let mesh = &meshes[orig_i];
        let views = &layout.baked[bi];
        s.push_str(r#"{"mesh":"#);
        s.push_str(&(mesh_baked_base + bi).to_string());
        s.push_str(r#","translation":["#);
        s.push_str(&format_f32(views.quant_translation[0]));
        s.push(',');
        s.push_str(&format_f32(views.quant_translation[1]));
        s.push(',');
        s.push_str(&format_f32(views.quant_translation[2]));
        s.push_str(r#"],"scale":["#);
        s.push_str(&format_f32(views.quant_scale[0]));
        s.push(',');
        s.push_str(&format_f32(views.quant_scale[1]));
        s.push(',');
        s.push_str(&format_f32(views.quant_scale[2]));
        s.push_str(r#"],"name":""#);
        push_json_string(&mut s, &mesh.guid);
        s.push_str(r#"","extras":{"guid":""#);
        push_json_string(&mut s, &mesh.guid);
        s.push_str(r#"","entity":""#);
        push_json_string(&mut s, &mesh.entity);
        s.push_str(r#"","source":""#);
        push_json_string(&mut s, mesh.source);
        s.push_str(r#"","segments":["#);
        for (si, seg) in mesh.segments.iter().enumerate() {
            if si > 0 {
                s.push(',');
            }
            s.push_str(r#"{"start":"#);
            s.push_str(&seg.index_start.to_string());
            s.push_str(r#","count":"#);
            s.push_str(&seg.index_count.to_string());
            s.push_str(r#","source":""#);
            push_json_string(&mut s, &seg.source);
            s.push_str(r#""}"#);
        }
        s.push_str(r#"]}}"#);
    }
    // 6b. Instance group nodes — one per group, with EXT_mesh_gpu_instancing
    //     attributes pointing at the T/R/S accessors. Per-instance
    //     identity (guid + entity + segments) goes into extras.instances
    //     as a parallel array indexed by instance order.
    let mut acc_inst_walker: u32 = (2 * n_baked) as u32; // accessor index of the first instance group's positions
    for (gi, group) in plan.instanced.iter().enumerate() {
        if !first {
            s.push(',');
        }
        first = false;
        // Positions+indices acc, then T, R, S acc.
        let t_acc = acc_inst_walker + 2;
        let r_acc = acc_inst_walker + 3;
        let s_acc = acc_inst_walker + 4;
        acc_inst_walker += 5;
        s.push_str(r#"{"mesh":"#);
        s.push_str(&(mesh_inst_base + gi).to_string());
        s.push_str(r#","name":"ifcfast_instances_"#);
        s.push_str(&group.rep_step_id.to_string());
        s.push_str(r#"","extensions":{"EXT_mesh_gpu_instancing":{"attributes":{"TRANSLATION":"#);
        s.push_str(&t_acc.to_string());
        s.push_str(r#","ROTATION":"#);
        s.push_str(&r_acc.to_string());
        s.push_str(r#","SCALE":"#);
        s.push_str(&s_acc.to_string());
        s.push_str(r#"}}},"extras":{"rep_step_id":"#);
        s.push_str(&group.rep_step_id.to_string());
        s.push_str(r#","instances":["#);
        // Per-instance metadata, indexed by glTF instance order.
        for (mi, &orig_i) in group.member_indices.iter().enumerate() {
            let mesh = &meshes[orig_i];
            if mi > 0 {
                s.push(',');
            }
            s.push_str(r#"{"guid":""#);
            push_json_string(&mut s, &mesh.guid);
            s.push_str(r#"","entity":""#);
            push_json_string(&mut s, &mesh.entity);
            s.push_str(r#"","source":""#);
            push_json_string(&mut s, mesh.source);
            s.push_str(r#"","segments":["#);
            for (si, seg) in mesh.segments.iter().enumerate() {
                if si > 0 {
                    s.push(',');
                }
                s.push_str(r#"{"start":"#);
                s.push_str(&seg.index_start.to_string());
                s.push_str(r#","count":"#);
                s.push_str(&seg.index_count.to_string());
                s.push_str(r#","source":""#);
                push_json_string(&mut s, &seg.source);
                s.push_str(r#""}"#);
            }
            s.push_str(r#"]}"#);
        }
        s.push_str(r#"]}}"#);
    }
    // 6c. Root rotator — quaternion for −90° about X (Z-up → Y-up).
    let n_content_nodes = n_baked + n_groups;
    if n_content_nodes > 0 {
        s.push(',');
    }
    s.push_str(r#"{"name":"ifcfast_root","rotation":[-0.70710677,0,0,0.70710677],"children":["#);
    first = true;
    for i in 0..n_content_nodes {
        if !first {
            s.push(',');
        }
        first = false;
        s.push_str(&i.to_string());
    }
    s.push_str("]}");
    s.push_str("],");

    // 7. Scene — only the root node; content nodes hang under it.
    let root_idx = n_content_nodes;
    s.push_str(r#""scenes":[{"nodes":["#);
    s.push_str(&root_idx.to_string());
    s.push_str(r#"]}],"scene":0}"#);

    s
}

fn push_view(s: &mut String, first: &mut bool, v: &View) {
    if !*first {
        s.push(',');
    }
    *first = false;
    s.push_str(r#"{"buffer":0,"byteOffset":"#);
    s.push_str(&v.byte_offset.to_string());
    s.push_str(r#","byteLength":"#);
    s.push_str(&v.byte_length.to_string());
    if v.target != 0 {
        s.push_str(r#","target":"#);
        s.push_str(&v.target.to_string());
    }
    s.push('}');
}

/// Emit a position accessor for a `KHR_mesh_quantization` u16-quantized
/// vertex buffer. `min` / `max` are encoded units per the spec
/// (`[0, 65535]` for live axes, `0` for collapsed axes).
fn push_accessor_positions_quantized(
    s: &mut String,
    first: &mut bool,
    bv_idx: u32,
    n_verts: u32,
    q_max: [u32; 3],
) {
    if !*first {
        s.push(',');
    }
    *first = false;
    s.push_str(r#"{"bufferView":"#);
    s.push_str(&bv_idx.to_string());
    s.push_str(r#","componentType":"#);
    s.push_str(&U16_T.to_string());
    s.push_str(r#","count":"#);
    s.push_str(&n_verts.to_string());
    s.push_str(r#","type":""#);
    s.push_str(VEC3);
    s.push_str(r#"","min":[0,0,0],"max":["#);
    s.push_str(&q_max[0].to_string());
    s.push(',');
    s.push_str(&q_max[1].to_string());
    s.push(',');
    s.push_str(&q_max[2].to_string());
    s.push_str("]}");
}

fn push_accessor_positions(
    s: &mut String,
    first: &mut bool,
    bv_idx: u32,
    n_verts: u32,
    min: [f32; 3],
    max: [f32; 3],
) {
    if !*first {
        s.push(',');
    }
    *first = false;
    s.push_str(r#"{"bufferView":"#);
    s.push_str(&bv_idx.to_string());
    s.push_str(r#","componentType":"#);
    s.push_str(&F32.to_string());
    s.push_str(r#","count":"#);
    s.push_str(&n_verts.to_string());
    s.push_str(r#","type":""#);
    s.push_str(VEC3);
    s.push_str(r#"","min":["#);
    s.push_str(&format_f32(min[0]));
    s.push(',');
    s.push_str(&format_f32(min[1]));
    s.push(',');
    s.push_str(&format_f32(min[2]));
    s.push_str(r#"],"max":["#);
    s.push_str(&format_f32(max[0]));
    s.push(',');
    s.push_str(&format_f32(max[1]));
    s.push(',');
    s.push_str(&format_f32(max[2]));
    s.push_str("]}");
}

fn push_accessor_indices(
    s: &mut String,
    first: &mut bool,
    bv_idx: u32,
    n_indices: u32,
    component: u32,
) {
    if !*first {
        s.push(',');
    }
    *first = false;
    s.push_str(r#"{"bufferView":"#);
    s.push_str(&bv_idx.to_string());
    s.push_str(r#","componentType":"#);
    s.push_str(&component.to_string());
    s.push_str(r#","count":"#);
    s.push_str(&n_indices.to_string());
    s.push_str(r#","type":""#);
    s.push_str(SCALAR);
    s.push_str(r#""}"#);
}

fn push_accessor_vec(
    s: &mut String,
    first: &mut bool,
    bv_idx: u32,
    count: u32,
    vec_type: &str,
) {
    if !*first {
        s.push(',');
    }
    *first = false;
    s.push_str(r#"{"bufferView":"#);
    s.push_str(&bv_idx.to_string());
    s.push_str(r#","componentType":"#);
    s.push_str(&F32.to_string());
    s.push_str(r#","count":"#);
    s.push_str(&count.to_string());
    s.push_str(r#","type":""#);
    s.push_str(vec_type);
    s.push_str(r#""}"#);
}

/// Per-entity-type default colour palette. Returns linear-space sRGB +
/// alpha. Translucent entries (alpha < 1) flag the material as BLEND in
/// the JSON emitter so the viewer can see *through* spaces and openings
/// to the solid geometry behind. Real IfcSurfaceStyle colour extraction
/// is tracked in ifcfast#3 — this is the neutral fallback.
fn default_color_for_entity(entity: &str) -> [f32; 4] {
    // Lowercase, strip "Ifc" prefix for matching.
    let e = entity.to_ascii_lowercase();
    let e = e.strip_prefix("ifc").unwrap_or(&e);
    let (r, g, b, a) = match e {
        "wall" | "wallstandardcase" => (0.88, 0.85, 0.78, 1.0),
        "slab" => (0.78, 0.78, 0.80, 1.0),
        "footing" => (0.55, 0.55, 0.55, 1.0),
        "beam" | "member" | "column" => (0.62, 0.66, 0.72, 1.0),
        "covering" => (0.85, 0.83, 0.78, 1.0),
        "railing" => (0.35, 0.35, 0.38, 1.0),
        "stairflight" | "stair" => (0.70, 0.68, 0.62, 1.0),
        "door" => (0.50, 0.36, 0.24, 1.0),
        "window" => (0.55, 0.72, 0.85, 0.55),
        "furnishingelement" => (0.78, 0.65, 0.48, 1.0),
        "space" => (0.62, 0.80, 0.78, 0.18),
        "openingelement" => (0.95, 0.55, 0.20, 0.22),
        "roof" => (0.45, 0.30, 0.25, 1.0),
        _ => (0.75, 0.75, 0.75, 1.0),
    };
    [r, g, b, a]
}

fn push_json_string(out: &mut String, s: &str) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
}

fn format_f32(v: f32) -> String {
    if v.is_finite() {
        format!("{}", v)
    } else {
        "0".to_string()
    }
}
