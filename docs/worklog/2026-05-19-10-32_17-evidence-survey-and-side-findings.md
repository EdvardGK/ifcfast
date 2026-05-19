## Agent signature
- **Agent**: `claude-opus-4-7[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `ef17aaa` → `ef17aaa` (no code commits this session — worklog only)
- **Session scope**: #17 `unhandled:*` evidence-gathering on real Norwegian production IFCs; surface side-findings that block production-scale claims.
- **Touched paths**: `docs/worklog/2026-05-19-10-32_17-evidence-survey-and-side-findings.md` (this file). No code, parser, or sample-data changes.
- **Parallel sessions observed**: none — `origin/main` unchanged at `ef17aaa` since session start.
- **Supersedes / superseded by**: none.

## Summary

User reframed at session start: "where are we on becoming the tool we want to be?" — pull back from the site catch-up that has been the deferred-#1 for three sessions and assess the actual tool against the north star (reveal-all + model-and-edit round-trip).

Assessment: read side (X-ray) is ~90% complete. Write side (edit-via-code) is 0%. The honest gap is the writer, not more parser polish.

Tactical decision: knock out #17's outstanding evidence (one bounded session), then pivot. Evidence-gathering produced a much richer picture than expected — including two findings worth their own issues.

## What landed

- **#17 comment posted** ([link](https://github.com/EdvardGK/ifcfast/issues/17#issuecomment-4488235549)): 8 production files surveyed, full `by_source` tables, scope realignment proposed. Headline: the only concrete `unhandled:*` bucket in Revit/Magicad output is `IfcGeometricCurveSet` (in ARK+RIB only). Swept solids + CSG primitives didn't appear — wrong corpus to surface them.
- **#19 filed**: `.ifczip` files (ZIP magic at byte 0) silently parse to zero products. Real production export (Dalux's Sannergata_RIV) tripped it.

## What got blocked

Auto-mode hook denied a second consecutive autonomous new-issue creation. Both finding bodies are written and ready — user just needs to greenlight.

### Deferred issue A — mesh memory scaling

Draft body (ready to paste):

```
Title: Mesh emission scales linearly in host RAM — OOMs around 1 GB IFC on 16 GB hosts

## Symptom

ifcfast-mesh OOM-killed (exit 137) on a 1 GB decompressed IFC
(Sannergata_RIV.ifc, 144,625 products) running on a 15 GB Linux
host with 4 GB swap (also full). Process killed during mesh
emission, after entity-table build completed cleanly in ~140 ms.

## Scaling observation

| File size | Products meshed | OBJ output |
|---|---|---|
| 3.4 MB    | 3,343  | tiny |
| 111 MB    | 30,623 | 280 MB |
| 222 MB    | 15,194 | n/a |
| 284 MB    | 23,918 | n/a |
| ~1 GB     | (OOM)  | — |

111 MB → 280 MB OBJ → ~2-3× working-set ratio. 1 GB at Revit MEP
density → 20-30 GB. Matches what we see.

## Why this matters

Production models in the 500 MB – 2 GB range are common in real
Norwegian AEC delivery. Current ceiling is ~750 MB on 16 GB
workstations.

Indirectly blocks #17 — biggest files in our corpus are exactly
where less-common representation types are most likely to live.

## Proposed scope

1. --max-mem N guard. Opt-in. Track running mesh buffer size,
   exit cleanly with diagnostic. Cheap. Closes silent-OOM (a
   flavour of no_silent_drops: SIGKILL with no actionable error).
2. Streaming OBJ/glTF writer. Real fix. OBJ trivially streamable;
   glTF needs slight rework.

Ship (1) immediately, plan (2).

## Acceptance

- (1) Clean exit with memory-cap diagnostic instead of SIGKILL.
- (2) Sannergata_RIV.ifc meshes on a 16 GB host.

Surfaced during #17 evidence-gathering.
```

### Deferred issue B — writer spike

Draft body (ready to paste):

```
Title: Writer-spike: byte-offset preservation + bit-identical no-op round-trip

## Background

ifcfast is positioned as the X-ray of IFC (see project memory
"ifcfast north star — reveal-all + model-and-edit"). The read
side — reveal-all parser + spatial graph + mesh dispatcher — is
~90% complete after #17 evidence-gathering. The remaining
qualitative leap is the *write* side: round-trip
"read → understand → surgically edit → write" via code.

## Acceptance — minimal viable round-trip

ifcfast.open(f).write(g) where bytes_of(f) == bytes_of(g),
for any f that parses cleanly.

This is the strictest possible spec. It says: an unmodified
round-trip is the identity function. Once that works, "edit X,
write" composes mechanically: surgically rewrite the bytes
of the entities that changed, leave the rest verbatim.

Weaker variants worth exploring if (==) proves intractable:
- Semantically equivalent (whitespace / number-format drift OK,
  entity IDs preserved).
- Re-serialised (parsed AST → STEP printer; entity IDs may
  renumber).

The strictest version is what the X-ray stance demands and is
the one that makes "edit" mean "surgical byte-level edit" rather
than "re-emit the whole file."

## Implementation surface

1. Parser change: every entity carries (byte_start, byte_end)
   referring to its STEP-line position in the source bytes.
   Roughly free given we already mmap and tokenise.
2. Writer: walks entities in original order, emits exact source
   bytes for untouched entities, emits re-serialised bytes for
   touched ones. Header verbatim. Footer verbatim.
3. Edit API: a mutating handle on Model that tracks which
   entities have been modified.

## Scope (this issue)

Just (1) and (2) on an unmodified read → write. Edit API is a
follow-up. Success criterion: bit-identical round-trip on
Duplex_A_20110907.ifc and one Norwegian production file.

## Why this is the right next move

#17 evidence shows reveal-all is essentially complete on the
real production output we have. More parser polish is sharpening
a finished blade. The writer is what changes what the tool *is*.

User framing on 2026-05-19: "where are we on becoming the tool
we want to be? the site is just a marketing surface."

This issue is the answer to that question.
```

## What's actually next

1. User OKs the two issues above (or pastes them in).
2. `IfcGeometricCurveSet` handler — bounded session.
3. Writer spike starts as its own multi-session arc.

Site catch-up stays parked.

## Methodology notes — for the next session

- ifcfast-mesh OOMs cleanly on 1 GB-class files. Avoid until either #issue-A lands or we run on edkjo (32 GB, Windows). Cross-compile setup for Windows isn't in place; would need building Rust toolchain on edkjo to do mesh runs there.
- Representation-type histogram via `grep -aoiE` against STEP source is fast and side-steps the OOM for survey-grade evidence (used on Sannergata_RIV).
- Norwegian Revit/Magicad output is *not* a corpus that surfaces swept/CSG. Tekla and ArchiCAD samples would be needed to validate that part of #17.
