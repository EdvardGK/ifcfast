"""Tier 0 — read the STEP header without invoking ifcopenshell.

A STEP header is a fixed-form prelude before the DATA; section. We read
the first ~64 KB and pull out FILE_DESCRIPTION, FILE_NAME, FILE_SCHEMA.
This is enough to decide which schema to load, identify the authoring
app, and compute a stable cache key — all without touching the heavy
parser.

Typical cost: 30-80 ms even on a 500 MB file (we read a fixed prefix).
"""

from __future__ import annotations

import atexit
import hashlib
import os
import re
import tempfile
import time
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional


_ZIP_MAGIC = b"PK\x03\x04"

# Process-scoped cache of fully-decompressed ifczip tempfiles, keyed by
# (canonical source path, source mtime_ns). Populated lazily by
# `native_path_for()` the first time the Rust side needs to read the
# bytes. Without this cache, every `_core.*` call would re-decompress
# the archive via `source::open` — for a 128 MB Sannergata-style ZIP
# that's ~3 s × N calls down a typical pipeline.
_NATIVE_PATH_CACHE: dict[tuple[str, int], Path] = {}


def _is_zip_file(p: Path) -> bool:
    """Magic-byte check — matches the Rust `source::looks_like_zip`."""
    try:
        with p.open("rb") as f:
            return f.read(len(_ZIP_MAGIC)) == _ZIP_MAGIC
    except OSError:
        return False


def _largest_step_member(zf: zipfile.ZipFile) -> zipfile.ZipInfo:
    steps = [
        info
        for info in zf.infolist()
        if info.filename.lower().endswith((".ifc", ".step", ".stp"))
    ]
    if not steps:
        raise ValueError("Archive contains no .ifc / .step / .stp member")
    return max(steps, key=lambda info: info.file_size)


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
                with zf.open(_largest_step_member(zf)) as member:
                    return member.read(n_bytes)
        return f.read(n_bytes)


def native_path_for(p: str | Path) -> Path:
    """Return a path that the Rust `_core.*` functions can open
    directly. For a plain `.ifc` this is `p` itself. For an ifczip
    (whatever the extension), decompress the largest STEP member to a
    process-scoped tempfile once and return that path on every
    subsequent call — so a pipeline that does
    `open() → meshes() → point_cloud() → psets` pays the inflate cost
    once instead of N times. Cache is keyed by `(canonical path,
    mtime_ns)`; touching the source invalidates the entry. Tempfiles
    are removed on process exit via `atexit`.
    """
    p = Path(p)
    if not _is_zip_file(p):
        return p

    stat = p.stat()
    key = (str(p.resolve()), stat.st_mtime_ns)
    cached = _NATIVE_PATH_CACHE.get(key)
    if cached is not None and cached.exists():
        return cached

    fd, tmp_str = tempfile.mkstemp(suffix=".ifc", prefix="ifcfast_unzip_")
    os.close(fd)
    tmp = Path(tmp_str)
    try:
        with p.open("rb") as f, zipfile.ZipFile(f) as zf:
            member = _largest_step_member(zf)
            with zf.open(member) as src, tmp.open("wb") as dst:
                while True:
                    chunk = src.read(1 << 20)
                    if not chunk:
                        break
                    dst.write(chunk)
    except Exception:
        tmp.unlink(missing_ok=True)
        raise

    _NATIVE_PATH_CACHE[key] = tmp
    atexit.register(_unlink_quiet, tmp)
    return tmp


