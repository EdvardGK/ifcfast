"""Tier 0 — read the STEP header without invoking ifcopenshell.

A STEP header is a fixed-form prelude before the DATA; section. We read
the first ~64 KB and pull out FILE_DESCRIPTION, FILE_NAME, FILE_SCHEMA.
This is enough to decide which schema to load, identify the authoring
app, and compute a stable cache key — all without touching the heavy
parser.

Typical cost: 30-80 ms even on a 500 MB file (we read a fixed prefix).
"""

from __future__ import annotations

import hashlib
import re
import time
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


_ZIP_MAGIC = b"PK\x03\x04"


def _read_step_prefix(p: Path, n_bytes: int) -> bytes:
    """Read up to `n_bytes` of STEP text from `p`. Transparently
    decompresses ZIP archives (the `.ifczip` convention — and the
    common in-the-wild case where an ifczip ships with a plain `.ifc`
    extension, which most tools mis-read as "corrupted STEP"). Picks
    the largest `.ifc` / `.step` / `.stp` member by uncompressed size,
    same rule as the Rust `source::open` so the Python header and the
    Rust parser agree on which member to read.
    """
    with p.open("rb") as f:
        magic = f.read(len(_ZIP_MAGIC))
        f.seek(0)
        if magic == _ZIP_MAGIC:
            with zipfile.ZipFile(f) as zf:
                steps = [
                    info
                    for info in zf.infolist()
                    if info.filename.lower().endswith((".ifc", ".step", ".stp"))
                ]
                if not steps:
                    raise ValueError(
                        f"Archive contains no .ifc / .step / .stp member: {p}"
                    )
                best = max(steps, key=lambda info: info.file_size)
                with zf.open(best) as member:
                    return member.read(n_bytes)
        return f.read(n_bytes)


_HEADER_READ_BYTES = 64 * 1024
_HASH_HEAD_BYTES = 4 * 1024 * 1024
_HASH_TAIL_BYTES = 4 * 1024 * 1024

# Bump this whenever the *meaning* of a cached parquet column changes —
# i.e. when the same input IFC, run through a new parser version, would
# yield different output bytes in any of the cached tables (products,
# psets, materials, quantities, classifications, drift, ...).
#
# This is independent of the package `__version__`: a bugfix release
# that only touches docs or non-cached code shouldn't invalidate user
# caches, while a release that changes a numeric scaling (like the
# v0.4.1 materials.thickness_mm metres→mm fix) MUST.
#
# When you bump, mention which observable cache field changed in the
# release notes so users with large file caches understand the cost of
# the re-extract.
#
# History:
#   1 — implicit baseline through v0.4.0
#   2 — v0.4.1: materials.layer_thickness_mm now scaled to mm via
#       unit_scale (was raw IFC value, silently in metres for
#       metre-authored files); products table now includes IfcSpace
#       rows with name/psets; instances substrate emits null-rep rows
#       for geometryless products.
#   3 — v0.4.4: drift.parquet gains `aabb_volume` (Float32) and
#       `mesh_quality` (Utf8, "closed"/"open_shell"/"degenerate")
#       columns. Old caches lack these and would KeyError on Python
#       readers that select them.
#   4 — v0.4.6: materials.parquet gains `fraction` (Float64, nullable)
#       — populated for `role="constituent"` rows with the IFC4
#       IfcMaterialConstituent.Fraction value. Lets composite-material
#       analytics ("what % of this RC beam is rebar?") work without a
#       separate raw-IFC re-parse.
_CACHE_SCHEMA_VERSION = 4

_FIELD_RE = re.compile(r"\(\s*(.*?)\s*\)\s*;", re.DOTALL)


@dataclass(frozen=True)
class IFCHeader:
    """Result of Tier 0 header parsing."""

    path: str
    size_bytes: int
    mtime_ns: int
    schema: str  # 'IFC2X3', 'IFC4', 'IFC4X3', ...
    description: list[str] = field(default_factory=list)
    name: Optional[str] = None
    time_stamp: Optional[str] = None
    author: list[str] = field(default_factory=list)
    organization: list[str] = field(default_factory=list)
    preprocessor_version: Optional[str] = None
    originating_system: Optional[str] = None
    authorisation: Optional[str] = None
    cache_key: str = ""  # short hex digest of size + head + tail
    parse_seconds: float = 0.0

    @property
    def size_mb(self) -> float:
        return self.size_bytes / (1024 * 1024)

    @property
    def authoring_app(self) -> Optional[str]:
        """Alias for ``originating_system`` (STEP ``FILE_NAME`` slot 6).

        Note this can differ from :attr:`Model.authoring_app`, which reads
        ``IfcApplication.ApplicationFullName`` from the entity table. The
        STEP header records the *exporter* that wrote the file; the
        IfcApplication entity records the *authoring tool* the user worked
        in. They often disagree (e.g. "Graphisoft - Archicad - 29.0.2" vs
        "Archicad 29.0.2 (3200) NOR FULL"). Both are exposed under the
        same name for ergonomics; use ``originating_system`` if you want
        the STEP-spec terminology.
        """
        return self.originating_system


