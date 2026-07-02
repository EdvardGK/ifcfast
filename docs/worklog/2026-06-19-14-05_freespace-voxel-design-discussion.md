## Agent signature
- **Agent**: `claude-opus-4-8[1m]`
- **Working tree**: `/home/edkjo/workspace/inbox/ifcfast`
- **Branch**: `main` @ `a9330c6` → `a9330c6` (no commits — design-only session)
- **Session scope**: Design sounding-board on voxelizing a BIM model, specifically free-space voxelization as the generalization of the reroute C-space field.
- **Touched paths**: none (discussion only; one GH issue filed)
- **Parallel sessions observed**: none (no commits on origin/main during window)
- **Supersedes / superseded by**: none

## Summary
Pure design discussion — no code. User asked whether voxelizing a BIM model
makes sense, and whether voxelizing the *empty* space inside the bounding box is
useful. Conclusion: solid voxelization is a niche derived product (fights
ifcfast's measurement strengths), but **free-space voxelization + Euclidean
distance transform is the high-value direction** — it's the natural
generalization of the GH #63 reroute clearance field, and it lets ifcfast derive
rooms and portals from hard mesh geometry without depending on IfcSpace. Filed
as GH #115.

## Changes
- No source changes.
- Filed **GH #115** — "Free-space voxel field + distance transform: geometric
  room/portal derivation, classification-driven rasterization layers".

## Technical Details
Key architectural conclusions reached:

1. **Division of labor.** ifcfast owns *physical connectivity* (hard boundaries:
   walls/slabs → void disconnects → connected-component rooms). The space model
   owns *semantic partition* (soft/virtual boundaries: open-plan zone lines,
   lease boundaries — no physical separation, must never be invented from
   geometry). Where IfcSpace exists we can cross-validate it against geometric
   truth.

2. **Empty-space > solid.** The valuable half is the void complement, refined by
   flood-filling from a boundary cell to label exterior; remaining empty =
   interior void. Enables: routing C-space, plenum detection, room inference
   without IfcSpace, clearance/accessibility via EDT, and an airtightness
   diagnostic (interior flood-fill leaking to exterior = envelope gap at voxel
   resolution).

3. **Doors are the crux, and the lever is selective rasterization.** Doors
   modeled as solids block flood-fill (rooms disconnect); modeled open/frame-only
   they leak (rooms merge). ifcfast already classifies products
   (`is_meshable_product`, product-type membership), so *what goes in the grid*
   is a per-question knob: walls+slabs only → connected void + portal graph;
   +doors → clean room segmentation. Compute both from one classified mesh set.

4. **Bottlenecks fall out of geometry, no door semantics needed.** A doorway is a
   constriction in the void. EDT → watershed ridges / medial-axis local minima =
   portals, even when IfcDoor is missing/wrong (cross-snap when present). EDT
   value ×2 = clear width everywhere → egress-width / wheelchair compliance for
   free.

5. **Implementation reuse.** Half-built already: reroute C-space clearance field
   (GH #63) + clash broad-phase spatial bucketing. Must be sparse (hashed grid /
   octree / VDB narrow-band) — dense per-cell u32 IDs hit 100+ MB at 50 mm.

6. **Two grids, not one.** *Architectural void* (structure+walls only) for
   room/portal topology; *clearance void* (everything incl. furniture/MEP) for
   accessibility. Same machinery, different classified input set.

Failure modes flagged: resolution floor (~50 mm needed for trustworthy egress
width vs ~100 mm fine for topology); vertical leakage through stairs/shafts/slab
openings (make floor-coupling a switch); clutter fragmentation (hence two grids).

## Next
- Decide whether to prototype the sparse free-space grid + EDT as a generalization
  of the reroute clearance field, or keep it as a tracked design (GH #114) until
  the reroute story (GH #63) firms up (tracked as GH #115).
- If prototyping: sketch the sparse-grid data structure reusing clash broad-phase
  bucketing; start with architectural-void connected-component room derivation on
  a G55 floor.

## Notes
- This composes with the unresolved reroute *demo framing* problem
  ([[demo-framing-needs-work]]) — geometric room/portal derivation could give the
  reroute demo a concrete spatial story (plenum-aware routing through real
  portals) rather than abstract free-space.
- No release, no commits — nothing to tag.
