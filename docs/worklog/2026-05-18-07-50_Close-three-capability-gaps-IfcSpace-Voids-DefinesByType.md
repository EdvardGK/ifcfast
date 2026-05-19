# Session: Close three capability gaps — IfcSpace / IfcRelVoidsElement / IfcRelDefinesByType

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast` (primary) + `/home/edkjo/workspace/inbox/ifcfast-site` (downstream UI)
- **Branch**: `main` @ `9c8c784` → `787afe8` (1 commit, pushed to origin)
- **Session scope**: Close the three "ifcfast capability gap" rows the demo Findings tab was advertising — make them real fixes in the Rust core + Python API, not decoration.
- **Touched paths**: `crates/core/src/indexer.rs`, `crates/core/src/lib.rs`, `python/ifcfast/model.py`, `python/ifcfast/cache.py`, `tests/test_graph.py`, `AGENTS.md`, `README.md` (unstaged WIP); `../ifcfast-site/app/dev/workbench/findings.ts`, `../ifcfast-site/components/findings-view.tsx`
- **Parallel sessions observed**: `none` (no commits landed on `origin/main` during the session — `git log origin/main --since='2026-05-17'` returned empty)
- **Supersedes / superseded by**: `none`

## Summary

Session opened as a BIM-coordinator QA pass on ifcfast.com Findings tab; produced 6 GitHub issues (#11–#16). User then reframed the philosophy: **capability gaps aren't a permanent feature, they're a TODO list. If a gap stays a gap across multiple sessions, that's bad.** Pivot: close the gaps in the parser, then make the site code reflect that. Result: parser commit `787afe8` (pushed) closes all three demo-advertised gaps natively; site-side polish (#11–#14) is applied locally but uncommitted (site repo has untracked WIP baseline — needs user merge strategy).

## Changes

**Parser (committed `787afe8`, pushed to `origin/main`):**

- `crates/core/src/indexer.rs` — new `EntityKind` variants `VoidsElement`, `DefinesByType`, `TypeObject`. `TypeObject` is a byte-suffix fallback (`IFC*TYPE`) inside the dispatch miss path, not a dispatch-map enumeration — schema additions surface automatically. Added parallel-vector columns `voids_opening` / `voids_host`, `defines_by_type_product` / `defines_by_type_type`, and `type_object_step_id` / `entity` / `guid` / `name`.
- `crates/core/src/lib.rs` — marshalled new tables under `raw["voids"]`, `raw["defines_by_type"]`, `raw["type_objects"]`.
- `python/ifcfast/model.py` — new dataclasses `SpaceRow`, `TypeObjectRow`. `Model` gains `spaces`, `type_objects`, `_voids_df`, `_spaces_df`, `_type_objects_df`. Properties `m.voids`, `m.spaces_df`, `m.type_objects_df`. `ProductRow` extended with `type_guid` / `type_name` / `type_source` (three-tier: `ifctype` > `objecttype` > `none`). `summary()`, `schemas`, `preview()` all surface the new tables.
- `python/ifcfast/cache.py` — `CACHE_VERSION` bumped to 3. New `spaces.parquet`, `voids.parquet`, `type_objects.parquet`. Manifest carries counts. Cold→hot round-trip verified.
- `tests/test_graph.py` — 5 new tests: empty-model surface, real-model void resolution, RelDefinesByType per-product fields, type-catalogue round-trip, cache parity for new tables. 60/60 pass.
- `README.md` + `AGENTS.md` — updated the "What ifcfast does NOT do" / "graph coverage" sections to reflect the closed gaps. (Note: these files had preexisting WIP modifications from before the session, so the README/AGENTS edits did NOT go into commit `787afe8`. Left for user to merge with their WIP.)

**Site (uncommitted; works against existing sample sidecars):**

- `app/dev/workbench/findings.ts`:
  - Hoisted `inContained` / `inAgg` / `inVoid` membership sets above the storeyless check so they can suppress false alarms.
  - Split "untyped" into `untyped: no type info` (warn, fires when `type_source === "none"`) and `object-type only` (info, fires when `type_source === "objecttype"`). Closes #11 — Revit-style ObjectType exports no longer read as "63% catastrophically untyped".
  - Added `VOID_DOMAIN_ENTITIES` and `AGGREGATE_DOMAIN_ENTITIES` sets. `no storey` now skips entities that ARE properly linked through their normative relation (openings via voids, curtain-wall members / stair flights / railings via aggregates). Closes #12 — dropped 4 false-alarm warns on the Duplex sample.
  - Sort comparator extended to `(severity, category, count)` from `(severity, count)`. Closes #13 — categories cluster, no more stranger rows interleaving.
  - New `buildCapabilityGaps(graph)` derives the two capability-gap rows from observable graph signals (`type_source !== "ifctype"` count + `spaces[].length > 0 && no IfcSpace in products[]`). Closes #14 — rows self-disable when the parser closes the gap and sidecars regenerate, no site code change needed.
- `components/findings-view.tsx` — deleted the hardcoded `PARSER_CAPABILITY_GAPS` constant, wired in `buildCapabilityGaps(graph)`. Top docstring updated.

**Verified live on `localhost:3000`:** severity strip moved from `7 err / 11 warn / 14 info` → `7 err / 7 warn / 14 info` (exactly the 4 expected false alarms dropped). Untyped rows replaced with `object-type only` rows carrying the Revit-pattern explanation. Capability-gap row at top now reads `"169 of 268 products fall back to IfcRoot.ObjectType or have no type info at all"` (concrete count, no longer roadmap-flavoured copy).

## Technical Details

- **Why a byte-suffix fallback for `IfcXxxType`** (not a dispatch-map enumeration): the IFC schema has ~80 `IfcXxxType` subclasses and the list grows across schema versions. Enumerating them in the dispatch map would age poorly. A 4-byte suffix check (`ends_with b"TYPE"`) costs ~one `memcmp` per un-dispatched record — bounded by total record count, and only on the hot-path miss (which already does a `HashMap::get`). On Sannergata (8873 products, 305 ms cold parse) the perf budget held.
- **Three-tier `type_source` resolution lives in Python, not Rust**: the Rust core emits two raw tables (`type_objects`, `defines_by_type`); Python builds the step_id→(guid, name) lookup and joins it onto products. Keeps the indexer single-pass and lets the fallback logic (`objecttype` vs `none`) live next to the rest of the per-product wiring in `_index_native`.
- **`buildCapabilityGaps` self-disabling pattern**: the two rows fire on observable post-parse signals, not on a `provenance.typedness_source` schema field. This means the gap rows close without needing the sidecar generator to learn a new provenance vocabulary — once `type_source === "ifctype"` is true for every product (i.e. the parser ships, the sidecars regenerate), the typedness gap row count drops to zero and the row disappears. Same for IfcSpace: once spaces show up in `graph.products` instead of `graph.spaces`, the row evaporates.
- **Cache version bump**: `CACHE_VERSION = 3`. Existing `~/.cache/ifcfast/*` entries from v2 invalidate automatically on the next `ifcfast.open()`.
- **Two preexisting site repo WIPs**: `app/dev/` and `components/findings-view.tsx` were UNTRACKED before this session — the entire workbench feature was uncommitted. My edits sit on top of that work, so a commit needs to package them together. The site has no `origin` remote; deploys go through `vercel deploy`.

## Next

- **Push site fixes**: user to decide commit strategy for the site (the workbench feature is uncommitted baseline; my fixes are additional changes on top), then `vercel deploy`. Once live on ifcfast.com, close issues #11, #12, #13, #14 with reference comments.
- **Regenerate `public/sample/duplex.bundle.json`**: with the new ifcfast, the sidecar generator's ifcopenshell typedness pass is redundant. Re-running `scripts/generate_sample_sidecars.py` against the Duplex IFC will produce graph.json with `type_source: "ifctype"` for the 99 products that have formal `IfcRelDefinesByType` links — the capability-gap typedness row will then show `"169 of 268 products fall back..."` go to `"0 of 268..."` and disappear. The Duplex IFC isn't in the repo (`scripts/...` takes `--ifc /path/to/duplex.ifc`); user has it locally.
- **README/AGENTS.md edits** I made (declaring the gaps closed) are unstaged because those files had preexisting WIP from before this session. User to merge.
- **Issue #16** (drop-your-own-IFC) is a feature ask, deferred.

## Notes

- ifcopenshell sidecar pass for typedness (`scripts/generate_sample_sidecars.py::_typing_via_ifcopenshell()`) is now redundant. Worth removing the fallback in a follow-up cleanup, but keep it for backward compat until the parser version pin in the script bumps.
- The byte-suffix `IFC*TYPE` fallback might catch entities I haven't anticipated. If a future schema adds a non-`IfcTypeObject` class ending in `TYPE`, it would land in `type_objects` incorrectly. Mitigation if needed: add an allowlist of known-good base entity names. Not a real risk in IFC2X3 or IFC4 from inspection.
- The session opened with a Chrome devtools MCP lock-file issue (stale `SingletonLock` from a prior session at `~/.cache/chrome-devtools-mcp/chrome-profile/`). User had to authorize `gio trash` of three lock files. Pattern: any new Claude session that uses chrome-devtools-mcp after an unclean shutdown will hit this — worth a startup-time pre-flight check in the MCP server itself, but not actionable from here.
- Parallel-session check: no commits landed on `EdvardGK/ifcfast` `origin/main` during the session window. The 4 GitHub issues that surfaced today (#7–#10, all by `EdvardGK`) predated my work and were filed roughly at session-start.
