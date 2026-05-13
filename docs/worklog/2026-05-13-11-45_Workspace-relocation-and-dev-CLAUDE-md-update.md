# Session: ifcfast workspace relocation + ~/dev/ overlay documented

## Summary

Caught a layout violation immediately after the extraction worklog — `ifcfast`
had been created as a real directory under `~/dev/projects/`, whereas every
other project there is a symlink into `~/workspace/`. Moved the files to the
canonical `~/workspace/inbox/ifcfast/` location, symlinked it back into
`~/dev/projects/`, and rewrote `~/dev/CLAUDE.md` so future sessions know that
`~/dev/` is a curated symlink overlay onto `~/workspace/`. Also relocated four
historical fastparse worklogs from `ifc-workbench` into the new repo.

## Changes

- `mv /home/edkjo/dev/projects/ifcfast → /home/edkjo/workspace/inbox/ifcfast`
- `ln -s /home/edkjo/workspace/inbox/ifcfast /home/edkjo/dev/projects/ifcfast`
- `/home/edkjo/dev/CLAUDE.md` — replaced the directory-structure preamble with
  an explicit "Real files live in `~/workspace/`; `~/dev/` is a navigation
  shortcut layer" rule. Added a per-subtree mapping table
  (projects → workspace/inbox or workspace/skiplum/dev, spruce →
  spruceforge, resources → workspace/resources). Directive: never create a
  real directory under `~/dev/projects/*` — make the real dir under
  `~/workspace/` and symlink it.
- Moved four worklogs from `ifc-workbench/docs/worklog/` into
  `ifcfast/docs/worklog/` (via `gio trash` on originals):
  - `2026-04-20-03-11_QTO-engine-migration-from-ifc-toolkit.md`
  - `2026-05-10-14-30_Fast-parser-v2-ownership.md`
  - `2026-05-11-08-50_Bbox-investigation-and-handover.md`
  - `2026-05-11-16-30_Fastparse-v3-native-Rust-tier1-bbox-v7-federated-floors-validation-arc.md`
- Created `2026-05-13-08-13_Standalone-repo-extraction-from-ifc-workbench.md`
  earlier in the session for the extraction itself.

## Technical Details

`~/dev/` audit before the fix:

- `~/dev/projects/` — 11 of 12 entries are symlinks into `~/workspace/inbox/`
  or `~/workspace/skiplum/dev/`; only `ifcfast/` was a real directory (and
  `bim-intelligence/`, etc., correctly symlink-back).
- `~/dev/spruce/` — all entries symlink into `~/workspace/spruceforge/`.
- `~/dev/resources/` — all entries symlink into `~/workspace/resources/`.
- `~/dev/archive/` — real directory by design.

`readlink -f /home/edkjo/dev/projects/ifcfast` now resolves to
`/home/edkjo/workspace/inbox/ifcfast`, matching the sibling pattern.

The CLAUDE.md change is durable instruction — future agent sessions opening
in `~/dev/projects/<anything>` will read it before doing anything else, so
the same mistake shouldn't recur.

## Next

- `git init` inside `/home/edkjo/workspace/inbox/ifcfast/` and create the
  first commit on `EdvardGK/ifcfast` whenever the user is ready.
- Consider whether `~/dev/projects/CLAUDE.md` (if it exists) needs a similar
  pointer — not checked.

## Notes

- The relocation preserved everything: `ls` through `~/dev/projects/ifcfast`
  still shows the full tree because of the symlink. No rebuilds needed (the
  `.venv` and `target/` were already cleaned at the end of the extraction
  session).
- ifc-workbench still has `docs/worklog/2026-03-03-17-41_Clash-zone…` — that
  one is unrelated to the parser arc and stays put.
