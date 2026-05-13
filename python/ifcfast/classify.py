"""Element-mode classification.

Maps an IFC entity name (or a live ``ifcopenshell.entity_instance``) to
an :class:`ElementMode` saying how it should contribute to a take-off:

* ``COUNT``   — cataloged product, count instances (lamps, valves, doors).
* ``MEASURE`` — bulk element, compute volume/area (walls, slabs, columns).
* ``LINEAR``  — linear run, profile × axis length (pipes, ducts, cable trays).
* ``SKIP``    — not a take-off product (spaces, openings, grids, ports).

The string-keyed variant ``classify_by_name`` is what the Rust tier-1
indexer feeds through — it doesn't carry live ``ifcopenshell`` handles.
Inheritance fallback uses the schema declaration lookup, which is
available off ``ifcopenshell.schema_by_name`` without opening any file.

If ``ifcopenshell`` isn't installed, the explicit-set checks still work;
the inheritance fallback returns ``SKIP`` for unknown entities.
"""

from __future__ import annotations

from enum import Enum

try:  # ifcopenshell is an optional dep; degrade gracefully if absent.
    import ifcopenshell  # type: ignore
    _HAVE_IFCOPENSHELL = True
except ImportError:  # pragma: no cover
    ifcopenshell = None  # type: ignore
    _HAVE_IFCOPENSHELL = False


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


# Cached ancestor chains per (entity, schema) — built once via
# ifcopenshell.schema_by_name(...), no file open required.
_ANCESTORS_CACHE: dict[tuple[str, str], tuple[str, ...]] = {}


def _ancestors(entity: str, schema: str) -> tuple[str, ...]:
    """Resolve an entity's full ancestor chain. Returns ``(entity,)`` if
    ``ifcopenshell`` is unavailable or the entity isn't in the schema."""
    key = (entity, schema)
    cached = _ANCESTORS_CACHE.get(key)
    if cached is not None:
        return cached
    if not _HAVE_IFCOPENSHELL:
        _ANCESTORS_CACHE[key] = (entity,)
        return (entity,)
    try:
        sch = ifcopenshell.schema_by_name(schema)
        decl = sch.declaration_by_name(entity).as_entity()
        chain: list[str] = []
        cur = decl
        while cur is not None:
            chain.append(cur.name())
            cur = cur.supertype()
        result = tuple(chain)
    except Exception:
        result = (entity,)
    _ANCESTORS_CACHE[key] = result
    return result


def classify_by_name(entity: str, schema: str = "IFC4") -> ElementMode:
    """Classify by entity-name string with schema-aware inheritance fallback.

    This is the primary classifier used by the Rust tier-1 path (which
    doesn't carry ``ifcopenshell.entity_instance`` handles). Falls back
    to ``SKIP`` for unknown entities when ``ifcopenshell`` isn't installed.
    """
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


def classify_element(element) -> ElementMode:  # type: ignore[no-untyped-def]
    """Classify a live ``ifcopenshell.entity_instance``. Requires
    ``ifcopenshell`` at runtime; raises if it's missing.
    """
    if not _HAVE_IFCOPENSHELL:
        raise RuntimeError(
            "classify_element requires ifcopenshell; install it or use "
            "classify_by_name(entity_str)"
        )
    entity = element.is_a()
    if entity in SKIP_ENTITIES:
        return ElementMode.SKIP
    if entity in COUNT_ENTITIES:
        return ElementMode.COUNT
    if entity in LINEAR_ENTITIES:
        return ElementMode.LINEAR
    if entity in MEASURE_ENTITIES:
        return ElementMode.MEASURE
    for parent in _COUNT_PARENT_TYPES:
        if element.is_a(parent):
            return ElementMode.COUNT
    if element.is_a("IfcFlowSegment"):
        return ElementMode.LINEAR
    if element.is_a("IfcBuildingElement"):
        return ElementMode.MEASURE
    return ElementMode.SKIP
