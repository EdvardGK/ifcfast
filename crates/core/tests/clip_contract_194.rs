//! GH #194 — a half-space clip on an `IfcBooleanClippingResult` is the
//! element's own shape, applied in every mode (no `cut_openings` needed).
//!
//! Fixtures are Revit 2025 IFC2X3 walls from the Snowdon Towers sample
//! (feet), pinned against ifcopenshell with opening subtraction disabled:
//!
//! * `clip_single_pbhs_194.ifc` — one wall, one
//!   `IfcPolygonalBoundedHalfSpace` `.T.`, no opening → 37.7596 m³
//!   (pre-#194 no-cut: 38.9135, the unclipped box).
//! * `clip_chain3_194.ifc` — the same wall under three chained clips
//!   (`.T.`, `.T.`, `.F.`), its `IfcRelVoidsElement` removed → 23.8721 m³.

#![cfg(feature = "mesh")]

use std::path::{Path, PathBuf};

use _core::mesh::qto;
use _core::mesh::{mesh_ifc_framed, BakeFrame, ProductMesh};

/// Feet → metres (both fixtures are authored in FOOT).
const FT: f32 = 0.3048;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures")
        .join(name)
}

fn wall(name: &str) -> (ProductMesh, _core::mesh::MeshStats) {
    let buf = std::fs::read(fixture(name)).expect("fixture readable");
    // Local bake: the wall sits ~1.4e6 ft from the origin, where a World
    // f32 bake quantises to 1/8 ft — the frame every QTO consumer uses.
    let (meshes, stats) = mesh_ifc_framed(&buf, BakeFrame::Local);
    let w = meshes
        .into_iter()
        .find(|m| m.guid == "1G_I00WlX2MBQekqB04dQj")
        .expect("the clipped wall is meshed");
    (w, stats)
}

fn assert_rel(got: f64, want: f64, what: &str) {
    assert!(
        ((got - want) / want).abs() < 1.0e-3,
        "{what}: got {got} m³, ifcopenshell (openings disabled) {want} m³"
    );
}

#[test]
fn single_bounded_clip_applies_without_cut_mode() {
    let (w, stats) = wall("clip_single_pbhs_194.ifc");
    let q = qto::compute(&w.vertices, &w.indices, FT);
    assert_rel(q.volume_m3.abs() as f64, 37.7596, "single clip, no-cut");
    assert_eq!(q.mesh_quality, "closed");
    assert!(q.volume_reliable);
    // The half-space is consumed: no stand-in slab, nothing to strip.
    assert!(
        w.segments.iter().all(|s| !s.source.contains("halfspace")),
        "{:?}",
        w.segments
    );
    assert_eq!(stats.halfspace_clip_unapplied, 0);
    assert_eq!(stats.halfspace_clip_manifold, 0);
}

#[test]
fn three_chained_clips_apply_without_cut_mode() {
    let (w, stats) = wall("clip_chain3_194.ifc");
    let q = qto::compute(&w.vertices, &w.indices, FT);
    assert_rel(q.volume_m3.abs() as f64, 23.8721, "chain of 3, no-cut");
    assert_eq!(q.mesh_quality, "closed");
    assert_eq!(stats.halfspace_clip_unapplied, 0);
    assert_eq!(stats.halfspace_clip_manifold, 0);
}

/// Cut mode on a clipped-but-unvoided wall is the same solid: the clip
/// already happened in the extractor, so `cut_openings::apply` finds no
/// cutter and passes it through unchanged.
#[cfg(feature = "csg")]
#[test]
fn cut_mode_equals_no_cut_on_clipped_unvoided_wall() {
    use _core::mesh::cut_openings::{apply, Outcome};
    for name in ["clip_single_pbhs_194.ifc", "clip_chain3_194.ifc"] {
        let (w, _) = wall(name);
        let mut cut = w.clone();
        assert_eq!(apply(&mut cut, FT), Outcome::Passthrough, "{name}");
        assert_eq!(cut.vertices, w.vertices, "{name}");
        assert_eq!(cut.indices, w.indices, "{name}");
    }
}

