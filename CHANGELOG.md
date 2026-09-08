# Changelog

All notable changes to ifcfast will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security — bounded `.ifczip` decompression (GH #175)

- **Zip bombs are refused loudly instead of buffered.** A ZIP's declared
  sizes are attacker-controlled: a few hundred KB of deflate can inflate
  to gigabytes, taking a process — or, since GH #172 put the parser in
  the visitor's tab, a browser tab — down. A compressed-size check
  upstream (ifcfast.com caps a dropped file at 300 MB *packed*) cannot
  detect this at all, so the bound lives at the decompression
  choke-point and is shared by every entry point.
  - `source::decompress_ifczip` now streams the inflate against a
    decompressed-byte cap (**4 GiB** native, **1 GiB** on
    `wasm32` — a tab cannot hold more) and an expansion-ratio cap
    (**200×**, enforced once 8 MiB have inflated). Measured deflate on
    real IFC is 4.77× (G55_RIV, 101 MB), 4.84× (G55_ARK, 72 MB), 5.14×
    (Duplex, 2.4 MB), 5.28× (G55_RIE, 23 MB), and 1.8–2.7× on the repo's
    small STEP fixtures — the cap carries ~38× headroom.
  - The container walk is bounded too: **4096** members (read out of the
    End-Of-Central-Directory record *before* the buffer reaches the ZIP
    reader, which pre-allocates one entry struct per declared member) and
    **1024 bytes** per member name.
  - Failures are `io::ErrorKind::InvalidData` naming the limit, the
    observed size or ratio, and the member; the buffer is dropped whole,
    never returned partially.
  - New `source::DecompressLimits` (with `Default`) plus
    `decompress_ifczip_with` / `open_with` / `open_bytes_with`; the
    existing `decompress_ifczip` / `open` / `open_bytes` signatures are
    unchanged and take the defaults.
  - Python enforces the same bounds on its own decompression path
    (`ifcfast.header.native_path_for`, which is what `_core.*` calls
    actually run), raising `ValueError` with the message intact — the
    same class the truncated-file guard raises. Overridable with
    `IFCFAST_MAX_DECOMPRESSED_BYTES` (int) and
    `IFCFAST_MAX_EXPANSION_RATIO` (float); an unparseable or non-positive
    value warns and keeps the default.
  - Genuine archives are unaffected: `.ifczip` inputs across the corpus
    open byte-identically.

### Added — ifcfast in the browser (GH #172)

- **`crates/wasm` (`ifcfast-wasm`)**: the pure-Rust core compiled to
  `wasm32-unknown-unknown` behind a `wasm-bindgen` class `IfcModel` —
  `fromBytes(bytes, name)` (STEP or `.ifczip`, magic-byte dispatch),
  `summaryJson()` / `graphJson()` / `qtoJson()` / `typesJson()` (the
  exact shapes `scripts/generate_sample_sidecars.py` writes — verified
  key-for-key against the shipped Duplex sample by
  `crates/wasm/test/parity.mjs`), `toGlb(perProductMaterials, instancing)`
  (same writer as `m.to_gltf()`), `bySourceJson()`, `statsJson()`.
  Duplex (2.4 MB) parses, extracts and meshes in ~220 ms in Node; the
  `.wasm` is 1.07 MB. No cut openings in v1 (manifold-csg is C++).
  Build: `crates/wasm/build.sh` (rustup + `wasm-bindgen --target web`).
- Core portability behind it, zero change for native builds: a
  `clock::Instant` shim (`web-time` on wasm), `memmap2` behind a
  default-on `mmap` feature with `source::open_bytes` for in-memory
  input, and the mesh pass taking its serial path on wasm without
  touching rayon.

## [0.5.1] - 2026-09-07

Lighter, exact circles; glTF materials you can pick by; and the mesher
tells you what it could not tessellate. Cache schema 30 → 31: every
circular product re-extracts.

### Changed — adaptive circle tessellation (GH #170, cache schema 30 → 31)

- Circles, ellipses and arcs (profile defs, `IfcCircle` / `IfcEllipse`
  curves, trimmed conic arcs, `IfcArcIndex` segments) and the CSG
  cylinder / cone / sphere primitives now sample **8 to 32 segments per
  full turn** chosen by a 0.5 mm sagitta tolerance — the radius is read
  in metres through the file's declared `LENGTHUNIT` (memoized on the
  entity table) — instead of a fixed 32 (profiles) / 24 (CSG). A 15 mm
  pipe gets 9 segments; anything ≥ ~105 mm radius keeps 32. Files with
  no resolvable length unit sample at the full 32 (unchanged output).
- Circles, ellipses and profile arcs (trimmed conics, `IfcArcIndex`)
  are emitted at an **area-preserving radius** — `sqrt(θ / (n·sin(θ/n)))`
  per sector, `sqrt(2π / (n·sin(2π/n)))` for a full turn — so extruded
  circular volumes are the analytic `π r² h` for every segment count,
  including Revit's two-semicircle pipe profiles. The old inscribed
  32-gon under-reported by 0.64 % (an 8-gon would by 10 %); expect a
  uniform +0.64 % on circular-profile QTO volumes and every circular
  product to re-extract. Verified per element: 2 882 Clinic pipes at a
  median ifcfast / π r² L ratio of 1.00000. Only 3D directrix arcs
  (swept paths) stay on the true curve.
- Measured on the buildingSMART Clinic Plumbing model: pipe
  (`IfcFlowSegment`) triangles fell 2.3×; the model total only 6 %,
  because its valves and fittings are pre-tessellated Revit family
  breps (one valve = 18 055 triangles) — tracked separately.

### Changed — glTF materials named by product GUID again (GH #146)

- `m.to_gltf(..., per_product_materials=True)` (new flag, **default
  on**) gives every baked primitive its own material named `"<guid>"`
  (first primitive) / `"<guid>#k"`, restoring the pick-by-material
  contract the v0.4.33 style pipeline had broken (materials were
  deduped by colour and named `#rrggbb`). Colours are unchanged;
  instanced groups keep one shared colour material. Pass `False` for
  the colour-deduped, smallest-JSON output.

### Added — mesh-pass counters on the Python surface (GH #166)

- `by_source` (`{representation kind: count}`, with everything the
  mesher could not tessellate as `"unhandled:IFCXXX"`), plus
  `products_seen` / `products_deferred`, now come back from every mesh
  entry point: `m.meshes().stats`, `m.mesh_qto()[0].attrs["mesh_stats"]`,
  and the `m.to_gltf()` / `m.point_cloud()` / `ifcfast.bundle()` stats
  dicts. Previously only the `ifcfast-mesh` debug binary printed it.

## [0.5.0] - 2026-09-06

The 0.5 line marks the end of the Layer-1 correctness programme: every
named QTO / mesh residue from the 0.4.x oracle sweeps is shipped or
parked, and the engine is now gated per class against `ifcopenshell`
on the G55 corpus. On top of that, 0.5.0 bundles everything landed
since 0.4.42 — first-class federation, the clash-oracle harness and
its narrow-phase speedups, the mixed-unit federation rescale, the
canonical STEP string escapes, and the nineteen-issue review sweep.
Cache schema 28 → 30: existing `.ifcfast` caches are rebuilt on first
open.

### Changed — federation accepts mixed units (GH #169)

`ifcfast.federate()` no longer refuses constituents authored in
different length units — the normal case on real projects, where
disciplines come from different tools and template years (the
buildingSMART Medical-Dental Clinic sample is Architectural /
Structural / Electrical in metres, HVAC / Plumbing in millimetres).
It used to raise `ValueError: unit_scale differs across bundles`,
which made cross-discipline `ifcfast.clash([...])` impossible on such
a set.

- Target unit = the **finest** constituent (smallest
  `ifcfast.unit_scale`); coarser sources are multiplied by
  `unit_scale_src / unit_scale_target` (metres → mm is ×1000). Scaling
  up preserves f32 relative precision; scaling down to metres would
  quantise far-from-origin site coordinates at centimetre level.
- Rescaled columns: `representations.vertices_le`,
  `local_bbox_min_xyz`, `local_bbox_max_xyz`; `instances.bbox_min_xyz`,
  `bbox_max_xyz`, `centroid_xyz`, `placement_xyz`, and `transform`'s
  translation slots (12/13/14) only. QTO columns (m² / m³),
  `materials[].thickness_mm`, `indices_le`, `segments` and
  `quantities[].value` are unit-independent or raw and stay untouched.
  Arrow dtypes and fixed-size-list shapes are preserved exactly.
- The merged parquets are stamped with the target `ifcfast.unit_scale`,
  so the clash engine converts to metres with no caller action.
  `federation.json` gains `unit_scales` (`{stem: raw}`) and
  `unit_factors` (`{stem: float}`); `unit_scale` is now the target.
- Fails loudly still: a missing / non-numeric / non-positive
  `unit_scale`, or a bundle whose own two parquets disagree.
- A single-unit federation is **byte-identical** to the previous
  output — the rescale pass is not entered. The federated cache key
  gains a `_FEDERATION_VERSION` salt so a pre-#169 cached merge is
  never served.
- Measured on the Clinic sample (5 disciplines, 3 m + 2 mm): federate
  0.95 s, clash 1.77 s, 61 485 pairs of which 36 425 cross-model.

### 2026-09-06 review sweep (GH #147–#165)

A full-codebase review filed nineteen issues and this sweep closes
them. Cache schema 29 → 30.

#### Fixed — correctness

