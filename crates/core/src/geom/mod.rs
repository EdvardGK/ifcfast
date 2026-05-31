//! ifcfast geometry kernel.
//!
//! Layered on `parry3d` + `nalgebra`. Operates on the same per-product
//! triangle data the existing mesh extractor emits — there's no parallel
//! representation. The kernel's job is to *operate on* those triangles:
//!
//! * **Broad phase** — pairwise AABB overlap with optional tolerance,
//!   the cheap pre-filter that turns N² of "fully compare every pair"
//!   into M of "actually candidate".
//! * **Narrow phase** — robust mesh-mesh intersection / distance /
//!   containment on the BVH-built `TriMesh` shapes. The honest answer to
//!   "do these two solids clash, and how badly".
//!
//! Future modules will land here as the same foundation grows:
//!
//! * **CSG** (via `manifold3d`) — net booleans for the wall-minus-opening
//!   default behaviour change.
//! * **PCA / OOBB** — principal-axis + oriented bbox fingerprints for
//!   broader cross-discipline duplicate matching.
//! * **Mesh repair / manifold checking** — tighter `mesh_quality`
//!   classification using real topology rather than the cheap divergence
//!   upper bound.
//!
//! Design stance: the kernel is the *engine* layer in the
//! engine-first / filter-after architecture for clash control. It
//! produces canonical per-pair geometric facts (do they intersect, by
//! how much, how far apart). Policy — connectivity dismissal, space
//! attribution, dup classification, BCF emit — lives in the consuming
//! layer above and queries the kernel's parquet output.

pub mod broad_phase;
pub mod mesh;
pub mod narrow_phase;

pub use broad_phase::{pairs_overlapping, AabbF32};
pub use mesh::{build_trimesh, MeshBuildError};
pub use narrow_phase::{intersects, min_distance, NarrowPhaseError};