/// The substrate carries the clipped solid: the wall's representation row
/// (its local, rep-frame mesh) has the clipped volume, so `clash()` and
/// every parquet consumer see the clipped wall. `instances.volume_m3` is
/// computed from the WORLD-baked f32 mesh, which at this fixture's
/// ~1.4e6 ft georeference quantises to 1/8 ft (the known far-origin
/// substrate limit, GH #117) — pinned at 2 %, below the unclipped value.
#[cfg(feature = "bundle")]
#[test]
fn substrate_carries_clipped_volume() {
    use arrow::array::{Array, AsArray, BinaryArray, BooleanArray, StringArray};
    use arrow::datatypes::{Float32Type, UInt64Type};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let buf = std::fs::read(fixture("clip_single_pbhs_194.ifc")).unwrap();
    let out_dir = std::env::temp_dir().join(format!(
        "ifcfast-clip194-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&out_dir).unwrap();
    let bundle = _core::bundle::Bundle::build(&buf);
    let mut sink =
        _core::bundle::parquet_sink::ParquetSink::create_in_dir(&out_dir, &bundle, "clip194")
            .expect("sink");
    let _ = _core::mesh::mesh_ifc_streaming(&buf, &mut sink);
    sink.finish().expect("finish");

    let read = |stem: &str| {
        let path = std::fs::read_dir(&out_dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(stem) && n.ends_with(".parquet"))
            })
            .unwrap_or_else(|| panic!("{stem} parquet written"));
        ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(&path).unwrap())
            .unwrap()
            .build()
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
    };

    let mut rep_id = None;
    for batch in read("instances") {
        let guid = batch
            .column_by_name("guid")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap()
            .clone();
        let vol = batch
            .column_by_name("volume_m3")
            .unwrap()
            .as_primitive::<Float32Type>()
            .clone();
        let rel = batch
            .column_by_name("volume_reliable")
            .unwrap()
            .as_any()
            .downcast_ref::<BooleanArray>()
            .unwrap()
            .clone();
        let rid = batch
            .column_by_name("rep_id")
            .unwrap()
            .as_primitive::<UInt64Type>()
            .clone();
        for i in 0..batch.num_rows() {
            if guid.value(i) == "1G_I00WlX2MBQekqB04dQj" {
                let v = vol.value(i) as f64;
                assert!(
                    ((v - 37.7596) / 37.7596).abs() < 0.02,
                    "instances.volume_m3 {v}"
                );
                assert!(v < 38.9135 * 0.99, "instances.volume_m3 {v} is not clipped");
                assert!(rel.value(i), "volume_reliable");
                rep_id = Some(rid.value(i));
            }
        }
    }
    let rep_id = rep_id.expect("wall instance row present");
    let mut found = false;
    for batch in read("representations") {
        let rid = batch
            .column_by_name("rep_id")
            .unwrap()
            .as_primitive::<UInt64Type>()
            .clone();
        let vb = batch
            .column_by_name("vertices_le")
            .unwrap()
            .as_any()
            .downcast_ref::<BinaryArray>()
            .unwrap()
            .clone();
        let ib = batch
            .column_by_name("indices_le")
            .unwrap()
            .as_any()
            .downcast_ref::<BinaryArray>()
            .unwrap()
            .clone();
        for i in 0..batch.num_rows() {
            if rid.value(i) != rep_id {
                continue;
            }
            let v: Vec<f32> = vb
                .value(i)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect();
            let idx: Vec<u32> = ib
                .value(i)
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| u32::from_le_bytes(*c))
                .collect();
            let q = qto::compute(&v, &idx, FT);
            assert_rel(
                q.volume_m3.abs() as f64,
                37.7596,
                "representations local mesh",
            );
            found = true;
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(found, "wall representation row present");
}

/// Corpus telemetry for the clip route (GH #194): no half-space clip may be
/// left unapplied on real exporter output, and every product that went
/// through the Manifold fallback is named. Run with
///   IFCFAST_CORPUS=/a.ifc:/b.ifc cargo test -p ifcfast-core \
///     --test clip_contract_194 -- --ignored --nocapture
#[test]
#[ignore = "requires real corpus files via IFCFAST_CORPUS (not committed — RAM/licence)"]
fn corpus_clips_all_apply() {
    let raw = std::env::var("IFCFAST_CORPUS").expect(
        "corpus gate invoked (--ignored) but IFCFAST_CORPUS is unset — set \
         IFCFAST_CORPUS=/a.ifc:/b.ifc",
    );
    for path in raw.split(':').filter(|s| !s.is_empty()) {
        let buf = std::fs::read(path).expect("read corpus file");
        let (meshes, stats) = mesh_ifc_framed(&buf, BakeFrame::Local);
        let tagged = |t: &str| -> Vec<String> {
            meshes
                .iter()
                .filter(|m| {
                    m.segments
                        .iter()
                        .any(|s| s.source.split('|').any(|l| l == t))
                })
                .map(|m| format!("{} {}", m.guid, m.entity))
                .collect()
        };
        let clipped = meshes
            .iter()
            .filter(|m| {
                m.parts
                    .iter()
                    .any(|p| p.source.starts_with("boolean_first_operand"))
            })
            .count();
        eprintln!(
            "{path}: products_meshed={} boolean_hosts={clipped} \
             halfspace_clip_unapplied={} halfspace_clip_manifold={}",
            stats.products_meshed, stats.halfspace_clip_unapplied, stats.halfspace_clip_manifold
        );
        for g in tagged(_core::mesh::CLIP_MANIFOLD_TAG) {
            eprintln!("  manifold fallback: {g}");
        }
        let unapplied = tagged(_core::mesh::CLIP_UNAPPLIED_TAG);
        assert!(
            unapplied.is_empty(),
            "{path}: unapplied clips {unapplied:?}"
        );
    }
}

