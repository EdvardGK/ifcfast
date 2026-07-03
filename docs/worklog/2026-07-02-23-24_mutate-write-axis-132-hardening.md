# Session: #133 attribute mutation shipped + #132 hardening items 2–8 + viability backlog capture

## Agent signature
- **Agent**: `claude-fable-5`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `dcb55a8` → `eed795b` (2 commits this session: `a0f5798`, `eed795b`)
- **Session scope**: write-axis attribute mutation (#133) + #132 hardening + viability-list backlog capture
- **Touched paths**: crates/core/src/doc/{mutate.rs,step_fmt.rs,refs.rs,mod.rs,hotswap.rs,rel_rules.rs}, crates/core/src/{guid.rs,lexer.rs,source.rs,lib.rs}, crates/core/tests/doc_{mutate,hotswap,subset,rel_rules,roundtrip}.rs, python/ifcfast/model.py, tests/test_{mutate,hotswap}.py, tests/fixtures/{mutate_shared,hotswap_styled}.ifc, AGENTS.md, CHANGELOG.md
- **Parallel sessions observed**: none (only this session's 2 commits on origin/main during the window)
- **Supersedes / superseded by**: none

## Summary
The write axis is complete: `m.mutate(ops)` (GH #133) shipped — batch attribute
mutation (pset values, rename, translate/rotate) with copy-on-write on shared
data and the byte-identical-elsewhere guarantee — followed by #132 hardening
items 2–8, whose testing surfaced and fixed a real hotswap flaw (styled items
pinning dead geometry alive, so hotswap never shrank Revit-coloured files).
Both issues closed; both commits pushed; all corpus gates green on 4 G55
disciplines. In parallel, Ed dictated a product-viability list that was
captured as tracker issues (#135 infra scoping, #136 duplicate detection,
#137 OOBB/aabb_fill_ratio, mesh-location check anchored on #94).

## Changes
- **`crates/core/src/doc/mutate.rs`** (new, ~900 lines): the mutation engine.
  `Editor` overlay over `Doc` (pending overrides + minted records + maintained
  out_refs/in_count reference graph so sharing checks are O(1)); ops =
  Rename / SetProperty / Translate / Rotate; two-phase per-op resolve-then-write;
  refcount-fixpoint GC over the live editor graph; atomic batches with ALL
  failures collected (`[op N] reason`).
- **CoW semantics** (the design crux, validated by a code-architect sub-agent
  per the architecture-advisor convention): pset applying to >1 element →
  clone with fresh GlobalId, siblings keep values; placement points/directions
  NEVER edited in place (mint + GC decides — the load-bearing safety property);
  shared `IfcLocalPlacement` CoW-cloned. `IfcElementQuantity` name-collision
  type guard (Quantities@5 ≠ HasProperties@4 — a name-only match would corrupt
  an IfcQuantityLength).
- **`crates/core/src/guid.rs`** (new): IFC compressed-GUID mint — 2-bit first
  char (2+21×6=128), RFC-4122 v4 bits, entropy-salted default / `seed=` opt-in
  (deterministic default would collide on later federation).
- **`crates/core/src/doc/step_fmt.rs`** (new): `fmt_real`/`fmt_tuple` (moved
  from hotswap) + `encode_string` — true inverse of lexer `decode_string`
  (raw UTF-8, æøå literal; control chars fail loud).
- **`refs.rs`**: `RecordSource` trait — subset/hotswap/mutate share one
  closure engine with pluggable bytes resolution.
- **#132 item 2**: `parse_record_span` terminates at FIRST top-level `;`
  (string/comment-aware `find_record_end`) — trailing inter-record comment
  with `;` no longer hides a record from rel/guid resolution.
- **#132 items 3+5**: `IfcRelAssignsToGroupByFactor`, `IfcStyledItem`,
  `IfcPresentationLayerAssignment`/`…WithStyle` added to REL_RULES — subsets
  now carry authored colours + CAD layers (10.5k–35.8k styled items per G55
  model previously dropped). Hotswap GC treats styled items/layers as WEAK
  referrers: orphaned styled items cascade-removed, layer sets spliced to
  survivors — hotswap now actually shrinks styled files.
- **#132 item 4**: `looks_rooted` shape guard on guid resolution (≥4 args,
  ref|$ @1, string|$ @2,3) — `IFCMATERIAL('Concrete')`/property Names no
  longer resolve as GlobalIds.
- **#132 items 6–8**: hotswap stats `pds_shared_with` + `body_reps`;
  `.ifczip` out_path writes real ZIP (`source::compress_ifczip`) on all three
  write verbs; all bad hotswap mesh input normalized to ValueError.
- **Bindings/API**: `mutate_ifc` in lib.rs (op-dict parsing, bool-before-int),
  `Model.mutate()` in model.py; AGENTS.md write-axis sections updated
  (mandatory contract rule); CHANGELOG Unreleased section.

## Technical Details
- **Corpus differential doctrine paid off twice**: (1) the rel-rules corpus
  gate caught `IFCSTYLEDITEM` with `$` Item (IFC2x3 material-styles idiom) —
  gate relaxed for exactly that case; (2) probing G55_RIB for the suspected
  styled-item dangling bug revealed the INVERSE bug (strong ref pinning dead
  geometry) which no synthetic fixture would have shown.
- Oracle gates: mutate gate reads property back via ifcopenshell, checks
  exact placement delta, zero dangling, and sibling-view preservation on a
  shared pset. All 4 G55 disciplines green (~5.5 min run).
- `cargo fmt` accident: formatted the whole non-fmt-enforced crate (66 files);
  reverted untouched files and re-applied lib.rs edits by hand — final diff
  935+/160− all intentional.
- Emit-order note: minted records append before ENDSEC in id order
  (BTreeMap), deterministic regardless of op order.

## Next
1. **#131** trust-band lower-bound design call (2-row residue; needs Ed or a
   design decision on wrong-but-in-band open-shell volumes).
2. **0.5.0 QTO frontier**: #62 window shell-closing (the +482% residue — the
   big one), #123 degenerate partial-collapse, #122 mesh coverage gap.
3. Viability follow-ons when prioritized: #136 duplicates verb, #137
   OOBB/aabb_fill_ratio, #94 geometric_storey/location trust columns.
4. Release: two unreleased commits on main — bundle into next release per
   convention (no tag pushed this session).

## Notes
- Write axis is now COMPLETE (subset + hotswap + mutate); AGENTS.md "not yet"
  list scopes what mutate deliberately excludes (quantity values,
  enum/bounded/list props, type-level psets).
- Ed's viability list (dictated mid-session) is fully on the tracker:
  #135 (IFC4x3 infra long-term yes / short-term no), #136 (GUID + mesh
  duplicate detection, confidence-tiered), #137 (AABB lies on rotated
  geometry — also weakens #121's aabb ceiling), #94 comment (meshes where
  they're "supposed to be": storey vs mesh + basepoint/envelope checks).
- MEMORY.md + ifc-writer memory updated with both ships.
