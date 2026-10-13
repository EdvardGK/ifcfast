#!/usr/bin/env bash
# usage: remove_worktree.sh <name>   (removes worktree incl. its .venv/target; keeps branch wt/<name>)
set -euo pipefail
REPO=/home/edkjo/workspace/inbox/ifcfast
WT_ROOT=${IFCFAST_WT_ROOT:-/home/edkjo/.cache/ifcfast-dev/wt}
NAME=${1:?usage: remove_worktree.sh <name>}
git -C "$REPO" worktree remove --force "$WT_ROOT/$NAME"
git -C "$REPO" worktree prune
echo "removed $WT_ROOT/$NAME; branch wt/$NAME kept (git branch -D wt/$NAME to drop)"
