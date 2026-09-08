#!/usr/bin/env bash
# Build the browser bundle for `ifcfast-wasm` (GH #172).
#
#   crates/wasm/pkg/   <- wasm-bindgen `--target web` output (gitignored)
#
# Toolchain: rustup lives user-locally in ~/.cargo/bin on this box; the
# system Rust (Arch) has no wasm target. `wasm-opt` is optional — when
# it is not on PATH the unoptimised (but already `--release`) module
# ships as-is.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"

target_dir="${CARGO_TARGET_DIR:-$root/target}"
wasm="$target_dir/wasm32-unknown-unknown/release/ifcfast_wasm.wasm"
out="$here/pkg"

echo "==> cargo build --release --target wasm32-unknown-unknown"
cargo build --manifest-path "$root/Cargo.toml" \
  -p ifcfast-wasm --target wasm32-unknown-unknown --release

echo "==> wasm-bindgen --target web"
rm -rf "$out"
wasm-bindgen "$wasm" --out-dir "$out" --target web

if command -v wasm-opt >/dev/null 2>&1; then
  echo "==> wasm-opt -Oz"
  wasm-opt -Oz "$out/ifcfast_wasm_bg.wasm" -o "$out/ifcfast_wasm_bg.wasm"
else
  echo "==> wasm-opt not on PATH — skipping (module ships unoptimised)"
fi

ls -l "$out"

# Node gates. `IFCFAST_WASM_SKIP_TESTS=1` opts out entirely.
#
# limits.mjs (the GH #175 zip-bomb guard) builds its own archives from
# `tests/fixtures/minimal.ifc`, so it runs on a clean checkout. parity
# and stream need `.local-samples/Duplex_A_20110907.ifc` (not in the
# repo); those are skipped with a note when it is absent.
if [[ "${IFCFAST_WASM_SKIP_TESTS:-0}" == "1" ]]; then
  echo "==> tests skipped (IFCFAST_WASM_SKIP_TESTS=1)"
else
  echo "==> node test/limits.mjs"
  node "$here/test/limits.mjs"

  if [[ ! -f "$root/.local-samples/Duplex_A_20110907.ifc" ]]; then
    echo "==> parity/stream skipped — .local-samples/Duplex_A_20110907.ifc not present"
  else
    echo "==> node test/parity.mjs"
    node "$here/test/parity.mjs"
    echo "==> node test/stream.mjs"
    node "$here/test/stream.mjs"
  fi
fi