def header(path: str | Path) -> IFCHeader:
    """Parse the STEP header of an IFC file."""
    p = Path(path)
    if not p.exists():
        raise FileNotFoundError(f"IFC file not found: {p}")

    started = time.time()
    stat = p.stat()
    size = stat.st_size
    mtime_ns = stat.st_mtime_ns

    prefix = _read_step_prefix(p, _HEADER_READ_BYTES)
    text = prefix.decode("utf-8", errors="replace")

    if not text.lstrip().startswith("ISO-10303-21"):
        if "ISO-10303-21" not in text[:1024]:
            raise ValueError(f"Not an ISO-10303-21 STEP file: {p}")

    fd = _extract_block(text, "FILE_DESCRIPTION")
    fn = _extract_block(text, "FILE_NAME")
    fs = _extract_block(text, "FILE_SCHEMA")

    description = _parse_string_list(fd, 0) if fd else []
    schemas = _parse_string_list(fs, 0) if fs else []
    schema = schemas[0] if schemas else "UNKNOWN"

    name = _parse_string(fn, 0) if fn else None
    time_stamp = _parse_string(fn, 1) if fn else None
    author = _parse_string_list(fn, 2) if fn else []
    organization = _parse_string_list(fn, 3) if fn else []
    preprocessor_version = _parse_string(fn, 4) if fn else None
    originating_system = _parse_string(fn, 5) if fn else None
    authorisation = _parse_string(fn, 6) if fn else None

    cache_key = _compute_cache_key(p, size)

    return IFCHeader(
        path=str(p.resolve()),
        size_bytes=size,
        mtime_ns=mtime_ns,
        schema=schema,
        description=description,
        name=name,
        time_stamp=time_stamp,
        author=author,
        organization=organization,
        preprocessor_version=preprocessor_version,
        originating_system=originating_system,
        authorisation=authorisation,
        cache_key=cache_key,
        parse_seconds=time.time() - started,
    )


def _extract_block(text: str, keyword: str) -> Optional[str]:
    idx = text.find(keyword)
    if idx < 0:
        return None
    rest = text[idx + len(keyword):]
    m = _FIELD_RE.match(rest.lstrip())
    if not m:
        return None
    return m.group(1)


def _parse_string(body: str, position: int) -> Optional[str]:
    fields = _split_top_level(body)
    if position >= len(fields):
        return None
    raw = fields[position].strip()
    if raw in ("$", "*", ""):
        return None
    if raw.startswith("'") and raw.endswith("'"):
        return raw[1:-1].replace("''", "'")
    return raw


def _parse_string_list(body: str, position: int) -> list[str]:
    fields = _split_top_level(body)
    if position >= len(fields):
        return []
    raw = fields[position].strip()
    if not raw.startswith("(") or not raw.endswith(")"):
        return []
    inner = raw[1:-1]
    out = []
    for part in _split_top_level(inner):
        s = part.strip()
        if s.startswith("'") and s.endswith("'"):
            out.append(s[1:-1].replace("''", "'"))
        elif s in ("$", "*", ""):
            continue
        else:
            out.append(s)
    return out


def _split_top_level(body: str) -> list[str]:
    out: list[str] = []
    depth = 0
    in_string = False
    start = 0
    i = 0
    n = len(body)
    while i < n:
        c = body[i]
        if in_string:
            if c == "'":
                if i + 1 < n and body[i + 1] == "'":
                    i += 2
                    continue
                in_string = False
        else:
            if c == "'":
                in_string = True
            elif c == "(":
                depth += 1
            elif c == ")":
                depth -= 1
            elif c == "," and depth == 0:
                out.append(body[start:i])
                start = i + 1
        i += 1
    out.append(body[start:])
    return out


def _compute_cache_key(p: Path, size: int) -> str:
    """sha256 of (schema_version, size, head 4MB, tail 4MB).

    Including the schema version means a parser change that alters the
    *meaning* of any cached column (e.g. v0.4.1 normalising
    `layer_thickness_mm` to actual millimetres) gets a different cache
    directory, and old caches become inert rather than serving stale
    numbers. See [`_CACHE_SCHEMA_VERSION`] for the bump policy.
    """
    h = hashlib.sha256()
    h.update(_CACHE_SCHEMA_VERSION.to_bytes(4, "little"))
    h.update(size.to_bytes(8, "little"))
    head_n = min(_HASH_HEAD_BYTES, size)
    tail_n = min(_HASH_TAIL_BYTES, max(0, size - head_n))
    with p.open("rb") as f:
        h.update(f.read(head_n))
        if tail_n > 0:
            f.seek(size - tail_n)
            h.update(f.read(tail_n))
    return h.hexdigest()[:16]
