//! Circular-arc segments in composite-curve profiles (GH #123).
//!
//! Thin *curved* walls (Revit "15mm flis", G55_ARK) author their profile as
//! an `IfcArbitraryClosedProfileDef` whose `OuterCurve` is an
//! `IfcCompositeCurve` of two `IfcTrimmedCurve` arcs (inner + outer radius)
//! joined by two short `IfcPolyline` end caps. Before the fix `profile.rs`
//! did not sample `IfcTrimmedCurve`, so the composite walk `continue`d past
//! both arcs and the profile collapsed to its two ~15 mm cap lines — a
//! near-collinear zero-area sliver. The extruded mesh became an open tube
//! (no caps, side walls only), its signed-tetra volume collapsed to ~0, and
//! `mesh_qto` fell to `prism_fallback` which over-counted the AABB ~8-9×
//! (G55_ARK: ours 0.1749 vs ios 0.0182). Sampling the arcs restores the true
//! curved-band profile.
//!
//! Synthetic proxy: a top half-annulus, inner radius 1, outer radius 2,
//! extruded to depth 1. True area = π(2²−1²)/2 = 1.5π; the inscribed
//! polygon (arcs sampled) lands just under. Pre-fix this collapses to ~0.

#![cfg(feature = "mesh")]

use _core::mesh::mesh_ifc;

