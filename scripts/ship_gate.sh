#!/bin/bash
# ifcfast ship gate — ONE chained script, never fan out (16 GB box OOMs on
# parallel cargo builds / ifcopenshell runs). See .claude/skills/oracle-gate.
#
#   scripts/ship_gate.sh <out-dir> [--skip-build] [--skip-clash] [--skip-pytest]
#
# Steps: cargo test → csg-only check → maturin develop --release → class_sweep
# CUT ×4 → class_sweep NOCUT ×4 (GH #194) → 5 Solibri clash truth rounds →
# mesh_roundtrip ×4 → corpus pytest. Every step prints `<name>_rc=<code>`;
# the last line is `##### <time> DONE`. Run detached and watch the log:
#
#   setsid nohup scripts/ship_gate.sh /path/out > /path/out/log.txt 2>&1 &
#
# Local-only inputs (scratch/ is gitignored): scratch/g55/G55_{ARK,RIB,RIE,RIV}.ifc,
# scratch/g55/baselines/*.json, scratch/g55/solibri/**. ifcopenshell volumes are
# reused via --refresh-fast; delete scratch/g55/cache*/<MODEL>_sweep*.json to
# force a full oracle recompute. NEVER edit this file while a gate is running:
# bash reads scripts incrementally and a running gate would execute the new text.
set -o pipefail
OUT=${1:?usage: ship_gate.sh <out-dir> [--skip-build] [--skip-clash] [--skip-pytest]}; shift
SKIP_BUILD=0; SKIP_CLASH=0; SKIP_PYTEST=0
for a in "$@"; do case "$a" in
  --skip-build) SKIP_BUILD=1;; --skip-clash) SKIP_CLASH=1;; --skip-pytest) SKIP_PYTEST=1;;
  *) echo "unknown flag $a"; exit 2;; esac; done
cd "$(dirname "$0")/.." || exit 2
mkdir -p "$OUT"
LOCK=/tmp/ifcfast-build.lock
step() { echo; echo "##### $(date +%T) $1"; }
# shellcheck disable=SC1091
source .venv/bin/activate

step "cargo test (default)"
flock $LOCK cargo test -p ifcfast-core 2>&1 | grep -E "^test result|FAILED|panicked|^error" | tail -8; echo "cargo_rc=${PIPESTATUS[0]}"
step "cargo check csg-only"
flock $LOCK cargo check -p ifcfast-core --no-default-features --features csg 2>&1 | tail -2; echo "csg_rc=${PIPESTATUS[0]}"

if [ $SKIP_BUILD = 0 ]; then
  step "maturin develop --release"
  # 0-byte target/{maturin,debug}/lib_core.so after interleaved builds makes
  # maturin fail with an ELF parse error; touching lib.rs forces a relink.
  touch crates/core/src/lib.rs
  flock $LOCK env -u CONDA_PREFIX maturin develop --release 2>&1 | tail -2; rc=${PIPESTATUS[0]}; echo "maturin_rc=$rc"
  if [ "$rc" != 0 ]; then echo "GATE ABORT: release build failed"; exit 1; fi
fi
step "installed .so vs sources"
ls -la --time-style=+%F_%T python/ifcfast/_core.abi3.so | awk '{print $5, $6}'
STALE=$(find crates/core/src -name '*.rs' -newer python/ifcfast/_core.abi3.so ! -name lib.rs | head -3)
if [ -n "$STALE" ]; then echo "GATE ABORT: sources newer than the installed .so:"; echo "$STALE"; exit 1; fi

for M in G55_ARK G55_RIB G55_RIE G55_RIV; do
  CD=scratch/g55/cache; [ "$M" = G55_RIV ] && CD=scratch/g55/cache_v35
  step "class_sweep CUT $M (refresh-fast)"
  python -m tests.oracle.class_sweep scratch/g55/$M.ifc --cache-dir $CD --baseline scratch/g55/baselines/$M.json --refresh-fast 2>&1 | tee "$OUT/cut_$M.txt" | tail -12; echo "cut_${M}_rc=${PIPESTATUS[0]}"
done
for M in G55_ARK G55_RIB G55_RIE G55_RIV; do
  step "class_sweep NOCUT $M (refresh-fast)"
  python -m tests.oracle.class_sweep scratch/g55/$M.ifc --mode nocut --cache-dir scratch/g55/cache_nocut --baseline scratch/g55/baselines/${M}_nocut.json --refresh-fast 2>&1 | tee "$OUT/nocut_$M.txt" | tail -12; echo "nocut_${M}_rc=${PIPESTATUS[0]}"
done

if [ $SKIP_CLASH = 0 ]; then
  STAMP=$(date +%Y%m%d%H%M)
  clash_round() { # name bcf ifc...
    local N=$1 B=$2; shift 2; local IFCS=(); for f in "$@"; do IFCS+=(--ifc "$f"); done
    step "clash round $N"
    # isolated cache per round AND per gate run (GH #144: bundle cache is keyed by stem)
    python -m tests.oracle.clash_sweep --bcf "$B" "${IFCS[@]}" --cache-dir "scratch/g55/cache_clash_gate_${STAMP}_$N" \
      --rule-tol '10.1. RIE - RIVv=0.1' --rule-tol 'RIVv=0.01' \
      --baseline scratch/g55/baselines/clash_$N.json --report "$OUT/clash_$N.json" 2>&1 | tail -15; echo "clash_${N}_rc=${PIPESTATUS[0]}"
  }
  S=scratch/g55/solibri; D3=$S/models_del3; D4=$S/models_del4; T13=$S/models_tmk13
  clash_round tmk13_plan5      $S/TMK13_Plan5.bcf          $T13/G55_RIE.ifc $T13/G55_RIV.ifc $T13/G55_ARK.ifc
  clash_round tmk13_plan5_del3 $S/tmk/TMK13_Plan5_Del3.bcf $D3/G55_RIE.ifc $D3/G55_RIV.ifc
  clash_round tmk13_plan5_del4 $S/tmk/TMK13_Plan5_Del4.bcf $D4/G55_RIE.ifc $D4/G55_RIV.ifc
  clash_round tmk12_plan2_del3 $S/tmk/TMK12_Plan2_Del3.bcf $D3/G55_ARK.ifc $D3/G55_RIE.ifc $D3/G55_RIV.ifc $D3/G55_RIB_Prefab.ifc
  clash_round tmk12_plan2_del4 $S/tmk/TMK12_Plan2_Del4.bcf $D4/G55_ARK.ifc $D4/G55_RIE.ifc $D4/G55_RIV.ifc
fi

step "mesh_roundtrip (4 disciplines)"
python -m tests.oracle.mesh_roundtrip scratch/g55/G55_ARK.ifc scratch/g55/G55_RIB.ifc scratch/g55/G55_RIE.ifc scratch/g55/G55_RIV.ifc 2>&1 | tail -12; echo "roundtrip_rc=${PIPESTATUS[0]}"

if [ $SKIP_PYTEST = 0 ]; then
  step "pytest full with corpus"
  export IFCFAST_CORPUS="$PWD/scratch/g55/G55_ARK.ifc:$PWD/scratch/g55/G55_RIB.ifc:$PWD/scratch/g55/G55_RIE.ifc:$PWD/scratch/g55/G55_RIV.ifc"
  python -m pytest tests -q -p no:cacheprovider 2>&1 | tail -15; echo "pytest_rc=${PIPESTATUS[0]}"
fi
step "DONE"
