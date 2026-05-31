//! Substrate-aware clash engine.
//!
//! Reads the bundle's `instances.parquet` + `representations.parquet`,
//! runs broad-phase AABB overlap (via `geom::pairs_overlapping`),
//! narrow-phases each candidate pair as a mesh-mesh intersection /
//! distance query (via `geom::intersects` + `geom::min_distance`),
//! and writes `clashes.parquet` next to the inputs.
//!
//! The substrate is the engine's input *and* its addressable identity:
//! agents point at a bundle directory, they get a parquet of clash
//! pairs in the same row-coordinate system they're already querying for
//! types / quantities / materials. No second parse of the source IFC.
//!
//! ```text
//!     bundle/
//!         instances.parquet           # (input — per-product rows)
//!         representations.parquet     # (input — per-rep tri buffers)
//!         clashes.parquet             # (output — pair-level facts)
//! ```
//!
//! ## Engine vs policy
//!
//! This module is the *engine*: it produces canonical per-pair facts
//! (do they intersect; if not, how far apart). It does NOT do
//! connectivity dismissal (wall-meets-slab is *not* a clash), space
//! attribution (room-of-clash), discipline routing, or BCF emit. Those
//! are *policy* and live in the layer above — agents query
//! `clashes.parquet` joined to `instances.parquet` to apply them.
//!
//! ## Coordinate handling
//!
//! Broad-phase uses each instance's world-coord AABB columns
//! (`bbox_min_xyz` / `bbox_max_xyz`) directly — already baked at
//! bundle time.
//!
//! Narrow-phase needs world-coord `TriMesh` shapes. The substrate
//! stores reps in two flavours: `composite` reps carry world-baked
//! triangle buffers (instance transform is identity); `shared_or_direct`
//! reps carry the local mesh and the instance's 4×4 transform maps
//! local → world. The engine bakes world per instance for both,
//! so the narrow phase always sees identity-isometry inputs. This
//! rebuilds the BVH per instance instead of sharing it across
//! instances of the same shared rep — fine for v1, a known optimization
//! handle for later when a real model proves it matters.

pub mod engine;
pub mod sink;
pub mod source;

pub use engine::{clash, ClashError, ClashKind, ClashOptions, ClashPair, ClashReport};
pub use sink::write_clashes_parquet;
