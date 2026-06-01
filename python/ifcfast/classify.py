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

from .data.schema_supertypes import SUPERTYPE


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

    parents = SUPERTYPE.get(schema)
    if parents is None:
        # Unknown schema — most files declare IFC2X3/IFC4/IFC4X3 but
        # variant casings ("Ifc4", "IFC4_ADD2") sneak through. Try a
        # case-insensitive lookup against the keys we have.
        for key_schema in SUPERTYPE:
            if key_schema.casefold() == schema.casefold():
                parents = SUPERTYPE[key_schema]
                break
    if parents is None:
        _ANCESTORS_CACHE[key] = (entity,)
        return (entity,)

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
        if p == "IfcBuildingElement":
            return ElementMode.MEASURE
    return ElementMode.SKIP