/// Half-annulus (r_in 1, r_out 2) extruded depth 1. The outer arc runs CCW
/// 0°→180° (top), a cap drops to the inner radius, the inner arc runs the
/// top back to +x (composite `SameSense=.F.` reverses the CCW basis into the
/// clockwise inner boundary), a cap returns to the outer radius.
const ARC_WALL_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('arc.ifc','2026-07-03T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#15=IFCLOCALPLACEMENT($,#6);
#30=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#31);
#31=IFCCOMPOSITECURVE((#40,#50,#60,#70),.F.);
#40=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#41);
#41=IFCTRIMMEDCURVE(#42,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(180.)),.T.,.PARAMETER.);
#42=IFCCIRCLE(#43,2.);
#43=IFCAXIS2PLACEMENT2D(#44,$);
#44=IFCCARTESIANPOINT((0.,0.));
#50=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#51);
#51=IFCPOLYLINE((#52,#53));
#52=IFCCARTESIANPOINT((-2.,0.));
#53=IFCCARTESIANPOINT((-1.,0.));
#60=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.F.,#61);
#61=IFCTRIMMEDCURVE(#62,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(180.)),.T.,.PARAMETER.);
#62=IFCCIRCLE(#43,1.);
#70=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#71);
#71=IFCPOLYLINE((#72,#73));
#72=IFCCARTESIANPOINT((1.,0.));
#73=IFCCARTESIANPOINT((2.,0.));
#80=IFCDIRECTION((0.,0.,1.));
#81=IFCEXTRUDEDAREASOLID(#30,#6,#80,1.);
#82=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#81));
#83=IFCPRODUCTDEFINITIONSHAPE($,$,(#82));
#90=IFCBUILDINGELEMENTPROXY('7Arc00000000000000001',$,'arc',$,$,#15,#83,$);
#91=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel00000000000000001',$,$,$,(#90),#10);
ENDSEC;
END-ISO-10303-21;
"#;

/// Signed-tetra divergence volume over the whole mesh, f64 accumulation.
fn signed_volume(vertices: &[f32], faces: &[u32]) -> f64 {
    let mut acc = 0.0_f64;
    for tri in faces.as_chunks::<3>().0 {
        let p = |i: u32| {
            let b = i as usize * 3;
            (
                vertices[b] as f64,
                vertices[b + 1] as f64,
                vertices[b + 2] as f64,
            )
        };
        let (ax, ay, az) = p(tri[0]);
        let (bx, by, bz) = p(tri[1]);
        let (cx, cy, cz) = p(tri[2]);
        acc += ax * (by * cz - bz * cy) - ay * (bx * cz - bz * cx) + az * (bx * cy - by * cx);
    }
    acc / 6.0
}

#[test]
fn composite_curve_arc_profile_has_true_curved_volume() {
    let (meshes, _stats) = mesh_ifc(ARC_WALL_IFC.as_bytes());
    assert_eq!(meshes.len(), 1, "expected exactly one product mesh");
    let vol = signed_volume(&meshes[0].vertices, &meshes[0].indices).abs();

    // True half-annulus volume = π(2²−1²)/2 × depth 1 = 1.5π ≈ 4.712.
    // Arcs are sampled, so the inscribed polygon lands just under; allow 3%.
    let analytic = 1.5 * std::f64::consts::PI;
    assert!(
        (vol - analytic).abs() / analytic < 0.03,
        "arc profile volume {vol:.4} != analytic {analytic:.4} (±3%) — arcs not sampled?"
    );
    // Hard regression floor: pre-fix the profile collapsed to a sliver and
    // the extruded tube volume was ~0.
    assert!(
        vol > 4.0,
        "arc profile volume {vol:.4} collapsed toward zero — GH #123 regression"
    );
}

// GH #139 — the same half-annulus authored on an **ellipse** basis
// (SemiAxis1 / SemiAxis2) instead of a circle. Before #139 an IfcEllipse
// basis returned `None` from `trimmed_curve_2d`, so both arcs dropped and the
// profile collapsed to its two straight caps — the exact #123 sliver
// signature, on a valid but unobserved-on-G55 basis curve. Outer ellipse
// a=2/b=1, inner a=1/b=0.5; the on-axis x-endpoints (±2, ±1) match the
// circle version so the caps are unchanged.
const ELLIPSE_WALL_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('ellipse.ifc','2026-07-03T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#15=IFCLOCALPLACEMENT($,#6);
#30=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#31);
#31=IFCCOMPOSITECURVE((#40,#50,#60,#70),.F.);
#40=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#41);
#41=IFCTRIMMEDCURVE(#42,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(180.)),.T.,.PARAMETER.);
#42=IFCELLIPSE(#43,2.,1.);
#43=IFCAXIS2PLACEMENT2D(#44,$);
#44=IFCCARTESIANPOINT((0.,0.));
#50=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#51);
#51=IFCPOLYLINE((#52,#53));
#52=IFCCARTESIANPOINT((-2.,0.));
#53=IFCCARTESIANPOINT((-1.,0.));
#60=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.F.,#61);
#61=IFCTRIMMEDCURVE(#62,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(180.)),.T.,.PARAMETER.);
#62=IFCELLIPSE(#43,1.,0.5);
#70=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#71);
#71=IFCPOLYLINE((#72,#73));
#72=IFCCARTESIANPOINT((1.,0.));
#73=IFCCARTESIANPOINT((2.,0.));
#80=IFCDIRECTION((0.,0.,1.));
#81=IFCEXTRUDEDAREASOLID(#30,#6,#80,1.);
#82=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#81));
#83=IFCPRODUCTDEFINITIONSHAPE($,$,(#82));
#90=IFCBUILDINGELEMENTPROXY('7Ell00000000000000001',$,'ell',$,$,#15,#83,$);
#91=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel00000000000000001',$,$,$,(#90),#10);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn ellipse_basis_arc_profile_has_true_curved_volume() {
    let (meshes, _stats) = mesh_ifc(ELLIPSE_WALL_IFC.as_bytes());
    assert_eq!(meshes.len(), 1, "expected exactly one product mesh");
    let vol = signed_volume(&meshes[0].vertices, &meshes[0].indices).abs();

    // Half elliptical annulus = (π/2)(a_out·b_out − a_in·b_in)
    //                         = (π/2)(2·1 − 1·0.5) = 0.75π ≈ 2.356, depth 1.
    let analytic = 0.75 * std::f64::consts::PI;
    assert!(
        (vol - analytic).abs() / analytic < 0.03,
        "ellipse arc volume {vol:.4} != analytic {analytic:.4} (±3%) — ellipse basis not sampled?"
    );
    // Pre-fix the ellipse basis returned None → profile collapsed toward zero.
    assert!(
        vol > 2.0,
        "ellipse arc volume {vol:.4} collapsed toward zero — GH #139 regression"
    );
}

// GH #139 — circular arcs whose conic trim parameters are authored in
// **radians** (Trim2 = π), with the model declaring RADIAN as its
// PLANEANGLEUNIT. Before #139 the trim reader hardcoded degrees
// (`.to_radians()`), so π was read as ~3.14° → a near-zero sweep → sliver →
// collapse. `resolve_plane_angle_scale` now reads the declared RADIAN unit
// (factor 1.0) and samples the full semicircle. Geometry is the #123
// half-annulus (r_in 1, r_out 2), so the true volume is 1.5π.
const RADIAN_TRIM_WALL_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('radian.ifc','2026-07-03T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3,#4));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#4=IFCSIUNIT(*,.PLANEANGLEUNIT.,$,.RADIAN.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#15=IFCLOCALPLACEMENT($,#6);
#30=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#31);
#31=IFCCOMPOSITECURVE((#40,#50,#60,#70),.F.);
#40=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#41);
#41=IFCTRIMMEDCURVE(#42,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(3.14159265358979)),.T.,.PARAMETER.);
#42=IFCCIRCLE(#43,2.);
#43=IFCAXIS2PLACEMENT2D(#44,$);
#44=IFCCARTESIANPOINT((0.,0.));
#50=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#51);
#51=IFCPOLYLINE((#52,#53));
#52=IFCCARTESIANPOINT((-2.,0.));
#53=IFCCARTESIANPOINT((-1.,0.));
#60=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.F.,#61);
#61=IFCTRIMMEDCURVE(#62,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(3.14159265358979)),.T.,.PARAMETER.);
#62=IFCCIRCLE(#43,1.);
#70=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#71);
#71=IFCPOLYLINE((#72,#73));
#72=IFCCARTESIANPOINT((1.,0.));
#73=IFCCARTESIANPOINT((2.,0.));
#80=IFCDIRECTION((0.,0.,1.));
#81=IFCEXTRUDEDAREASOLID(#30,#6,#80,1.);
#82=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#81));
#83=IFCPRODUCTDEFINITIONSHAPE($,$,(#82));
#90=IFCBUILDINGELEMENTPROXY('7Rad00000000000000001',$,'rad',$,$,#15,#83,$);
#91=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel00000000000000001',$,$,$,(#90),#10);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn radian_authored_trims_sample_full_arc() {
    let (meshes, _stats) = mesh_ifc(RADIAN_TRIM_WALL_IFC.as_bytes());
    assert_eq!(meshes.len(), 1, "expected exactly one product mesh");
    let vol = signed_volume(&meshes[0].vertices, &meshes[0].indices).abs();

    // With RADIAN honoured, Trim2 = π is a true semicircle → 1.5π half-annulus.
    let analytic = 1.5 * std::f64::consts::PI;
    assert!(
        (vol - analytic).abs() / analytic < 0.03,
        "radian-trim volume {vol:.4} != analytic {analytic:.4} (±3%) — PLANEANGLEUNIT not honoured?"
    );
    // Pre-fix π was read as ~3.14° → near-zero sweep → sliver collapse.
    assert!(
        vol > 4.0,
        "radian-trim volume {vol:.4} collapsed toward zero — GH #139 degree/radian regression"
    );
}

// GH #139 — one radial cap authored as a trimmed **IfcLine** instead of an
// IfcPolyline (magnitude-2 direction vector, PARAMETER trims 0 → 0.5, so the
// cap runs (-2,0) → (-1,0)). Line-basis trimmed curves previously returned
// `None`. Note: unlike arcs, a dropped straight segment is silently
// reconstructed by polygon closure, so this cannot fail via area collapse —
// it instead guards the arithmetic (`P(u) = Pnt + u·Magnitude·dir`): a
// magnitude bug would jog the endpoint off (-1,0) and distort the profile.
// Geometry is the #123 half-annulus, so the true volume stays 1.5π.
const LINE_CAP_WALL_IFC: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''),'2;1');
FILE_NAME('line.ifc','2026-07-03T00:00:00',('test'),('skiplum'),'ifcfast','ifcfast','');
FILE_SCHEMA(('IFC2X3'));
ENDSEC;
DATA;
#1=IFCPROJECT('0Test000000000000000001',$,'p',$,$,$,$,(#5),#2);
#2=IFCUNITASSIGNMENT((#3));
#3=IFCSIUNIT(*,.LENGTHUNIT.,$,.METRE.);
#5=IFCGEOMETRICREPRESENTATIONCONTEXT($,'Model',3,1.0E-5,#6,$);
#6=IFCAXIS2PLACEMENT3D(#7,$,$);
#7=IFCCARTESIANPOINT((0.,0.,0.));
#10=IFCSITE('1Site000000000000000001',$,'s',$,$,#15,$,$,.ELEMENT.,$,$,$,$,$);
#15=IFCLOCALPLACEMENT($,#6);
#30=IFCARBITRARYCLOSEDPROFILEDEF(.AREA.,$,#31);
#31=IFCCOMPOSITECURVE((#40,#50,#60,#70),.F.);
#40=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#41);
#41=IFCTRIMMEDCURVE(#42,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(180.)),.T.,.PARAMETER.);
#42=IFCCIRCLE(#43,2.);
#43=IFCAXIS2PLACEMENT2D(#44,$);
#44=IFCCARTESIANPOINT((0.,0.));
#50=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#51);
#51=IFCTRIMMEDCURVE(#54,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(0.5)),.T.,.PARAMETER.);
#54=IFCLINE(#55,#56);
#55=IFCCARTESIANPOINT((-2.,0.));
#56=IFCVECTOR(#57,2.);
#57=IFCDIRECTION((1.,0.));
#60=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.F.,#61);
#61=IFCTRIMMEDCURVE(#62,(IFCPARAMETERVALUE(0.)),(IFCPARAMETERVALUE(180.)),.T.,.PARAMETER.);
#62=IFCCIRCLE(#43,1.);
#70=IFCCOMPOSITECURVESEGMENT(.CONTINUOUS.,.T.,#71);
#71=IFCPOLYLINE((#72,#73));
#72=IFCCARTESIANPOINT((1.,0.));
#73=IFCCARTESIANPOINT((2.,0.));
#80=IFCDIRECTION((0.,0.,1.));
#81=IFCEXTRUDEDAREASOLID(#30,#6,#80,1.);
#82=IFCSHAPEREPRESENTATION(#5,'Body','SweptSolid',(#81));
#83=IFCPRODUCTDEFINITIONSHAPE($,$,(#82));
#90=IFCBUILDINGELEMENTPROXY('7Lin00000000000000001',$,'lin',$,$,#15,#83,$);
#91=IFCRELCONTAINEDINSPATIALSTRUCTURE('8Rel00000000000000001',$,$,$,(#90),#10);
ENDSEC;
END-ISO-10303-21;
"#;

#[test]
fn line_basis_cap_samples_correct_endpoint() {
    let (meshes, _stats) = mesh_ifc(LINE_CAP_WALL_IFC.as_bytes());
    assert_eq!(meshes.len(), 1, "expected exactly one product mesh");
    let vol = signed_volume(&meshes[0].vertices, &meshes[0].indices).abs();

    // Arcs carry the area (1.5π half-annulus); the trimmed line just caps at
    // (-1,0). A magnitude/projection bug jogs that endpoint and distorts the
    // band. Tight tolerance so an arithmetic slip is caught.
    let analytic = 1.5 * std::f64::consts::PI;
    assert!(
        (vol - analytic).abs() / analytic < 0.02,
        "line-cap volume {vol:.4} != analytic {analytic:.4} (±2%) — IfcLine trim endpoint wrong?"
    );
    assert!(
        vol > 4.0,
        "line-cap volume {vol:.4} collapsed — GH #139 line-basis regression"
    );
}
