# Agent-first contract hardening — GH #68 #70 #71 #78 #79 #83 #84

**Agent:** Claude (edkjo, the-super-tester/sidehustles) — tester-in-chief
shipping fixes for its own findings, per Ed's "plan a PR and ship it".
**Scope:** Python layer only; no Rust core changes, no cache-schema bump.
**Branch:** `agent-first-contract-hardening`. No parallel sessions on this repo.

## Why this batch

The 2026-06-09/10 test sweep (issues #68–#84) found that ~13 of 17
findings share one shape: *plausible answer, exit 0*. For an agent-first
tool that's the worst failure mode — agents can't smell wrongness. This
PR fixes the wrong-answer bugs with one-to-few-line root causes and
flips the silent-failure class to loud, plus closes the biggest product
gap from the 2026-06 product review: MCP had no psets/quantities/
materials access.

## Touched

- `python/ifcfast/model.py` — `_is_missing`/`_values_equal`/
  `_index_products_by_guid` (GH #68); `_GraphIndex.storey_guids` +
  `_walk_to_storey` (GH #79); `products_in` fast-path removal (GH #78);
  `preview`/`filter`/`by_type` validation + call-time raise (GH #71);
  `diff(PathLike)` (GH #84); dead preview branch removed.
- `python/ifcfast/header.py` — `_check_step_trailer` (GH #70).
- `python/ifcfast/cli.py` — clean bad-content errors (GH #84).
- `python/ifcfast/mcp_server.py` — staleness-checked `_resolve`
  (GH #83); new tools `psets`/`quantities`/`materials`/`product_card`.
- `python/ifcfast/__init__.py` — namespace hygiene (GH #71).
- `tests/test_contract_hardening.py` — 17 regression tests (every fix
  has a test that fails on 0.4.36 behaviour).
- `tests/test_agent_surface.py`, `tests/test_types_and_diff.py` — two
  assertions updated; they encoded the old silent behaviour.
- `tests/test_mcp_server.py` — tool list + spawn the in-repo server
  (`sys.executable -m ifcfast.mcp_server`) instead of PATH's
  `ifcfast-mcp`, so the test exercises the code under test.
- `AGENTS.md`, `CHANGELOG.md` — contract updates per repo CLAUDE.md.

## Verification

- Suite: 95 passed, 12 skipped (baseline 78/12; skips unchanged).
- Original repros re-run against the repo layer on ARK_E (8 873
  products): diff one-rename = exactly 1 changed in cold/cold,
  warm/warm and mixed cache states (was 8 769 mixed); truncated-90%
  open raises; preview/filter/by_type typos raise with vocabulary;
  CLI bad content exits 1 clean; `dir(ifcfast)` leak-free.
- products_in over 13 ARK_E storeys: 0.108 s, Σ(storeys) == building
  == 8 381 (consistency the fast path used to break); the 492
  unresolved storey_of products are exactly the opening elements.
