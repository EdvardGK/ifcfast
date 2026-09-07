//! Monotonic-clock shim (GH #172).
//!
//! `std::time::Instant::now()` compiles on `wasm32-unknown-unknown` but
//! panics at runtime — the target has no monotonic clock of its own. The
//! browser does: `performance.now()`, which the `web-time` crate wraps
//! behind the exact `std::time::Instant` API.
//!
//! Every timer in the pure-Rust core imports `Instant` from here so the
//! native build keeps `std::time::Instant` verbatim (zero cost, zero
//! behaviour change) while the wasm build gets a clock that works.
#[cfg(target_arch = "wasm32")]
pub use web_time::Instant;

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;
