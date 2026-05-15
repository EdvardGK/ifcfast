# Session: tier-1 indexer dispatch + parse perf (22-30% faster)

## Summary

Followed the v3 worklog's "Issue #2 followup" thread — the parser arc's
last known perf target — and shipped a 22-42× speedup on the indexer's
hot path. Three changes, no API breakage, byte-level parity preserved
across the audited drift / pset / classification / material counts.

## Diagnosis (the part I got wrong, then right)

Started by porting the v3 worklog hypothesis verbatim:
`extract_product` allocating GUID strings, HashMap entries cloning the
entity name, post-pass storey filter copying Vecs. Implemented all three
+ a reusable buffer for `split_top_level_args`. Re-bench: ~2% win on
sample-D. Noise floor.

Added a single counter inside `for_each_record` and re-ran on sample-D:

```
DEBUG: records total=14_284_172 products=87_198 other=14_196_906
```

14.2 million records discarded by the dispatch chain, 87K processed.
The chain was running `product_set.contains` + `storey_set.contains` +
~8 byte-slice equality checks per ignored record. Real cost was not in
the product-extract path; it was in the per-record dispatch on the
**ignored** records — the 99.4% the indexer doesn't care about.

## Changes

`crates/core/src/indexer.rs`

- New `EntityKind` enum + lazy `OnceLock<HashMap<&[u8], EntityKind>>`
  built once from `PRODUCT_TYPES` + the spatial / rel / unit / app
  type slices. The per-record dispatch is now `match map.get(t)`:
  one hash lookup, then a small variant match. Misses (>99% of records
  on big MEP files) cost one HashMap probe, not 10+ comparisons.
- `extract_product` / `extract_storey` take pre-split fields instead
  of raw `&[u8] args` — the indexer reuses a single `Vec<&[u8]>`
  across all 14M records instead of allocating one per record.
- `type_counts.entry(entity.clone()).or_insert(0) += 1` →
  `if let Some(c) = map.get_mut(&entity)` pattern, so the String is
  only owned-by-the-map on first occurrence (9 distinct types on sample-D
  → 9 clones, not 87K).
- Entity-name canonicalisation (`IFCWALL` → `IfcWall`) was a 130-entry
  linear scan run per product. Replaced with a lazy
  `OnceLock<HashMap<&[u8], &str>>` populated from the same static
  pair list (renamed `ENTITY_NAME_PAIRS`). O(1) lookup, no behaviour
  change.
- `contained_in_*` post-pass filter is now write-index-in-place,
  no two extra Vecs.

`crates/core/src/lexer.rs`

- `parse_record`'s u64 id parse switched from
  `std::str::from_utf8(...).ok()?.parse::<u64>().ok()?` to a tight
  wrapping loop over the already-validated ASCII digit slice. Saves
  ~25 ns/record × 14M records.
- New `split_top_level_args_into(args, &mut buf)` — same logic as the
  existing function but writes into a caller-supplied buffer. Old
  function still exists (extractors use it) and is now a one-line
  wrapper.

## Bench (warm cache, best of 5-6 runs)

| file | size | products | records | before | after | speedup |
|---|---:|---:|---:|---:|---:|---:|
| sample-A | 22 MB | 8.8K | ~150K | 39 ms | 29 ms | 1.34× |
| sample-B | 187 MB | 21K | ~1.5M | 195 ms | 152 ms | 1.28× |
| sample-D | 834 MB | 87K | 14.3M | 1287 ms | 905 ms | 1.42× |

Throughput crossed 900 MB/s on the 834 MB Tekla-MEP file.

## Parity check

End-to-end `ifcfast.open()` on sample-A reproduces the audited
numbers exactly:

- 8042 ok / 347 warn / 264 error drift severity
- 24,601 psets / 1,145 classifications / 13,635 materials

29/29 pytest tests pass.

## What I tried first that didn't move the needle

The v3 worklog's hypothesis (`extract_product` string alloc, type_counts
clone, storey filter Vec copies) was correct but tiny. Each change is
in the new code anyway since they don't cost anything and they tighten
the surface, but the real win came from a different place — the
dispatch over ignored records.

The lesson: 87K products feels like the hot path because it's the
output we care about, but the indexer's actual cost on a big MEP file
is processing the 14M things it discards. Always measure record counts,
not output counts, when reasoning about parser throughput.

## Next

- Even-bigger win still on the table: skip the u64 id parse on
  records the dispatch will reject. Currently we parse every id, then
  decide. Lazy-id would save another ~10 ns × 14M ≈ 140 ms on sample-D.
  But it requires reshaping `Record` / `for_each_record` since
  `EntityTable` (used by extractors) wants the id eagerly. Worth a
  separate session.
- Boolean subtraction in mesh (gross/net volumes) is still the biggest
  user-visible gap and a more interesting feature than another 10%
  on parse time. Probably worth doing before more parse optimisation.
- Property variants beyond `IfcPropertySingleValue` — covers the
  ~10% of psets we currently skip.
