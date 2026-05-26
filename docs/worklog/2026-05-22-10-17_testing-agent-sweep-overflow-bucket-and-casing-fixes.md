# Session: Testing-agent sweep — overflow-bucket + casing fixes

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `d58891f` → uncommitted (4 modified files, no commits this session)
- **Session scope**: Multi-agent test+verify sweep across ifcfast (Rust core + Python wheel + GeoParquet substrate). Triage findings, then autonomously land the two highest-confidence, low-risk fixes.
- **Touched paths**: `crates/core/src/mesh/qto.rs`, `crates/core/src/mesh/mod.rs`, `crates/core/src/extractors/psets.rs`, `crates/core/src/indexer.rs`
- **Parallel sessions observed**: none — `origin/main` unchanged since `d58891f`
- **Supersedes / superseded by**: none

## Summary
Ran 6 testing agents (4 round-1 + 2 round-2 confirmation) against ifcfast as a coordinated feedback loop. Surfaced 15 distinct findings; landed 2 verified fixes — an overflow-bucket area-drop bug in `mesh::qto` (every product with >64 distinct face normals lost surface data) and an entity-name casing bug on the `mesh_qto` path (`IfcFurnishingelement` instead of `IfcFurnishingElement` — broke joins to the indexer `products_df`). Pending: 6 confirmed bugs that need schema decisions before fix.

## Changes
- `crates/core/src/mesh/qto.rs:220-247` — replaced the broken bucket-migration control flow. New invariant: once `overflow_buckets` is non-empty, it is the only sink. Pre-fix, post-drain triangles leaked back into `small_buckets` and were silently dropped at assembly.
- `crates/core/src/mesh/qto.rs:406-499` — added `many_unique_normals_overflow_buckets` test. Builds a 3D lattice of unit-sphere directions whose quantized keys are distinct by construction (NORMAL_QUANT_SCALE=10, step=0.15 → ~133 distinct keys). Forces migration. Verified fail→pass: pre-fix `expected 133, got 69`; post-fix passes.
- `crates/core/src/indexer.rs:924` — made `type_name_uppercase_with_proper_case` `pub(crate)` so other modules can use the canonical entity-name table (`ENTITY_NAME_PAIRS`).
- `crates/core/src/mesh/mod.rs:328` + deleted local helper at line 744 — replaced naive `type_name_titlecase` with `crate::indexer::type_name_uppercase_with_proper_case`.
- `crates/core/src/extractors/psets.rs:155` + deleted local helper at line 251 — same replacement.

## Technical Details

### Overflow-bucket bug (mesh/qto.rs)
The original code, on triangle 65+ when `small_buckets.len() == 64`, would:
1. Linear-scan `small_buckets` for the key — `placed=false`.
2. Hit the migration `else` branch: `drain(..)` into `overflow_buckets`, then insert the new key into `overflow_buckets`. `small_buckets` is now empty.
3. **On triangle 66+**, line 223 scanned the now-empty `small_buckets`, returned `placed=false`, then the `if small_buckets.len() < 64` branch (0 < 64 = true) pushed the key back into `small_buckets` instead of looking up `overflow_buckets`.
4. At assembly (line 254), `if !overflow_buckets.is_empty()` is true, so only `overflow_buckets` is read. Everything that leaked back into `small_buckets` is silently dropped from the output.

Fix routes all post-migration inserts to `overflow_buckets` directly. The old `placed`-scan path only runs while `overflow_buckets` is empty.

### Test that exposed it
Agent E's first attempt (70 normals evenly around a 2D circle) produced 60 buckets both pre- and post-fix — the legitimate quantized count, because 70 evenly-spaced 2D angles collide heavily under the 0.1 quantizer. Rewrote with a 3D unit-sphere lattice (step=0.15, hemisphere) producing 133 distinct quantized keys by construction. Now pre-fix yields 69, post-fix yields 133. Bug confirmed empirically.

### Casing bug
Three copies of the same naive title-case algorithm existed in the repo (`indexer.rs`, `mesh/mod.rs`, `extractors/psets.rs`). The indexer copy is fronted by a lookup table (`ENTITY_NAME_PAIRS`) holding the canonical IFC entity names — `IfcWallStandardCase`, `IfcFurnishingElement`, etc. The other two copies skipped the lookup and went straight to the naive algorithm, which only capitalises the first letter after the `Ifc` prefix. Fix: consolidate to the indexer's helper. Verified on Duplex_A:

