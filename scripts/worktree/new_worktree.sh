#!/usr/bin/env bash
# usage: new_worktree.sh <name> [base-ref]
set -euo pipefail
REPO=/home/edkjo/workspace/inbox/ifcfast
WT_ROOT=${IFCFAST_WT_ROOT:-/home/edkjo/.cache/ifcfast-dev/wt}
NAME=${1:?usage: new_worktree.sh <name> [base-ref]}
BASE=${2:-HEAD}
WT=$WT_ROOT/$NAME
BR=wt/$NAME
[ -e "$WT" ] && { echo "ABORT: path exists: $WT" >&2; exit 1; }
git -C "$REPO" show-ref --verify --quiet "refs/heads/$BR" && { echo "ABORT: branch exists: $BR" >&2; exit 1; }
git -C "$REPO" worktree add "$WT" -b "$BR" "$BASE"
cd "$WT"
# everything (venv, target, tmp) lives inside the worktree, on disk (/tmp is a quota-limited tmpfs)
mkdir -p "$WT/target/tmp"
DISK=$WT
export TMPDIR="$WT/target/tmp"

PYHOME=$(sed -n 's/^home *= *//p' "$REPO/.venv/pyvenv.cfg")
PYVER=$(sed -n 's/^version *= *//p' "$REPO/.venv/pyvenv.cfg")
echo "== venv ($PYHOME/python3, $PYVER)"
t0=$(date +%s)
"$PYHOME/python3" -m venv .venv
# runtime deps + [dev] extra, WITHOUT the package itself (no maturin build)
"$REPO/.venv/bin/python" - > /tmp/ifcfast-wt-$NAME-reqs.txt <<PY
import tomllib
p = tomllib.load(open("pyproject.toml", "rb"))["project"]
for r in p.get("dependencies", []) + p["optional-dependencies"]["dev"]:
    print(r)
print("maturin")
PY
.venv/bin/pip install -q --disable-pip-version-check -r /tmp/ifcfast-wt-$NAME-reqs.txt
rm -f /tmp/ifcfast-wt-$NAME-reqs.txt  # our own temp file
echo "venv install: $(( $(date +%s) - t0 ))s"

echo "== debug build (serialized via /tmp/ifcfast-build.lock)"
export CARGO_TARGET_DIR="$DISK/target"
t0=$(date +%s)
flock /tmp/ifcfast-build.lock env -u CONDA_PREFIX VIRTUAL_ENV="$WT/.venv" .venv/bin/maturin develop
echo "debug build (incl. lock wait): $(( $(date +%s) - t0 ))s"
.venv/bin/python -c "import ifcfast,sys; print('ifcfast ->', ifcfast.__file__)"

echo "== pytest (non-corpus, non-oracle)"
t0=$(date +%s)
flock /tmp/ifcfast-oracle.lock env -u IFCFAST_CORPUS .venv/bin/python -m pytest tests -q -x -p no:cacheprovider --ignore=tests/oracle || echo "PYTEST FAILED rc=$?"
echo "pytest: $(( $(date +%s) - t0 ))s"

cat <<USAGE

== worktree ready: $WT  (branch $BR)
cd $WT
export CARGO_TARGET_DIR=$DISK/target TMPDIR=$WT/target/tmp
# build (ALWAYS wrapped; one cargo/maturin at a time fleet-wide; never --release unless coordinator says so)
flock /tmp/ifcfast-build.lock env -u CONDA_PREFIX VIRTUAL_ENV=$WT/.venv .venv/bin/maturin develop
# cargo test/check also go through the build lock:
flock /tmp/ifcfast-build.lock cargo test -p <crate>
# any ifcopenshell-loading script / pytest touching corpus or oracle:
flock /tmp/ifcfast-oracle.lock .venv/bin/python <script>
# quick test run (no corpus):
env -u IFCFAST_CORPUS .venv/bin/python -m pytest tests -q -x -p no:cacheprovider --ignore=tests/oracle
# remove when done (branch $BR is kept):
$WT_ROOT/remove_worktree.sh $NAME
USAGE
