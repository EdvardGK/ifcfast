"""Element-mode classification.

Maps an IFC entity name to an :class:`ElementMode` saying how it should
contribute to a take-off:

* ``COUNT``   — cataloged product, count instances (lamps, valves, doors).
* ``MEASURE`` — bulk element, compute volume/area (walls, slabs, columns).
* ``LINEAR``  — linear run, profile × axis length (pipes, ducts, cable trays).
* ``SKIP``    — not a take-off product (spaces, openings, grids, ports).

The string-keyed variant ``classify_by_name`` is what the Rust tier-1
indexer feeds through — it doesn't carry live ``ifcopenshell`` handles.
Unknown entities fall through to an inheritance-chain walk via a static
per-schema supertype map (extracted once at build time from
``ifcopenshell``; see :mod:`ifcfast.data.schema_supertypes` and the
codegen at ``scripts/gen_schema_supertypes.py``). No runtime
``ifcopenshell`` dependency.
"""

from __future__ import annotations

from enum import Enum
from typing import Optional

from .data.schema_supertypes import ALL_ENTITIES, SUPERTYPE


class ElementMode(str, Enum):
    """How an element contributes to a take-off."""

    COUNT = "count"
    MEASURE = "measure"
    LINEAR = "linear"
    SKIP = "skip"


# Cataloged products — value is "how many" + a spec sheet.
COUNT_ENTITIES: frozenset[str] = frozenset({
    # Architectural fittings
    "IfcFurnishingElement", "IfcSystemFurnitureElement", "IfcFurniture",
    "IfcSanitaryTerminal", "IfcMedicalDevice",
    # Electrical
    "IfcLamp", "IfcLightFixture", "IfcElectricAppliance",
    "IfcElectricFlowStorageDevice", "IfcElectricGenerator", "IfcElectricMotor",
    "IfcOutlet", "IfcSwitchingDevice", "IfcProtectiveDevice",
    "IfcAudioVisualAppliance", "IfcCommunicationsAppliance",
    "IfcTransformer", "IfcMotorConnection", "IfcJunctionBox",
    "IfcCableFitting",
    # HVAC / plumbing terminals and equipment
    "IfcAirTerminal", "IfcAirTerminalBox", "IfcFireSuppressionTerminal",
    "IfcSpaceHeater", "IfcSensor", "IfcAlarm", "IfcController", "IfcActuator",
    "IfcFlowMeter", "IfcValve", "IfcFan", "IfcPump", "IfcCompressor",
    "IfcChiller", "IfcBoiler", "IfcCoolingTower", "IfcHeatExchanger",
    "IfcHumidifier", "IfcEvaporator", "IfcCondenser", "IfcEvaporativeCooler",
    "IfcUnitaryEquipment", "IfcTank", "IfcFilter", "IfcDamper",
    "IfcPipeFitting", "IfcDuctFitting", "IfcDuctSilencer",
    "IfcDistributionChamberElement",
    "IfcDoor", "IfcDoorStandardCase", "IfcWindow", "IfcWindowStandardCase",
    # Additions from coverage audit (2026-05-10).
    "IfcDiscreteAccessory", "IfcFastener", "IfcMechanicalFastener",
    "IfcVibrationIsolator", "IfcVibrationDamper",
    "IfcCableCarrierFitting",
    "IfcBuildingElementPart",
    "IfcTransportElement", "IfcElementAssembly",
    "IfcImpactProtectionDevice", "IfcMooringDevice", "IfcNavigationElement",
    "IfcSign", "IfcVehicle", "IfcTransportationDevice",
})


LINEAR_ENTITIES: frozenset[str] = frozenset({
    "IfcPipeSegment", "IfcDuctSegment",
    "IfcCableSegment", "IfcCableCarrierSegment",
    "IfcRailing",
})


MEASURE_ENTITIES: frozenset[str] = frozenset({
    "IfcWall", "IfcWallStandardCase", "IfcWallElementedCase",
    "IfcSlab", "IfcSlabStandardCase", "IfcSlabElementedCase",
    "IfcRoof", "IfcFooting", "IfcPile", "IfcChimney",
    "IfcBeam", "IfcBeamStandardCase",
    "IfcColumn", "IfcColumnStandardCase",
    "IfcMember", "IfcMemberStandardCase",
    "IfcPlate", "IfcPlateStandardCase",
    "IfcCovering", "IfcCurtainWall",
    "IfcStair", "IfcStairFlight", "IfcRamp", "IfcRampFlight",
    "IfcShadingDevice", "IfcBuildingElementProxy",
    # Rebar / tendons — bulk material-by-volume in structural models.
    "IfcReinforcingBar", "IfcReinforcingMesh", "IfcReinforcingElement",
    "IfcTendon", "IfcTendonAnchor", "IfcTendonConduit",
    "IfcProxy",
    # Civil / geographic catchalls.
    "IfcCivilElement", "IfcGeographicElement",
})


