//! IDS 1.0 XML → [`CompiledIds`] (buildingSMART namespace + XSD restrictions).

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

use super::compiled::{
    Cardinality, CompiledFacet, CompiledIds, CompiledSpec, FacetKind, MaxOccurs, ValueConstraint,
};

pub fn parse_ids_xml(path: &str, xml: &str) -> Result<CompiledIds, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut specs: Vec<CompiledSpec> = Vec::new();
    let mut buf = Vec::new();
    let mut in_specs = false;
    let mut cur_spec: Option<CompiledSpec> = None;
    let mut section: Option<Section> = None;
    let mut facet_stack: Vec<FacetBuilder> = Vec::new();
    let mut path_stack: Vec<String> = Vec::new();
    let mut in_restriction = false;

    loop {
        let ev = reader.read_event_into(&mut buf).map_err(|e| e.to_string())?;
        match ev {
            Event::Start(e) => {
                let local = local_name(e.name().as_ref());
                path_stack.push(local.clone());
                match local.as_str() {
                    "specifications" => in_specs = true,
                    "specification" if in_specs => {
                        let name = attr_value(&e, "name").unwrap_or_else(|| "Unnamed".into());
                        let ifc_versions = attr_value(&e, "ifcVersion")
                            .map(|v| split_ifc_versions(&v))
                            .unwrap_or_default();
                        cur_spec = Some(CompiledSpec {
                            name,
                            ifc_versions,
                            min_occurs: 0,
                            max_occurs: default_max_occurs(),
                            applicability: vec![],
                            requirements: vec![],
                        });
                        section = None;
                    }
                    "applicability" => {
                        section = Some(Section::Applicability);
                        if let Some(spec) = &mut cur_spec {
                            if let Some(m) = attr_value(&e, "minOccurs") {
                                spec.min_occurs = m.parse().unwrap_or(0);
                            }
                            if let Some(m) = attr_value(&e, "maxOccurs") {
                                spec.max_occurs = parse_max_occurs(&m);
                            }
                        }
                    }
                    "requirements" => section = Some(Section::Requirements),
                    "restriction" => in_restriction = true,
                    "entity"
                    | "attribute"
                    | "property"
                    | "classification"
                    | "material"
                    | "partOf" => {
                        let kind = match local.as_str() {
                            "entity" => FacetKind::Entity,
                            "attribute" => FacetKind::Attribute,
                            "property" => FacetKind::Property,
                            "classification" => FacetKind::Classification,
                            "material" => FacetKind::Material,
                            "partOf" => FacetKind::PartOf,
                            _ => continue,
                        };
                        let card = attr_value(&e, "cardinality")
                            .as_deref()
                            .map(parse_cardinality)
                            .unwrap_or(Cardinality::Required);
                        let data_type = attr_value(&e, "dataType").map(|d| d.to_uppercase());
                        facet_stack.push(FacetBuilder {
                            kind,
                            cardinality: card,
                            entity_names: vec![],
                            predefined_type: None,
                            predefined_type_constraint: None,
                            attribute_name: None,
                            attribute_names: vec![],
                            attribute_name_constraint: None,
                            property_sets: vec![],
                            property_set_constraint: None,
                            base_names: vec![],
                            base_name_constraint: None,
                            data_type,
                            value: None,
                            partof_relation: None,
                            classification_system: None,
                            material_value: None,
                        });
                    }
                    _ => {}
                }
                if in_restriction {
                    if let Some(fb) = facet_stack.last_mut() {
                        if let Some(v) = attr_value(&e, "value") {
                            apply_restriction_attr(fb, &path_stack, &local, &v);
                        }
                    }
                }
            }
            Event::Empty(e) => {
                let local = local_name(e.name().as_ref());
                if in_restriction {
                    if let Some(v) = attr_value(&e, "value") {
                        if let Some(fb) = facet_stack.last_mut() {
                            apply_restriction_attr(fb, &path_stack, &local, &v);
                        }
                    }
                }
                if local == "enumeration" {
                    if let Some(v) = attr_value(&e, "value") {
                        if let Some(fb) = facet_stack.last_mut() {
                            push_entity_enum_if_name(fb, &path_stack, &v);
                            push_value_enumeration(fb, &path_stack, v);
                        }
                    }
                }
            }
            Event::Text(t) => {
                if facet_stack.is_empty() {
                    continue;
                }
                let text = t.unescape().map_err(|e| e.to_string())?.into_owned();
                if text.is_empty() {
                    continue;
                }
                let fb = facet_stack.last_mut().unwrap();
                let path: Vec<&str> = path_stack.iter().map(|s| s.as_str()).collect();
                if path.ends_with(&["name", "simpleValue"]) {
                    match fb.kind {
                        FacetKind::Entity | FacetKind::PartOf => {
                            fb.entity_names.push(text.to_uppercase())
                        }
                        FacetKind::Attribute => fb.attribute_name = Some(text),
                        _ => {}
                    }
                } else if path.ends_with(&["system", "simpleValue"]) {
                    if fb.kind == FacetKind::Classification {
                        fb.classification_system = Some(ValueConstraint::Simple { text });
                    }
                } else if path.ends_with(&["propertySet", "simpleValue"]) {
                    fb.property_sets.push(text);
                } else if path.ends_with(&["baseName", "simpleValue"]) {
                    fb.base_names.push(text);
                } else if path.ends_with(&["predefinedType", "simpleValue"]) {
                    fb.predefined_type = Some(text);
                } else if path.ends_with(&["relation", "simpleValue"]) {
                    fb.partof_relation = Some(text.to_uppercase());
                } else if path.ends_with(&["value", "simpleValue"]) {
                    if fb.kind == FacetKind::Material {
                        fb.material_value = Some(text);
                    } else {
                        fb.value = Some(ValueConstraint::Simple { text });
                    }
                }
            }
            Event::End(e) => {
                let local = local_name(e.name().as_ref());
                if local == "restriction" {
                    in_restriction = false;
                }
                if local == "entity"
                    || local == "attribute"
                    || local == "property"
                    || local == "classification"
                    || local == "material"
                    || local == "partOf"
                {
                    if let Some(fb) = facet_stack.pop() {
                        if let (Some(spec), Some(sec)) = (&mut cur_spec, section) {
                            let facet = fb.into_facet();
                            match sec {
                                Section::Applicability => spec.applicability.push(facet),
                                Section::Requirements => spec.requirements.push(facet),
                            }
                        }
                    }
                } else if local == "specification" {
                    if let Some(spec) = cur_spec.take() {
                        specs.push(spec);
                    }
                    section = None;
                }
                path_stack.pop();
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(CompiledIds {
        ids_path: Some(path.to_string()),
        specifications: specs,
    })
}

fn split_ifc_versions(raw: &str) -> Vec<String> {
    raw.split_whitespace()
        .map(|s| s.trim().to_uppercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn default_max_occurs() -> MaxOccurs {
    MaxOccurs::Unbounded("unbounded".into())
}

fn push_entity_enum_if_name(fb: &mut FacetBuilder, path: &[String], value: &str) {
    if !matches!(fb.kind, FacetKind::Entity | FacetKind::PartOf) {
        return;
    }
    let path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
    if path.contains(&"name") {
        fb.entity_names.push(value.to_uppercase());
    }
}

fn push_value_enumeration(fb: &mut FacetBuilder, path: &[String], value: String) {
    let Some(slot) = constraint_slot_mut(fb, path) else {
        return;
    };
    match slot {
        Some(ValueConstraint::Enumeration { values }) => values.push(value),
        _ => *slot = Some(ValueConstraint::Enumeration { values: vec![value] }),
    }
}

fn constraint_slot_mut<'a>(
    fb: &'a mut FacetBuilder,
    path: &[String],
) -> Option<&'a mut Option<ValueConstraint>> {
    let path: Vec<&str> = path.iter().map(|s| s.as_str()).collect();
    if fb.kind == FacetKind::Attribute && path.contains(&"name") {
        return Some(&mut fb.attribute_name_constraint);
    }
    if fb.kind == FacetKind::Classification && path.contains(&"system") {
        return Some(&mut fb.classification_system);
    }
    if matches!(fb.kind, FacetKind::Entity | FacetKind::PartOf) && path.contains(&"name") {
        return Some(&mut fb.value);
    }
    if matches!(fb.kind, FacetKind::Entity | FacetKind::PartOf)
        && path.contains(&"predefinedType")
    {
        return Some(&mut fb.predefined_type_constraint);
    }
    if path.contains(&"value") {
        return Some(&mut fb.value);
    }
    if fb.kind == FacetKind::Property && path.contains(&"baseName") {
        return Some(&mut fb.base_name_constraint);
    }
    if fb.kind == FacetKind::Property && path.contains(&"propertySet") {
        return Some(&mut fb.property_set_constraint);
    }
    None
}

fn apply_restriction_attr(fb: &mut FacetBuilder, path: &[String], constraint: &str, value: &str) {
    let Some(slot) = constraint_slot_mut(fb, path) else {
        return;
    };
    let c = constraint.to_ascii_lowercase();
    match c.as_str() {
        "enumeration" => {
            // Enumeration values are collected from Empty events to avoid duplicate Start+Empty.
            let _ = value;
        }
        "pattern" => {
            let pat = value.to_string();
            match slot {
                Some(ValueConstraint::Pattern { patterns }) => patterns.push(pat),
                _ => *slot = Some(ValueConstraint::Pattern {
                    patterns: vec![pat],
                }),
            }
        }
        "mininclusive" => set_bounds(slot, |b| {
            if let ValueConstraint::Bounds { min_inclusive, .. } = b {
                *min_inclusive = parse_f64(value);
            }
        }),
        "maxinclusive" => set_bounds(slot, |b| {
            if let ValueConstraint::Bounds { max_inclusive, .. } = b {
                *max_inclusive = parse_f64(value);
            }
        }),
        "minexclusive" => set_bounds(slot, |b| {
            if let ValueConstraint::Bounds { min_exclusive, .. } = b {
                *min_exclusive = parse_f64(value);
            }
        }),
        "maxexclusive" => set_bounds(slot, |b| {
            if let ValueConstraint::Bounds { max_exclusive, .. } = b {
                *max_exclusive = parse_f64(value);
            }
        }),
        "length" => {
            if let Ok(n) = value.parse() {
                *slot = Some(ValueConstraint::Length { length: n });
            }
        }
        "minlength" => {
            if let Ok(n) = value.parse() {
                *slot = Some(ValueConstraint::MinLength { min: n });
            }
        }
        "maxlength" => {
            if let Ok(n) = value.parse() {
                *slot = Some(ValueConstraint::MaxLength { max: n });
            }
        }
        _ => {}
    }
}

fn ensure_bounds(slot: &mut Option<ValueConstraint>) -> &mut ValueConstraint {
    if !matches!(slot, Some(ValueConstraint::Bounds { .. })) {
        *slot = Some(ValueConstraint::Bounds {
            min_inclusive: None,
            max_inclusive: None,
            min_exclusive: None,
            max_exclusive: None,
        });
    }
    slot.as_mut().unwrap()
}

fn set_bounds(slot: &mut Option<ValueConstraint>, set: impl FnOnce(&mut ValueConstraint)) {
    set(ensure_bounds(slot));
}

fn parse_f64(s: &str) -> Option<f64> {
    s.parse().ok()
}

#[derive(Clone, Copy)]
enum Section {
    Applicability,
    Requirements,
}

struct FacetBuilder {
    kind: FacetKind,
    cardinality: Cardinality,
    entity_names: Vec<String>,
    predefined_type: Option<String>,
    predefined_type_constraint: Option<ValueConstraint>,
    attribute_name: Option<String>,
    attribute_names: Vec<String>,
    attribute_name_constraint: Option<ValueConstraint>,
    property_sets: Vec<String>,
    property_set_constraint: Option<ValueConstraint>,
    base_names: Vec<String>,
    base_name_constraint: Option<ValueConstraint>,
    data_type: Option<String>,
    value: Option<ValueConstraint>,
    partof_relation: Option<String>,
    classification_system: Option<ValueConstraint>,
    material_value: Option<String>,
}

impl FacetBuilder {
    fn into_facet(self) -> CompiledFacet {
        let mut entity_names = self.entity_names;
        entity_names.sort();
        entity_names.dedup();
        CompiledFacet {
            kind: self.kind,
            cardinality: self.cardinality,
            entity_names,
            predefined_type: self.predefined_type,
            predefined_type_constraint: self.predefined_type_constraint,
            attribute_name: self.attribute_name,
            attribute_names: self.attribute_names,
            attribute_name_constraint: self.attribute_name_constraint,
            property_set: self.property_sets.first().cloned(),
            property_sets: self.property_sets,
            property_set_constraint: self.property_set_constraint,
            base_name: self.base_names.first().cloned(),
            base_names: self.base_names,
            base_name_constraint: self.base_name_constraint,
            data_type: self.data_type,
            value: self.value,
            partof_relation: self.partof_relation,
            classification_system: self.classification_system,
            material_value: self.material_value,
        }
    }
}

fn local_name(name: &[u8]) -> String {
    let s = std::str::from_utf8(name).unwrap_or("");
    s.rsplit(':').next().unwrap_or(s).to_string()
}

fn attr_value(e: &BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .filter_map(|a| a.ok())
        .find(|a| {
            let k = std::str::from_utf8(a.key.as_ref()).unwrap_or("");
            k == key || k.ends_with(&format!(":{key}"))
        })
        .and_then(|a| std::str::from_utf8(&a.value).ok().map(|s| s.to_string()))
}

fn parse_cardinality(s: &str) -> Cardinality {
    match s.to_lowercase().as_str() {
        "optional" => Cardinality::Optional,
        "prohibited" => Cardinality::Prohibited,
        _ => Cardinality::Required,
    }
}

fn parse_max_occurs(s: &str) -> MaxOccurs {
    if s.eq_ignore_ascii_case("unbounded") {
        MaxOccurs::Unbounded("unbounded".into())
    } else {
        MaxOccurs::Count(s.parse().unwrap_or(u32::MAX))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_inline_simple_wall() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ids xmlns="http://standards.buildingsmart.org/IDS">
  <specifications>
    <specification name="Wall Existence" ifcVersion="IFC4">
      <applicability>
        <entity><name><simpleValue>IFCWALL</simpleValue></name></entity>
      </applicability>
      <requirements>
        <attribute cardinality="required">
          <name><simpleValue>Name</simpleValue></name>
        </attribute>
      </requirements>
    </specification>
  </specifications>
</ids>"#;
        let ids = parse_ids_xml("test.ids", xml).expect("parse");
        assert_eq!(ids.specifications.len(), 1);
        assert_eq!(ids.specifications[0].applicability[0].entity_names, vec!["IFCWALL"]);
    }

    #[test]
    fn parse_h29_style_entity_enumeration_and_datatype_attr() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ids:ids xmlns:ids="http://standards.buildingsmart.org/IDS" xmlns:xs="http://www.w3.org/2001/XMLSchema">
  <ids:specifications>
    <ids:specification ifcVersion="IFC2X3 IFC4" name="Test">
      <ids:applicability minOccurs="1" maxOccurs="unbounded">
        <ids:entity>
          <ids:name>
            <xs:restriction base="xs:string">
              <xs:enumeration value="IFCWALL"/>
              <xs:enumeration value="IFCDOOR"/>
            </xs:restriction>
          </ids:name>
        </ids:entity>
      </ids:applicability>
      <ids:requirements>
        <ids:property dataType="IFCLABEL" cardinality="optional">
          <ids:propertySet><ids:simpleValue>Pset_A</ids:simpleValue></ids:propertySet>
          <ids:baseName><ids:simpleValue>FireRating</ids:simpleValue></ids:baseName>
        </ids:property>
      </ids:requirements>
    </ids:specification>
  </ids:specifications>
</ids:ids>"#;
        let ids = parse_ids_xml("h29.ids", xml).expect("parse");
        let spec = &ids.specifications[0];
        assert_eq!(spec.ifc_versions, vec!["IFC2X3", "IFC4"]);
        assert_eq!(spec.min_occurs, 1);
        assert_eq!(
            spec.applicability[0].entity_names,
            vec!["IFCDOOR", "IFCWALL"]
        );
        let prop = &spec.requirements[0];
        assert_eq!(prop.data_type.as_deref(), Some("IFCLABEL"));
        assert_eq!(prop.cardinality, Cardinality::Optional);
        assert_eq!(prop.property_sets, vec!["Pset_A"]);
    }
}