/// A clip that cannot be evaluated (half-space on a non-planar base
/// surface) is never silent: the host comes back UNCLIPPED, tagged
/// `halfspace_unclipped`, counted, and its QTO row is not trusted.
const UNRESOLVABLE_CLIP_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION(('ViewDefinition [ReferenceView]'),'2;1');
FILE_NAME('unresolvable.ifc','2026-09-28T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
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
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#11=IFCBUILDING('2Bldg000000000000000001',$,'b',$,$,#15,$,$,.ELEMENT.,$,$,$);
#12=IFCBUILDINGSTOREY('3Stor000000000000000001',$,'L1',$,$,#15,$,$,.ELEMENT.,0.0);
#15=IFCLOCALPLACEMENT($,#6);
#16=IFCLOCALPLACEMENT(#15,#6);
#20=IFCRELAGGREGATES('4Agg000000000000000001',$,$,$,#1,(#10));
#21=IFCRELAGGREGATES('5Agg000000000000000001',$,$,$,#10,(#11));
#22=IFCRELAGGREGATES('6Agg000000000000000001',$,$,$,#11,(#12));
#30=IFCRECTANGLEPROFILEDEF(.AREA.,'WallRect',#31,1000.,200.);
#31=IFCAXIS2PLACEMENT2D(#7,$);
#32=IFCDIRECTION((0.,0.,1.));
#33=IFCEXTRUDEDAREASOLID(#30,#6,#32,3000.);
#50=IFCCARTESIANPOINT((0.,0.,1500.));
#51=IFCAXIS2PLACEMENT3D(#50,$,$);
#52=IFCCYLINDRICALSURFACE(#51,5000.);
#53=IFCHALFSPACESOLID(#52,.F.);
#60=IFCBOOLEANCLIPPINGRESULT(.DIFFERENCE.,#33,#53);
#70=IFCSHAPEREPRESENTATION(#5,'Body','Clipping',(#60));
#71=IFCPRODUCTDEFINITIONSHAPE($,$,(#70));
#80=IFCWALL('7Wall00000000000000001',$,'Unresolvable',$,$,#16,#71,'tag',.STANDARD.);
#90=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel000000000000000001',$,$,$,(#80),#12);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn unresolvable_clip_is_marked_counted_and_untrusted() {
    let (meshes, stats) = mesh_ifc_framed(UNRESOLVABLE_CLIP_IFC.as_bytes(), BakeFrame::Local);
    let w = meshes
        .iter()
        .find(|m| m.entity == "IfcWall")
        .expect("wall meshed");
    assert_eq!(stats.halfspace_clip_unapplied, 1);
    assert!(_core::mesh::has_unapplied_clip(w), "{:?}", w.segments);
    let mut q = qto::compute(&w.vertices, &w.indices, 0.001);
    // Unclipped operand: the full 1000×200×3000 mm box.
    assert!((q.volume_m3.abs() - 0.6).abs() < 1e-6);
    qto::mark_clip_unapplied(&mut q);
    assert!(!q.volume_reliable);
    assert_eq!(q.volume_method, "mesh_unclipped");
}

// ----------------------------------------------------------------------
// GH #194 review fixtures (millimetre IFC, 1000 mm cube hosts), pinned
// against ifcopenshell 0.8.5 (`use-world-coords`, openings disabled
// unless stated; `ifcopenshell.util.shape.get_volume`).
// ----------------------------------------------------------------------

/// mm → m.
const MM: f32 = 0.001;

fn meshes_of(name: &str) -> (Vec<ProductMesh>, _core::mesh::MeshStats) {
    let buf = std::fs::read(fixture(name)).expect("fixture readable");
    mesh_ifc_framed(&buf, BakeFrame::Local)
}

fn by_guid<'a>(ms: &'a [ProductMesh], guid: &str) -> &'a ProductMesh {
    ms.iter()
        .find(|m| m.guid == guid)
        .unwrap_or_else(|| panic!("{guid} meshed"))
}