```
56  IfcWallStandardCase   (was IfcWallstandardcase)
61  IfcFurnishingElement  (was IfcFurnishingelement)
50  IfcOpeningElement     (was IfcOpeningelement)
 2  IfcStairFlight        (was IfcStairflight)
```

### Test status
All 18 Rust tests pass (`cargo test --workspace --all-features`): 9 library unit tests + 9 `mesh_reveal.rs` integration tests. New `many_unique_normals_overflow_buckets` test included.

## Outstanding findings (not fixed this session)

Surfaced and verified but pending design decisions:

1. **16,584-product silent drop** `mesh/mod.rs:306-310, 319-323` — products with no representation or no body items are `continue`'d entirely, never appear in `instances.parquet`. Their psets/materials/classifications become invisible from the substrate. Biggest "reveal-all" violation; needs schema decision (add `has_geometry: bool`, emit null-geometry rows).
2. **`.ifczip` silent drop** (#19) — `bundle.rs` mmaps the file and feeds raw bytes to the STEP lexer. ZIP magic bytes are scanned as STEP, find no `DATA;` section, yield 0 products. No error. Needs magic-byte check + decompression.
3. **Material `thickness_mm` holds metres** `extractors/materials.rs:75-82` — values 0.003, 0.012, 0.016, 0.435 on Duplex; should be 3, 12, 16, 435 mm. Fix is `thickness.map(|t| t * 1000.0 / unit_scale)` — but `unit_scale` is not currently threaded into `materials::build`.
4. **IfcSpace `ifc_id=0` sentinel** — `IFCSPACE` is missing from `PRODUCT_TYPES` (indexer) but is meshed by `mesh/mod.rs:666`'s `is_product_type()`. All 21 spaces in Duplex collapse to `ifc_id=0`. Needs unification.
5. **`volume_m3 > aabb_volume_m3`** — 9.4% of Duplex instances have physically impossible volumes (open mesh shells via divergence theorem). No validity flag. Consumers doing naive sums get garbage.
6. **5 `panic!` in `parquet_sink`** lines 185, 195, 680, 684, 686 — schema mismatch / flush failures abort instead of returning `Result`.

### Low-risk nits worth knowing
- `qto.rs:158` bounds guard only checks `off_c`, not `off_a`/`off_b` (panic risk for external callers; mesher's own output is sorted so it doesn't fire).
- Cache drift: `type_counts` empty on cache hit, populated on fresh parse. Affects `m.types()` after caching.
- CLI footgun: `ifcfast-bundle <file> -o /tmp/out` silently creates a literal `-o` directory next to the binary. Stray `-o/` left in tree as evidence.
- Test coverage gaps (agent A): zero tests for `ParquetSink`, all 4 extractors, `analyse_drift`, all mesh sub-modules except `qto.rs`.
- 110 clippy `-D warnings` errors, mostly redundant deref + `into_iter` noise.
- Wheel metadata says 0.3.0 but runtime `__version__=0.4.0` (editable install; `pip install -e .` would sync).

## Next
1. **Decide schema for #1 product silent drop** — emit null-geometry rows with `has_geometry: bool`? This is the biggest trust violation per "reveal-all" north star and unblocks the next round of substrate consumer work.
2. **Material thickness unit fix** — needs `unit_scale` threaded into `materials::build`. Low ambiguity but touches the extractor API.
3. **IfcSpace unification** — pick one source of truth (`PRODUCT_TYPES` set in indexer, or `is_product_type()` exclusion in mesh). Should be the same list.
4. **Commit the two landed fixes** — currently uncommitted. Suggested split: one commit for the qto overflow fix + test, one for the casing consolidation. Or one bundled commit if these are pre-release polish.
5. **`.ifczip` decompression (#19)** — needed for the Sannergata 1 GB validation that pre-pass interning work was supposed to enable.

## Notes
- Stray `-o/` directory in repo root is from agent B's CLI footgun test. Contains a valid bundle output (`instances.parquet`, `representations.parquet`, `view.sql`). Not deleted (CLAUDE.md: no `rm` without approval; safe to `gio trash` if not useful as evidence).
- Six confirmed bugs is a lot for a v0.4.0 just-shipped-to-PyPI release. The mesh QTO engine in particular has two distinct correctness issues (overflow bucket now fixed; casing now fixed; volume > AABB pending). Worth considering a 0.4.1 patch release once schema-decision items are resolved.
- Agent coordination pattern worked well: round-1 (parallel testing) → triage → round-2 (focused confirmation/root-cause) → autonomous fix with empirical verification. Total 6 agents, mixed sonnet+haiku+code-reviewer. RAM stayed under control (peak ~5GB used on top of 10GB baseline).
