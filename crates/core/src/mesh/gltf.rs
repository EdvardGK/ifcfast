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
//! Strategy for IFC product meshes:
//!   * One concatenated binary buffer carrying ALL product vertex + index data
//!   * Per-product `bufferView` × 2  (positions + indices)
//!   * Per-product `accessor` × 2
//!   * Per-product `mesh` (with one primitive) and `node` (linked to that mesh)
//!   * Single scene listing every node
//!
//! Per-node `extras` carries `guid` + `entity` so the viewer can pick by GUID.
//!
//! No materials, no textures, no animations. The viewer computes face normals
//! on load (Three.js / Babylon / xeokit all do this for normal-less meshes).

use std::io::{self, BufWriter, Write};

use crate::mesh::ProductMesh;

// glTF component types
const F32: u32 = 5126;
const U32_T: u32 = 5125;
const U16_T: u32 = 5123;

// glTF accessor types (string in JSON)
const VEC3: &str = "VEC3";
const SCALAR: &str = "SCALAR";

// glTF buffer view targets
const ARRAY_BUFFER: u32 = 34962;
const ELEMENT_ARRAY_BUFFER: u32 = 34963;

/// Write `meshes` as a glTF 2.0 binary file.
pub fn write<W: Write>(meshes: &[ProductMesh], out: &mut W) -> io::Result<()> {
    // 1. Build the binary buffer (positions + indices for every product,
    //    each aligned to 4 bytes).
    let (binary, views) = pack_binary(meshes);

    // 2. Build the JSON.
    let json = build_json(meshes, &views, binary.len() as u32);

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

struct BufferViews {
    /// Per-product: (positions_view_idx, indices_view_idx, n_verts, n_indices,
    /// min_xyz, max_xyz, indices_byte_offset, indices_byte_len)
    products: Vec<ProductViews>,
}

struct ProductViews {
    pos_view: u32,
    idx_view: u32,
    n_verts: u32,
    n_indices: u32,
    min: [f32; 3],
    max: [f32; 3],
    // Component type for indices: U16 if n_verts < 65535, else U32.
    idx_component: u32,
}

/// Concatenate all product buffers into a single binary blob, aligned to
/// 4 bytes between regions, and produce the bufferView table.
fn pack_binary(meshes: &[ProductMesh]) -> (Vec<u8>, BufferViews) {
    // Reserve a reasonable amount up front.
    let total_verts: usize = meshes.iter().map(|m| m.vertices.len()).sum();
    let total_indices: usize = meshes.iter().map(|m| m.indices.len()).sum();
    let mut bin = Vec::with_capacity(total_verts * 4 + total_indices * 4);
    let mut products = Vec::with_capacity(meshes.len());

    for (i, mesh) in meshes.iter().enumerate() {
        // Positions block (f32 × 3 per vertex).
        let pos_offset = bin.len();
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        for chunk in mesh.vertices.chunks_exact(3) {
            for k in 0..3 {
                let v = chunk[k];
                if v < min[k] { min[k] = v; }
                if v > max[k] { max[k] = v; }
            }
        }
        for v in &mesh.vertices {
            bin.extend_from_slice(&v.to_le_bytes());
        }
        let pos_len = bin.len() - pos_offset;
        pad4(&mut bin);

        // Indices block. Use u16 when possible to save 50% space, else u32.
        let n_verts = (mesh.vertices.len() / 3) as u32;
        let idx_offset = bin.len();
        let idx_component = if n_verts <= u16::MAX as u32 + 1 {
            for &i in &mesh.indices {
                bin.extend_from_slice(&(i as u16).to_le_bytes());
            }
            U16_T
        } else {
            for &i in &mesh.indices {
                bin.extend_from_slice(&i.to_le_bytes());
            }
            U32_T
        };
        let idx_len = bin.len() - idx_offset;
        pad4(&mut bin);

        // Two bufferViews per product (i*2, i*2+1).
        let _pos_view_byte_offset = pos_offset as u32;
        let _pos_view_byte_length = pos_len as u32;
        let _idx_view_byte_offset = idx_offset as u32;
        let _idx_view_byte_length = idx_len as u32;
        let _ = (idx_offset, idx_len); // keep clippy quiet

        products.push(ProductViews {
            pos_view: (i * 2) as u32,
            idx_view: (i * 2 + 1) as u32,
            n_verts,
            n_indices: mesh.indices.len() as u32,
            min,
            max,
            idx_component,
        });
    }

    (
        bin,
        BufferViews { products },
    )
}

fn pad4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

fn pad_json(mut json: Vec<u8>) -> Vec<u8> {
    // JSON chunk padded with ASCII spaces per spec.
    while json.len() % 4 != 0 {
        json.push(b' ');
    }
    json
}

fn pad_bin(mut bin: Vec<u8>) -> Vec<u8> {
    while bin.len() % 4 != 0 {
        bin.push(0);
    }
    bin
}

fn build_json(meshes: &[ProductMesh], views: &BufferViews, binary_len: u32) -> String {
    // We hand-roll the JSON because the structure is regular and the
    // serde / gltf-json overhead would dwarf the actual content cost on
    // 20K+ products.
    let mut s = String::with_capacity(meshes.len() * 400 + 1024);
    s.push_str(r#"{"asset":{"version":"2.0","generator":"ifcfast-mesh"},"#);

    // 1. Single buffer
    s.push_str(r#""buffers":[{"byteLength":"#);
    s.push_str(&binary_len.to_string());
    s.push_str("}],");

    // 2. bufferViews (positions + indices alternating)
    s.push_str(r#""bufferViews":["#);
    let mut byte_offset: u32 = 0;
    let mut first = true;
    for (mesh, pv) in meshes.iter().zip(&views.products) {
        let pos_bytes = (mesh.vertices.len() * 4) as u32;
        let idx_bytes = match pv.idx_component {
            U16_T => (mesh.indices.len() * 2) as u32,
            U32_T => (mesh.indices.len() * 4) as u32,
            _ => 0,
        };
        if !first { s.push(','); }
        first = false;
        // Positions view
        s.push_str(r#"{"buffer":0,"byteOffset":"#);
        s.push_str(&byte_offset.to_string());
        s.push_str(r#","byteLength":"#);
        s.push_str(&pos_bytes.to_string());
        s.push_str(r#","target":"#);
        s.push_str(&ARRAY_BUFFER.to_string());
        s.push('}');
        byte_offset += pos_bytes;
        byte_offset = align4(byte_offset);
        s.push(',');
        // Indices view
        s.push_str(r#"{"buffer":0,"byteOffset":"#);
        s.push_str(&byte_offset.to_string());
        s.push_str(r#","byteLength":"#);
        s.push_str(&idx_bytes.to_string());
        s.push_str(r#","target":"#);
        s.push_str(&ELEMENT_ARRAY_BUFFER.to_string());
        s.push('}');
        byte_offset += idx_bytes;
        byte_offset = align4(byte_offset);
    }
    s.push_str("],");

    // 3. Accessors (positions + indices per product)
    s.push_str(r#""accessors":["#);
    first = true;
    for pv in &views.products {
        if !first { s.push(','); }
        first = false;
        // Positions accessor
        s.push_str(r#"{"bufferView":"#);
        s.push_str(&pv.pos_view.to_string());
        s.push_str(r#","componentType":"#);
        s.push_str(&F32.to_string());
        s.push_str(r#","count":"#);
        s.push_str(&pv.n_verts.to_string());
        s.push_str(r#","type":""#);
        s.push_str(VEC3);
        s.push_str(r#"","min":["#);
        s.push_str(&format_f32(pv.min[0]));
        s.push(',');
        s.push_str(&format_f32(pv.min[1]));
        s.push(',');
        s.push_str(&format_f32(pv.min[2]));
        s.push_str(r#"],"max":["#);
        s.push_str(&format_f32(pv.max[0]));
        s.push(',');
        s.push_str(&format_f32(pv.max[1]));
        s.push(',');
        s.push_str(&format_f32(pv.max[2]));
        s.push_str("]}");
        s.push(',');
        // Indices accessor
        s.push_str(r#"{"bufferView":"#);
        s.push_str(&pv.idx_view.to_string());
        s.push_str(r#","componentType":"#);
        s.push_str(&pv.idx_component.to_string());
        s.push_str(r#","count":"#);
        s.push_str(&pv.n_indices.to_string());
        s.push_str(r#","type":""#);
        s.push_str(SCALAR);
        s.push_str(r#""}"#);
    }
    s.push_str("],");

    // 4. Materials — one per product, named with the GUID so viewers can
    //    address each product individually (the demo viewer recolors by
    //    `material.name == guid`). Default PBR with an entity-aware
    //    fallback palette; richer colour via IfcSurfaceStyle is tracked
    //    separately (ifcfast#3).
    s.push_str(r#""materials":["#);
    first = true;
    for mesh in meshes.iter() {
        if !first { s.push(','); }
        first = false;
        let (r, g, b, a) = default_color_for_entity(&mesh.entity);
        let translucent = a < 1.0;
        s.push_str(r#"{"name":""#);
        push_json_string(&mut s, &mesh.guid);
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

    // 5. Meshes — each primitive references its product's material.
    s.push_str(r#""meshes":["#);
    first = true;
    for (i, _pv) in views.products.iter().enumerate() {
        if !first { s.push(','); }
        first = false;
        let pos_acc = (i * 2) as u32;
        let idx_acc = (i * 2 + 1) as u32;
        s.push_str(r#"{"primitives":[{"attributes":{"POSITION":"#);
        s.push_str(&pos_acc.to_string());
        s.push_str(r#"},"indices":"#);
        s.push_str(&idx_acc.to_string());
        s.push_str(r#","material":"#);
        s.push_str(&i.to_string());
        s.push_str(r#","mode":4}]}"#);
    }
    s.push_str("],");

    // 6. Nodes — product nodes (with GUID + entity extras) plus a root
    //    rotator node that wraps them in a glTF-conformant Y-up frame.
    //    BIM data is authored Z-up; glTF is spec'd Y-up. The root carries
    //    a quaternion rotation of −90° about X so the whole scene reads
    //    upright in any glTF viewer without a viewer-side patch.
    s.push_str(r#""nodes":["#);
    first = true;
    for (i, mesh) in meshes.iter().enumerate() {
        if !first { s.push(','); }
        first = false;
        s.push_str(r#"{"mesh":"#);
        s.push_str(&i.to_string());
        s.push_str(r#","name":""#);
        push_json_string(&mut s, &mesh.guid);
        s.push_str(r#"","extras":{"guid":""#);
        push_json_string(&mut s, &mesh.guid);
        s.push_str(r#"","entity":""#);
        push_json_string(&mut s, &mesh.entity);
        s.push_str(r#"","source":""#);
        push_json_string(&mut s, mesh.source);
        s.push_str(r#"","segments":["#);
        // Per-segment provenance — lets the viewer split, colour, or
        // filter a product's triangles by representation role. A wall
        // built from IfcBooleanClippingResult will have two entries
        // here, e.g. one tagged "boolean_first_operand|extrusion"
        // (the host bulk) and one "boolean_second_operand|halfspace_bounded"
        // (the clip volume) — both visible.
        for (si, seg) in mesh.segments.iter().enumerate() {
            if si > 0 { s.push(','); }
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
    // Root rotator at the end: rotation quat (x, y, z, w) for −90° about X
    //   = (sin(-π/4), 0, 0, cos(-π/4)) ≈ (-0.7071068, 0, 0, 0.7071068).
    if !meshes.is_empty() { s.push(','); }
    s.push_str(r#"{"name":"ifcfast_root","rotation":[-0.70710677,0,0,0.70710677],"children":["#);
    first = true;
    for i in 0..meshes.len() {
        if !first { s.push(','); }
        first = false;
        s.push_str(&i.to_string());
    }
    s.push_str("]}");
    s.push_str("],");

    // 7. Scene — only the root node; product nodes hang under it.
    let root_idx = meshes.len();
    s.push_str(r#""scenes":[{"nodes":["#);
    s.push_str(&root_idx.to_string());
    s.push_str(r#"]}],"scene":0}"#);

    s
}

/// Per-entity-type default colour palette. Returns linear-space sRGB +
/// alpha. Translucent entries (alpha < 1) flag the material as BLEND in
/// the JSON emitter so the viewer can see *through* spaces and openings
/// to the solid geometry behind. Real IfcSurfaceStyle colour extraction
/// is tracked in ifcfast#3 — this is the neutral fallback.
fn default_color_for_entity(entity: &str) -> (f32, f32, f32, f32) {
    // Lowercase, strip "Ifc" prefix for matching.
    let e = entity.to_ascii_lowercase();
    let e = e.strip_prefix("ifc").unwrap_or(&e);
    match e {
        "wall" | "wallstandardcase" => (0.88, 0.85, 0.78, 1.0),
        "slab"                       => (0.78, 0.78, 0.80, 1.0),
        "footing"                    => (0.55, 0.55, 0.55, 1.0),
        "beam" | "member" | "column" => (0.62, 0.66, 0.72, 1.0),
        "covering"                   => (0.85, 0.83, 0.78, 1.0),
        "railing"                    => (0.35, 0.35, 0.38, 1.0),
        "stairflight" | "stair"      => (0.70, 0.68, 0.62, 1.0),
        "door"                       => (0.50, 0.36, 0.24, 1.0),
        "window"                     => (0.55, 0.72, 0.85, 0.55),
        "furnishingelement"          => (0.78, 0.65, 0.48, 1.0),
        "space"                      => (0.62, 0.80, 0.78, 0.18),
        "openingelement"             => (0.95, 0.55, 0.20, 0.22),
        "roof"                       => (0.45, 0.30, 0.25, 1.0),
        _                             => (0.75, 0.75, 0.75, 1.0),
    }
}

fn align4(n: u32) -> u32 {
    (n + 3) & !3
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