SKIP_ENTITIES: frozenset[str] = frozenset({
    "IfcSpace", "IfcOpeningElement", "IfcOpeningStandardCase",
    "IfcProjectionElement", "IfcVoidingFeature", "IfcSurfaceFeature",
    "IfcGrid", "IfcGridAxis", "IfcAnnotation",
    "IfcVirtualElement", "IfcSpatialZone",
    "IfcDistributionPort", "IfcPort",
    # IFC4X3 alignment / positioning are reference geometry.
    "IfcAlignment", "IfcAlignmentSegment", "IfcAlignmentHorizontal",
    "IfcAlignmentVertical", "IfcAlignmentCant",
    "IfcReferent", "IfcLinearPositioningElement", "IfcPositioningElement",
    # IFC4X3 facility-level wrappers — these aggregate parts; the parts
    # are the products, the wrapper is spatial.
    "IfcFacility", "IfcFacilityPart", "IfcFacilityPartCommon",
    "IfcBridge", "IfcBridgePart", "IfcRoad", "IfcRoadPart",
    "IfcRailway", "IfcRailwayPart", "IfcMarineFacility", "IfcMarinePart",
})


_COUNT_PARENT_TYPES = frozenset({
    "IfcFlowTerminal", "IfcFlowController", "IfcFlowMovingDevice",
    "IfcFlowStorageDevice", "IfcFlowTreatmentDevice",
    "IfcEnergyConversionDevice", "IfcDistributionControlElement",
})


# Cached ancestor chains per (entity, schema) — built once by walking
# the static `SUPERTYPE` table that ships with the wheel. No live IFC
# library involved.
_ANCESTORS_CACHE: dict[tuple[str, str], tuple[str, ...]] = {}


def _ancestors(entity: str, schema: str) -> tuple[str, ...]:
    """Resolve an entity's full ancestor chain. Returns ``(entity,)``
    when the schema isn't known or the entity isn't in it (which is
    indistinguishable from a root-level entity — both terminate the
    classifier's inheritance walk in the next step)."""
    key = (entity, schema)
    cached = _ANCESTORS_CACHE.get(key)
    if cached is not None:
        return cached

    schema_key = _resolve_schema(schema)
    if schema_key is None:
        _ANCESTORS_CACHE[key] = (entity,)
        return (entity,)
    parents = SUPERTYPE[schema_key]

    chain: list[str] = [entity]
    seen: set[str] = {entity}
    cur = entity
    while True:
        nxt = parents.get(cur)
        if nxt is None or nxt in seen:
            break
        chain.append(nxt)
        seen.add(nxt)
        cur = nxt
    result = tuple(chain)
    _ANCESTORS_CACHE[key] = result
    return result


# Cached descendant sets per (entity, schema). Built once by inverting
# the static `SUPERTYPE` table — child→parent becomes parent→{children},
# then transitively closed. Same static map, no live IFC library.
_DESCENDANTS_CACHE: dict[tuple[str, str], frozenset[str]] = {}

# Cached inverted child-lists (parent → immediate children) per schema,
# built from `SUPERTYPE` on first use for that schema.
_CHILDREN_CACHE: dict[str, dict[str, list[str]]] = {}


def _resolve_schema(schema: str) -> Optional[str]:
    """Return the canonical SUPERTYPE key matching ``schema``, or ``None``
    when the schema isn't known.

    Resolution order:

    1. Exact match (``"IFC4X3"``).
    2. Case-insensitive match (``"ifc4x3"`` / ``"Ifc4"``).
    3. Addendum / technical-corrigendum suffix strip: real-world headers
       declare ``FILE_SCHEMA(('IFC4X3_ADD2'))`` / ``IFC4_ADD2`` /
       ``IFC4X3_TC1`` — none of which are SUPERTYPE keys. We match the
       longest known key that the (folded) schema starts with, so
       ``"ifc4x3_add2"`` resolves to ``"IFC4X3"`` rather than ``"IFC4"``.

    ``"UNKNOWN"`` (and the empty string) resolve to ``None`` so the caller
    can apply its own default schema."""
    if schema in SUPERTYPE:
        return schema
    folded = schema.casefold()
    if not folded or folded == "unknown":
        return None
    for key_schema in SUPERTYPE:
        if key_schema.casefold() == folded:
            return key_schema
    # Suffix-bearing variant: prefix-match against known keys, longest first
    # so the more specific schema wins (IFC4X3 before IFC4).
    for key_schema in sorted(SUPERTYPE, key=len, reverse=True):
        if folded.startswith(key_schema.casefold()):
            return key_schema
    return None


def _children_index(schema_key: str) -> dict[str, list[str]]:
    """Immediate parent→children index for a known schema key, cached."""
    cached = _CHILDREN_CACHE.get(schema_key)
    if cached is not None:
        return cached
    children: dict[str, list[str]] = {}
    for child, parent in SUPERTYPE[schema_key].items():
        children.setdefault(parent, []).append(child)
    _CHILDREN_CACHE[schema_key] = children
    return children


