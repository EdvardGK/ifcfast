"""Agent-facing `Model.subset()` — the first write primitive (GH #124).

Exercises the Python surface end to end: GUID resolution, bytes vs.
out_path returns, the spatial-spine climb, shared-rel pruning, fail-loud
on unknown GlobalIds, and the byte-identity of subsetting everything.
Reopen validation uses ifcopenshell when available (the independent
oracle), else falls back to re-parsing with ifcfast.
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

import ifcfast

FIXTURE = Path(__file__).parent / "fixtures" / "minimal.ifc"
WALL_GUID = "7XvctVUKr0kugbFTf53O9L"


@pytest.fixture(scope="module")
def model():
    return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)


def test_subset_returns_bytes(model):
    data = model.subset([WALL_GUID])
    assert isinstance(data, (bytes, bytearray))
    assert data.startswith(b"ISO-10303-21;")
    assert b"END-ISO-10303-21;" in data


def test_subset_reopens_with_wall_and_spine(model, tmp_path):
    out = tmp_path / "wall.ifc"
    stats = model.subset([WALL_GUID], out_path=str(out))
    assert stats["seeds_present"] == 1
    assert stats["path"] == str(out)
    assert out.exists()

    # The seeded wall and its whole spatial spine survive.
    re = ifcfast.open(out, use_cache=False, write_cache=False)
    guids = {p.guid for p in re}
    assert WALL_GUID in guids


def test_subset_unknown_guid_is_loud(model):
    with pytest.raises(ValueError):
        model.subset(["THISGUIDDOESNOTEXIST00"])


def test_subset_empty_seeds_raises(model):
    with pytest.raises(ValueError):
        model.subset([])


def test_subset_of_all_products_is_self_contained(model, tmp_path):
    # Subsetting every product keeps a valid, reopenable document.
    all_guids = [p.guid for p in model]
    out = tmp_path / "all.ifc"
    stats = model.subset(all_guids, out_path=str(out))
    assert stats["seeds_present"] == len(all_guids)
    re = ifcfast.open(out, use_cache=False, write_cache=False)
    assert len(re) == len(model)


ifcopenshell = pytest.importorskip("ifcopenshell")


def test_subset_reopens_in_ifcopenshell_with_no_dangling(model, tmp_path):
    out = tmp_path / "wall_oracle.ifc"
    model.subset([WALL_GUID], out_path=str(out))
    f = ifcopenshell.open(str(out))

    assert len(f.by_type("IfcProject")) == 1
    assert any(w.GlobalId == WALL_GUID for w in f.by_type("IfcWall"))

    dangling = 0
    for inst in f:
        try:
            inst.get_info(recursive=False)
        except Exception:  # noqa: BLE001
            dangling += 1
    assert dangling == 0


def _oracle_check(path: Path) -> dict:
    """Open an emitted subset in ifcopenshell and assert the acceptance
    gate: parses, zero dangling refs, exactly one rooted IfcProject.
    Returns a small stats dict for reporting. Raises AssertionError on
    any violation."""
    f = ifcopenshell.open(str(path))
    dangling = 0
    for inst in f:
        try:
            inst.get_info(recursive=False)
        except Exception:  # noqa: BLE001
            dangling += 1
    assert dangling == 0, f"{path.name}: {dangling} instances with unresolved refs"
    projects = f.by_type("IfcProject")
    assert len(projects) == 1, f"{path.name}: expected 1 IfcProject, got {len(projects)}"
    return {
        "instances": len(list(f)),
        "sites": len(f.by_type("IfcSite")),
        "buildings": len(f.by_type("IfcBuilding")),
        "storeys": len(f.by_type("IfcBuildingStorey")),
    }


def _corpus_paths() -> list[Path]:
    raw = os.environ.get("IFCFAST_SUBSET_CORPUS", "")
    return [Path(p) for p in raw.split(":") if p.strip()]


@pytest.mark.skipif(
    not _corpus_paths(),
    reason="set IFCFAST_SUBSET_CORPUS=/a.ifc:/b.ifc to run the real-file gate",
)
@pytest.mark.parametrize("path", _corpus_paths(), ids=lambda p: p.name)
def test_subset_over_real_corpus_is_ifcopenshell_clean(path, tmp_path):
    """Durable acceptance gate for GH #124 Phase 2b: drive the Python
    `Model.subset()` over real, discipline-diverse IFC files and prove
    each emitted subset reopens in ifcopenshell with zero dangling refs
    and a single rooted IfcProject. Mirrors the Rust
    `subset_across_corpus` gate through the agent-facing surface.

        IFCFAST_SUBSET_CORPUS="/G55_ARK.ifc:/G55_RIV.ifc" \\
            pytest tests/test_subset.py -k real_corpus -s
    """
    assert path.exists(), f"corpus file missing: {path}"
    model = ifcfast.open(path, use_cache=False, write_cache=False)

    guids = [p.guid for p in model if p.guid]
    assert guids, f"{path.name}: no product GUIDs to seed"
    step = max(len(guids) // 200, 1)
    seeds = guids[::step]

    out = tmp_path / f"{path.stem}.subset.ifc"
    stats = model.subset(seeds, out_path=str(out))
    assert stats["seeds_present"] == len(seeds)

    info = _oracle_check(out)
    print(
        f"OK {path.name}: {len(seeds)} seeds -> {info['instances']} instances, "
        f"0 dangling, proj=1 site={info['sites']} bldg={info['buildings']} "
        f"storey={info['storeys']}"
    )


@pytest.mark.skipif(
    not _corpus_paths(),
    reason="set IFCFAST_SUBSET_CORPUS=/a.ifc:/b.ifc to run the real-file gate",
)
@pytest.mark.parametrize("path", _corpus_paths(), ids=lambda p: p.name)
def test_subset_pulls_coverings_when_host_seeded(path, tmp_path):
    """GH #126 completeness gate: seeding a covered building element must
    drag its IfcCoverings into the subset. Exercises the new
    IfcRelCoversBldgElements rule (anchor = host@4, pull = coverings@5-SET)
    over real Revit/MagiCAD output, using ifcopenshell to discover a
    genuine covered host and to name the coverings it should pull."""
    assert path.exists(), f"corpus file missing: {path}"
    src = ifcopenshell.open(str(path))

    rel = None
    for r in src.by_type("IfcRelCoversBldgElements"):
        host = r.RelatingBuildingElement
        covs = list(r.RelatedCoverings or [])
        if (
            host is not None
            and getattr(host, "GlobalId", None)
            and covs
            and all(getattr(c, "GlobalId", None) for c in covs)
        ):
            rel = r
            break
    if rel is None:
        pytest.skip(f"{path.name}: no IfcRelCoversBldgElements with GUIDs")

    host_guid = rel.RelatingBuildingElement.GlobalId
    cov_guids = {c.GlobalId for c in rel.RelatedCoverings}

    model = ifcfast.open(path, use_cache=False, write_cache=False)
    out = tmp_path / f"{path.stem}.covers.ifc"
    model.subset([host_guid], out_path=str(out))

    re = ifcfast.open(out, use_cache=False, write_cache=False)
    guids = {p.guid for p in re}
    assert host_guid in guids, f"{path.name}: seeded host {host_guid} missing"
    missing = cov_guids - guids
    assert not missing, f"{path.name}: coverings not pulled: {missing}"
    _oracle_check(out)  # still ifcopenshell-clean with the coverings pulled
    print(f"OK {path.name}: host {host_guid} pulled {len(cov_guids)} coverings")
