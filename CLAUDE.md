# Claude instructions for `ifcfast`

This file is loaded automatically by Claude Code sessions opened in
this repo. Keep it short and focused on conventions that aren't
obvious from the code or from `AGENTS.md`.

## Keep AGENTS.md current — it's a public contract

`AGENTS.md` at the repo root is the canonical guide for LLMs and
agent frameworks using `ifcfast`. It is the audience this project
exists for, and it's the file that drifts fastest when nobody is
watching.

**Rule:** any change that adds, removes, or renames an agent-visible
primitive **also touches `AGENTS.md` in the same change**:

- New / renamed / removed Python API method on `Model` or top-level
  `ifcfast.*`.
- New / renamed / removed MCP tool exposed by `ifcfast-mcp`.
- New / renamed / removed CLI subcommand (`ifcfast …`,
  `ifcfast-bundle …`).
- New / renamed / removed substrate parquet column or table.
- Any change to a stable convention (units, naming, missing-value
  encoding, cache-key composition).

Bumping `_CACHE_SCHEMA_VERSION` in `python/ifcfast/header.py` is a
strong signal that `AGENTS.md` needs an update too — the column set
is what agents read.

Also verify `README.md` still links to `AGENTS.md` (currently the
"See AGENTS.md for the full agent guide…" line near the top). If a
README reorg drops the link, restore it.

Tone for `AGENTS.md` edits: direct, decision-tree-flavored,
agent-actionable. No marketing.

## Worklogs

Worklog convention follows the global system rule (Agent signature
header, narrow scope, touched paths, parallel-session declaration).
Location: `docs/worklog/yyyy-mm-dd-hh-mm_short-description.md`.

## Release

`maturin publish` is forbidden locally — see `release-flow` in the
project memory. Releases happen by pushing a `v<version>` tag; CI
does the publish.
