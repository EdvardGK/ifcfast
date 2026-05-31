//! Which columnar extractors are required for a compiled IDS document.

use super::compiled::{CompiledFacet, CompiledIds, CompiledSpec, FacetKind};

#[derive(Debug, Clone, Copy, Default)]
pub struct ExtractNeeds {
    pub psets: bool,
    pub quantities: bool,
    pub classifications: bool,
    pub materials: bool,
}

impl ExtractNeeds {
    pub fn all() -> Self {
        Self {
            psets: true,
            quantities: true,
            classifications: true,
            materials: true,
        }
    }

    pub fn any(&self) -> bool {
        self.psets || self.quantities || self.classifications || self.materials
    }

    pub fn from_compiled(ids: &CompiledIds) -> Self {
        let mut needs = Self::default();
        for spec in &ids.specifications {
            needs.merge_spec(spec);
        }
        needs
    }

    fn merge_spec(&mut self, spec: &CompiledSpec) {
        for facet in spec
            .applicability
            .iter()
            .chain(spec.requirements.iter())
        {
            self.merge_facet(facet);
        }
    }

    fn merge_facet(&mut self, facet: &CompiledFacet) {
        match facet.kind {
            FacetKind::Property => {
                self.psets = true;
                self.quantities = true;
            }
            FacetKind::Classification => {
                self.classifications = true;
            }
            FacetKind::Material => {
                self.materials = true;
            }
            FacetKind::Entity | FacetKind::Attribute | FacetKind::PartOf => {}
        }
    }
}