# Case-insensitive lookup table over every entity name across all
# supported schemas (folded name → canonical name), built once.
_ALL_ENTITIES_FOLDED: Optional[dict[str, str]] = None


def canonical_entity_any_schema(entity: str) -> str:
    """Return the canonical casing of ``entity`` if it is a known IFC
    entity in *any* supported schema (case-insensitive), else ``entity``
    unchanged. Used to give :func:`Model.by_type` case-insensitive parity
    with ``ifcopenshell`` without policing schema versions."""
    global _ALL_ENTITIES_FOLDED
    if entity in ALL_ENTITIES:
        return entity
    if _ALL_ENTITIES_FOLDED is None:
        _ALL_ENTITIES_FOLDED = {name.casefold(): name for name in ALL_ENTITIES}
    return _ALL_ENTITIES_FOLDED.get(entity.casefold(), entity)


def canonical_entity(entity: str, schema: str = "IFC4") -> str:
    """Return the schema's canonical casing for ``entity`` (e.g.
    ``"ifcwall" -> "IfcWall"``), or ``entity`` unchanged when the schema
    or the name isn't known. Case-insensitive lookup against both the
    child and supertype name spaces of the static ``SUPERTYPE`` map."""
    schema_key = _resolve_schema(schema)
    if schema_key is None:
        return entity
    parent_map = SUPERTYPE[schema_key]
    folded = entity.casefold()
    for name in parent_map:
        if name.casefold() == folded:
            return name
    for name in parent_map.values():
        if name.casefold() == folded:
            return name
    return entity


def subtypes_of(entity: str, schema: str = "IFC4") -> frozenset[str]:
    """All entity names that are ``entity`` or descend from it, for the
    given schema.

    Case-insensitive on ``entity``: the returned names are the canonical
    schema-cased names (e.g. ``"IfcWallStandardCase"``). The result always
    includes ``entity`` itself (canonically cased when the schema knows it,
    otherwise echoed as given). Walks the static per-schema ``SUPERTYPE``
    map shipped with the wheel — no runtime IFC library.

    When the schema or the entity is unknown, returns
    ``frozenset({entity})`` — the caller falls back to exact match, which
    is the safe degradation.
    """
    key = (entity, schema)
    cached = _DESCENDANTS_CACHE.get(key)
    if cached is not None:
        return cached

    schema_key = _resolve_schema(schema)
    if schema_key is None:
        result = frozenset({entity})
        _DESCENDANTS_CACHE[key] = result
        return result

    # Canonicalise the requested entity name against the schema's casing.
    # An entity may appear as a key (it has a supertype) and/or as a value
    # (it is a supertype of something); canonical_entity searches both.
    canon = canonical_entity(entity, schema_key)
    if canon == entity and entity not in SUPERTYPE[schema_key] \
            and entity not in SUPERTYPE[schema_key].values():
        # Entity not present anywhere in this schema's inheritance graph.
        result = frozenset({entity})
        _DESCENDANTS_CACHE[key] = result
        return result

    children = _children_index(schema_key)
    out: set[str] = set()
    stack = [canon]
    while stack:
        cur = stack.pop()
        if cur in out:
            continue
        out.add(cur)
        stack.extend(children.get(cur, ()))
    result = frozenset(out)
    _DESCENDANTS_CACHE[key] = result
    return result


def classify_by_name(entity: str, schema: str = "IFC4") -> ElementMode:
    """Classify by entity-name string with schema-aware inheritance fallback.

    This is the primary classifier used by the Rust tier-1 path (which
    doesn't carry live element handles). Walks the static per-schema
    supertype map shipped with the wheel — no runtime IFC library.
    Returns ``SKIP`` for unknown entities."""
    if entity in SKIP_ENTITIES:
        return ElementMode.SKIP
    if entity in COUNT_ENTITIES:
        return ElementMode.COUNT
    if entity in LINEAR_ENTITIES:
        return ElementMode.LINEAR
    if entity in MEASURE_ENTITIES:
        return ElementMode.MEASURE
    ancestors = _ancestors(entity, schema)
    for p in ancestors:
        if p in _COUNT_PARENT_TYPES:
            return ElementMode.COUNT
        if p == "IfcFlowSegment":
            return ElementMode.LINEAR
        # IFC4/IFC2X3 root the bulk built elements at IfcBuildingElement;
        # IFC4X3 renamed that supertype to IfcBuiltElement, so the walk
        # must accept either (IfcKerb, IfcPavement, IfcCourse, … chain
        # through IfcBuiltElement and would otherwise fall through to SKIP).
        if p in ("IfcBuildingElement", "IfcBuiltElement"):
            return ElementMode.MEASURE
    return ElementMode.SKIP
