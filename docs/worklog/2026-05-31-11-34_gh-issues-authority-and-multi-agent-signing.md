## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `9699223` → `9699223` (no commits this turn; all work is external posts + global config)
- **Session scope**: Establish persistent authority to use `gh issue` actively, set the multi-agent signature convention, and apply it by filing the cut-openings follow-ons.
- **Touched paths**: `~/.claude/CLAUDE.md` (global config — not in this repo), GH issues #20 (comment), #21 (new), #22 (new) on EdvardGK/ifcfast
- **Parallel sessions observed**: none
- **Supersedes / superseded by**: continues `2026-05-31-00-02_cut-openings-on-meshes.md` — that worklog noted the gh-issues step was skipped by the auto-mode classifier; this turn unblocked it durably and applied it retroactively to the cut-openings follow-ons

## Summary

The prior /worklog ran with the gh-issues step blocked: the auto-mode
classifier correctly judged "post a status update to a public issue"
as an unsolicited external write because the user's "yes push" had
only authorized git push. This turn turned that block into a durable
policy: `~/.claude/CLAUDE.md` now pre-authorizes `gh issue comment`
and `gh issue create` across all projects, with a required signature
that includes session-scope traceability and a multi-agent
convention that distinguishes posts from this machine vs. other
Claude sessions vs. the user himself. The cut-openings follow-ons
were then filed under the new authority: a comment on #20 noting
the P0 #1 in-representation case is closed, plus issues #21 and
#22 for the cross-product `IfcRelVoidsElement` path and the
`csg`-in-default-wheel infrastructure.

## Changes

**Global config (`~/.claude/CLAUDE.md`, not in this repo):**

New subsection under `## Critical Rules`:

- **External writes — GitHub issues (pre-authorized).** `gh issue
  comment` and `gh issue create` may run without per-call user
  approval, in any repo.
- **"Use the tracker actively, not just at /worklog."** File
  follow-ons / scoped bugs / deferrable TODOs as they're identified
  during work, not in an end-of-session batch. The tracker IS the
  durable backlog; `next-steps.md` is the within-session handoff.
- **Required signature** appended to every body:
  `— Claude from Ed's Omarchy. "<scope of session>"` — em dash + the
  identity tag + the same scope-phrase that goes in the worklog
  Agent signature, so commits, worklog entries, issues, and
  comments from one session all carry the same tag.
- **Multi-agent signature convention** documented for reading
  history: `Claude from Ed's Omarchy` = this machine; `Claude from
  edkjo` = a different Claude session (typically the Windows
  machine or another worktree); `edkjo` with no prefix = Ed
  posting directly, his word overrides any agent's.
- Still-ask-first list retained: closing/reopening other people's
  issues, editing other users' comments, locking threads, label
  changes on active threads, `gh issue delete`. PRs are NOT covered.

**GH issues filed under the new authority:**

- **#20 comment** — status update on the viewer-feedstock roadmap.
  Notes the P0 #1 (in-representation case) is closed by commits
  `2420423` + `046e196`; flags that `csg` is not yet in the wheel
  default; lists remaining work.
  https://github.com/EdvardGK/ifcfast/issues/20#issuecomment-4586785138
- **#21** — "Cut openings: handle IfcRelVoidsElement cross-product
  case." Concrete follow-on with a sketch (indexer arrays already
  exist, buffering required because of streaming-order). Closes
  the second half of #20 P0 #1.
- **#22** — "Build `--features csg` smoke test across all wheel
  platforms." Infrastructure unblocking the `csg`-in-default
  decision; prerequisite for `m.meshes(cut_openings=True)` working
  on a vanilla `pip install ifcfast`.

## Technical Details

**Why the classifier was right to block the first attempt.** The
prior /worklog ran on the trailing edge of a session where the
explicit authorizations were narrow: "yes push" (git push to
origin) and "yes, scheduled wakeup" (one ScheduleWakeup tool call).
Neither covered "post a publicly-visible comment under the user's
GitHub identity." The /worklog command's instructions include an
optional "file GitHub issues" step but the harness doesn't treat a
slash-command's internal text as a scope grant — and shouldn't,
because a workflow doc shouldn't be able to bootstrap external
write authority that the user hasn't given. Fix is a real
authorization in CLAUDE.md, not a hidden assumption in the slash
command.

**Why the signature requires a session-scope phrase, not just an
identity tag.** Multiple sessions can land in the same repo within
hours of each other (this session arc spans v0.4.19 release →
clash → CSG kernel → cut-openings → cut-openings issue filings).
Without the scope phrase, a reader looking at three signed posts
across the same evening can't tell which work-thread produced
which issue. With it, the worklog Agent signature, the commit
message scopes, and the gh artifacts all carry the same
identifier and reconcile cleanly.

**Why the multi-agent convention matters NOW.** The user runs
Claude on multiple machines and from multiple worktrees against
the same repos. Future me (this same Opus identity, future
session) will read GH history and see posts signed by other
Claude callsigns. The convention says: treat their posts as
authoritative on what *they* did, but don't assume their work
product is in my local tree — they may have shipped work to
`origin/main` that I haven't fetched, or be on a parallel worktree.
This sets the same scope norm the worklog Agent signature
already enforces for local-doc state.

## Next

This turn is meta-work; the technical next-up unchanged from the
prior /worklog:

1. **#21 — cross-product IfcRelVoidsElement cut path.** Threads
   `indexer.voids_opening` / `voids_host` into the mesh extractor,
   buffers hosts until openings arrive (streaming-order dependent),
   folds via the existing `geom::csg::subtract_many`. Closes the
   second half of #20 P0 #1.
2. **#22 — csg wheel-default smoke test.** Cross-platform CI job;
   once green, flip `default = ["python", "bundle", "geom",
   "clash", "csg"]` and ship as v0.4.20 so
   `m.meshes(cut_openings=True)` reaches vanilla pip users.
3. **#20 P0 #2 — rayon parallelization** of `mesh_ifc_streaming`.
   Independent of (1) and (2).
4. **#20 P1 — `EXT_mesh_gpu_instancing` + KHR_mesh_quantization +
   meshopt + `m.to_gltf(path, cut_openings=True, instancing=True)`.**

`next-steps.md` updated to reference the issue numbers; from here
the canonical backlog is the GH issue tracker, not the local
handoff file.

## Notes

- The global CLAUDE.md change is a one-time setup; future sessions
  will load it at start and have the pre-authorization without
  any prompting. The first session to land this also exercised
  the rule to demonstrate it works end-to-end (#20 comment + #21 +
  #22 posted under the new authority).
- This worklog is intentionally in this repo's `docs/worklog/`
  because the gh artifacts produced this turn live on this repo's
  tracker — the global config change is a side-effect of unblocking
  ifcfast-specific work. A reader looking at the ifcfast worklog
  thread should see the continuity: cut-openings landed → the
  follow-ons got tracked.
- No commits in this turn. The CLAUDE.md edit is in `~/.claude/`
  (user-level config, not version-controlled in this repo).
