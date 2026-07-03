"""Agent-facing ``Model.mutate()`` — the attribute write axis (GH #133).

Exercises the Python surface: batch ops (set_property / rename /
translate / rotate), copy-on-write on shared psets and shared
placements, atomic multi-failure reporting, and the fail-loud guards
(quantity sets, new properties without ``ifc_type``).

The real proof — that a mutated file reopens in ifcopenshell with the
property readable, siblings untouched, and the placement moved by
exactly the delta — runs over the discipline-diverse corpus when
``IFCFAST_CORPUS`` is set (same corpus var the subset/hotswap gates
use).
"""

from __future__ import annotations

import os
from pathlib import Path

import pytest

import ifcfast

try:
    import ifcopenshell
except ImportError:  # pragma: no cover
    ifcopenshell = None

FIXTURE = Path(__file__).parent / "fixtures" / "mutate_shared.ifc"
WALL_A = "WallAGuid00000000000A"
WALL_B = "WallBGuid00000000000B"
WALL_C = "WallCGuid00000000000C"


@pytest.fixture(scope="module")
def model():
    return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)


def test_mutate_returns_bytes_by_default(model):
    data = model.mutate(
        [{"op": "rename", "guid": WALL_A, "name": "Vegg Æblåbær"}]
    )
    assert isinstance(data, (bytes, bytearray))
    assert "Vegg Æblåbær".encode() in data


def test_mutate_out_path_returns_stats(model, tmp_path):
    out = tmp_path / "mutated.ifc"
    stats = model.mutate(
        [
            {
                "op": "set_property",
                "guid": WALL_A,
                "pset": "Pset_WallCommon",
                "name": "FireRating",
                "value": "REI 60",
            }
        ],
        out_path=str(out),
    )
    assert stats["path"] == str(out)
    assert stats["props_set"] == 1
    # Pset_WallCommon anchors walls A+B through one rel → CoW.
    assert stats["psets_cloned"] == 1
    assert stats["rels_cloned"] == 1
    # The output re-opens in ifcfast.
    m2 = ifcfast.open(out, use_cache=False, write_cache=False)
    assert len(m2.products) == 3


def test_cow_preserves_sibling_view(model):
    data = model.mutate(
        [
            {
                "op": "set_property",
                "guid": WALL_A,
                "pset": "Pset_WallCommon",
                "name": "FireRating",
                "value": "REI 60",
            }
        ]
    )
    # Both values coexist: wall A's clone carries REI 60, wall B keeps
    # the original REI30 record.
    assert b"IFCLABEL('REI 60')" in data
    assert b"IFCLABEL('REI30')" in data


def test_shared_placement_cow(model):
    # Walls B and C share one IfcLocalPlacement.
    data = model.mutate(
        [{"op": "translate", "guid": WALL_C, "delta": [0.0, 7.0, 0.0]}]
    )
    text = data.decode()
    assert "IFCCARTESIANPOINT((5.0,7.0,0.0))" in text  # wall C moved
    assert "IFCCARTESIANPOINT((5.,0.,0.))" in text  # wall B verbatim


def test_atomic_batch_reports_all_failures(model):
    with pytest.raises(ValueError) as ei:
        model.mutate(
            [
                {"op": "rename", "guid": "nope", "name": "x"},
                {
                    "op": "set_property",
                    "guid": WALL_A,
                    "pset": "NoSuchPset",
                    "name": "X",
                    "value": 1.0,
                },
            ]
        )
    msg = str(ei.value)
    assert "[op 0]" in msg and "[op 1]" in msg


def test_quantity_sets_are_refused(model):
    with pytest.raises(ValueError, match="IfcElementQuantity"):
        model.mutate(
            [
                {
                    "op": "set_property",
                    "guid": WALL_A,
                    "pset": "Qto_WallBase",
                    "name": "Length",
                    "value": 9.0,
                }
            ]
        )


def test_new_property_requires_ifc_type(model):
    with pytest.raises(ValueError, match="ifc_type"):
        model.mutate(
            [
                {
                    "op": "set_property",
                    "guid": WALL_A,
                    "pset": "Pset_Solo",
                    "name": "U-verdi",
                    "value": 0.18,
                }
            ]
        )
    data = model.mutate(
        [
            {
                "op": "set_property",
                "guid": WALL_A,
                "pset": "Pset_Solo",
                "name": "U-verdi",
                "value": 0.18,
                "ifc_type": "IFCTHERMALTRANSMITTANCEMEASURE",
            }
        ]
    )
    assert b"IFCTHERMALTRANSMITTANCEMEASURE(0.18)" in data


def test_empty_ops_fail_loud(model):
    with pytest.raises(ValueError, match="empty"):
        model.mutate([])


# ---------------------------------------------------------------------------
# Real-corpus oracle gate
# ---------------------------------------------------------------------------


def _corpus_paths() -> list[Path]:
    raw = os.environ.get("IFCFAST_CORPUS", "") or os.environ.get(
        "IFCFAST_SUBSET_CORPUS", ""
    )
    return [Path(p) for p in raw.split(":") if p.strip()]


def _pset_value(prod, pset_name, prop_name):
    """Read a single-value property the way a consumer would."""
    for rel in getattr(prod, "IsDefinedBy", []) or []:
        if not rel.is_a("IfcRelDefinesByProperties"):
            continue
        pd = rel.RelatingPropertyDefinition
        pds = pd if isinstance(pd, tuple) else (pd,)
        for ps in pds:
            if not ps.is_a("IfcPropertySet") or ps.Name != pset_name:
                continue
            for p in ps.HasProperties:
                if p.is_a("IfcPropertySingleValue") and p.Name == prop_name:
                    nv = p.NominalValue
                    return nv.wrappedValue if nv is not None else None
    return None