- **Truncated DATA sections are refused** (GH #148). A stray byte
  (doubled `;;`) used to stop the record walk silently; every native
  entry point now raises `IfcfastError` with the offset, `ifcfast-bundle`
  exits 2.
- **Unit scale never silently defaults to metres** (GH #149). Unknown SI
  prefix / inconsistent SI name / broken conversion chain / missing
  assignment each leave `unit_scale` unset with a reason on the new
  `m.warnings`; the case-sensitive `IFCSIUNIT` compare is fixed.
- **Cut-openings fallback strips the synthetic half-space slabs**
  (GH #147) — a failed subtraction no longer sums a 20 000-unit tool
  quad into `volume_m3`, the AABB and the glTF.
- **IFC2x3 brep / shell-based surface models get the far-origin
  `rep_origin` rebase** (GH #153) — the faceset fix from GH #52 now
  covers the dominant 2x3 geometry path.
- **Substrate shared-rep rows carry the item's `rep_origin`** (GH #167,
  found by the clash oracle while gating #153): `parts[i].instance_transform`
  now folds the rebase, so `instances.transform × representations.vertices_le`
  reproduces the baked mesh. Before, every far-origin faceset (and, after
  #153, every IFC2x3 brep) was placed `rep_origin` short in
  `ifcfast.clash()` and in glTF instancing — on G55 the misplacement was
  ~0.4–0.5 m and had already cost Solibri-truth pairs on polygonal
  facesets before this sweep.
- **Reveal-all restored** (GH #154): an unsupported profile inside a
  multi-item Body and a composite curve with an untessellatable segment
  now surface as `unhandled:` counters instead of vanishing / bridging.
- **`IfcRevolvedAreaSolid.Angle` honours `PLANEANGLEUNIT`**, is clamped
  to one turn and the step count is capped (GH #155).
- **`mutate` translate/rotate keeps child placements attached**
  (GH #150): copy-on-write fires only when another *product* shares the
  placement, so a wall's openings move with it.
- **Bundle and clash parquet writes are atomic** (GH #151): staged to
  `<file>.tmp.<pid>` and renamed; a failure leaves the previous output
  intact.
- **Deterministic output order** (GH #152): voided-host flush, pset /
  quantity type inheritance and QTO surface buckets no longer follow
  `HashMap` iteration order.
- **Clash narrow phase rebases to a scene f64 anchor** before f32
  packing (GH #156); negative / NaN `tolerance_m` raise `ValueError`;
  per-residual diagnostics in `df.attrs["narrow_phase_residual_details"]`
  (GH #161).
- Parse-core cluster (GH #159): minted GUID version nibble, `tag` no
  longer reads `LongName` on spatial elements, header probe scans to
  `DATA;` (was 64 KB), ifczip pre-allocation capped, IFC2x3
  `IfcRoof` / `IfcSpace` `predefined_type` suppressed for ifcopenshell
  parity, unrecognised `IfcQuantity*` emit an `unhandled:` marker row,
  duplicate STEP ids warn.
- Geometry cluster (GH #160): unit-scaled drift floor, negative
  extrusion depth winding, sampler bounds, placement cycle guard and
  `IfcGridPlacement` marker, manifold status check, polygonal-face
  voids cut, per-colour 0–255 rescale, UTF-8-safe report truncation,
  modular `arc_span`.
- Python cluster (GH #157, #158, #162): `ifcfast://agents-guide` reads
  the packaged `AGENTS.md` and raises if missing (was a silent
  fallback); `system_prompt()` teaches the `volume_reliable` rule;
  manifest writes take an advisory lock and refresh source stat; the
  classifier signature covers the supertype map; cache-write failures
  go through the strict channel; `federate()` refuses `out_dir` equal
  to an input, checks the representations schema and revalidates its
  cache fingerprint; CLI catches `IfcfastError`; MCP tree tools take
  `limit` and the model cache is bounded.

#### Added

- **`materials.source` and `classifications.assignment_source`**
  (`"instance"` / `"type"`) — both layers now inherit through
  `IfcRelDefinesByType` like psets / quantities (GH #165).
- **`m.warnings`** (also `summary()["warnings"]`, `index` CLI JSON):
  the indexer's non-fatal anomaly list. Library code no longer prints.
- MCP `classifications` tool; `ifcfast.agents_guide()`.
- CI: `cargo fmt --check` + `cargo clippy -D warnings` lane (GH #164).

#### Changed

- `AGENTS.md` re-synced with the code (GH #163).

### Performance — clash narrow phase (GH #143)

- **Intersection-first narrow phase, rayon-parallel pairs.** The narrow
  phase used to call parry's global `distance` for every candidate pair,
  even at `tolerance_m == 0`, and ran serially — the federated G55
  RIE+RIV set (33k instances, 79k candidate pairs, 12.5M triangles) did
  not finish in 3 h. Class filters now run before any geometry work,
  TriMeshes are built once per participating instance in parallel,
  `intersection_test` runs first and `distance` only when a clearance
  band needs it. Same output; 16 min on the same set (debug profile).
- **Step 1 — single `distance()` on the clearance path.** parry's
  best-first distance visitor returns exactly `0.0` on the first
  touching leaf pair, so one query answers both the hard-clash and the
  clearance question. Bit-identical output.
- **Step 2 — band-capped reject-only prefilter.**
  `geom::min_distance_within(a, b, cap)` seeds the BVH traversal with
  the band cap so beyond-band candidates reject near the root. The probe
  is reject-only (parry's composite distance is only
  schedule-deterministic at ~1e-6 m); emitted distances still come from
  the exact query, and the cap is padded 5 mm / 5 % so a reject can
  never contradict the exact band verdict. Full G55 TMK sweep 10.5 h →
  4 min on a release `.so`; output bit-identical.

### Fixed — write

- **`encode_string` emits canonical ISO-10303-21 escapes** (GH #142).
  `mutate()` / `hotswap()` wrote non-ASCII as raw UTF-8 inside STEP
  string literals — readable by our lenient reader but silently dropped
  by strict readers (ifcopenshell 0.8.5 verified): æøå data loss on
  rename / set_property. Now `\X2\…\X0\` runs for BMP and
  `\X4\…\X0\` for non-BMP; `'` and `\` doubling unchanged; control
  characters still fail loud.

### Added — oracles (test-only, no runtime change)

- **Clash oracle harness** (GH #141, `tests/oracle/{bcf_truth,federate,
  clash_sweep}.py`): Solibri TMK BCF exports as ground truth,
  version-matched by STEP header, per-rule tolerance, selection-scoped
  supplemental runs. Five sweepable rounds on G55 Del3 / Del4; pair
  recall 13/13 on the founding round.
- **Mesh round-trip fidelity oracle** (`tests/oracle/mesh_roundtrip.py`,
  `tests/test_mesh_roundtrip.py`): parse → `hotswap`-rebuild → re-parse,
  gating scale-relative Hausdorff, triangle count, signed volume and
  winding. IFC4 direct geometry round-trips byte-identical in the local
  frame; IFC2x3 re-tessellates set-preserving.
- **Receipts / sidecar generators for ifcfast.com**
  (`scripts/generate_receipts.py`, sidecar type-gallery step): measured
  numbers for the site, stamped with version + machine.

### Added — first-class federation (GH #50)

- **`ifcfast.federate(bundles, out_dir, *, on_collision, reference_only)`** —
  materializing merge of N substrate bundles into one clash-able bundle
  (+ `federation.json` sidecar + `view.sql`), promoted from the clash
  oracle's hand-merge (`tests/oracle/federate.py`, which stays frozen as
  the differential spec — `tests/test_federate_parity.py` gates bitwise
  table parity between the two). Per-source `rep_id` offsetting, strict
  schema and `unit_scale` agreement, guid-collision policies
  (`warn` / `fail` / `dedup`).
- **`ifcfast.clash([a, b, …])`** — a list of bundle dirs federates into
  a content-keyed cache dir (`cache_root()/federated/<key>`) and runs
  the single-substrate engine; `df.attrs` carries `federated_dir` +
  the `federation` sidecar.
- **`source_model` column on `instances.parquet`** (Utf8, non-null):
  the source IFC's file stem, re-stamped with the constituent bundle
  dir's name by `federate()`. In a federated bundle the canonical join
  keys are `(guid, source_model)` / `(ifc_id, source_model)` — bare
  `guid` / `ifc_id` may collide across constituent models. Cache
  schema 28 → 29.
- **`source_model_a` / `source_model_b` on `clashes.parquet`** (empty
  string for pre-v29 bundles) — cross-model attribution without the
  sidecar dance: `df[df.source_model_a != df.source_model_b]`.
- **`reference_only` on `ifcfast.clash()`** — pairs whose BOTH sides
  come from reference models are dropped before narrow-phase
  (reference-vs-active still clashes). Enforced engine-side so
  `clashes.parquet` and the DataFrame agree, and one cached federation
  serves every reference-set choice.

## [0.4.42] - 2026-07-03

### Fixed — mesh geometry: composite-curve arc profiles (GH #123, #139)

- **Circular arc segments in composite-curve profiles (GH #123)** —
  `profile::extract` now samples `IfcTrimmedCurve` arcs on an `IfcCircle`
  basis inside an `IfcCompositeCurve` (thin curved walls, Revit "15mm
  flis"). Before, the arcs were skipped and the profile collapsed to its
  two straight end-cap lines — a zero-area sliver whose extruded mesh was
  an open tube (volume ~0 → `prism_fallback` over-counting the AABB
  ~8-9×; G55_ARK: 0.1749 vs ios 0.0182). The curved band now tessellates.
  Cache schema 26 → 27.
- **Ellipse / line basis + radian conic trims (GH #139)** — the arc
  sampling above is extended beyond the circular / degree-authored case.
  `IfcEllipse` (SemiAxis1 / SemiAxis2) and `IfcLine`
  (`Pnt + u·Magnitude·dir`) basis curves are now sampled (both previously
  returned `None` and could collapse a profile to the same sliver), and
  conic `IfcParameterValue` trims are scaled by the model's declared
  `PLANEANGLEUNIT` instead of assuming degrees — a radian-authored
  semicircle (0 → π) was misread as a ~3.14° sliver. The degree default
  is preserved for files that omit the declaration (Revit / ArchiCAD
  convention), so the circular + degree corpus path is arithmetically
  identical (G55 oracle sweep: zero drift on ARK + RIB). The
  `IfcUnitAssignment` lookup is memoized on `EntityTable` (one scan per
  model, not per arc — the assignment is the penultimate entity in
  G55_ARK's ~2.8M-entity table). Cache schema 27 → 28.

## [0.4.41] - 2026-07-03

### Fixed — QTO / mesh geometry (GH #62, #138)

- **Profile winding normalisation (GH #62)** — `profile::extract` now
  enforces the `Polygon2D` invariant (outer CCW, holes CW) by signed
  area before extrusion. Revit authors `IfcPolyline` profile voids
  clockwise; the previous blind `reverse()` inverted every hole-wall
  normal on swept solids with voids, so the divergence-theorem volume
  ADDED the void instead of subtracting it (G55_ARK: all 208 windows,
  mesh volume 8× the kernel → classified `open_shell` →
  `prism_fallback` → the +482% window residue). Post-fix all 208 ARK
  windows and doors sit at exact oracle parity. Cache schema 24 → 25.
- **Negative-direction extrusion winding (GH #138)** — `extrude_polygon`
  built cap/wall winding assuming the extrusion runs up the profile
  normal (+Z). An `IfcExtrudedAreaSolid` authored along
  `IFCDIRECTION((0.,0.,-1.))` therefore meshed inside-out: negative
  signed-tetra volume that CANCELLED against sibling solids in a
  multi-item Body (G55_ARK covering −14.2 %) and corrupted CSG
  difference subtractors (4 slabs +5–15 %). Winding now flips when the
  direction runs against the profile normal, so those solids add /
  subtract correctly. Oracle-gated: IfcSlab 1.0212 → 1.0000, IfcCovering
  0.9934 → 0.9999, all other classes byte-identical. Cache schema
  25 → 26. Substrates with such profiles re-extract via the cache key.

### Fixed / Changed — write-axis hardening (GH #132 items 2–8)

- **Subsets now carry authored colours and CAD layers**: `IfcStyledItem`
  rides along with every kept geometry item (pulling its surface-style
  chain) and `IfcPresentationLayerAssignment` / `…WithStyle` are kept
  with member lists pruned to survivors. On G55 this recovers 10.5k–36k
  styled items per model that were silently dropped before.
  `IfcRelAssignsToGroupByFactor` (zone weighting) is followed too.
- **Hotswap actually shrinks styled files**: an `IfcStyledItem`'s
  inbound edge no longer counts as ownership in orphan GC — it used to
  keep every styled (Revit-coloured) element's dead body alive. Orphaned
  styled items are cascade-removed with their item; layer assignments
  are pruned (or dropped when emptied). New stats: `pds_shared_with`
  (other products sharing the swapped `IfcProductDefinitionShape` — the
  swap changed their geometry too) and `body_reps` (only the first
  `Body` rep is swapped).
- **Lexer**: `parse_record_span` now finds the FIRST top-level `;`
  (string- and comment-aware) instead of the last `;` in the span — a
  trailing inter-record comment containing `;` made the record invisible
  to rel/guid resolution.
- **GlobalId resolution is shape-guarded**: only records with the
  `IfcRoot` argument shape resolve; a mistyped seed can no longer
  silently match `IFCMATERIAL('Concrete')` or a property `Name`.
- **`.ifczip` out_path is honoured**: `subset` / `hotswap` / `mutate`
  write a real single-member ZIP archive when the target ends in
  `.ifczip` (previously raw STEP under a lying extension).
- **Error normalisation**: every bad hotswap mesh input — negative or
  > 2³²−1 triangle index, flat 1-D array — raises `ValueError`
  (previously `OverflowError` / `TypeError` leaked from the binding).
- Corpus gates: `doc_roundtrip` now reads the unified `IFCFAST_CORPUS`
  (multi-path) like every other gate; legacy `IFCFAST_ROUNDTRIP_FILE`
  still accepted.

### Added — attribute mutation: `m.mutate(ops)` (GH #133)

- **`m.mutate(ops, out_path=None, seed=None)`** — the last write axis:
  batch attribute mutation on the owned STEP document with the same
  byte-identical-elsewhere guarantee as `subset`/`hotswap`. Op
  vocabulary: `set_property` (replace or add an
  `IfcPropertySingleValue`; authored wrapper type preserved, explicit
  `ifc_type` to retype, REQUIRED when adding — a Python float can't
  disambiguate `IFCREAL` vs `IFCLENGTHMEASURE`), `rename`
  (Name/Description on any rooted entity, raw-UTF-8 STEP string
  encoding so æøå survive), `translate` (parent-frame delta, native
  units) and `rotate` (axis-angle about the element's own location,
  composing with existing tilt). **Copy-on-write on shared data**: a
  pset applying to several elements is cloned (fresh GlobalId, valid
  22-char IFC compressed form) for the target element and siblings keep
  their values; placement points/directions are never edited in place —
  new records are minted and orphan GC (refcount fixpoint over the
  live, editor-aware graph) reclaims only what the element uniquely
  owned. **Atomic batches**: any failure aborts with a `ValueError`
  listing every failing op (`[op N] reason`), nothing half-written.
  Quantity sets (`IfcElementQuantity`) are type-guarded and refused —
  same-named pset/qto can't corrupt an `IfcQuantityLength`. Gated on
  the 4-discipline G55 corpus via the ifcopenshell oracle (property
  read-back, exact placement delta, zero dangling refs).
- Rust core: `doc::mutate` (editor overlay with maintained ref graph),
  `doc::step_fmt` (shared STEP REAL/string encoders — `encode_string`
  is the true inverse of the lexer's `decode_string`), `guid` (IFC
  compressed-GUID mint; 2-bit first char; ambient-entropy salt by
  default, opt-in `seed` for reproducible output),
  `doc::refs::RecordSource` (pluggable bytes resolution so subset /
  hotswap / mutate share one closure engine).

## [0.4.40] - 2026-07-02

### Added — local-frame mesh extraction + placement matrix (GH #127)

- **`m.mesh(guid, frame="local")` / `m.meshes(frame="local")`** — the
  hotswap round-trip bridge. Returns each element's
  **representation-item frame**: the coordinates its `Body` items
  store, *before* `ObjectPlacement`, in the file's **native length
  unit** — byte-for-byte the `m.hotswap` input, so
  extract-local → decimate → `m.hotswap` no longer double-applies the
  placement. Rows come back as `LocalMesh(guid, entity, vertices,
  faces, placement)` where `placement` is the `float64[4, 4]`
  row-major `world_from_local` matrix (native units); world-frame
  `_core.extract_meshes` / `_core.extract_mesh_one` output carries the
  per-product `placement` too. `frame="local"` rejects
  `cut_openings=True` and a non-default `unit=` loudly. Rust core: new
  `BakeFrame::ItemLocal` (drops the placement chain entirely; keeps
  the `IfcMappedItem` composition + `rep_origin` rebase through the
  f64 anchor). Round-trip gated on fixture (translation + rotation +
  mapped-item placements) and, via `IFCFAST_SUBSET_CORPUS`, on the
  real-file corpus.

### Added — surgical IFC writing: `m.subset()` + `m.hotswap()` (GH #124, #126)

- **`m.subset([guid, …])`** — carve a valid standalone IFC out of a
  model: forward-reachability closure over the picked elements, the
  spatial spine up to `IfcProject`, and 13 pruned `IfcRel*` rule types
  (properties, types, materials, classifications, voids/fillings,
  coverings, system-service, nests, groups, declares). Output re-opens
  in ifcfast **and** ifcopenshell with zero dangling references
  (corpus-gated on all four G55 disciplines). Returns STEP `bytes`, or
  writes a file + returns a stats dict via `out_path=`.
- **`m.hotswap(guid, vertices, triangles)`** — swap one element's Body
  representation for a caller-supplied triangle mesh (e.g. a decimated
  one), schema-aware: `IfcTriangulatedFaceSet` on IFC4,
  faceted-brep/surface-model on IFC2x3. Orphaned geometry records are
  reference-counted and garbage-collected. Coordinates are the
  element's **local** frame — extract it with
  `m.mesh(guid, frame="local")` (the GH #127 bridge above); default
  world-frame `m.meshes()` output cannot be fed back directly.
- Foundation: an owned round-trippable STEP document with
  **byte-identical emit** for untouched records.

### Fixed — write-axis hardening from the 2026-07-02 deep review

- **`m.hotswap` rejects non-finite coordinates** (`NaN`/`±inf` →
  `ValueError`) and always emits STEP REALs with a decimal point —
  Rust's shortest float form (`1e-5`) is not a valid ISO-10303-21
  REAL token (GH #128).
- **`m.subset` forward-closes everything a retained relationship
  names outside its anchor field** — a rel's own `IfcOwnerHistory`
  and IFC4's inline `IfcPropertySetDefinitionSet` aggregate could
  previously dangle in the output (GH #129).
- **`m.hotswap`'s orphan-GC counts the swapped rep's surviving
  references** — a uniquely-owned mapped body could previously
  cascade-delete the `IfcGeometricRepresentationSubContext` the
  swapped representation still points at (GH #130).
- **Corpus gates unified on `IFCFAST_CORPUS`** (legacy
  `IFCFAST_SUBSET_CORPUS` still accepted) and the Rust gates now
  fail loudly when invoked with `--ignored` but no corpus, instead
  of reporting green with zero assertions (GH #132).

### Fixed — open-shell QTO over-count on doors / windows / railings (GH #121)

- **`mesh_qto` now trusts an open shell's mesh volume within its tight
  upper bound instead of substituting the inflated prism.** The G55_ARK
  oracle sweep found `volume_m3` over-counting open-shell doors, windows,
  and railings **40–66×**: their signed-tetra mesh volume already matched
  the ifcopenshell kernel, but the lower-bound collapse tripwire
  (`mesh_vol < prism × 0.1`) mistook a legitimately-small volume (1.5–2.5 %
  of a loose prism bound) for a W4 CSG collapse and emitted the prism. The
  tripwire is retuned to a near-zero `mesh_vol < min(prism, aabb) × 1e-3`
  test, so only a genuine mesh → ~0 against a real multi-litre solid
  escalates; thin glazed shells keep their kernel-matching mesh value.
- **New `volume_method` value `"mesh_open"`** distinguishes a trusted
  open-shell mesh volume from a closed-manifold `"mesh"` (same
  `volume_reliable = true`; filter on `mesh_quality == "closed"` for
  watertight-only figures). The over-report and degenerate paths still
  yield `"prism_fallback"`, the only `volume_reliable = false` method.
- **Cache schema bumped `23` → `24`** — open-shell `volume_m3` /
  `volume_best_m3` / `volume_method` change; closed-manifold rows are
  byte-identical, so substrates with open-shell products must be
  re-extracted.

### Fixed — stale bounded-halfspace default-cut test (GH #114)

- Updated `tight_bounded_halfspace_default_over_cuts` (now
  `..._default_honors_polygon`) to assert the post-`add0465` default-path
  behaviour (~0.48 m³, honoring the bounding polygon) instead of the
  obsolete pre-fix over-cut (~0.20 m³).

## [0.4.39] - 2026-06-15

Tractable-backlog batch: a single-product mesh getter, a tighter QTO
prism bound, and a build-config fix. **Cache schema bumped `21` → `22`**
for the prism change (non-closed QTO rows only; closed rows are
byte-identical).

### Added — `m.mesh(guid)` single-product mesh getter (GH #47)

- **`Model.mesh(guid, *, unit="m", cut_openings=False, keep_cutters=False)
  -> Mesh | None`.** Tessellates *only* the target product's placement +
  representation chain (and, in cut mode, the openings voiding it),
  skipping the rest of the model — O(target) instead of meshing the whole
  file and filtering, the right shape for interactive picking (viewer
  click → mesh) and per-element edit pipelines. Returns one `Mesh(guid,
  entity, vertices, faces)` or `None` (unknown GUID, geometryless product,
  or — in cut mode — the target is itself an opening, or the cut consumed
  the host). New Rust entry `_core.extract_mesh_one` + the
  `mesh::mesh_products_by_step` primitive (tessellates a subset of
  products by step id via the same `tessellate_one` the batch streaming
  pass uses).
- **Cut parity by construction.** The host + its openings are routed
  through the *same* `CrossProductCut` the batch path uses, so the cut
  result is identical to the matching product from
  `meshes(cut_openings=True)`. Validated on G55_RIB + Duplex_A.
- **Coordinate contrast (deliberate).** `m.mesh()` returns `vertices` as
  **`float64` absolute world coordinates** — full precision, no
  `global_shift` to add back, because a single product can't overflow f32
  the way a whole georeferenced model can. Batch `meshes()` keeps shifted
  `float32` + `MeshList.global_shift`.

### Changed — QTO prism bound is now the min over three axis projections (GH #62)

- **`volume_prism_bound_m3` = min(footprint × perpendicular-extent) over
  X, Y, Z**, not the Z-axis prism alone. The Z-only prism over-counted a
  horizontal beam by its bounding slab; the min of the three per-axis
  prisms is the tightest valid upper bound regardless of orientation —
  tight for beams, columns, and slabs alike. `footprint_xy_raw` is
  generalised to `footprint_raw(u_axis, v_axis, …)`; the two extra rasters
  run only on the non-closed minority (~0.3 %), so the closed hot path
  stays raster-free. Because the prism is also the reliability tripwire
  and the fallback value, a tighter bound can flip `volume_reliable` /
  `volume_method` and change `volume_m3` / `volume_best_m3` for non-closed
  rows. **Cache schema `21` → `22`**; cached substrates of files with
  non-closed QTO rows must be re-extracted, closed-manifold rows
  (prism = `NaN`) are byte-identical. Refinement 2 from the issue (exact
  `i_overlay` 2D union instead of the raster) remains deferred.

### Fixed — `python`-without-`mesh` build no longer fails to compile (GH #112)

- **`set_cut_openings_stats` is now gated on `feature = "mesh"`.** The
  helper and its `CutOpeningsStats` type live under `crate::mesh`, but the
  fn definition lacked the `#[cfg(feature = "mesh")]` guard its only
  callers (the mesh-gated `extract_meshes` / `write_gltf` / `mesh_qto`
  pyfunctions) had, so `cargo build --no-default-features --features
  python` failed to compile. Default and pure-Rust builds were unaffected
  (CI builds the default feature set), so this never shipped in a wheel.
  No cache-schema impact.

### Fixed — denormalised `storey_guid` now agrees with `products_in` (GH #88)

- **Cache schema bumped `20` → `21`.** The denormalised `storey_guid` /
  `storey_name` columns (on `ProductRow` and on `instances.parquet`) now
  **inherit transitively through `IfcRelAggregates`.** Before this fix
  the columns were populated from direct
  `IfcRelContainedInSpatialStructure` containment only, so an aggregate
  part — a curtain-wall plate under its wall, a stair flight under its
  stair, where only the host is spatially contained — had
  `storey_guid = None`. Meanwhile `m.products_in(storey)` walks the
  unified graph and **includes** those parts (since GH #78), so the two
  storey-membership APIs disagreed and the columnar
  `m.filter(storey_guid=S)` / DuckDB `WHERE storey_guid = S` path silently
  returned a plausible-but-incomplete set ("exit 0, silently wrong").
  The column is now resolved by the same upward graph walk
  (`_walk_to_storey`) that backs `m.storey_of` / `m.products_in`: a
  product with no direct containment inherits the storey of the
  aggregate ancestor that *is* contained. **After the fix,
  `m.filter(storey_guid=S)` returns the exact same set as
  `m.products_in(S)`.** Direct storey containment keeps precedence (a
  directly-contained product uses its own storey, never an ancestor's),
  and the walk is cycle-guarded against malformed aggregate / containment
  loops. Both the Python indexer path (`ProductRow`) and the Rust bundle
  path (`instances.parquet`, via a new `resolve_storey_sid` resolver)
  were fixed in lockstep. On a real model (Grønland 55 ARK, 13 652
  products) this attaches a storey to 1 754 aggregate parts that the old
  column dropped; files with no aggregate-only parts are byte-identical.
  Cached substrates of affected files must be re-extracted.

### Changed — panic-safety guarantee is now universal (GH #27)

- **Every native PyO3 entry point now catches Rust panics at the
  boundary and re-raises them as the catchable `ifcfast.IfcfastError`**,
  instead of letting an uncatchable `pyo3_runtime.PanicException` abort
  the interpreter / kill a `ProcessPoolExecutor` worker. v0.4.20 (GH #23)
  applied the `catch_panic` wrapper only to the geometry-pipeline
  entries; the data-layer extractors were unwrapped. Now wrapped:
  `index_ifc`, `extract_psets`, `extract_quantities`,
  `extract_materials`, `extract_classifications`, `extract_all`,
  `bundle`, and `clash`. Hygiene + future-proofing — these paths
  aren't known to panic today, but the guarantee is now consistent
  across the whole surface so a panic introduced by a future extractor
  feature can't silently break corpus pipelines. **Behavioural on the
  failure path only**: the success path is byte-identical and explicit
  `?`-propagated errors keep their original type (`OSError`,
  `ValueError`); no cache-schema bump. The unwind capture moved into a
  pyo3-free `catch_unwind_to_message` helper so the panic→error
  translation is covered by Rust unit tests that link without libpython.

### Tested — IfcPolygonalBoundedHalfSpace base-surface normal (GH #52)

- **Regression test locking in the GH #52 fix.** Added a Rust unit test
  (`mesh::boolean::polygonal_bounded_halfspace_normal_from_base_surface`)
  that builds an `IfcPolygonalBoundedHalfSpace` whose `BaseSurface.Position`
  axis (`(-0.02, 0, -0.9998)`, the Sannergata `3_6AbaPP55…` tilt) diverges
  from the polygon's own `Position` frame (schema-default `(0, 0, 1)`), and
  asserts the slab's first-triangle normal — the direction
  `cut_openings::derive_plane_from_slab` removes — and the W6
  `BoundedHalfspacePayload.plane_normal` both follow the `BaseSurface`
  axis, never the polygon frame. Guards the `ac3b1ae` plane-normal fix and
  its interaction with the `!AgreementFlag` convention (GH #39) against
  regression. No behaviour change, no cut-result change, no cache-schema
  bump.

### Fixed — Python minor batch: fail loudly on edge states (GH #71, items 4-8)

Low-severity correctness items from the 0.4.36 sweep. Items 1-3 (loud
failures on typo'd table / entity / mode + top-level namespace hygiene)
shipped in 0.4.38 via #85; this completes the batch. **No cache schema
bump** — no on-disk substrate column or dtype meaning changes.

- **No-cache opens no longer require a home directory (item 4).**
  `ifcfast.open(path, use_cache=False, write_cache=False)` resolves the
  cache root (`Path.home()`) lazily, only when a read/write happens. The
  no-cache flags now propagate to the lazy data layers
  (`m.psets`/`m.quantities`/…), so accessing them on such a model no
  longer raises `RuntimeError: Could not determine home directory` in a
  stripped CI container / sandboxed subprocess.
- **Duplicate STEP ids collapse last-wins (item 5).** A malformed file
  repeating a record id (`#30=IFCWALL(...)` twice) no longer yields
  duplicate-keyed product rows. The later declaration wins, `step_id`
  stays a unique key, and `m.summary()["duplicate_step_ids"]` counts the
  collapsed rows (0 on a well-formed file). The count persists across the
  parquet cache.
- **Empty tables report canonical dtypes (item 6).** A model with no
  quantities / no geometry no longer reports `schemas["quantities"]` /
  `schemas["drift"]` columns as all-`float64`. Empty `psets`/
  `quantities`/`materials`/`classifications`/`drift`/`segments` layers
  now carry their canonical per-column dtypes, on build and on cache
  read.
- **`spaces_df` carries name + storey (item 7).** New columns `name`,
  `storey_guid`, `storey_name` joined from the `products` table (IfcSpace
  is a mode-filtered product). `summary()` / `schemas` advertise the
  enriched column set.
- **Multi-member ifczip warns (item 8).** A ZIP container with more than
  one STEP member emits a `warnings.warn` naming the chosen (largest) and
  ignored members instead of silently using one with no trace.

### Fixed — header decode + no-drift cache gate (GH #87, leftovers from #84)

- **`ifcfast.header()` no longer decodes with `errors="replace"`.** The
  Tier-0 STEP-header parser turned raw cp1252 æøå in `FILE_NAME` author /
  organization into U+FFFD silently, and passed `\X2\…\X0\` escapes
  through verbatim. It now decodes strict-UTF-8 → lossless cp1252 /
  latin-1 (never U+FFFD), sets a new `IFCHeader.encoding_lossy` flag on
  the fallback, and resolves STEP string escapes (`\X2\` UTF-16BE,
  `\X\HH` ISO-8859-1, `\S\C` Latin-1 short form) — mirroring the
  entity-string fix (#77) on this separate Python header path. Tier-0
  header fields only; **no cache-schema bump** (cached parquet columns
  and the cache key are unchanged).
- **No-mesh builds now satisfy the data-layer cache gate.** On a `_core`
  built without the `mesh` feature, `analyse_drift` is absent so the
  `drift` / `segments` layers never cached — `all_cached` stayed `False`
  forever and the four good data layers (`psets`, `quantities`,
  `materials`, `classifications`) re-extracted on every process, silently
  defeating the documented `<200 ms` hot-reload contract. The manifest
  now records `drift_unavailable: true` on such builds and the gate
  excludes drift, so the four layers serve from cache. A standalone
  `ifcfast extract` (drift not requested) does not set the flag, so a
  later drift-wanting reader still cold-parses drift. Composes with the
  #80 `_source_matches` freshness check + atomic writes (gate logic only;
  no on-disk layout change).

### Fixed — Rust lexer/extractor escape + set-value correctness batch (GH #76)

- **Cache schema bumped `19` → `20`.** Six small Rust-core correctness
  items from the extractor review. Each changes extracted *values* or
  adds previously-dropped *rows* for affected files, so such cached
  substrates must be re-extracted; files without these constructs are
  byte-identical.
- **(1) Encoded literal backslash `\\` collapses to one `\`.** STEP
  encodes a literal backslash as `\\`. `decode_string` had no rule for
  it, so `'C:\\path'` decoded to `C:\\path` (doubled), and a literal
  backslash immediately before `X2`/`S`/`X` text could be misread as the
  start of a Unicode/Latin-1 escape. The `\\` pair is now consumed first,
  emitting exactly one `\` and never acting as an escape introducer.
- **(2) `\X4\…\X0\` non-BMP escapes decode.** The ed.3 eight-hex-per-
  code-point escape (emoji / supplementary-plane CJK) was passed through
  as literal text. It now decodes the same way `\X2\` does, with invalid
  scalar values mapped to U+FFFD. `A\X4\0001F600\X0\B` → `A😀B`.
- **(3) `\X2\` unpaired surrogate no longer drops the whole run.**
  `String::from_utf16` Err'd on a malformed surrogate and pushed nothing,
  silently dropping every valid unit in the same `\X2\` body. Now uses
  `from_utf16_lossy` (U+FFFD substitution), matching ifcopenshell's
  best-effort behaviour; surrounding text survives.
- **(4) Dangling duplicate `IfcSIUnit` no longer clobbers the project
  default.** `extractors/quantities.rs` kept one SIUnit per `UnitType`
  with last-write-wins *before* the `IfcUnitAssignment`-membership
  filter. A same-type SIUnit declared after the assigned one (e.g. nested
  in an unresolved `IfcConversionBasedUnit`) overwrote it, then the
  membership filter dropped the survivor — leaving `unit_step_id` null
  where the assigned unit's id was expected. Every candidate per type is
  now kept; the post-pass picks the assignment-backed one regardless of
  declaration order. Adjacent to the GH #43 fix.
- **(5) Set-valued `RelatingPropertyDefinition` honoured.** An IFC4
  `IfcRelDefinesByProperties` whose `RelatingPropertyDefinition` is an
  `IfcPropertySetDefinitionSet` (inline list `((#1,#2))` or typed wrapper
  `IFCPROPERTYSETDEFINITIONSET((#1,#2))`) hit `_ => continue` and dropped
  the whole relation. Both `extractors/psets.rs` and
  `extractors/quantities.rs` now accept ref / list / typed-wrapper —
  the same list-or-ref tolerance already used for `RelatedObjects` —
  and bind every member set. Single-ref form unchanged.
- **(6) `IfcPhysicalComplexQuantity` members surface.** A complex
  quantity bundling nested simple quantities was dropped silently
  (nested `Width`/`Height` vanished). It now flattens into one row per
  nested member with a dot-joined name (`Profile.Width`), mirroring the
  `IfcComplexProperty` handling in `extractors/psets.rs`. Depth-capped
  (8).
- Items 1, 2, 4, 5, 6 verified on the 0.4.36 wheel by the tester; item 3
  was traced in source. Smoke-tested on `G55_ARK.ifc` (97k pset rows,
  Norwegian escapes decode clean, no raw escape text leaks). Adds Rust
  unit tests per sub-item in `lexer.rs`, `extractors/psets.rs`,
  `extractors/quantities.rs`.

### Fixed — truncation refusal moved into the Rust core (GH #89)

- **No cache-schema bump.** This is a behavioural open-time guard, not a
  change to any cached column's shape or meaning — caches stay valid.
- The truncated-file guard (refuse a STEP file missing its
  `END-ISO-10303-21;` trailer, added in GH #70) lived only in the Python
  `header()` wrapper. Any path that reached `_core.*` without going
  through `header()` — and every Rust binary (`ifcfast-bundle`,
  `ifcfast-mesh`, `ifcfast-clash`) — scanned records to EOF with no
  trailer check, so a truncated uncompressed IFC yielded a *partial*
  substrate at exit 0, feeding wrong QTO / clash / diff.
- The check now lives in `crates/core/src/source.rs::open`, the single
  choke-point every `_core.*` entry and every Rust binary funnels
  through. A plain-STEP buffer whose final 256 bytes lack
  `END-ISO-10303-21` is rejected with `InvalidData` before any records
  are scanned; the Python `header()` wrapper still runs its own check
  first, so callers see exactly one error, never a double-refusal.
  `.ifczip` inputs are exempt (a truncated archive already fails ZIP's
  own central-directory check in `decompress_ifczip`). AGENTS.md's
  "refused at open" contract is now true at the core, not just the
  Python skin.

## [0.4.38] - 2026-06-14

> Correctness + security batch (10 issues + a pyo3 security bump). Parser:
> STEP framing is now comment/string-aware (#72) and raw-UTF-8 strings no
> longer mojibake (#77). Indexer/extractors: bare `IfcTypeProduct`/
> `IfcTypeObject` types are captured (#69), IFC4 `IfcDoor`/`IfcWindow`
> `predefined_type` is correct (#74), hierarchical classification chains
> resolve `system_name`/`edition`/`source` (#75). Units: imperial
> `IfcConversionBasedUnit` files resolve `unit_scale` (#73). Classifier:
> IFC4X3 built elements no longer `skip` (#82). Python: `by_type` expands
> subtypes (#81); the parquet cache validates source freshness + writes
> atomically (#80); mesh no-cut surfaces strip synthetic half-space cutter
> slabs (#66). Security: pyo3 `0.24` → `0.29` clears RUSTSEC-2026-0176 /
> -0177. **Cache schema v16 → v19 — re-extraction required** for affected
> files (imperial units, raw-UTF-8 strings, IFC4 door/window
> predefined_type, hierarchical classifications, bare type objects,
> comment/string-laden STEP, clipped-product drift/segments).

### Fixed — bare `IfcTypeProduct` / `IfcTypeObject` dropped (GH #69)

- **Cache schema bumped `18` → `19`.** Bare type base classes are now
  captured. The indexer's `EntityKind::TypeObject` classifier and the
  two `is_type_object` membership tests (`extractors/psets.rs`,
  `extractors/quantities.rs`) only accepted `*Type`-suffixed names plus
  the IFC2x3 `IfcDoorStyle` / `IfcWindowStyle` exceptions. The
  non-abstract base classes `IfcTypeProduct` / `IfcTypeObject` end in
  `PRODUCT` / `OBJECT`, fell through every check, and were skipped — so:
  - the type never appeared in `m.type_objects_df` /
    `type_objects.parquet`;
  - its occurrences carried `type_guid = None` (name-only fallback)
    even with an explicit `IfcRelDefinesByType`;
  - any type-level psets / quantities silently dropped (the GH #36 /
    #45 silent-drop class, resurfacing through the membership filter).
- **Why it matters:** Revit emits these base classes for types that
  have no schema-specific subtype — e.g. roof / stair / ramp types on
  IFC2X3, which has no `IfcRoofType`. Real export path, not a schema
  curiosity. On `G55_ARK` (IFC2X3, Revit): 11 bare `IfcTypeProduct`
  types now visible; 33 Roof / Stair / Ramp occurrences gain their
  `type_guid`.
- **Fail-loud:** the three membership rules now also match
  `IfcTypeProduct` / `IfcTypeObject`; the proper-cased entity name map
  spells both out so the `type_objects` entity column is correctly
  cased without relying on the consumer fold map.

### Fixed — STEP section/record framing is now comment- and string-aware (GH #72)

Three silent wrong-output bugs in the DATA-section scanner, one root
class: framing did not treat ISO-10303-21 `/* */` comments or
single-quoted strings as inert when locating section boundaries and
record terminators.

- **A `/* */` comment between records dropped everything after it.**
  `for_each_record` broke the record walk on the first non-`#` byte, so
  a comment (`/* exported by FooCAD */`) sitting between two records
  ended the DATA scan early. It now skips whitespace **and** `/* */`
  comments between records, and a comment containing `;`/`'` inside a
  record no longer desyncs `find_record_end`.
- **`ENDSEC` inside a quoted value truncated the section.**
  `endsec_position` was a raw substring search, so a wall named
  `'SEE ENDSEC FOR DETAILS'` ended DATA at the wrong place and dropped
  the matching record plus everything after it. The scan now skips
  quoted strings and comments (SIMD `memchr3` fast path preserved — no
  throughput regression on the full-section scan).
- **`DATA;` inside a HEADER string started the section early →
  0 products.** `data_section_start` checked only the preceding byte, so
  `FILE_DESCRIPTION(('Bridge DATA; rev2'),'2;1')` was mistaken for the
  section marker, emptying the parse. It now skips header strings and
  comments and matches `DATA` only as a bare token.

Files using comments between records, or containing the literal
`ENDSEC`/`DATA;` inside a string, now parse the previously-dropped
entities. `_CACHE_SCHEMA_VERSION` bumped **18 → 19** (value change for
affected files; clean files are byte-identical → cache hits unchanged).

### Fixed — hierarchical classification chains lose system_name/edition/source (GH #75)

- **`m.classifications` now walks the full `ReferencedSource` chain.** A
  leaf `IfcClassificationReference` whose `ReferencedSource` points at a
  parent *reference* (rather than directly at the `IfcClassification`) —
  the multi-level hierarchy ArchiCAD/Solibri NS 3451 and Uniclass
  exports produce (leaf → group → table → `IfcClassification`) — was
  only resolved one hop. The terminal `IfcClassification` was never
  reached, so `system_name` / `edition` / `source` came back `None` and
  the entire hierarchy-exported population was invisible to consumers
  grouping by `system_name` (the NS 3451 use case). The extractor now
  follows parent references to the terminal `IfcClassification`,
  depth-capped (32) and cycle-guarded (a self/loop reference yields
  `None` system fields rather than hanging). `identification` / `name` /
  `location` still come from the leaf reference.
- **Cache schema `v18 → v19`.** Values change for files with
  hierarchical classifications, so their cached substrates re-extract;
  flat (single-hop) classifications are byte-identical. `AGENTS.md`
  updated.

### Security — bump pyo3 to 0.29 (RUSTSEC-2026-0176 / -0177)

- **`pyo3` bumped `0.24` → `0.29`** (with `pyo3-ffi`,
  `pyo3-build-config`, `pyo3-macros`, `pyo3-macros-backend` moving in
  lockstep) to clear two RustSec advisories that `cargo audit` flagged
  on every branch (GH #51):
  - **RUSTSEC-2026-0176** — out-of-bounds read in `PyList` / `PyTuple`
    iterators.
  - **RUSTSEC-2026-0177** — missing `Sync` bound (soundness).
  - Both fixed in pyo3 `>= 0.29.0`.
- **No Python API change.** Pure dependency bump; the
  `extension-module` + `abi3-py310` features and the `ifcfast._core`
  surface are identical. The only source churn is the 0.26 rename
  `Python::allow_threads` → `Python::detach` (21 GIL-release call
  sites in `crates/core/src/lib.rs`). No cache-schema / column-shape
  change.

### Fixed — parquet cache integrity: stale-on-same-size-edit + non-atomic writes (GH #80)

- **Same-size mid-file edits no longer serve a stale model.** The cache
  key only hashes `(schema_version, size, head 4 MB, tail 4 MB)`, so an
  in-place edit confined to the middle of a >8 MB file (e.g. a tool
  rewriting a value/name without changing byte length) kept the same key
  and the old substrate was returned with no error. The manifest already
  recorded `(size_bytes, mtime_ns)` but no read path compared it. Every
  cache read (`is_index_cached`, `read_index`, and the lazy data-layer
  reader) now validates the manifest's `(size, mtime_ns)` against the
  live file stat; a mismatch is a hard miss → re-parse. `mtime_ns` is
  deliberately kept out of the *key* so a plain copy re-validates after
  one re-hash instead of a full re-parse.
- **Cache writes are now atomic.** `write_index` / the data-layer writer
  wrote each parquet (and the manifest) in place with no temp+rename, so
  a crash mid-write could leave a truncated/empty parquet behind a
  valid-looking manifest — read back as a (wrong) cache hit, with the
  Model fabricating empty relationship DataFrames forever. Parquets and
  the manifest now write to a sibling `.tmp` + `os.replace` (atomic on
  POSIX and Windows, same filesystem). An index is only honoured when its
  manifest carries `has_index` AND `index.parquet` is present and
  non-empty; a relationship table the manifest flags as written but that
  is missing on disk is treated as corruption → clean re-parse, never
  silent empty edges.
- **No cache-schema bump.** This is a cache-*validation* change, not a
  cache-*key* change: the key formula is unchanged, so existing caches
  are not orphaned. Manifests written before this change lack the
  `(size, mtime_ns)` comparison data the new check requires, so they
  re-validate as a miss exactly once and are rewritten — the safe
  fail-loud direction.

### Fixed — IFC4X3 built elements no longer classify as `skip` (GH #82)

- **`IfcBuiltElement` rename handled.** IFC4X3 renamed the bulk-element
  supertype `IfcBuildingElement` → `IfcBuiltElement`. The classifier's
  inheritance walk (`python/ifcfast/classify.py`) only checked for
  `IfcBuildingElement`, so every IFC4X3-only built element that chains
  through `IfcBuiltElement` — `IfcKerb`, `IfcPavement`, `IfcCourse`, … —
  and isn't in the hardcoded `MEASURE_ENTITIES` set fell through to
  `mode='skip'`, silently dropping out of `mode='measure'` take-offs.
  The walk now treats `IfcBuiltElement` and `IfcBuildingElement` as
  equivalent.
- **Addendum / technical-corrigendum schema suffixes resolve.**
  `_resolve_schema` now strips `_ADD2` / `_TC1` / etc. by longest-prefix
  match against the known schema keys, so `FILE_SCHEMA(('IFC4X3_ADD2'))`
  resolves to `IFC4X3` (not `IFC4`) and `IFC4_ADD2` to `IFC4`. Before
  this, any suffixed schema header missed the supertype lookup entirely
  and the *whole* inheritance fallback went dead — every non-hardcoded
  entity classified as `skip`. `schema == 'UNKNOWN'` (and the empty
  string) now resolve to "unset" so the caller's default schema applies,
  instead of being treated as a real, unknown schema.
- IFC4 / IFC2X3 classification is unchanged. No `_core` rebuild required
  (pure-Python classifier + static supertype map). The shipped
  `data/schema_supertypes.py` already carried the IFC4X3 entities and the
  `IfcWall → IfcBuiltElement` mapping, so no regeneration was needed.

### Fixed — imperial files now resolve `unit_scale` (GH #73)

- **`IfcConversionBasedUnit` is now resolved for LENGTHUNIT.** Imperial
  (foot/inch) files declare length via `IfcConversionBasedUnit`, never
  `IfcSIUnit` — but unit resolution in `crates/core/src/indexer.rs`
  only ever read the SI path, so `unit_scale` stayed `None` and
  `length_unit` reported `'m'` on US/UK exports: a silent 3.28× error
  on every derived length / area / volume (mesh QTO, clash tolerances,
  `layer_thickness_mm`, parquet `ifcfast.unit_scale` metadata). The
  parser now dispatches `IfcConversionBasedUnit` →
  `ConversionFactor` → `IfcMeasureWithUnit(value, si_base)` and derives
  `value × si_base_scale` (FOOT → `0.3048`, INCH → `0.0254`). The dead
  `"FOOT"` / `"INCH"` match arms on the SI path (never reachable —
  not legal `IfcSIUnitName` values) are removed.
- **Fail-loud on unresolvable units.** When a file declares a LENGTHUNIT
  whose conversion chain is missing/malformed, `unit_scale` stays
  `None` (consumers see "unknown") and the parser emits a loud
  `[ifcfast] WARNING` to stderr rather than silently implying metres.
  No cache-schema/column-shape change — `unit_scale` was already a
  manifest field; this corrects its *value* on imperial files.

> Broken-mesh class fix (GH #66 / #94): synthetic half-space cutter
> slabs no longer masquerade as element geometry on any no-cut
> surface. Cache schema v17 (cached drift/segments from v16 hold
> foreign-extent values for clipped products).

### Fixed — IFC4 IfcDoor/IfcWindow `predefined_type` (GH #74)

- **`predefined_type` for IFC4 `IfcDoor` / `IfcWindow` was the WRONG
  enum.** The indexer found `predefined_type` by walking attributes
  from the right and taking the first trailing enum. But IFC4 `IfcDoor`
  / `IfcWindow` (and their `*StandardCase` subtypes) carry TWO trailing
  enums — `PredefinedType` then `OperationType`/`PartitioningType` —
  plus a trailing `UserDefined…` string. The walk grabbed
  `OperationType`/`PartitioningType` (e.g. `SINGLE_SWING_LEFT` instead
  of `DOOR`), and when the `UserDefined…` string was set it stopped on
  the string and returned `None` even though `PredefinedType` was
  `.USERDEFINED.`. Revit IFC4 exports set both enums on essentially
  every door/window, so anything filtering or aggregating on
  `predefined_type` got garbage. `predefined_type` is now read
  positionally (third-from-last attribute) for these entities and
  `USERDEFINED` is preserved. **Cache schema v18** (cached door/window
  substrates from ≤v17 hold the wrong value → re-bundle).
- **IFC2X3 unaffected:** `IfcDoor` / `IfcWindow` have no
  `PredefinedType` in IFC2X3 and continue to return `None`; the new
  positional path only runs for IFC4 door/window entities.

### Fixed — raw UTF-8 strings no longer mojibaked (GH #77)

- **`decode_string` now decodes raw UTF-8 STEP strings correctly.** The
  lexer forced Latin-1 on every byte ≥ 0x80, so exporters that write raw
  UTF-8 `æøå`/CJK directly (Bonsai/BlenderBIM, some ArchiCAD/Tekla) came
  back mojibaked — a wall named `Dør-æå` returned as `DÃ¸r-Ã¦Ã¥`, and the
  garbage propagated into names, psets, materials, classifications and
  diff keys. Un-escaped high-byte runs are now UTF-8-decoded first, with
  per-byte Latin-1 only as the deterministic fallback for byte runs that
  are not valid UTF-8 (legacy Latin-1 files are unaffected). STEP escapes
  (`\X\HH`, `\X2\HHHH…\X0\`, `\S\C`) and ASCII are byte-for-byte
  unchanged. Scoped to the encoding bug; the escape-batch and framing
  items (GH #76, #72) are separate.
- Cache schema **v18** — string values change for any raw-UTF-8 source
  file; caches written by ≤ v17 wheels carry the old mojibake and
  re-extract.

### Changed — `by_type` expands subtypes by default (GH #81)

- **`Model.by_type(entity)` now mirrors
  `ifcopenshell.file.by_type(type, include_subtypes=True)`.** It
  expands subtypes by default and matches the entity name
  case-insensitively, where it previously did an exact, case-sensitive
  match. On a typical Revit IFC2x3 export `by_type("IfcWall")` now
  returns the `IfcWall` **and** `IfcWallStandardCase` instances (was:
  just the bare `IfcWall`), and the very common `by_type("IfcElement")`
  / `by_type("IfcProduct")` idioms return all element/product subtypes
  present instead of `[]`.
- **New `include_subtypes` keyword** (default `True`, matching
  ifcopenshell): pass `include_subtypes=False` for an exact
  single-entity match (still case-insensitive on the name).
- Expansion resolves against the static per-schema supertype map
  already shipped with the wheel (`data/schema_supertypes.py`) — **no
  runtime ifcopenshell dependency.** Counts remain over the
  meshable-product substrate, so abstract supertypes resolve to the
  concrete products the model carries (e.g. `IfcProduct` excludes
  non-meshable products such as `IfcSpace`). Unknown entity names still
  raise `ValueError` (GH #71).
- New `subtypes_of` / `canonical_entity_any_schema` helpers in
  `ifcfast.classify`.

### Fixed — synthetic half-space stand-ins stripped from no-cut output (GH #66)

- **`iter_meshes`/`meshes` no longer return giant degenerate planes
  for clipped products.** An infinite `IfcHalfSpaceSolid` cutter is
  tessellated as a ±20 000-model-unit visualisation slab; the
  reveal-all path emitted it inside the product mesh, so a 7 m floor
  strip arrived as three boxes spanning 54 m (GH #66 — `iter_meshes`
  wrong, `mesh_qto` correct, because only the cut path consumed the
  cutters). A new `strip_synthetic_cutters` pass removes fragments
  tagged `boolean_second_operand` + `halfspace_plane*`/
  `halfspace_bounded*` — with vertex compaction, segment re-indexing
  and `parts` bookkeeping — on every surface where the cut does not
  run: `extract_meshes`, `mesh_qto(cut_openings=False)`, both
  point-cloud sinks, `to_gltf`, `analyse_drift` (and therefore the
  `drift` + `segments` tables), and the **bundle substrate** (26
  contaminated representations measured on a real architectural
  model were feeding `clash()` false positives and poisoning
  instance bboxes).
- **Scope discipline:** authored solid subtractors
  (`boolean_second_operand|extrusion` — a void modelled as a real
  solid) still emit verbatim; `boolean_union_operand` /
  intersection operands untouched; `cut_openings=True` unchanged.

### Added

- **`keep_cutters=True`** on `meshes()` / `iter_meshes()` /
  `_core.extract_meshes` — explicit reveal-all opt-in that restores
  the synthetic cutter slabs (debugging cut placement). The
  `extract_meshes` stats dict now reports **`cutters_stripped`** and
  echoes `keep_cutters`.
- Cache schema **v17** — value change in drift/segments without a
  column change; v16 caches re-extract.

## [0.4.37] - 2026-06-10

> Agent-first contract hardening (Python layer only — no Rust core
> changes, no cache-schema bump). Wrong-answer fixes for diff and the
> spatial graph, loud failures where typos and truncation used to read
> as empty/partial results, and MCP data access. GH #68, #70, #71,
> #78, #79, #83, #84.

### Fixed

- **`Model.diff` no longer reports false changes when the two sides
  are in different cache states (GH #68).** A cold-parsed Model
  materialises missing fields as `None`, a cache-hit Model as pandas
  `NaN`; `diff()` compared them as unequal and flagged ~99% of
  products as changed on a one-element edit. Missing values are now
  one equivalence class, canonicalised at the comparison boundary.
  The MCP `diff` tool inherits the fix.
- **`storey_of()` resolves through aggregate hops in models without an
  `IfcBuilding` (GH #79).** "Is this container a storey?" was decided
  by the storey having a building edge (plus a dead-code disjunct
  keyed on the wrong guid); it now checks a storey-guid set seeded
  from the storeys table, so storeys aggregated directly under
  `IfcSite` (infra/landscape exports) resolve like any other.
- **`products_in(storey)` includes aggregate sub-products (GH #78).**
  The storey fast path returned only directly-contained elements,
  dropping parts reached via `IfcRelAggregates` (curtain-wall plates,
  stair flights) that `products_in(building)` included — the same
  query now BFS-walks at every level, so per-storey results sum
  exactly to the building result.
- **`Model.diff` accepts `pathlib.Path` (GH #84).**
- **CLI: bad-content errors print cleanly (GH #84).** `ValueError`
  (not a STEP file, truncated, no-IFC archive) and `BadZipFile` now
  exit 1 with a one-line `ifcfast: …` message instead of a traceback,
  matching the GH #42 treatment of path errors.

### Changed — loud failures (GH #70, #71)

- **Truncated / unterminated STEP files are refused at open — and at
  `bundle()`.** A file whose tail lacks the `END-ISO-10303-21;`
  trailer raises `ValueError` instead of silently parsing to a
  partial model (a 90% truncation used to return 8 687 of 8 873
  products with no diagnostic). `bundle()` / `ifcfast bundle` route
  through the same guard, so a truncated IFC can no longer stream a
  silently-partial clash substrate (PR #85 review). ZIP containers
  are exempt — ZIP's own integrity check covers them.
- **`preview()` raises on unknown table names**, listing the valid
  tables; **`filter()` / `by_type()` raise on entity names no IFC
  schema defines and on unknown modes** — at call time, not first
  iteration. A typo no longer reads as "the model has none of these";
  a valid-but-absent entity still returns empty. Validation is
  against the new `ALL_ENTITIES` vocabulary (every entity declaration
  in IFC2X3/IFC4/IFC4X3, **including supertype-less roots** like
  `IfcPerson` / `IfcGridAxis` — PR #85 review F1), emitted by
  `scripts/gen_schema_supertypes.py`.
- **`by_type` docstring no longer claims ifcopenshell parity** — it
  documents exact-match semantics and points at GH #81 (subtype
  expansion) for the gap.
- **Namespace hygiene (GH #71):** `ifcfast.Path` and
  `ifcfast.annotations` no longer leak from `ifcfast.__init__`.

### Added — MCP data access (GH #83 + product-review batch)

- **New MCP tools `psets`, `quantities`, `materials`** — filtered
  long-format rows (by `guid` / set name / property name, capped by
  `limit`, default 200), so "what's the FireRating of this door?" is
  answerable over MCP at all.
- **New MCP tool `product_card(path, guid, limit=200)`** — one
  element's product row + psets + quantities + materials +
  classifications + resolved storey/building/ancestors in a single
  round-trip. Sub-tables cap at `limit`; a bitten cap is labelled in
  the response's `truncated` field (`{table: total}`), never silent
  (PR #85 review).
- **MCP model cache is staleness-checked (GH #83).** Every tool call
  stats the file and reopens transparently when size/mtime changed
  (re-export from the authoring tool); the docstring no longer claims
  an LRU that didn't exist.

## [0.4.36] - 2026-06-08

> Regression hotfix for the v0.4.35 half-space cut over-report on
> millimetre-unit models (GH #65), bundled with the GH #64 W6
> cut-openings hardening. Default builds: net cut meshes / `mesh_qto`
> volumes change only for **non-metre** files (corrected toward
> ifcopenshell ground truth); metre files are byte-identical.

### Fixed — half-space cut over-reported volume on mm-unit models (GH #65)

- **The half-space clip's "on-plane" tolerance is a numerical round-off
  guard in source units again — not a physical 1 mm.** v0.4.35's W3
  change resolved the guard as `1 mm / unit_scale`, which is `1.0`
  source units in a millimetre file — coarse enough to classify
  near-plane faces as "outside" and drop them *without a replacement
  cap*, leaving an open shell whose `mesh_qto` volume over-integrated.
  This re-opened the #39 half-space over-report on every mm-unit model
  (Sannergata ARK_E walls +6 %…+136 %, incl. the original #39 headline
  wall `2Nf9lR2y`). The guard is back to `1e-3` source units
  (metre / mm / foot resolve identically; km-scale files tighten so the
  band stays sub-millimetre). Sannergata ARK_E vs ifcopenshell 0.8.5:
  383/389 walls within ±1 % (98.5 %, up from v0.4.34's 97.4 %),
  over-reporters 6 → 1, `mesh_quality` `open_shell` → `closed` on the
  fixed walls; metre files byte-identical. **Reverses the v0.4.35
  "unit-aware cut tolerance" change below.**

### Fixed — W6 bounded-halfspace cut-openings hardening (GH #64)

- **Multi-cutter / rotated-boundary / mirror-safe** corrections to the
  `prism-csg-fast` bounded-halfspace fast-path: reduce the host to a
  prism once and evaluate every carried payload against the original
  (region-decomposition into footprint-disjoint prisms — no coincident
  internal caps); polygon-containment boundary-tightness test (replaces
  the axis-aligned bbox compare); payload-owned exact cutting planes
  (retires the slab-centroid plane + the float-tolerance plane match);
  inverse-transpose normal baking for mirrored/negative-determinant
  families. Polygon-bounded correctness + the #61 build-gating fix also
  corrected the default path on two Sannergata walls (`2tg5mWEt`,
  `3qxjXn6G`).

## [0.4.35] - 2026-06-07

> Bundled CSG-foundation + QTO-reliability release. Cache schema → 16
> (re-extraction on first open). Note: the 0.4.32–0.4.34 releases shipped
> without changelog entries; this entry covers the changes since 0.4.34.

### Added — volume-reliability columns + prism fallback (GH #60)

- **`mesh_qto()` / `instances.parquet` now self-label volume
  confidence** so pipelines can route untrustworthy rows to an
  authoritative kernel. New columns: `volume_reliable` (bool routing
  flag), `volume_method` (`"mesh"` / `"prism_fallback"`),
  `volume_mesh_m3` (raw signed-tetra value, always), and
  `volume_prism_bound_m3` (`footprint × height` tight bound, `NaN` on
  closed rows). `mesh_quality` is now also exposed in `mesh_qto()`.
- **`volume_m3` is now the best estimate** — the mesh volume when
  trustworthy, else a prism fallback — so `SUM(volume_m3)` no longer
  mixes open-shell garbage into totals. A non-closed mesh keeps its mesh
  value when it's within its tight prism bound (the edge-pairing check
  over-flags dedup-imperfect-but-accurate shells); only volumes that
  provably exceed the bound are replaced. Validated on the G55 ACC files
  vs the Solibri ITO benchmark: flagged set 2.5 %, 0 regressions, and
  the `IfcFaceBasedSurfaceModel` slab goes from 1504 m³ garbage to
  180.1 m³ (= Solibri's QTO prism). See
  `examples/hybrid_qto_routing.py`.

### Added — cut-openings manifold-replacement programme, Phase 1 (GH #58)

- **W1+W2** — real source-chain encoding + `Outcome::Unsupported`
  taxonomy for the boolean/cut pipeline.
- **W3** — unit-aware cut tolerance: the half-space clip epsilon is now
  scaled by the file's `unit_scale` (physical 1 mm in any unit system),
  fixing silent misbehaviour on mm / imperial files.
- **W4** — operator-aware `IfcBooleanResult`: UNION / INTERSECTION
  operators are honoured instead of being treated as subtraction
  (previously ~2× volume errors on those products).
- **W5a** — property-based correctness harness
  (`tests/cut_openings_proptest.rs`), the validation gate for the
  pure-Rust CSG paths.
- **W9** (behind the opt-in `prism-csg-fast` Cargo feature) — pure-Rust
  prism-CSG through-cut fast path via an `i_overlay` 2D-boolean facade,
  including cross-product `IfcRelVoidsElement` primitives wired into the
  void-flush. Default builds are byte-identical; manifold remains the
  default kernel until the feature earns its benchmark/correctness gate.

### Fixed

- **Faceted-brep inner holes (GH #53)** — `mesh::brep::mesh_face` now
  honours inner `IfcFaceBound` holes (Newell projection + ear-clipping)
  instead of fan-filling the outer loop, fixing over-reported solid
  volume (+6 %…+122 %) on Revit-exported walls with punched openings.
- **pyo3 0.22 → 0.24 (GH #51)** — resolves RUSTSEC-2025-0020; CI green
  under `-D warnings`.

## [0.4.31] - 2026-06-03

### Fixed — `__version__` single source of truth (GH #46)

- **`ifcfast.__version__` now reads from the installed package
  metadata** via `importlib.metadata`, not a hand-bumped string in
  `__init__.py`. The version string lived in four files
  (`pyproject.toml`, `Cargo.toml`, `crates/core/Cargo.toml`,
  `python/ifcfast/__init__.py`) and silently drifted out of sync
  across releases — every release required four manual edits, one
  of which got forgotten or stale. Two of those four now collapse:
  `crates/core/Cargo.toml` inherits `version` / `edition` /
  `license` / `repository` from `[workspace.package]`, and
  `__init__.py` reads from `importlib.metadata`. A release now
  needs two coordinated bumps: `pyproject.toml` (wheel side) and
  `[workspace.package].version` (Rust side). Pinned by
  `tests/test_smoke.py::test_version_matches_installed_metadata`.

### Fixed — `world_coordinate_baked` detector rewrite (GH #33)

- **`m.world_coordinate_baked` is now symptom-based, not
  cause-based.** The v0.4.27 detector required ≥80% of meshed
  products to have placement within 1 mm of world origin — a
  Tekla-specific guess at the underlying authoring style. It
  missed every other baked-coords variant: building-origin-anchored
  placements with geometry authored further out, prefab-heavy
  structural files, and mixed-baked exports like G55_RIB
  (Ed's tester-in-chief re-verify on 2026-06-02 showed 382/896
  `error` rows still flagged after v0.4.27 shipped). The new
  detector trips when ≥25% of meshed products would carry
  `drift_severity == "error"` under the per-row rule (file must
  have ≥20 meshed products to qualify). When tripped, every
  `error` and `warn` row demotes to `info`. Raw `drift_distance_m`
  and `drift_ratio` columns are unchanged — the demotion is
  cosmetic on the severity column, the underlying signal is
  intact for analysts who want it.
- **`ifcfast drift` banner rewritten to match the new semantic.**
  No longer claims "origin-placed products demoted"; explains the
  model-level pattern, names the common authoring styles that
  produce it, and points at the raw drift columns as the un-
  demoted signal.
- **`ifcfast drift --top` widens past `error`.** In a baked-
  pattern model all interesting rows are `info`; the top-N now
  ranks any non-`ok` row by `drift_distance_m` so the worst
  cases stay visible.

### Schema

- `_CACHE_SCHEMA_VERSION` 11 → 12. Old caches re-extract on next
  open; severity counts shift on baked-pattern models, raw drift
  columns are byte-identical.

## [0.4.30] - 2026-06-02

### Fixed — IfcArcIndex tessellation in IfcIndexedPolyCurve (GH #48)

- **`IfcArcIndex` segments inside `IfcIndexedPolyCurve` are now
  curve-sampled, not chorded.** Old behavior treated every
  3-index arc tuple as a straight chord between the first and
  third indexed points, collapsing Revit MEP pipes / ducts
  authored via `IfcArbitraryProfileDef(WithVoids)` to square
  prisms. Cross-validation on G55_RIV vs ifcopenshell:
  `IfcPipeSegment` volume-ratio 0.21 → 1.003 (-79% error → <1%),
  `IfcDuctSegment` 0.997. New shared module
  `mesh::indexed_curve` exposes 2D and 3D arc evaluators
  (32-segment-per-full-circle budget, matching
  `IfcCircleProfileDef`), wired into `profile.rs`,
  `curveset.rs`, and `boolean.rs`. CCW orientation forced on
  output — Revit MEP authors CW and the earcut + extrusion
  pipeline silently inverts cap triangulation otherwise (volume
  drops to 1/3 with correct arc geometry).

### Schema

- `_CACHE_SCHEMA_VERSION` 10 → 11. Old caches re-extract on next
  open on any RIV / HVAC / MEP model.

## [0.4.29] - 2026-06-02

### Fixed — pset / quantity extractor batch (GH #36, #38, #43, #45)

- **`m.psets` now inherits type-level properties (GH #36).** Properties
  carried on `IfcTypeObject.HasPropertySets` and bound via
  `IfcRelDefinesByType` surface on every related instance, tagged
  `source = "type"`. Instance-declared properties carry
  `source = "instance"` and shadow same-named type properties on
  collision (matches `ifcopenshell.util.element.get_psets(..., should_inherit=True)`).
  Real-world payoff verified on G55_RIB.ifc: 17 `IfcBuildingElementProxy`
  instances now show `Pset_ManufacturerTypeInformation.Manufacturer = 'Wurth'`
  that were entirely absent pre-fix. Same GUIDs and manufacturer
  values ifcopenshell returns. `m.psets` gains the `source` column.
- **`m.quantities.unit_step_id` falls back to the project's
  `IfcUnitAssignment` (GH #43).** When `IfcQuantity*.Unit` is null —
  the common Revit / ArchiCAD authoring pattern — the column resolves
  to the project's `IfcSIUnit` for the quantity's kind
  (`Length`→`LENGTHUNIT`, `Area`→`AREAUNIT`, `Volume`→`VOLUMEUNIT`,
  `Weight`→`MASSUNIT`, `Time`→`TIMEUNIT`; `Count` stays null —
  dimensionless). Explicit per-quantity `Unit` refs still win.
  `IfcConversionBasedUnit` and `IfcDerivedUnit` resolution stay out
  of scope (separate feature). Verified on A4_RIB_B.ifc (16,742 qty
  rows): pre-fix every `unit_step_id` was null; post-fix all four
  resolved kinds match the IfcSIUnit step_ids ifcopenshell returns.
- **`IfcPropertyTableValue` now surfaces as a row and unhandled
  property classes emit a marker (GH #38).** `IfcPropertyTableValue`
  parses to a single row with `value = "d1=>v1, d2=>v2, ..."`
  pairing DefiningValues + DefinedValues; `value_type` takes the
  DefinedValues axis type. Any `IfcSimpleProperty` subclass without
  a per-class parser (`IfcPropertyReferenceValue`, future `*Value`
  classes, authoring-tool customs) emits a marker row with
  `value = None` and `value_type = "unhandled:IFCXXX"`. Enumerate
  blind spots:
  ```python
  m.psets[m.psets.value_type.fillna("").str.startswith("unhandled:")]
  ```
- **`m.quantities` inherits type-attached `IfcElementQuantity`
  (GH #45).** Mirrors the GH #36 pset path: types can carry
  quantities the same way they carry psets because
  `IfcTypeObject.HasPropertySets` accepts any
  `IfcPropertySetDefinition` and `IfcElementQuantity` IS-A
  `IfcPropertySetDefinition`. Inherited rows carry `source = "type"`,
  deduped against instance ones on `(qto_name, quantity_name)`.
  GH #43 unit fallback composes through — inherited rows still get
  project-default `unit_step_id` resolution. Type-attached quantities
  are rare in practice (0 hits across 81 real files in the sweep)
  but the fix is principled and free at that cardinality.

### Schema

- `_CACHE_SCHEMA_VERSION` 6 → 10. Four bumps stacked across this
  release (one per behavior change). Old caches re-extract on next
  open.
- Substrate `instances.parquet` nested `psets` struct gains
  `source : string (not null)`.
- Substrate `instances.parquet` nested `quantities` struct gains
  `source : string (not null)`.
- `m.psets` / `m.quantities` DataFrames gain a `source` column
  with the same two values.

## [_pre-0.4.29 unreleased]

### Fixed — tester-in-chief cross-validation batch (GH #29–#35)

- **`m.products` is no longer silently empty on cache hits (GH #29).**
  The attribute used to be a list backing the cold-parse path and was
  left empty when the model came back from a parquet cache, so the
  README's `for p in m.products:` quickstart yielded nothing. Now a
  property: returns the materialised list either way, building it
  lazily from `m.products_df` on cache-hit Models. `len(m)` and
  `len(m.products)` agree. `iter(m)` also added — streams
  `ProductRow` without materialising the list.
- **CLI `ifcfast demo` no longer crashes on Windows cp1252 consoles
  (GH #30).** `_force_utf8_stdio()` at the CLI entry reconfigures
  stdout/stderr to UTF-8 so the em-dash and `→` glyphs in the pretty
  output don't trip `UnicodeEncodeError`. `--json` paths were
  already ASCII-safe; this only mattered for pretty mode and the
  argparse `--help` banner.
- **`m.drift` columns are SI-suffixed and the values match
  `m.mesh_qto()` (GH #31).** The Rust drift extract now applies the
  file's `unit_scale` before emitting, so columns are
  `drift_distance_m`, `max_extent_m`, `surface_area_m2`,
  `volume_abs_m3`, `aabb_volume_m3`, `placement_{x,y,z}_m`,
  `centroid_{x,y,z}_m`. Joining `m.drift` against `m.mesh_qto()` no
  longer needs an out-of-band rescale (used to be off by 10⁶–10⁹
  on mm-unit files with no signal in the column names). Cache schema
  v4 — old drift caches are rebuilt on next access.
- **`m.contained_in` captures every spatial-container kind, not
  just storey (GH #32).** The indexer no longer filters
  `IfcRelContainedInSpatialStructure` to storey edges. The
  DataFrame schema is now `(product_guid, container_guid,
  container_kind)` with `container_kind ∈ {site, building, storey,
  space}`. `m.parent(guid)` falls back to whichever spatial
  container the element sits in; `m.storey_of(guid)` walks through
  non-storey containers (e.g. an element contained in an
  `IfcSpace` resolves to the space's storey); `m.building_of(guid)`
  honours direct building containment in addition to
  storey-then-building. Cache schema v5. Adds
  `tests/fixtures/site_annotation.ifc` covering site- and
  building-level containment.
- **`drift_severity` no longer carpet-bombs world-coordinate-baked
  models (GH #33).** Per-row severity recomputed against SI values
  with a unit-independent 10 mm absolute threshold (the old `drift
  < 10.0` test was 10 mm on mm-files but 10 m on metre-files —
  over-strict in one direction, over-lenient in the other). New
  file-level detector: when ≥ 80 % of meshed products are placed
  at the origin within 1 mm, the file is flagged
  `m.world_coordinate_baked == True` and the per-row severity of
  origin-placed products is demoted to `info` so the file-level
  fact is the actionable signal instead of N "errors". Adds the
  `info` severity bucket; CLI `ifcfast drift` surfaces the flag.
- **README/AGENTS/system_prompt mismatch fixes (GH #34).** Softened
  the "NaN-not-None" invariant claim (materials/classifications
  carry Python `None` in object-dtype columns; `pd.isna()` catches
  both); documented `mesh_qto()` returning a `(products_df,
  surfaces_df)` tuple in both `system_prompt()` and the README;
  added an `iter(m)` row to the AGENTS.md decision tree; fleshed
  out the `cache.py` module docstring (missing `segments.parquet`,
  `voids.parquet`, `spaces.parquet`, `type_objects.parquet`).
  `Model.__iter__` ships as part of the GH #29 fix.
- **Quantity extractor now covered end-to-end against
  `ifcopenshell` ground truth (GH #35).** Real-world test set the
  tester used happened to lack `IfcElementQuantity` across all six
  files. Adds `tests/fixtures/quantities.ifc` exercising every
  `IfcQuantity*` subtype (Length, Area, Volume, Count, Weight,
  Time) via `IfcRelDefinesByProperties` and a cross-check that
  diffs `m.quantities` against `ifcopenshell.util.element.get_psets(
  el, qtos_only=True)`.

### Added

- **`m.meshes(cut_openings=True)` — net booleans on demand.** Opt-in
  CSG path that folds every `boolean_second_operand|...` mesh
  segment (the door / window void emitted alongside the host wall
  by the reveal-all pipeline) into the host via `manifold-csg`.
  Doors and windows render as actual holes instead of solid
  volumes-on-volumes. Default `cut_openings=False` preserves the
  reveal-all stance (both operands visible). The substrate stays
  reveal-all unconditionally — the flag only affects `m.meshes()` /
  `m.iter_meshes()` callers. Closes the viewer-integrator's P0 #1
  ask (GH #20). Requires a wheel built with the new `csg` Cargo
  feature; raises `RuntimeError` if the underlying wheel was
  compiled without it. Cross-product `IfcRelVoidsElement` openings
  (host wall + separately-modelled `IfcOpeningElement`, no boolean
  in the wall's own representation) are NOT cut by this path yet —
  a follow-on.
- New `csg` Cargo feature pulling in `manifold-csg = "0.2"` (Apache-2
  / MIT, f64 precision, Send-safe, cmake-built C++ core). Off in the
  default Python wheel until cross-platform wheel-build smoke
  testing lands. Build locally with
  `maturin develop --features csg`.

- **`ifcfast.clash()` — substrate-aware narrow-phase clash engine.**
  Reads `instances.parquet` + `representations.parquet` from a
  bundle directory, runs broad-phase AABB overlap (via the
  `geom::pairs_overlapping` kernel landed earlier in v0.4.19), then
  narrow-phases each candidate pair as a true mesh-mesh
  intersection / distance query against `parry3d` BVH-built
  TriMeshes. Writes `clashes.parquet` next to the inputs.
  ```python
  import ifcfast
  df = ifcfast.clash("model.bundle/", tolerance_m=0.05)
  ```
  Output columns: `ifc_id_a/b`, `guid_a/b`, `class_a/b`, `kind`
  (`"hard"` or `"clearance"`), `min_distance_m`. `tolerance_m` and
  `min_distance_m` are always in metres regardless of the source
  IFC's linear unit. The bundle now records the project's unit
  scale as parquet schema metadata (`ifcfast.unit_scale`) and the
  clash engine converts at load time. The engine is the *fact*
  layer ("do they touch, by how much, how far apart"); policy
  (connectivity dismissal, BCF emit, discipline routing) lives in
  the layer above and queries `clashes.parquet` joined to
  `instances.parquet`. See the "Narrow-phase clash" section in
  [`AGENTS.md`](AGENTS.md) for the worked DuckDB example.
- New `ifcfast-clash` binary mirroring the Python entry:
  `ifcfast-clash BUNDLE_DIR [--tolerance N] [--out file.parquet]`.
- `_core.clash(bundle_dir, tolerance_m, write_parquet, ...)` PyO3
  binding returning the same column-dict shape as the other
  extractors.
- `clash` Cargo feature stacking `bundle` + `geom`. The default
  Python wheel build now ships `bundle`, `geom`, and `clash` —
  previously it shipped only `python`, so the substrate writer
  and geom kernel only worked from a `cargo build --features
  bundle,geom` build. Agents shouldn't need to know about extras
  for first-class promises.
- **Geometric fingerprint columns on `instances.parquet`** — phase 1a
  of the federated-model clash-control / cross-discipline duplicate
  detection feature. Three new non-nullable columns on every instance
  row:
    * `centroid_xyz` — `FixedSizeList[Float32, 3]`, world-AABB
      midpoint when the product has mesh geometry, falling back to
      `placement_xyz` for geometryless products so location queries
      don't collapse no-body elements onto the world origin.
    * `vertex_count` — `UInt32`, world-baked mesh vertex count
      (zero when geometryless).
    * `triangle_count` — `UInt32`, world-baked mesh triangle count
      (zero when geometryless).
  Lets agents compose cross-model duplicate detection, version-diff,
  and broad-phase clash candidate filtering as pure DuckDB queries
  against the substrate (centroid distance + bbox overlap +
  complexity match), without re-running the parser or recomputing
  midpoints on every join. See the "Substrate output" section in
  [`AGENTS.md`](AGENTS.md) for the worked example.

### Changed

- `instances.parquet` and `representations.parquet` now carry
  `ifcfast.unit_scale` and `ifcfast.version` as parquet schema
  metadata. Backwards-compatible — readers that ignore the metadata
  see no change. The clash engine uses `unit_scale` to convert
  source-unit vertex / bbox data to metres at load time.
- `_CACHE_SCHEMA_VERSION` bumped 4 → 5. Existing caches become
  orphaned automatically — re-extract on next open is automatic.

## [0.4.0] - 2026-05-21

### Added

- **`Model.mesh_qto()`** — the geometric QTO engine is now reachable
  from the PyPI wheel, no cargo build required. Returns a tuple
  `(products_df, surfaces_df)`:
    * **products_df** — one row per meshed product. Columns:
      `guid, entity, volume_m3, aabb_volume_m3, surface_area_m2,
      area_top_m2, area_bottom_m2, area_side_m2, area_inclined_m2,
      largest_surface_m2, smallest_surface_m2, surface_count`.
    * **surfaces_df** — long-format, one row per distinct planar
      surface per product (sorted by area within a product).
      Columns: `guid, surface_index, area_m2, nx, ny, nz`. Normal-
      bucket aggregation at ~5.7° granularity collapses coplanar
      triangles into one surface; curved geometry resolves to one
      tessellation-wedge per row.
  All values are in m² / m³ regardless of source unit. The
  computation runs `mesh_ifc_streaming` once and emits both
  DataFrames in a single pass — no second walk, no intermediate
  Parquet round-trip. Authored `IfcElementQuantity` values stay in
  `m.quantities` and remain the gold-standard override when present.
- PyO3 binding `_core.mesh_qto(path)` returns the raw column dict
  for callers who want to skip the pandas wrapper.

### Changed

- The PyPI wheel now exposes the geometric QTO engine alongside the
  existing `analyse_drift` mesh path. This closes the gap from 0.3.0
  where the engine code shipped in the wheel but wasn't reachable
  from Python (only via the opt-in `ifcfast-bundle` binary).

## [0.3.0] - 2026-05-21

### Added

- **Per-product geometric QTO engine** in the bundle. One
  O(triangles) pass over the world-coord mesh during the streaming
  pass emits volume + surface area decomposed by face orientation +
  the full list of distinct planar surfaces per product. New columns
  on `instances.parquet`:
    * `volume_m3`, `aabb_volume_m3`
    * `surface_area_m2` total
    * `area_top_m2` / `area_bottom_m2` (triangles within 20° of ±Z)
    * `area_side_m2` (within 20° of horizontal plane)
    * `area_inclined_m2` (everything else)
    * `largest_surface_m2`, `smallest_surface_m2`, `surface_count`
    * `surfaces`: `List<Struct<area_m2, nx, ny, nz>>` — every
      distinct planar surface, normal-bucket aggregated at ~5.7°
      granularity, sorted by area descending. DuckDB UNNEST gives
      one row per face for "every surface on type X" queries.
  All values in m² / m³ regardless of source unit (mm → m via
  `unit_scale`). Authored `IfcElementQuantity` values stay in
  `quantities` as the gold-standard override; these geometric values
  are the truth that survives when authors omit `Qto_*` sets.
- Bundle output grew ~30 MB on the 27M-triangle ST28_RIV (834 MB
  IFC, 85,976 instances) for the per-surface list; compute pass +12%
  over the prior bundle pass; query latency against the materialized
  parquet is sub-15 ms for typical group-by-entity QTO queries on
  86K-row substrates.

### Changed

- Bundle `instances.parquet` schema gains 11 non-nullable columns
  (the QTO columns above). Strict-schema consumers expecting the
  v0.2.0 shape will need to update; permissive readers (DuckDB,
  pyarrow with column-projection) are unaffected.

## [0.2.0] - 2026-05-21

### Added

- **Streaming GeoParquet substrate writer (`ifcfast-bundle`).** New
  cargo feature `bundle` + binary `ifcfast-bundle <file.ifc> [out_dir]`
  emits a two-table substrate (`representations.parquet` +
  `instances.parquet`) in one streaming pass. Pairs geometry with full
  IFC semantics (psets, materials, quantities, classifications,
  storey, type) so the downstream analyser can join geometry to
  metadata without re-parsing the IFC. Working-set RAM bounded by the
  Parquet row-group buffer; the old `Vec<ProductMesh>` accumulator's
  OOM class is gone. DuckDB queries via the emitted `view.sql` join.
  Cargo feature is opt-in (default off); the Python wheel does not
  bundle the heavy arrow + parquet crates.
- **Hierarchical / instanced substrate layout.** The substrate now
  splits into `representations.parquet` (one row per unique mesh
  shape, keyed by `rep_id`) and `instances.parquet` (one row per
  `IfcProduct`, geometry-free except for a `rep_id` foreign key and a
  4×4 world transform). Cross-product dedup on `IfcMappedItem` /
  `IfcRepresentationMap` collapses N instances of the same window-
  facade family to ONE rep row — ST28_RIV (834 MB, 87K products)
  output dropped from 180 MB single-file to 68.6 MB across the two
  files (−62%).
- **Bundle pre-pass: `Arc<str>` interning + zero-clone regrouping.**
  Pset / material / quantity / classification regrouping now interns
  repeated semantic strings (set_name, prop_name, source_class,
  storey_name, type_name, …) and consumes the extractor's
  `Vec<String>` columns by-move rather than by-clone. On ST28_RIV
  (2.57M pset rows): peak RSS 2709 → 2627 MB (−3.0%), wall 33.06 →
  30.28 s (−8.4%), output bit-identical.
- **MCP server (`ifcfast-mcp`).** Standalone Model Context Protocol
  server exposing 18 tools (open_ifc / summary / schemas / preview /
  types / by_type / parent / children / ancestors / descendants /
  storey_of / building_of / products_in / diff / list_open / close /
  system_prompt / example_path) plus an `ifcfast://agents-guide`
  resource. Plug into Claude Desktop, Cursor, or any MCP-aware
  client by adding `{"command": "ifcfast-mcp"}` to the client's
  MCP server config. Install with `pip install 'ifcfast[mcp]'`.
- **`Model.diff(other)`** — first-class model-version comparison.
  Returns JSON-friendly dict with products added/removed/changed
  (and exact counts), type cardinality deltas, and storey changes.
  Makes "what changed since v3?" a one-liner.
- **`Model.type_summary()` and `Model.type_bank()`** — type-first
  extraction shaped for TypeBank-style workflows. Cheap (no extracts
  for `type_summary`; lazily pulls materials + classifications for
  `type_bank`).
- **`Model.by_type(entity)`** — ifcopenshell-compat shortcut. Same
  shape as `ifcopenshell.file.by_type(entity)`.
- **`ifcfast types FILE`** CLI subcommand — JSON-friendly type
  extraction with optional `--with-data` for the full TypeBank shape.
- **Agent-first surface.** New top-level helpers
  `ifcfast.example_path()` (path to a bundled 2 KB IFC4 fixture) and
  `ifcfast.system_prompt()` (paste-into-LLM description of the API).
  `Model.summary()` returns a JSON-friendly snapshot — schema, counts,
  every available table with shape + loaded-state. `Model.schemas`
  exposes column-level dtypes. `Model.preview(table, n=5)` returns
  sample rows as plain list-of-dicts.
- CLI: every subcommand now takes `--json` and emits a stable
  JSON shape. New subcommands: `ifcfast demo` (runs the showcase
  against the bundled fixture) and `ifcfast schema FILE` (full
  schema introspection without paying any extract cost).
- `py.typed` marker — type checkers (pyright, mypy, IDE LSPs) now
  pick up annotations from the package.
- `AGENTS.md` at the repo root: agent-onboarding guide, decision
  tree, performance budget table, and the conventions an agent can
  rely on.
- Spatial hierarchy & relationship graph on the `Model`. Three new
  long-format DataFrame properties — `m.contained_in`, `m.aggregates`,
  `m.storey_building` — plus seven traversal helpers (`parent`,
  `children`, `ancestors`, `descendants`, `storey_of`, `building_of`,
  `products_in`). The helpers walk the unified aggregates +
  spatial-containment graph so a single `ancestors(wall_guid)` reaches
  the project, and `products_in(building_guid)` returns every product
  in every storey of that building.
- Tier-1 cache bumped to v2: relationship tables persist as
  `contained_in.parquet`, `aggregates.parquet`, and
  `storey_building.parquet` alongside the existing index parquets. Old
  v1 caches re-parse on first open. Disk overhead: <500 KB on a 200 MB
  IFC.

### Changed

- Tier-1 indexer is 22-30% faster end-to-end. Hot-path dispatch now
  uses a single HashMap lookup keyed by STEP type name (was a chain of
  two HashSet lookups + ~8 byte-slice equality checks per record).
  Step-id parsing skips std's UTF-8 + checked-overflow path in favour
  of a tight wrapping loop. The argument splitter reuses a buffer
  across records instead of allocating one `Vec` per STEP entity.
- Entity name canonicalisation (`IFCWALL` → `IfcWall`) is now O(1)
  via a lazy `OnceLock<HashMap>` instead of a 130-entry linear scan.
- `IfcRelContainedInSpatialStructure` post-pass filter is now
  in-place; previously allocated two fresh `Vec`s sized to the
  unfiltered input.

Measured against the published audit set (results, throughput on a
warm cache):

| file shape | before | after | speedup |
|---|---:|---:|---:|
| Small ARK (22 MB, 8.8K products) | 39 ms | 29 ms | 1.34× |
| Federated mid-size (187 MB, 21K products) | 195 ms | 152 ms | 1.28× |
| Large MEP (834 MB, 87K products, 14.3M records) | 1287 ms | 905 ms | 1.42× |

Byte-level parity vs `ifcopenshell` preserved across the audit set
(drift severity histograms reproduce exactly on every file).

## [0.1.0] - 2026-05-14

Initial PyPI release. Library was extracted on 2026-05-13 from the
`EdvardGK/ifc-workbench` scratch repo; see [`docs/history/origin.md`](docs/history/origin.md)
for the trail and rename table.

### Added

- `ifcfast.open(path)` — tier-1 parse with lazy data layers (`psets`,
  `quantities`, `materials`, `classifications`, `drift`).
- `ifcfast.header(path)` — tier-0 STEP header read in 30-80 ms regardless
  of file size.
- Parquet cache (`~/.cache/ifcfast/<cache_key>/`) — second open returns
  in tens of milliseconds. Override via `IFCFAST_CACHE` env var.
- `ifcfast.classify` — element-mode policy (count / measure / linear / skip).
- `ifcfast.federated_floors` — multi-discipline floor synthesis with
  project-supplied YAML rules.
- CLI: `ifcfast {index,extract,drift,cache} FILE`.
- Rust binary `ifcfast-mesh` — writes OBJ / glTF / CSV from extrusion,
  mapped, face-set, and BREP representations.
- Pre-built abi3 wheels for Linux (x86_64/aarch64), macOS (x86_64/arm64),
  and Windows (x64) on Python 3.10+.

### Validated

- Byte-level parity vs `ifcopenshell` across 234,144 products from 5
  authoring tools (Tekla, Archicad, Revit IFC4/IFC2X3, MagiCAD, BSProLib).
  See [`docs/history/audit/`](docs/history/audit/).
- 100% parity confirmed on the standalone repo against 4 production
  IFCs from Skiplum projects (issue #1).
- Warm-cache speedup vs `ifcopenshell.open()`: 59-678× on production files.

[0.5.1]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.5.1
[0.5.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.5.0
[0.4.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.4.0
[0.3.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.3.0
[0.2.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.2.0
[0.1.0]: https://github.com/EdvardGK/ifcfast/releases/tag/v0.1.0
