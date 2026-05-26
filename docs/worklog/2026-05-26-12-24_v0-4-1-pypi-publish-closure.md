# Session: v0.4.1 PyPI publish — closure

## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `6b8b020` (no new commits this addendum; just a tag push)
- **Session scope**: Close out the v0.4.1 release by pushing the tag and triggering the CI publish workflow. Substantive work is in the companion worklog [[2026-05-26-09-30_v0-4-1-substrate-reveal-all-and-ifczip]].
- **Touched paths**: none (tag-only push)
- **Parallel sessions observed**: none — `origin/main` unchanged since `6b8b020`
- **Supersedes / superseded by**: continuation of `2026-05-26-09-30_v0-4-1-substrate-reveal-all-and-ifczip.md`

## Summary
Pushed the `v0.4.1` tag to `origin`, triggering the existing
`.github/workflows/release.yml` workflow. CI built multi-platform
wheels (Linux x64/aarch64, macOS x64/arm64, Windows x64) and published
to PyPI via Trusted Publishing in 2m 50s. v0.4.1 is now live on PyPI.

The earlier `maturin publish --release` invocation failed because
`--release` is not a valid `maturin publish` flag (release builds are
implicit). The right path is the CI workflow — same pattern v0.4.0
shipped with — triggered by `git push origin v0.4.1`.

## Changes
- Tagged `v0.4.1` on `6b8b020` (the release commit landed in the prior worklog).
- GitHub Actions run `26450493587` — completed success.
- PyPI: https://pypi.org/project/ifcfast/0.4.1/

## Next
1. Verify the wheel works in a fresh venv: `pip install -U ifcfast==0.4.1`
   and run a quick `ifcfast.open(ifcfast.example_path())` to confirm.
2. Tackle the three residual issues from the testing sweep:
   - `volume_m3 > aabb_volume_m3` open-shell validity flag
   - 5× `panic!` in `parquet_sink` → `Result`
   - IfcSpace `storey_name` resolution via aggregates (not just `contained_in`)

## Notes
- The release CI is on a `v*` tag trigger via PyPI Trusted Publishers
  (OIDC, no token in repo). `git push origin v<version>` is the only
  step needed; local `maturin publish` would only ship a Linux x64
  wheel and would re-introduce token management.
