# 2026-10-12 — v0.6.4: tester batch #216–#219, arc sampling #218, discovery wave

## Agent signature
- **Agent**: `claude-fable-5-1` (coordinator; opus/sonnet/fable agents for the legwork)
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `311817e` → `f2a0c0d` (+ open worktree branches `wt/gh220`, `wt/ids-215-224`; checkpoint, session continues)
- **Session scope**: close the five tester issues from the Skiplum ifc-substrate parity harness (#216–#219, +#213), release v0.6.4, then start a self-directed discovery sweep across all local models
- **Touched paths**: crates/core/src/extractors/{classifications,materials,psets,property_graph,predefined_psets}.rs, crates/core/src/mesh/{indexed_curve,profile}.rs, crates/core/src/{bundle,entity_table,ids/graph,lib}.rs, crates/wasm/src/{analysis,lib}.rs, crates/wasm/test/stream.mjs, python/ifcfast/{cache,header,mcp_server}.py, python/ifcfast/data/AGENTS.md, AGENTS.md, README.md, CHANGELOG.md, Cargo.toml, pyproject.toml, scripts/{gen_predefined_psets.py,generate_sample_sidecars.py,worktree/}, tests/oracle/{extraction_parity.py,test_extraction_parity_corpus.py,test_materials_oracle.py}, tests/{test_empty_string_217,test_predefined_psets_drift,test_arc_sampling_218,test_far_origin_gltf_188,test_materials_rollup_185}.py, tests/fixtures/gh218_*.ifc, docs/ids/ambiguities.md; ifcfast-site `157ff58`
- **Parallel sessions observed**: none on origin/main; another local session runs sprucelab pytest (load 7–10, RAM pressure)
- **Supersedes / superseded by**: none

## Summary
Hold lifted ("attack it"). All five tester issues reproduced on the reporter's own models fetched from ACC (KNM_ARK v3, HI90_ARK v14) and fixed with ifcopenshell 0.8.5 differential on 8 models:

- `911d62b` extraction batch (cache v39): #219 `material_name` = IfcMaterial.Name on every role + new `item_name`/`set_name` (#213, graph.json layer sets populated); #217 `''` kept in classifications (issue-#9 rule was the only collapser; opposite gap → #221); #216 predefined psets generated from the schemas (IFC2x3 13 / IFC4 6 classes, drift test). 0 mismatches on KNM/HI90×3/G55×4.
- `a29c3f9` #218 (cache v40): NOT booleans/clips — profile arcs capped at 32 chords/turn + #170 area-radius applied to endpoints (jog 169 mm at R=98 m). Now sagitta-derived count (f64, ≤256/arc, through `chord_count`), pinned endpoints, interior-only area factor, f64 circumcircle. KNM_ARK >1 mm 55→0, max 146→0.36 mm; sliver walls closed at oracle volume. Ship gate 18/18 green, no class past ±0.005.
- `f42aa55` wasm round10 in layer_sets + per-group roundness in stream.mjs; `0050ddf` release v0.6.4 (PyPI + GH release + site `157ff58`).
- `dbfa875` #214: IfcTester 0.9.0 identical to 0.8.5 on 334/334, pin stays; found A6 gap → #224.
- `f2a0c0d` scripts/worktree/: per-worktree venv + `flock /tmp/ifcfast-build.lock` / `/tmp/ifcfast-oracle.lock` convention (memory `build-oracle-locks`).

Issues filed: #220 (open-shell bound lets +21 % sliver through as reliable), #221 (non-null name columns), #222 (full circles keep 32-chord cap), #224 (IDS A6 restriction mapping).

## Evidence / numbers
- KNM_ARK bbox parity (1460 bodied): before p50 13.3 µm, >1 mm 55, >1 cm 33, max 146.4 mm; after p50 13.1 µm, >1 mm 0, max 0.36 mm. Volume >1 %: 23→3 (IfcFlowTerminal, unrelated).
- Triangles +5.97 % KNM_ARK, +0.16 % G55_ARK. wasm 1,401,178 → 1,422,013 B.
- Corpus pytest on release .so: 855 passed in 21 min (load from other session).
- Discovery sweep (fable sub-coordinator) running on 16 models: bbox / per-element volume / extraction (+quantities, attributes, storeys) / tripwire precision. Oracle JSON cached under scratch/discovery/.

## In flight at checkpoint
- `wt/gh220` (opus): profile self-intersection detector → typed `volume_reliable=False` reason; item-level bound evaluation.
- `wt/ids-215-224` (opus): A6 restriction mapping + IfcRelAssignsToGroup partOf + groups table.
- Discovery sweep (fable): report + issues per mechanism.
Both code branches bump the cache schema independently — coordinator resolves the collision at merge (v41/v42), then ONE oracle gate on the merged main.

## Next
1. Merge wt/gh220 + wt/ids-215-224 → resolve cache-schema numbers → `scripts/ship_gate.sh` → commit/push.
2. Act on the discovery report's top-3; file/fix loop continues (#222 experiment on KNM_RIV/HI90_RIV triangle cost, #221, #209, #182, #207).
3. ifcfast-site#6 receipts regen vs 0.6.4.
4. Ed: close PR #24 (personal note) and PR #90; say whether #213/#216/#217/#218/#219/#214 may be closed (fixed in v0.6.4 / answered).
5. Leftovers not deleted (no-rm rule): `~/.cache/ifcfast/venv-ifctester09-partial-quota-fail` (400 MB), `~/.cache/ifcfast/venv-ifctester09` (keep for #214 reruns), `.local-samples/sidecars-v0.6.4/`, `scratch/gate_218/`, stale prunable worktree entry from another session.