fn trusted_volume(m: &ProductMesh) -> f64 {
    let q = qto::compute(&m.vertices, &m.indices, MM);
    assert_eq!(q.mesh_quality, "closed", "{} {:?}", m.guid, m.segments);
    assert!(q.volume_reliable, "{}", m.guid);
    assert!(!_core::mesh::has_unapplied_clip(m), "{}", m.guid);
    q.volume_m3.abs() as f64
}

/// Review blocker: the cut pass collapses `segments` to one
/// `"cut_openings"` segment and clears `parts`, which used to erase the
/// `halfspace_unclipped` token — an unresolvable clip plus an
/// `IfcRelVoidsElement` opening came back `volume_reliable = true`. The
/// fact now rides on `ProductMesh::clip_unapplied` and survives the cut.
/// ifcopenshell: 1.0 m³ openings disabled, 0.75 m³ with the opening (it
/// leaves the cylindrical half-space unapplied too).
#[cfg(feature = "csg")]
#[test]
fn unapplied_clip_survives_cross_product_cut() {
    use _core::mesh::cut_openings::{CrossProductCut, Outcome, Routed};
    use _core::mesh::{mesh_ifc_streaming_framed, ProductSink};

    struct Capture(Vec<ProductMesh>);
    impl ProductSink for Capture {
        fn on_product(&mut self, mesh: ProductMesh) {
            self.0.push(mesh);
        }
    }
    let buf = std::fs::read(fixture("clip_unresolvable_opening_194.ifc")).unwrap();
    let idx = _core::indexer::index(&buf);
    let mut cross = CrossProductCut::from_indexer(&idx.voids_opening, &idx.voids_host);
    let mut sink = Capture(Vec::new());
    let stats = mesh_ifc_streaming_framed(&buf, &mut sink, BakeFrame::Local);
    assert_eq!(stats.halfspace_clip_unapplied, 1);
    let host = by_guid(&sink.0, "7Wall00000000000000001");
    assert!(host.clip_unapplied);
    assert!(_core::mesh::has_unapplied_clip(host));
    for m in sink.0 {
        assert!(matches!(cross.route(m), Routed::Suppressed | Routed::Held));
    }
    let folded = cross.flush(MM, None);
    assert_eq!(folded.len(), 1);
    let (w, outcome) = &folded[0];
    assert_eq!(*outcome, Outcome::Cut);
    // The segment chain is gone (informational only) …
    assert_eq!(w.segments.len(), 1);
    assert_eq!(w.segments[0].source, "cut_openings");
    assert!(w.parts.is_empty());
    // … the typed flag is not.
    assert!(w.clip_unapplied);
    assert!(_core::mesh::has_unapplied_clip(w));
    let mut q = qto::compute(&w.vertices, &w.indices, MM);
    assert!((q.volume_m3.abs() - 0.75).abs() < 1e-4, "{}", q.volume_m3);
    assert_eq!(q.volume_method, "mesh");
    qto::mark_clip_unapplied(&mut q);
    assert!(!q.volume_reliable);
    assert_eq!(q.volume_method, "mesh_unclipped");
}

/// `mark_clip_unapplied` relabels only a mesh-derived volume; a prism
/// fallback keeps its method name and is just marked unreliable.
#[test]
fn mark_clip_unapplied_keeps_prism_fallback_label() {
    let (ms, _) = meshes_of("clip_unresolvable_opening_194.ifc");
    let w = by_guid(&ms, "7Wall00000000000000001");
    let mut q = qto::compute(&w.vertices, &w.indices, MM);
    q.volume_method = "prism_fallback";
    qto::mark_clip_unapplied(&mut q);
    assert!(!q.volume_reliable);
    assert_eq!(q.volume_method, "prism_fallback");
    let mut q = qto::compute(&w.vertices, &w.indices, MM);
    q.volume_method = "mesh_open";
    qto::mark_clip_unapplied(&mut q);
    assert_eq!(q.volume_method, "mesh_unclipped");
}