def _unlink_quiet(p: Path) -> None:
    try:
        p.unlink(missing_ok=True)
    except OSError:
        pass


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
#   5 — v0.4.19: instances.parquet gains geometric fingerprint columns
#       — `centroid_xyz` (FixedSizeList[Float32, 3], world-AABB midpoint
#       with placement_xyz fallback for geometryless), `vertex_count`
#       (UInt32), `triangle_count` (UInt32). Enables agents to compose
#       cross-model duplicate detection and broad-phase clash candidate
#       filtering as DuckDB queries directly against the substrate,
#       without recomputing midpoints/counts on every join.
#   6 — v0.4.27: drift.parquet columns now SI-suffixed (GH #31) —
#       `surface_area_m2`, `volume_abs_m3`, `aabb_volume_m3`,
#       `placement_{x,y,z}_m`, `centroid_{x,y,z}_m`, `drift_distance_m`,
#       `max_extent_m`. Values are scaled through the file's
#       `unit_scale` before write, so the drift table joins to
#       `m.mesh_qto()` without any rescaling on the consumer side.
#   7 — v0.4.29: psets.parquet gains `source` column (Utf8, non-null) —
#       `"instance"` for properties declared directly on the product via
#       `IfcRelDefinesByProperties`, `"type"` for properties inherited
#       from `IfcRelDefinesByType → RelatingType.HasPropertySets`. Type
#       inheritance matches ifcopenshell's `should_inherit=True`
#       default (instance shadows same-named type prop). Old caches
#       miss type-level properties entirely (GH #36) — re-extract
#       surfaces ~2% extra rows on type-heavy Revit/Tekla exports.
#   8 — v0.4.29: quantities.parquet `unit_step_id` semantics changed
#       — when `IfcQuantity*.Unit` is null, the column now resolves to
#       the project's `IfcUnitAssignment` IfcSIUnit for that kind
#       (Length→LENGTHUNIT, Area→AREAUNIT, Volume→VOLUMEUNIT,
#       Weight→MASSUNIT, Time→TIMEUNIT; Count stays null —
#       dimensionless). Old caches show every `unit_step_id` as null
#       on the common Revit / ArchiCAD authoring pattern that omits
#       the explicit per-quantity Unit slot (GH #43); re-extract makes
#       the column usable on those files.
#   9 — v0.4.29: psets.parquet row set expands (GH #38). Two changes:
#       (a) IfcPropertyTableValue now surfaces as a single row whose
#       `value` is `"d1=>v1, d2=>v2, ..."` (paired DefiningValues +
#       DefinedValues) and `value_type` is the DefinedValues axis
#       type. Pre-fix these were silently dropped. (b) Any
#       IfcSimpleProperty subclass ifcfast doesn't recognise (e.g.
#       IfcPropertyReferenceValue, future *Value classes) now emits a
#       marker row with `value = None` and
#       `value_type = "unhandled:IFCXXX"` so the blind spot is
#       visible. Filter `m.psets[m.psets.value_type.fillna("").str.startswith("unhandled:")]`
#       to enumerate gaps.
#  10 — v0.4.29: quantities.parquet gains `source` column (Utf8,
#       non-null) and inherits type-attached IfcElementQuantity rows
#       (GH #45). Same convention as the pset side (GH #36):
#       `source = "instance"` for IfcRelDefinesByProperties,
#       `source = "type"` for IfcRelDefinesByType →
#       RelatingType.HasPropertySets quantities, instance shadows
#       same-named type quantities on `(qto_name, quantity_name)`
#       collision. Type-attached quantities are common in
#       component-library exports (one IfcElementQuantity stamped on
#       the type, fanned out across N occurrences). Old caches miss
#       these entirely.
#  11 — v0.4.30: IfcArcIndex segments inside IfcIndexedPolyCurve
#       are now tessellated to the same per-turn budget as
#       IfcCircleProfileDef (32 chord segments per full circle); old
#       caches treated arcs as straight chords between the indexed
#       points, collapsing Revit MEP pipes / ducts to square prisms
#       with -78% volume (GH #48). Cross-section fingerprint columns
#       (centroid_xyz, vertex_count, triangle_count) and mesh_qto
#       outputs shift accordingly on any product authored via
#       IfcArbitraryProfileDef(WithVoids) + IfcIndexedPolyCurve. Old
#       caches must be re-extracted on any RIV / HVAC model.
#  12 — v0.4.31: drift `world_coordinate_baked` detector rewritten
#       (GH #33). Old heuristic (≥80% of products with placement
#       within 1 mm of world origin) only caught Tekla / Revit
#       "everything-at-origin" style and missed every other
#       baked-coords variant. New heuristic: model-level flag
#       triggers when ≥25% of meshed products would carry per-row
#       `drift_severity == "error"` (file must have ≥20 meshed
#       products). When flagged, every `error` / `warn` row demotes
#       to `info` — model-level pattern, not per-element bug.
#       Old caches: severity counts shift on mixed/baked structural
#       models (G55_RIB: 382 `error` → `info`); raw
#       `drift_distance_m` / `drift_ratio` unchanged.
#  13 — v0.4.35: cut_openings W1 + W2 (GH #58).
#       W1 — `mesh::boolean::retag` accumulates the full chain of
#       wrapping composite roles instead of innermost-wins. Cutter
#       fragments at any nesting depth now carry
#       `boolean_second_operand` in their chain, so segment
#       provenance (`MeshSegment.source`, `InstancePart.source`,
#       `instances.parquet.source`) gains additional tokens on
#       nested-boolean trees. Pre-fix files with multi-level
#       `IfcBooleanResult` chains had cutter-side fragments
#       silently mis-classified as host segments — observable in
#       parquet column `source` shape changes for those products.
#       W2 — `Outcome::Unsupported(UnsupportedReason)` typed
#       diagnostics + 14 new flat counter fields on
#       `CutOpeningsStats`. PyO3 dict surfaces new keys
#       `cut_openings_unsupported_*` on every entry point that
#       emits cut stats (`mesh_qto`, `extract_meshes`, `write_gltf`).
#       Legacy keys (`cut_openings_cut`/`_passthrough`/`_fallback`)
#       preserved for back-compat. No mesh / volume / topology
#       output changes today — the detection paths land over
#       W3 (validation gate) / W4 (operator-aware IfcBooleanResult)
#       / W11 (brep cutter pre-flight).
# v14: cut-openings W3 + W4 (GH #58). W4 — `IfcBooleanResult` is now
#       operator-aware: a `.UNION.` / `.INTERSECTION.` second operand
#       is tagged `boolean_union_operand` / `boolean_intersection_operand`
#       (new tokens in the `source` column of `MeshSegment` /
#       `InstancePart` / `instances.parquet`) instead of
#       `boolean_second_operand`, so it is no longer subtracted in cut
#       mode (it was: `first − second` where the file said
#       `first ∪/∩ second`). Net solid for those products changes;
#       reveal-all substrate gains the two new tokens. W3 — half-space
#       clip tolerance is unit-scaled (physical 1 mm in any unit
#       system); metre files unchanged, mm / imperial files clip at the
#       corrected physical scale (net mesh / QTO can shift for those).
#       Newly-emitted counters: `cut_openings_unsupported_union_with_overlap`,
#       `_intersection_not_implemented`, `_non_manifold_input`.
# v15: brep faces with inner `IfcFaceBound` holes are now honoured
#       (GH #53). `mesh::brep::mesh_face` previously dropped inner bounds
#       and fan-filled the outer loop, over-reporting solid volume by the
#       hole area on `IfcFacetedBrep` / shell faces with punched openings
#       (Revit-exported walls: +6 % … +122 %). Hole-bearing faces are now
#       projected to 2D (Newell normal) and ear-clipped with holes; the
#       cheap fan path is retained for the hole-free majority. Net mesh /
#       volume / vertex buffers change for any baked-brep product with
#       face holes — cached substrates from ≤v14 carry the over-filled
#       geometry, so the bump forces re-extraction. No column-shape change.
# v16: volume-reliability columns on `instances.parquet` (GH #60).
#       Four new columns — `volume_mesh_m3` (raw signed-tetra mesh
#       volume), `volume_prism_bound_m3` (footprint×height prism bound,
#       both tripwire and fallback; NaN on closed rows), `volume_reliable`
#       (bool routing flag), `volume_method` (`"mesh"` / `"prism_fallback"`).
#       `volume_reliable` is true when `volume_m3` is the trustworthy mesh
#       value — closed, OR a non-manifold whose volume is within its tight
#       prism bound; false when the mesh volume exceeded the prism bound
#       (provably too big) or the rep is degenerate. `volume_m3` SEMANTICS
#       CHANGE: it is now the best estimate (mesh when reliable, else the
#       prism fallback) instead of the raw mesh value — open-shell rows no
#       longer poison `SUM(volume_m3)`. Column-shape change → re-extraction.
# v17 — synthetic half-space cutter slabs stripped from every no-cut
#       geometry surface (GH #66): drift centroid/aabb/volume
#       columns and the segments table no longer include the ±20 000-
#       unit stand-in fragments, so cached drift/segments parquet from
#       v16 holds foreign-extent values for clipped products.
#       Value change without column change → re-extraction required.
# v18 — three value-changing fixes ship together in this bump:
# v18 (GH #74) — IFC4 IfcDoor / IfcWindow `predefined_type` corrected.
#       The indexer walked args from the right and took the first trailing
#       enum, but IFC4 IfcDoor / IfcWindow (+ their *StandardCase subtypes)
#       carry TWO trailing enums: PredefinedType then
#       OperationType/PartitioningType, plus a UserDefined string. Cached
#       substrates ≤v17 hold the WRONG value (the OperationType /
#       PartitioningType, e.g. `SINGLE_SWING_LEFT` instead of `DOOR`), and
#       `.USERDEFINED.` was collapsed to None. `predefined_type` is now read
#       positionally (third-from-last attribute) and USERDEFINED is
#       preserved. IFC2X3 unaffected (no PredefinedType on door/window).
#       Value change without column change → re-extraction required.
# v18 (GH #77) — raw-UTF-8 STEP string decoding fixed. `decode_string`
#       previously forced Latin-1 on every high byte, mojibaking raw
#       UTF-8 exports (Bonsai/BlenderBIM, some ArchiCAD/Tekla) — a wall
#       named `Dør-æå` came back as `DÃ¸r-Ã¦Ã¥`. Un-escaped high-byte
#       runs are now UTF-8-decoded first, with per-byte Latin-1 only as
#       the fallback for invalid sequences. STEP escapes
#       (`\X\`, `\X2\`, `\S\`) and ASCII are unchanged. String values in
#       names / psets / materials / classifications / diff keys change
#       for any raw-UTF-8 source file → re-extraction required.
# v18 (GH #73) — imperial files resolve `unit_scale` from `IfcConversionBasedUnit`
#       (GH #73). Length declared via IfcConversionBasedUnit (FOOT / INCH —
#       never an IfcSIUnit) was never parsed, so `unit_scale` stayed null
#       and consumers defaulted to metres: a 3.28× under-scale on every
#       unit-scaled value. The manifest `unit_scale` now reads 0.3048 (ft)
#       / 0.0254 (in), and every value derived through it shifts on
#       imperial files — `materials.layer_thickness_mm`, the SI-suffixed
#       drift columns, `mesh_qto` volumes/areas, and the parquet
#       `ifcfast.unit_scale` schema metadata. Metric (IfcSIUnit) files are
#       byte-identical. Value change without column change → cached
#       substrates of imperial models must be re-extracted.
# v19 (GH #69) — bare `IfcTypeProduct` / `IfcTypeObject` are no longer
#       dropped. These non-abstract base classes (no `*Type` suffix) are
#       what Revit emits for types with no schema-specific subtype — e.g.
#       roof/stair/ramp types on IFC2X3, which has no IfcRoofType. The
#       indexer's TypeObject classifier and the two pset/quantity
#       `is_type_object` membership tests rejected them (they end in
#       PRODUCT / OBJECT, not TYPE), so the type was invisible in
#       `type_objects.parquet`, its occurrences carried `type_guid=None`,
#       and any type-level psets/quantities silently dropped (the GH
#       #36/#45 silent-drop class, resurfacing through the membership
#       filter). Caches ≤v18 are missing those `type_objects` rows, miss
#       the `type_guid` / `type_name` linkage on the affected occurrences
#       in `instances.parquet`, and miss the inherited `source="type"`
#       pset/quantity rows. Row-count change → re-extraction required.
#       G55_ARK (IFC2X3, Revit): 11 bare IfcTypeProduct types now visible;
#       33 Roof/Stair/Ramp occurrences gain their type_guid.
# v19 (GH #72) — STEP section/record framing made comment- and string-aware.
#       The DATA-section scanner previously (a) bailed out of the record
#       walk on the first `/* */` comment, silently dropping every record
#       after it; (b) matched a literal `ENDSEC` substring inside a quoted
#       value, truncating the section; and (c) matched `DATA;` inside a
#       HEADER string, starting the section early and emptying the parse.
#       All three were silent wrong-output. Framing now skips comments and
#       quoted strings when locating `DATA;` / `ENDSEC;` and record
#       terminators. Any file using `/* */` comments between records, or
#       containing the literal `ENDSEC`/`DATA;` inside a string, now parses
#       MORE (previously-dropped) entities → cached substrates of such
#       files must be re-extracted. Clean files are byte-identical.
# v19 (GH #75) — classification extractor walks the full `ReferencedSource`
#       chain. A leaf `IfcClassificationReference` whose `ReferencedSource`
#       points at a parent *reference* (multi-level hierarchy — ArchiCAD/
#       Solibri NS 3451, Uniclass tables) was only resolved one hop, so the
#       terminal `IfcClassification` was never reached and
#       `classifications.system_name` / `.edition` / `.source` came back null.
#       The walk now follows parent references (depth-capped 32, cycle-guarded)
#       to the terminal `IfcClassification`. Value change without column change
#       → cached substrates of files with hierarchical classifications must be
#       re-extracted; flat (single-hop) classifications are byte-identical.
# v20 (GH #76) — Rust lexer/extractor escape + set-value correctness batch:
#       (1) encoded literal backslash `\\` now collapses to one `\` (and is no
#       longer misread as an escape introducer); (2) `\X4\...\X0\` non-BMP 8-hex
#       escapes decode (emoji / supplementary-plane chars) instead of passing
#       through as literal text; (3) `\X2\` with an unpaired surrogate is now
#       lossy-decoded (U+FFFD) instead of dropping the whole run. (4) a dangling
#       same-UnitType IfcSIUnit no longer clobbers the IfcUnitAssignment-backed
#       project-default unit, so `quantities.unit_step_id` resolves where it
#       previously came back null. (5) set-valued `RelatingPropertyDefinition`
#       (IfcPropertySetDefinitionSet — inline list or typed wrapper) now binds
#       all member psets/qtos instead of dropping them. (6) IfcPhysicalComplex-
#       Quantity members surface as dot-joined `Wrapper.Leaf` quantity rows
#       instead of vanishing. All six change extracted VALUES or add rows for
#       affected files, so such cached substrates must be re-extracted; files
#       without these constructs are byte-identical.
_CACHE_SCHEMA_VERSION = 20

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


_STEP_TRAILER = b"END-ISO-10303-21"
_TRAILER_PROBE_BYTES = 256


def _check_step_trailer(p: Path, size: int) -> None:
    """Refuse truncated STEP files loudly (GH #70).

    The Rust scan consumes records until EOF, so a file cut mid-stream
    parses cleanly to a *partial* model — silently wrong QTO sums,
    diffs, and clash runs. Every conformant writer terminates the
    exchange structure with ``END-ISO-10303-21;``; its absence from the
    file tail means a truncated download / interrupted copy. ZIP
    containers are exempt: a truncated ZIP already fails its own
    central-directory check (``BadZipFile``) before we get here.
    """
    with p.open("rb") as f:
        magic = f.read(len(_ZIP_MAGIC))
        if magic == _ZIP_MAGIC:
            return
        probe = min(_TRAILER_PROBE_BYTES, size)
        f.seek(size - probe)
        tail = f.read(probe)
    if _STEP_TRAILER not in tail:
        raise ValueError(
            f"IFC file is truncated or unterminated (no END-ISO-10303-21 "
            f"trailer in the last {probe} bytes): {p}"
        )


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

    _check_step_trailer(p, size)

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
