# Parallel dev worktrees

One agent per worktree, each with its own venv, debug `.so` and cargo
target, so several code changes can be developed at once on a 16 GB box
without ever running two builds (or two ifcopenshell processes) together.

```
scripts/worktree/new_worktree.sh <name> [base-ref]   # ~6 min cold; builds under the lock
cd ~/.cache/ifcfast-dev/wt/<name>
export CARGO_TARGET_DIR=$PWD/target TMPDIR=$PWD/target/tmp
flock /tmp/ifcfast-build.lock env -u CONDA_PREFIX VIRTUAL_ENV=$PWD/.venv .venv/bin/maturin develop
flock /tmp/ifcfast-build.lock cargo test -p ifcfast-core
flock /tmp/ifcfast-oracle.lock .venv/bin/python <anything that imports ifcopenshell>
scripts/worktree/remove_worktree.sh <name>            # keeps branch wt/<name>
```

Rules: `/tmp/ifcfast-build.lock` around EVERY cargo/maturin/wasm build
(`scripts/ship_gate.sh` holds the same lock); `/tmp/ifcfast-oracle.lock`
around every ifcopenshell-loading process; never `maturin develop
--release` in a worktree (the release `.so` in the main tree's `.venv` is
the gate build). Worktrees live under `~/.cache/ifcfast-dev/wt`, never
under /tmp (tmpfs quota). Footprint ≈ 2.1 GB each.
