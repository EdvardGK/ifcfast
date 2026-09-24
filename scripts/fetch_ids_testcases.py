#!/usr/bin/env python3
"""Fetch the buildingSMART IDS 1.0 conformance test cases at a pinned commit.

Design: docs/plans/2026-09-24_ids-validation-design.md §4 ("Suite source").

The cases live in ``buildingSMART/IDS`` (default branch ``development``) under
``Documentation/ImplementersDocumentation/TestCases/``. They are licensed
CC BY-ND 4.0 (c) buildingSMART International Ltd. and are deliberately NOT
vendored into this MIT repo: they are downloaded into a user cache and verified
against a committed manifest.

Layout
------
- Cache:    ``~/.cache/ifcfast/ids-testcases/<sha>/`` holding the TestCases tree
            verbatim (``attribute/``, ``entity/``, ..., ``scripts.md``).
            ``IFCFAST_IDS_TESTCASES=<dir>`` overrides the directory (it then IS
            the case root; no ``<sha>`` subdir is appended).
- Manifest: ``tests/oracle/ids_testcases.lock`` (JSON: repo, path, sha,
            fetched_at, count, files{relpath: sha256}).

Modes
-----
- default:   read the lock, download any missing file at the LOCKED sha, then
             verify every file's sha256 and that the file set matches exactly.
             Any mismatch / missing / extra file -> exit 1 (fail loudly).
- ``--offline``: verify only; never touches the network.
- ``--pin``: resolve the ``development`` head (or ``--sha``), download the full
             tree, write a fresh lock. Use this to move the pin deliberately.

Download mechanism: one git-trees API call (recursive, on the TestCases subtree
only) + ``raw.githubusercontent.com`` per file. The whole repo tarball is ~100 MB;
the subtree is a small fraction of that. Each download is checked against the
git blob sha1 from the tree before it is written. ``GITHUB_TOKEN`` / ``GH_TOKEN``
is used for the API calls if set (unauthenticated: 60 req/h, we need ~4).

Usage::

    python scripts/fetch_ids_testcases.py            # ensure + verify vs lock
    python scripts/fetch_ids_testcases.py --offline  # verify only
    python scripts/fetch_ids_testcases.py --pin      # re-pin to development head
    python scripts/fetch_ids_testcases.py --pin --sha <commit>
    python scripts/fetch_ids_testcases.py --print-dir
"""

from __future__ import annotations

import argparse
import datetime as _dt
import hashlib
import json
import os
import sys
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = "buildingSMART/IDS"
BRANCH = "development"
SUBPATH = "Documentation/ImplementersDocumentation/TestCases"
LICENSE_NOTE = "CC BY-ND 4.0, (c) buildingSMART International Ltd. Not vendored."

REPO_ROOT = Path(__file__).resolve().parents[1]
LOCK_PATH = REPO_ROOT / "tests" / "oracle" / "ids_testcases.lock"
CACHE_BASE = Path.home() / ".cache" / "ifcfast" / "ids-testcases"
ENV_OVERRIDE = "IFCFAST_IDS_TESTCASES"


class FetchError(RuntimeError):
    pass


# --------------------------------------------------------------------------- #
# paths
# --------------------------------------------------------------------------- #
def read_lock(lock_path: Path = LOCK_PATH) -> dict:
    if not lock_path.exists():
        raise FetchError(
            f"lock file missing: {lock_path}. Run with --pin to create it."
        )
    return json.loads(lock_path.read_text(encoding="utf-8"))


def cases_dir(sha: str | None = None) -> Path:
    """Return the case root: env override, else the cache dir for ``sha``
    (default: the sha in the committed lock)."""
    env = os.environ.get(ENV_OVERRIDE)
    if env:
        return Path(env).expanduser()
    if sha is None:
        sha = read_lock()["sha"]
    return CACHE_BASE / sha


# --------------------------------------------------------------------------- #
# network
# --------------------------------------------------------------------------- #
def _api(url: str) -> dict:
    req = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json"})
    tok = os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")
    if tok:
        req.add_header("Authorization", f"Bearer {tok}")
    try:
        with urllib.request.urlopen(req, timeout=60) as r:
            return json.loads(r.read())
    except urllib.error.HTTPError as e:
        raise FetchError(f"GitHub API {url} -> HTTP {e.code}: {e.read()[:300]!r}") from e


def resolve_head(branch: str = BRANCH) -> str:
    return _api(f"https://api.github.com/repos/{REPO}/commits/{branch}")["sha"]


def _subtree_sha(commit: str) -> str:
    """Walk SUBPATH one component at a time (non-recursive tree calls)."""
    tree = _api(f"https://api.github.com/repos/{REPO}/git/commits/{commit}")["tree"]["sha"]
    for part in SUBPATH.split("/"):
        entries = _api(f"https://api.github.com/repos/{REPO}/git/trees/{tree}")["tree"]
        hit = [e for e in entries if e["path"] == part and e["type"] == "tree"]
        if not hit:
            raise FetchError(f"{SUBPATH!r}: component {part!r} not found at {commit}")
        tree = hit[0]["sha"]
    return tree


def list_remote(commit: str) -> dict[str, str]:
    """{relpath under SUBPATH: git blob sha1}."""
    sub = _subtree_sha(commit)
    data = _api(f"https://api.github.com/repos/{REPO}/git/trees/{sub}?recursive=1")
    if data.get("truncated"):
        raise FetchError("git trees API response truncated; cannot list the suite completely")
    return {e["path"]: e["sha"] for e in data["tree"] if e["type"] == "blob"}