@pytest.mark.skipif(
    not _corpus_paths(),
    reason="set IFCFAST_CORPUS=/a.ifc:/b.ifc to run the real-file gate",
)
@pytest.mark.parametrize("path", _corpus_paths(), ids=lambda p: p.name)
def test_mutate_over_real_corpus_is_ifcopenshell_clean(path, tmp_path):
    """Durable acceptance gate for GH #133 over authored Revit/MagiCAD
    output: pick a real element carrying a real single-value property
    (preferring one whose pset is SHARED, the CoW danger case), set the
    property, rename the element, and translate it — then prove in
    ifcopenshell that the target changed exactly as asked and a sibling
    sharing the pset still reads the original value.
    """
    assert path.exists(), f"corpus file missing: {path}"
    src = ifcopenshell.open(str(path))

    # Find (element, pset, prop, sibling): a string-valued
    # IfcPropertySingleValue on an element with a plain IfcLocalPlacement.
    # Prefer a rel anchoring >1 element so the CoW path is exercised.
    best = None
    for rel in src.by_type("IfcRelDefinesByProperties"):
        pd = rel.RelatingPropertyDefinition
        pds = pd if isinstance(pd, tuple) else (pd,)
        for ps in pds:
            if not ps.is_a("IfcPropertySet"):
                continue
            prop = next(
                (
                    p
                    for p in ps.HasProperties
                    if p.is_a("IfcPropertySingleValue")
                    and p.NominalValue is not None
                    and isinstance(p.NominalValue.wrappedValue, str)
                ),
                None,
            )
            if prop is None:
                continue
            elems = [
                o
                for o in rel.RelatedObjects
                if o.is_a("IfcProduct")
                and getattr(o, "ObjectPlacement", None) is not None
                and o.ObjectPlacement.is_a("IfcLocalPlacement")
                and o.ObjectPlacement.RelativePlacement.is_a(
                    "IfcAxis2Placement3D"
                )
            ]
            if not elems:
                continue
            cand = (elems[0], ps.Name, prop.Name, elems[1] if len(elems) > 1 else None)
            if cand[3] is not None:
                best = cand
                break
            if best is None:
                best = cand
        if best is not None and best[3] is not None:
            break
    if best is None:
        pytest.skip(f"{path.name}: no mutable single-value property found")
    elem, pset_name, prop_name, sibling = best

    guid = elem.GlobalId
    old_value = _pset_value(elem, pset_name, prop_name)
    old_xyz = list(
        elem.ObjectPlacement.RelativePlacement.Location.Coordinates
    )
    delta = [1.5, -2.0, 0.75]
    marker = "ifcfast-mutate-gate"

    model = ifcfast.open(path, use_cache=False, write_cache=False)
    out = tmp_path / f"{path.stem}.mutate.ifc"
    stats = model.mutate(
        [
            {
                "op": "set_property",
                "guid": guid,
                "pset": pset_name,
                "name": prop_name,
                "value": marker,
            },
            {"op": "rename", "guid": guid, "name": marker},
            {"op": "translate", "guid": guid, "delta": delta},
        ],
        out_path=str(out),
    )

    f = ifcopenshell.open(str(out))
    dangling = sum(
        1
        for inst in f
        if not _get_info_ok(inst)
    )
    assert dangling == 0, f"{path.name}: {dangling} dangling after mutate"

    prod = next(
        p for p in f.by_type("IfcProduct") if getattr(p, "GlobalId", None) == guid
    )
    assert prod.Name == marker
    assert _pset_value(prod, pset_name, prop_name) == marker
    new_xyz = list(prod.ObjectPlacement.RelativePlacement.Location.Coordinates)
    for old, new, d in zip(old_xyz, new_xyz, delta):
        assert abs((old + d) - new) < 1e-6, f"{path.name}: placement off"

    if sibling is not None:
        sib = next(
            p
            for p in f.by_type("IfcProduct")
            if getattr(p, "GlobalId", None) == sibling.GlobalId
        )
        assert _pset_value(sib, pset_name, prop_name) == old_value, (
            f"{path.name}: sibling {sibling.GlobalId} lost its original value — "
            "CoW leaked"
        )

    print(
        f"OK {path.name}: {prod.is_a()} {guid} pset='{pset_name}' "
        f"prop='{prop_name}' shared={sibling is not None} "
        f"cloned={stats['psets_cloned']} gc={stats['records_gc']} "
        f"out={stats['records_out']}"
    )


def _get_info_ok(inst) -> bool:
    try:
        inst.get_info(recursive=False)
        return True
    except Exception:  # noqa: BLE001
        return False


def test_ifczip_out_path_writes_real_zip(model, tmp_path):
    """GH #132 item 7: an .ifczip out_path must be a real ZIP archive
    (magic-byte dispatch reopens it), not raw STEP in a lying filename."""
    out = tmp_path / "mutated.ifczip"
    stats = model.mutate(
        [{"op": "rename", "guid": WALL_A, "name": "zipped"}],
        out_path=str(out),
    )
    assert stats["path"] == str(out)
    raw = out.read_bytes()
    assert raw[:2] == b"PK", "must be a ZIP archive, got raw STEP"
    m2 = ifcfast.open(out, use_cache=False, write_cache=False)
    assert len(m2.products) == 3
