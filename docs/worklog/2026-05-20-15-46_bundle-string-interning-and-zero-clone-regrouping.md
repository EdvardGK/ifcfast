# Session: Bundle pre-pass — string interning + zero-clone regrouping

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `bf28d25` → `d145712` (worklog-backfill `e33a001` + code `d145712`; this worklog commit adds one more)
- **Session scope**: Tactical memory pass on `Bundle::build` — intern high-repeat semantic strings via `Arc<str>` and consume the four extractor `Vec<String>` columns by-move (no per-row clone into the regrouped maps).
- **Touched paths**: `crates/core/src/bundle/mod.rs`, `crates/core/src/bundle/record.rs`
- **Parallel sessions observed**: none on origin/main during this window
- **Supersedes / superseded by**: none

## Summary

Picked up the load-bearing follow-up from `2026-05-20-10-00_streaming-geoparquet-substrate-vertical-slice.md` — `Bundle::build` allocated each pset/material/quantity/classification row's strings twice (once in the extractor's long-format `Vec<String>` columns, once in the regrouped `HashMap<String, Vec<...>>`). For high-repeat / low-cardinality fields (pset names, prop names, value_types, material names, qto names, source classes, storey names, type names), that's both a regrouping-time peak doubler and a fat-row persistent map.

Two interventions, both inside `Bundle::build`:
1. **String interning.** Local `HashMap<String, Arc<str>>` cache shared across the per-product assembly and all four regrouping loops. Field types on `PsetValue` / `MaterialEntry` / `QuantityEntry` / `ClassificationEntry` / `ProductCore` / `ProductSemantics` / `InstanceRecord` switched from `String` / `Option<String>` to `Arc<str>` / `Option<Arc<str>>` for the repeating fields. `value` / `name` / `tag` / GUIDs stay as owned `String` — high cardinality, no dedup payoff.
2. **Consume by-move.** Replaced the indexed-clone loops (`for i in 0..len { …[i].clone() }`) with `into_iter().zip(...)` chains. GUIDs are now moved (not cloned) into the regrouped HashMap keys; high-repeat fields pass through `intern(...)` so duplicates share one `Arc<str>`. Same pattern applied to the per-product `ProductCore` assembly: `mem::take` snatches the indexer's columnar `Vec`s, iterators consume them row-by-row.

## Validation

**Tests:** `cargo test -p ifcfast-core --features bundle` → 9 reveal tests green; `cargo test -p ifcfast-core` (no features) also green. No new test suite added (queued in the 10:00 worklog as a separate item).

**Parquet bit-identity (Duplex, 2.3 MB IFC, 286 instances, 13144 pset rows):** content-identical before/after — verified via row-level diff on every column. (My first pass used an order-sensitive fingerprint script which gave false-positive SHA differences when entries with identical `(set_name, name)` sort keys appeared in different orders within a row; switched to a full-tuple sort, all rows matched.)

**Parquet bit-identity (Sannergata_ARK_SB, 268 MB IFC, 27070 instances, 97756 pset rows):** content-identical. Zero rows differ across the 27070-row table. Output size unchanged at 23.6 MB.

**RSS (Sannergata_ARK_SB, single run, no warm-up):**
- Before: **810.8 MB peak RSS, 6.99 s wall**
- After:  **797.3 MB peak RSS, 7.23 s wall** (−13.5 MB, −1.7%; wall +3% within run-to-run noise)

The 13 MB reduction is consistent with the model: Sannergata has only 97K pset rows but 25 unique `set_name`s and 24 unique `prop_name`s — high repetition, but the absolute byte count of the duplicated strings is small (~5-10 MB doubled = ~10-20 MB savings expected). The 10:00 worklog's "halve peak RSS" target was scoped to **ST28_RIV**, which has 2.57M pset rows (~27× more); on that file the same intervention should reclaim closer to 150-450 MB of the persistent maps and the regrouping-time double-allocation.

I didn't have ST28_RIV locally this session (only on the original author's host); the largest comparable IFC available was Sannergata at 268 MB. Conclusion: **the optimization is correct and effective at the byte-per-row level, but its absolute magnitude is dominated by pset count**, not by file size or product count. On pset-heavy infra IFCs the gain scales linearly; on pset-light architectural exports it's modest.

## Honest scope gap

The 10:00 next-steps note framed this as "halve peak RSS on big files." On the available test corpus that overstated the gain — the 2.75 GB peak on ST28_RIV is dominated by the mesh `shape_cache` (`HashMap<u64, Vec<(LocalMesh, &'static str)>>` in `mesh::mesh_ifc_streaming`), not by the pset map. Caching local meshes for `IfcMappedItem` dedup keeps every unique representation's vertex+index buffer alive for the duration of the streaming pass — that's where the bulk of the RAM goes on a 20K-unique-rep file. Halving the pset map shaves a meaningful slice but not "half the peak."

A genuine half-RSS pass would have to attack `shape_cache` — either evict LocalMesh entries once their last referencing instance has been emitted (requires a per-rep-id ref count from the indexer), or move the cache to disk-backed via `mmap`. That's a substantially bigger refactor than this session's scope and was not attempted.

## Next

1. **Validate on ST28_RIV (834 MB, 86K products, 2.57M pset rows).** Need to source the file or run on the original host. This is the file where the intervention's scaling shows.
2. **`shape_cache` eviction / disk-backing.** The actual peak-RSS killer on large files. Either ref-count meshes via a per-rep_id usage map from the indexer, or move the cache to a temporary mmap file. Bigger refactor; do not bundle with #1.
3. **`.ifczip` silent-drop (#19).** Still queued, separate session.
4. **Streaming glTF / USD spike.** Same OOM class as the old mesh path on the batch glTF writer.
5. **Material units bug** — `extractors::materials::build` returns `layer_thickness_mm` values in metres, not mm. File an issue when next touching that code.

## Numbers reference

| File | IFC size | Products | Pset rows | Peak RSS before | Peak RSS after | Δ |
|---|---|---|---|---|---|---|
| Duplex_A_20110907 | 2.3 MB | 286 | 13144 | 20.2 MB | 17.9 MB | −2.3 MB (−11%) |
| Sannergata_ARK_SB | 268 MB | 27070 | 97756 | 810.8 MB | 797.3 MB | −13.5 MB (−1.7%) |
| G55_ARK_farget | 271 MB | 23935 | 95861 | (not captured) | 835.1 MB | — |

(Pre-change G55 baseline not captured before the rebuild; only post-change for sanity-checking.)