def _git_blob_sha1(data: bytes) -> str:
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def _download(commit: str, rel: str, blob_sha: str | None, dest_root: Path) -> None:
    from urllib.parse import quote

    url = f"https://raw.githubusercontent.com/{REPO}/{commit}/{quote(SUBPATH + '/' + rel)}"
    try:
        with urllib.request.urlopen(url, timeout=120) as r:
            data = r.read()
    except urllib.error.HTTPError as e:
        raise FetchError(f"download {rel} -> HTTP {e.code}") from e
    if blob_sha is not None and _git_blob_sha1(data) != blob_sha:
        raise FetchError(f"download {rel}: git blob sha1 mismatch (corrupt transfer?)")
    out = dest_root / rel
    out.parent.mkdir(parents=True, exist_ok=True)
    tmp = out.with_suffix(out.suffix + ".part")
    tmp.write_bytes(data)
    tmp.replace(out)


def download_all(commit: str, rels: dict[str, str | None], dest_root: Path) -> None:
    if not rels:
        return
    print(f"downloading {len(rels)} file(s) @ {commit[:12]} -> {dest_root}", file=sys.stderr)
    with ThreadPoolExecutor(max_workers=8) as ex:
        futs = [ex.submit(_download, commit, r, b, dest_root) for r, b in sorted(rels.items())]
        errors = []
        for f in futs:
            try:
                f.result()
            except FetchError as e:
                errors.append(str(e))
    if errors:
        raise FetchError(f"{len(errors)} download(s) failed:\n  " + "\n  ".join(errors))


# --------------------------------------------------------------------------- #
# verify
# --------------------------------------------------------------------------- #
def sha256_file(p: Path) -> str:
    return hashlib.sha256(p.read_bytes()).hexdigest()


def local_files(root: Path) -> list[str]:
    return sorted(
        p.relative_to(root).as_posix()
        for p in root.rglob("*")
        if p.is_file() and not p.name.endswith(".part")
    )


def verify(root: Path, lock: dict) -> list[str]:
    """Return a list of problems (empty == verified)."""
    problems: list[str] = []
    if not root.is_dir():
        return [f"case dir does not exist: {root}"]
    want: dict[str, str] = lock["files"]
    have = set(local_files(root))
    for rel in sorted(set(want) - have):
        problems.append(f"missing: {rel}")
    for rel in sorted(have - set(want)):
        problems.append(f"unexpected file (not in lock): {rel}")
    for rel in sorted(set(want) & have):
        got = sha256_file(root / rel)
        if got != want[rel]:
            problems.append(f"sha256 mismatch: {rel} (lock {want[rel][:12]}, got {got[:12]})")
    if lock.get("count") != len(want):
        problems.append(f"lock count {lock.get('count')} != {len(want)} files listed")
    return problems


# --------------------------------------------------------------------------- #
# modes
# --------------------------------------------------------------------------- #
def pin(sha: str | None) -> Path:
    commit = sha or resolve_head()
    root = cases_dir(commit)
    remote = list_remote(commit)
    have = set(local_files(root)) if root.is_dir() else set()
    extra = have - set(remote)
    if extra:
        raise FetchError(f"{root} holds files not in {commit}: {sorted(extra)[:5]}... — clear it first")
    # Re-download everything for a pin (cheap; guarantees content == remote).
    download_all(commit, remote, root)
    files = {rel: sha256_file(root / rel) for rel in sorted(remote)}
    lock = {
        "repo": REPO,
        "branch": BRANCH,
        "path": SUBPATH,
        "sha": commit,
        "license": LICENSE_NOTE,
        "fetched_at": _dt.datetime.now(_dt.timezone.utc).replace(microsecond=0).isoformat(),
        "count": len(files),
        "files": files,
    }
    LOCK_PATH.write_text(json.dumps(lock, indent=2, sort_keys=False) + "\n", encoding="utf-8")
    print(f"pinned {commit} ({len(files)} files) -> {LOCK_PATH.relative_to(REPO_ROOT)}", file=sys.stderr)
    return root


def ensure(sha: str | None, offline: bool) -> Path:
    lock = read_lock()
    if sha and sha != lock["sha"]:
        raise FetchError(
            f"--sha {sha} differs from locked sha {lock['sha']}; use --pin to move the pin"
        )
    root = cases_dir(lock["sha"])
    if not offline:
        have = set(local_files(root)) if root.is_dir() else set()
        missing = {rel: None for rel in lock["files"] if rel not in have}
        # blob sha is unknown without an API call; sha256 verify below is the gate.
        download_all(lock["sha"], missing, root)
    problems = verify(root, lock)
    if problems:
        raise FetchError(
            f"IDS test cases at {root} do NOT match {LOCK_PATH.name} "
            f"({len(problems)} problem(s)):\n  " + "\n  ".join(problems[:50])
        )
    print(f"verified {lock['count']} files @ {lock['sha'][:12]} in {root}", file=sys.stderr)
    return root


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--pin", action="store_true", help="resolve head (or --sha), download, write the lock")
    ap.add_argument("--sha", help="commit sha (with --pin: pin this; otherwise must equal the lock)")
    ap.add_argument("--offline", action="store_true", help="verify only, no network")
    ap.add_argument("--print-dir", action="store_true", help="print the case dir and exit")
    a = ap.parse_args(argv)
    try:
        if a.print_dir:
            print(cases_dir(a.sha))
            return 0
        root = pin(a.sha) if a.pin else ensure(a.sha, a.offline)
    except FetchError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 1
    print(root)
    return 0


if __name__ == "__main__":
    sys.exit(main())
