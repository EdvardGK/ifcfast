"""IDS facet evaluators on ifcfast tables."""

from ifcfast.ids.facets.attribute import attribute_mask
from ifcfast.ids.facets.classification import classification_mask
from ifcfast.ids.facets.entity import entity_mask
from ifcfast.ids.facets.material import material_mask
from ifcfast.ids.facets.partof import partof_mask
from ifcfast.ids.facets.property import property_mask

__all__ = [
    "entity_mask",
    "attribute_mask",
    "property_mask",
    "classification_mask",
    "material_mask",
    "partof_mask",
]
