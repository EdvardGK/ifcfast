## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `fb9b07d` → `c90908d` (4 commits this session)
- **Session scope**: Oracle-validated + shipped the #121 open-shell QTO fix, then opened ifcfast's write axis (#124) — owned round-trippable STEP doc + subset reachability engine, validated across a diverse multi-file corpus.
- **Touched paths**: `crates/core/src/mesh/qto.rs`, `crates/core/src/doc/{mod,emit,refs}.rs`, `crates/core/src/lexer.rs`, `crates/core/src/lib.rs`, `crates/core/tests/{cut_openings_integration,doc_roundtrip,doc_subset}.rs`, `python/ifcfast/{header,model}.py`, `AGENTS.md`, `CHANGELOG.md`
- **Parallel sessions observed**: none (no non-Omarchy commits on origin/main during the session window)
- **Supersedes / superseded by**: none

## Summary
Two threads closed with hard evidence. First, the **GH #121 open-shell QTO over-count fix** (uncommitted at session start) was oracle-validated on G55_ARK vs `ifcopenshell.geom` — railings +1987%→exact parity, doors +69%→+9.3%, zero regression on closed structural classes — then committed, pushed, and #121/#114 closed. Second, and the bigger arc: Ed scoped a **"blazing-fast subset creator + mesh hotswap"** as ifcfast's first write capability (#124). I surveyed the code, got an architecture blueprint, then **built and proved the foundation** — an owned round-trippable STEP document with byte-identical emit and a forward-reachability closure engine — validated across a deliberately diverse 8-file corpus (IFC2x3/4/4x3, Revit/IfcOpenShell/MagiCAD, 60 KB–201 MB). Along the way I ran an empirical primitive-fit measurement, then **killed my own escalation of it** (general mesh→parametric recovery) as off-strategy.

## Changes
- **`4b17c4c` fix(qto)** — trust open-shell mesh volume within `min(prism,aabb)`; new `volume_method="mesh_open"`; collapse tripwire `0.1→1e-3`; cache schema 23→24. AGENTS.md/model.py/CHANGELOG synced.
- **`e834e4b` feat(doc)** — `doc::Doc` (owned bytes + record-span index) + `doc::emit`. **No `unsafe`**: `EntityRefs` stores offsets not slices, so accessors take `&buf` — the blueprint's `Arc`+transmute was unnecessary and retired. `lexer::for_each_record_span` reports byte spans for verbatim re-emit.
- **`eb3aa11` feat(doc)** — `lexer::scan_ref_tokens` + `doc::refs` (`forward_refs`, `reachable_closure`). Forward-closed (no-dangling-ref) invariant; robust to pre-broken source.
- **`c90908d` test(doc)** — env-driven multi-file corpus gate (`IFCFAST_CORPUS`), byte-identity + closure across 8 diverse files.
- **Decisions**: (1) PARKED general mesh→parametric recovery (#125) — kept only a read-only shape classifier + open-shell QC detector. (2) Priority = finish subset moat, then QTO #62. (3) Hotswap stays mesh→mesh (original ask), not mesh→parametric.
- **Issues**: closed #121, #114; filed #123 (degenerate-mesh prism over-count), #124 (writer epic), #125 (shape classifier / park decision).

## Technical Details
- **Oracle harness**: `tests/oracle/_geom_adapter.diff_geometry_volumes` per-GlobalId vs a single `ifcopenshell.geom.iterator` pass; debug `.so` via `maturin develop` (never `--release` on the 16 GB box → OOM; and `unset CONDA_PREFIX` first — both VIRTUAL_ENV+CONDA_PREFIX set makes maturin refuse). ARK differential ~53s ifcfast + ~1000s ifcopenshell in debug.
- **Byte-identity trick**: each record's emit span is `[start, next_record_start)` so trailing separators travel with the record → spans tile the DATA section exactly → keep-all is provably identical; drop = omit span. Sparse original ids preserved (no renumber) so untouched records emit as verbatim byte slices.
- **Closure ≠ subset yet**: forward closure alone drops all `IfcRel*` (a product never forward-references the rel that contains it) — spatial-path + rel-pruning is a separate pass (Phase 2b).
- **Primitive measurement** (`scratch primfit.py`): OBB via PCA + face-normal extrusion test (side⊥/cap∥) + single-vs-assembly cap clustering. Findings: 98% of ARK representable as `IfcExtrudedAreaSolid` (89% of volume as single extrusions), deterioration is **bimodal**, volume-alone misclassifies (column reads box-like by volume but round by face-structure), and profile-fill>1 flags open-shell geometry for free. "Representable" ≠ "robustly recoverable" → parked the emit.
- **Test invocation**: `cargo test -p ifcfast-core --no-default-features --test doc_subset --test doc_roundtrip` (no-default-features dodges the pyo3-link issue that blocks integration binaries under the default `python` feature).

## Next
- **Phase 2b (subset rel-pass)**: `doc/rel_rules.rs` (~12 IfcRel* types; 4 field-positions verified vs repo extractors, ~8 need pinning fixtures) + spatial-path-to-IfcProject seeding + `doc/subset.rs` two-phase closure↔prune fixpoint (keep rel iff relating kept; rewrite related-SET to ∩keep by arg splice; drop if empty).
- **Validate 2b across the WHOLE corpus** (Ed's standing rule): extend `doc_subset` to re-open each subset in ifcopenshell + assert valid spatial tree + zero dangling — not just G55.
- Then PyO3 `open_editable`/`subset` + `Model.subset()` + **AGENTS.md same-commit**.
- After writer: QTO **#62** (windows +482%, biggest remaining gap), **#123** (degenerate-mesh → reliable=false hybrid route).

## Notes
- Corpus file list for the multi-file gate is in `next-steps.md` (8 files across schemas/exporters/sizes). Add ST28_ARK (284 MB) for extra scale if wanted.
- `.so` in `target/` now carries #121 code (fresh debug); `target/release/lib_core.so` is STALE.
- The primitive-fit + park reasoning is fully captured in #125 and `roadmap-geometry-hotswap` memory — the MEP-narrow duct/pipe recovery stays alive (sequenced behind #91), only the general version is dead.