/// A `IfcRepresentationMap` whose item is an `IfcBooleanClippingResult`
/// (plane through (0,0,700) mm, normal (1,0,1)/√2, `.F.`), used by an
/// identity `IfcMappedItem` and a mirrored (Axis1 = −X) + non-uniformly
/// scaled (Scale2 = 1.5) one; a third product carries the same clip
/// directly. The clip runs in the map frame, so both instances share one
/// clipped local mesh (one rep, keyed by the boolean's step id) and each
/// world volume is the directly clipped value times |det|.
/// ifcopenshell: 0.68 / 1.02 / 0.68 m³.
#[test]
fn mapped_clipping_result_is_shared_and_exact() {
    let (ms, stats) = meshes_of("clip_mapped_194.ifc");
    assert_eq!(stats.halfspace_clip_unapplied, 0);
    let a = by_guid(&ms, "0MapA00000000000000001");
    let b = by_guid(&ms, "0MapB00000000000000001");
    let c = by_guid(&ms, "0MapC00000000000000001");
    assert!((trusted_volume(a) - 0.68).abs() < 1e-5);
    assert!((trusted_volume(b) - 1.02).abs() < 1e-5);
    assert!((trusted_volume(c) - 0.68).abs() < 1e-5);
    assert_eq!(a.parts.len(), 1);
    assert_eq!(b.parts.len(), 1);
    // Keyed by the clipping result (#60), shared by both map instances;
    // the direct use of the same leaf (#70) is a different rep.
    assert_eq!(a.parts[0].rep_step_id, 60);
    assert_eq!(b.parts[0].rep_step_id, 60);
    assert_eq!(c.parts[0].rep_step_id, 70);
    assert_eq!(a.parts[0].local_vertices, b.parts[0].local_vertices);
    assert_eq!(a.parts[0].local_indices, b.parts[0].local_indices);
    let local = qto::compute(&a.parts[0].local_vertices, &a.parts[0].local_indices, MM);
    assert!((local.volume_m3.abs() - 0.68).abs() < 1e-5);
    let det = glam::Mat4::from_cols_array(&b.parts[0].instance_transform).determinant();
    assert!(
        (det + 1.5).abs() < 1e-5,
        "mirrored + scaled instance, det {det}"
    );
}

/// `IfcBoxedHalfSpace` is an `IfcHalfSpaceSolid` whose `Enclosure` is a
/// computational hint: same clip. ifcopenshell: 0.68 m³ for both.
#[test]
fn boxed_half_space_equals_half_space_solid() {
    let (ms, stats) = meshes_of("clip_boxed_194.ifc");
    assert_eq!(stats.halfspace_clip_unapplied, 0);
    let hs = trusted_volume(by_guid(&ms, "0HsSolid00000000000001"));
    let bx = trusted_volume(by_guid(&ms, "0HsBoxed00000000000001"));
    assert!((hs - 0.68).abs() < 1e-5, "{hs}");
    assert!((bx - hs).abs() < 1e-9, "{bx} vs {hs}");
}

/// IFC2X3 defaults. `0DefPlane…`: the `IfcPlane` Position has `$` Axis and
/// RefDirection (plane z = 600 mm, normal +Z, `.T.` removes below) →
/// 0.4 m³. `0PbhsAxisX…`: an `IfcPolygonalBoundedHalfSpace` whose base
/// plane AND boundary Position are `Axis = (1,0,0)`, `RefDirection = $` —
/// before the `IfcFirstProjAxis` fix the frame was singular (y = X × X)
/// and the boundary collapsed to a line. The 500 × 500 boundary square is
/// centred, so the result does not depend on which in-plane x-axis the
/// default picks (the spec's +Y; ifcopenshell's +Z): removes x > 200 mm,
/// |y| < 250, z < 250 → 0.9625 m³. ifcopenshell: 0.4 / 0.9625 m³.
#[test]
fn ifc2x3_default_axes_and_axis_x_pbhs() {
    let (ms, stats) = meshes_of("clip_default_axes_2x3_194.ifc");
    assert_eq!(stats.halfspace_clip_unapplied, 0);
    let plane = trusted_volume(by_guid(&ms, "0DefPlane0000000000001"));
    let pbhs = trusted_volume(by_guid(&ms, "0PbhsAxisX000000000001"));
    assert!((plane - 0.4).abs() < 1e-5, "{plane}");
    assert!((pbhs - 0.9625).abs() < 1e-5, "{pbhs}");
}

/// A plain unbounded `IfcHalfSpaceSolid` cutting diagonally (normal
/// (1,1,1)/√3 through (100,0,600) mm, `.T.`) through a host with no void,
/// no-cut mode. ifcopenshell: 0.352666667 m³.
#[test]
fn diagonal_unbounded_half_space_no_cut() {
    let (ms, stats) = meshes_of("clip_diagonal_hs_194.ifc");
    assert_eq!(stats.halfspace_clip_unapplied, 0);
    let v = trusted_volume(by_guid(&ms, "0Diag00000000000000001"));
    assert!((v - 0.352_666_667).abs() < 1e-5, "{v}");
}
