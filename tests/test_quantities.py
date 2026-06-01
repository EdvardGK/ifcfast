"""Quantity-extractor coverage tests.

The bundled ``minimal.ifc`` exercises only two of the six
``IfcQuantity*`` subtypes (Length + Area). The dedicated
``quantities.ifc`` fixture in this directory covers all six —
``IfcQuantityLength``, ``IfcQuantityArea``, ``IfcQuantityVolume``,
``IfcQuantityCount``, ``IfcQuantityWeight``, ``IfcQuantityTime`` — on
two products linked through ``IfcRelDefinesByProperties`` so the
extractor's rel-resolution path is also exercised.

Cross-checks against ``ifcopenshell`` ground truth when available so
divergence between the two surfaces is caught at CI time, not in
the field (GH #35).
"""

from __future__ import annotations

from pathlib import Path

import pytest

import ifcfast


FIXTURE = Path(__file__).parent / "fixtures" / "quantities.ifc"

WALL_GUID = "7WALL1UIDgUIDgUIDgUID0"
SLAB_GUID = "9SLAB1UIDgUIDgUIDgUID0"


@pytest.fixture(scope="module")
def model():
    return ifcfast.open(FIXTURE, use_cache=False, write_cache=False)


def test_all_six_quantity_types_extract(model):
    df = model.quantities
    assert len(df) == 8

    wall_qtos = df[df.guid == WALL_GUID]
    slab_qtos = df[df.guid == SLAB_GUID]

    assert set(wall_qtos.quantity_type) == {
        "Length", "Area", "Volume", "Count", "Weight", "Time",
    }
    assert set(slab_qtos.quantity_type) == {"Area", "Volume"}


def test_quantity_values_match_authored(model):
    df = model.quantities
    by_key = {
        (row.guid, row.quantity_name): (float(row.value), row.quantity_type)
        for row in df.itertuples()
    }
    assert by_key[(WALL_GUID, "Length")] == (3.0, "Length")
    assert by_key[(WALL_GUID, "NetSideArea")] == (7.5, "Area")
    assert by_key[(WALL_GUID, "GrossVolume")] == (1.275, "Volume")
    assert by_key[(WALL_GUID, "FastenerCount")] == (12.0, "Count")
    assert by_key[(WALL_GUID, "GrossWeight")] == (3060.0, "Weight")
    assert by_key[(WALL_GUID, "InstallDuration")] == (2.5, "Time")
    assert by_key[(SLAB_GUID, "NetArea")] == (25.0, "Area")
    assert by_key[(SLAB_GUID, "GrossVolume")] == (5.0, "Volume")


def test_qto_name_attached_per_product(model):
    df = model.quantities
    assert (df[df.guid == WALL_GUID]["qto_name"] == "Qto_WallBaseQuantities").all()
    assert (df[df.guid == SLAB_GUID]["qto_name"] == "Qto_SlabBaseQuantities").all()


def test_matches_ifcopenshell_ground_truth():
    """Cross-validate against ``ifcopenshell`` (GH #35).

    Skips when ifcopenshell isn't installed — it's a dev-only extra so
    CI on a wheel-test container that doesn't pull it in stays green.
    """
    ifcopenshell = pytest.importorskip("ifcopenshell")
    from ifcopenshell.util.element import get_psets

    m = ifcfast.open(FIXTURE, use_cache=False, write_cache=False)
    f = ifcopenshell.open(str(FIXTURE))

    ours: dict[str, dict[str, set[tuple[str, float]]]] = {}
    for row in m.quantities.itertuples():
        ours.setdefault(row.guid, {}).setdefault(row.qto_name, set()).add(
            (row.quantity_name, float(row.value))
        )

    for el in f.by_type("IfcProduct"):
        if not hasattr(el, "GlobalId"):
            continue
        truth_psets = get_psets(el, qtos_only=True)
        if not truth_psets:
            continue
        truth: dict[str, set[tuple[str, float]]] = {}
        for qto_name, payload in truth_psets.items():
            truth[qto_name] = {
                (k, float(v))
                for k, v in payload.items()
                if k != "id"
            }
        assert el.GlobalId in ours, (
            f"ifcfast missed all quantities on {el.is_a()} {el.GlobalId}"
        )
        assert ours[el.GlobalId] == truth, (
            f"diverged on {el.is_a()} {el.GlobalId}: "
            f"ifcfast={ours[el.GlobalId]} truth={truth}"
        )
