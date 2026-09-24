//! Strict IDS 1.0 XML parser (design §2.2): `roxmltree` DOM → [`IdsDocument`].
//!
//! Strict means: the structure accepted is exactly the IDS 1.0 XSD
//! (`http://standards.buildingsmart.org/IDS`, version 1.0.0; the copy
//! shipped in ifctester is byte-identical in structure to
//! buildingSMART/IDS `Schema/ids.xsd`). Unknown elements or attributes,
//! wrong namespaces, out-of-order sequences, missing required elements,
//! stray text, bad enumeration tokens and bad cardinality combinations
//! are all `IdsError::InvalidIds` naming the element or attribute.
//! Beyond the XSD, a few rules from the IDS 1.0 documentation that need
//! no IFC schema knowledge are enforced here too (prohibited
//! specifications must not carry requirements; min/maxOccurs must be
//! one of the three documented combinations; patterns and bounds must
//! be well-formed). Everything that needs the IFC schema (entity
//! names, attribute names, dataType validity, value lexical forms per
//! dataType) belongs to `compile`.
//!
//! `xsi:schemaLocation` / `xsi:noNamespaceSchemaLocation` are accepted
//! on any element (every XSD validator does); `xs:annotation` is
//! accepted (and skipped) inside `xs:restriction` and its facets.

use roxmltree::{Document, Node, NodeType, ParsingOptions};

use super::ir::{
    EntityFacet, Facet, FacetCardinality, IdsDocument, IdsInfo, Relation, Requirement,
    Restriction, Schema, Spec, SpecCardinality, Val, XsdBase,
};
use super::audit::{audit_facet, audit_requirement_entities, facet_allowed, lexical_ok};
use super::restriction::CompiledVal;
use super::xsd_regex::compile_xsd_pattern;
use super::IdsError;

pub const IDS_NS: &str = "http://standards.buildingsmart.org/IDS";
pub const XS_NS: &str = "http://www.w3.org/2001/XMLSchema";
pub const XSI_NS: &str = "http://www.w3.org/2001/XMLSchema-instance";

/// Parse and structurally validate an IDS 1.0 document.
///
/// Accepts UTF-8 (with or without BOM) and UTF-16 with BOM. DTDs are
/// refused. Every structural violation is `IdsError::InvalidIds` with
/// the element path and source line; a legal-but-unimplemented XML
/// construct (other encodings, some XSD bases/facets) is
/// `IdsError::Unsupported`.
pub fn parse_ids(xml: &[u8]) -> Result<IdsDocument, IdsError> {
    let text = decode(xml)?;
    let opts = ParsingOptions {
        allow_dtd: false,
        ..ParsingOptions::default()
    };
    let doc = Document::parse_with_options(&text, opts).map_err(|e| {
        let pos = e.pos();
        IdsError::InvalidIds {
            path: String::new(),
            line: Some(pos.row),
            msg: format!("not well-formed XML: {e}"),
        }
    })?;
    let p = Parser { doc: &doc };
    p.document()
}

fn decode(xml: &[u8]) -> Result<String, IdsError> {
    if let Some(rest) = xml.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec())
            .map_err(|e| IdsError::invalid(format!("IDS has a UTF-8 BOM but is not valid UTF-8: {e}")));
    }
    let utf16 = |rest: &[u8], le: bool| -> Result<String, IdsError> {
        if rest.len() % 2 != 0 {
            return Err(IdsError::invalid("IDS has a UTF-16 BOM but an odd byte length"));
        }
        let units: Vec<u16> = rest
            .chunks_exact(2)
            .map(|c| {
                if le {
                    u16::from_le_bytes([c[0], c[1]])
                } else {
                    u16::from_be_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16(&units).map_err(|e| IdsError::invalid(format!("IDS is not valid UTF-16: {e}")))
    };
    if let Some(rest) = xml.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, true);
    }
    if let Some(rest) = xml.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, false);
    }
    match std::str::from_utf8(xml) {
        Ok(s) => Ok(s.to_string()),
        Err(e) => {
            // A declared non-UTF encoding is legal XML we do not decode.
            if let Some(enc) = declared_encoding(xml) {
                let lower = enc.to_ascii_lowercase();
                if lower != "utf-8" && lower != "utf8" {
                    return Err(IdsError::unsupported(format!("xml-encoding:{enc}")));
                }
            }
            Err(IdsError::invalid(format!("IDS is not valid UTF-8: {e}")))
        }
    }
}

/// `encoding="..."` from an ASCII-compatible XML declaration.
fn declared_encoding(xml: &[u8]) -> Option<String> {
    let head = &xml[..xml.len().min(200)];
    let head: String = head.iter().take_while(|b| b.is_ascii()).map(|&b| b as char).collect();
    if !head.starts_with("<?xml") {
        return None;
    }
    let decl_end = head.find("?>")?;
    let decl = &head[..decl_end];
    let at = decl.find("encoding")?;
    let rest = decl[at + "encoding".len()..].trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let body = &rest[1..];
    let end = body.find(quote)?;
    Some(body[..end].to_string())
}

/// Allowed attribute set for an element.
type Attrs<'s> = &'s [&'s str];

struct Parser<'d, 'input> {
    doc: &'d Document<'input>,
}

impl<'d, 'input: 'd> Parser<'d, 'input> {
    fn line(&self, n: Node) -> Option<u32> {
        Some(self.doc.text_pos_at(n.range().start).row)
    }

    fn err(&self, n: Node, path: &str, msg: impl Into<String>) -> IdsError {
        IdsError::InvalidIds {
            path: path.to_string(),
            line: self.line(n),
            msg: msg.into(),
        }
    }

    fn is_ids(n: Node, local: &str) -> bool {
        n.is_element() && n.tag_name().namespace() == Some(IDS_NS) && n.tag_name().name() == local
    }

    fn is_xs(n: Node, local: &str) -> bool {
        n.is_element() && n.tag_name().namespace() == Some(XS_NS) && n.tag_name().name() == local
    }

    fn describe(n: Node) -> String {
        match n.tag_name().namespace() {
            Some(IDS_NS) => format!("<{}>", n.tag_name().name()),
            Some(XS_NS) => format!("<xs:{}>", n.tag_name().name()),
            Some(ns) => format!("<{{{ns}}}{}>", n.tag_name().name()),
            None => format!("<{}> (no namespace)", n.tag_name().name()),
        }
    }

    /// Reject any attribute outside `allowed` (unqualified names) and the
    /// xsi schema-location hints.
    fn check_attrs(&self, n: Node, path: &str, allowed: Attrs) -> Result<(), IdsError> {
        for a in n.attributes() {
            match a.namespace() {
                None if allowed.contains(&a.name()) => {}
                Some(XSI_NS) if matches!(a.name(), "schemaLocation" | "noNamespaceSchemaLocation") => {}
                ns => {
                    let q = match ns {
                        Some(XSI_NS) => format!("xsi:{}", a.name()),
                        Some(other) => format!("{{{other}}}{}", a.name()),
                        None => a.name().to_string(),
                    };
                    let expected = if allowed.is_empty() {
                        "no attributes".to_string()
                    } else {
                        allowed.join(", ")
                    };
                    return Err(self.err(
                        n,
                        path,
                        format!("unexpected attribute '{q}' on {} (allowed: {expected})", Self::describe(n)),
                    ));
                }
            }
        }
        Ok(())
    }

    fn attr<'a, 'i>(n: Node<'a, 'i>, name: &str) -> Option<&'a str> {
        n.attributes()
            .find(|a| a.namespace().is_none() && a.name() == name)
            .map(|a| a.value())
    }

    /// Child elements of element-only content. Non-whitespace text is an
    /// error; comments and processing instructions are ignored.
    fn elements(&self, n: Node<'d, 'input>, path: &str) -> Result<Vec<Node<'d, 'input>>, IdsError> {
        let mut out = Vec::new();
        for c in n.children() {
            match c.node_type() {
                NodeType::Element => out.push(c),
                NodeType::Text => {
                    let t = c.text().unwrap_or("");
                    if !t.trim().is_empty() {
                        return Err(self.err(
                            c,
                            path,
                            format!("unexpected text '{}' inside {}", t.trim(), Self::describe(n)),
                        ));
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Text content of a simple-typed element (`xs:string`: verbatim).
    fn text(&self, n: Node, path: &str) -> Result<String, IdsError> {
        let mut s = String::new();
        for c in n.children() {
            match c.node_type() {
                NodeType::Text => s.push_str(c.text().unwrap_or("")),
                NodeType::Element => {
                    return Err(self.err(
                        c,
                        path,
                        format!("{} must contain text only, found {}", Self::describe(n), Self::describe(c)),
                    ))
                }
                _ => {}
            }
        }
        Ok(s)
    }

    /// Match an ordered XSD sequence of IDS-namespace elements, each with
    /// (min, max) occurrences (`max = None` = unbounded). Returns the
    /// nodes per slot.
    fn sequence(
        &self,
        parent: Node<'d, 'input>,
        path: &str,
        children: &[Node<'d, 'input>],
        slots: &[(&str, usize, Option<usize>)],
    ) -> Result<Vec<Vec<Node<'d, 'input>>>, IdsError> {
        let mut out: Vec<Vec<Node>> = vec![Vec::new(); slots.len()];
        let mut i = 0usize;
        for (k, (name, min, max)) in slots.iter().enumerate() {
            while i < children.len() && Self::is_ids(children[i], name) {
                if max.is_some_and(|m| out[k].len() >= m) {
                    return Err(self.err(
                        children[i],
                        path,
                        format!("<{name}> may appear at most {} time(s) here", max.unwrap_or(0)),
                    ));
                }
                out[k].push(children[i]);
                i += 1;
            }
            if out[k].len() < *min {
                return Err(self.err(
                    parent,
                    path,
                    format!("missing required element <{name}> in {}", Self::describe(parent)),
                ));
            }
        }
        if let Some(extra) = children.get(i) {
            let expected: Vec<String> = slots.iter().map(|(n, _, _)| format!("<{n}>")).collect();
            return Err(self.err(
                *extra,
                path,
                format!(
                    "unexpected element {} in {} (expected, in this order: {})",
                    Self::describe(*extra),
                    Self::describe(parent),
                    expected.join(", ")
                ),
            ));
        }
        Ok(out)
    }

    fn document(&self) -> Result<IdsDocument, IdsError> {
        let root = self.doc.root_element();
        let path = "/ids";
        if root.tag_name().name() != "ids" || root.tag_name().namespace() != Some(IDS_NS) {
            return Err(self.err(
                root,
                path,
                format!(
                    "root element must be <ids> in namespace {IDS_NS}, found {}",
                    Self::describe(root)
                ),
            ));
        }
        self.check_attrs(root, path, &[])?;
        let kids = self.elements(root, path)?;
        let slots = self.sequence(root, path, &kids, &[("info", 1, Some(1)), ("specifications", 1, Some(1))])?;
        let info = self.info(slots[0][0], "/ids/info")?;
        let specs_node = slots[1][0];
        let specs_path = "/ids/specifications";
        self.check_attrs(specs_node, specs_path, &[])?;
        let spec_kids = self.elements(specs_node, specs_path)?;
        let spec_slots = self.sequence(specs_node, specs_path, &spec_kids, &[("specification", 1, None)])?;
        let mut specs = Vec::with_capacity(spec_slots[0].len());
        for (i, s) in spec_slots[0].iter().enumerate() {
            let sp = format!("{specs_path}/specification[{}]", i + 1);
            specs.push(self.spec(*s, &sp, i as u32)?);
        }
        Ok(IdsDocument { info, specs })
    }

    fn info(&self, n: Node<'d, 'input>, path: &str) -> Result<IdsInfo, IdsError> {
        self.check_attrs(n, path, &[])?;
        let kids = self.elements(n, path)?;
        let names = [
            "title",
            "copyright",
            "version",
            "description",
            "author",
            "date",
            "purpose",
            "milestone",
        ];
        let slots: Vec<(&str, usize, Option<usize>)> = names
            .iter()
            .map(|nm| (*nm, usize::from(*nm == "title"), Some(1)))
            .collect();
        let got = self.sequence(n, path, &kids, &slots)?;
        let mut vals: Vec<Option<String>> = Vec::with_capacity(names.len());
        for (k, nm) in names.iter().enumerate() {
            match got[k].first() {
                Some(node) => {
                    let p = format!("{path}/{nm}");
                    self.check_attrs(*node, &p, &[])?;
                    vals.push(Some(self.text(*node, &p)?));
                }
                None => vals.push(None),
            }
        }
        let mut it = vals.into_iter();
        let mut next = || it.next().flatten();
        let title = next().unwrap_or_default();
        let copyright = next();
        let version = next();
        let description = next();
        let author = next();
        let date = next();
        let purpose = next();
        let milestone = next();
        if let Some(a) = &author {
            let re = compile_xsd_pattern(r"[^@]+@[^\.]+\..+")?;
            if !re.is_match(a) {
                let node = got[4][0];
                return Err(self.err(
                    node,
                    &format!("{path}/author"),
                    format!("author '{a}' is not an e-mail address (XSD pattern [^@]+@[^\\.]+\\..+)"),
                ));
            }
        }
        if let Some(d) = &date {
            if !lexical_ok(XsdBase::Date, d) {
                let node = got[5][0];
                return Err(self.err(
                    node,
                    &format!("{path}/date"),
                    format!("date '{d}' is not an xs:date (YYYY-MM-DD with optional timezone)"),
                ));
            }
        }
        Ok(IdsInfo {
            title,
            copyright,
            version,
            description,
            author,
            date,
            purpose,
            milestone,
        })
    }

    fn spec(&self, n: Node<'d, 'input>, path: &str, idx: u32) -> Result<Spec, IdsError> {
        self.check_attrs(n, path, &["name", "ifcVersion", "identifier", "description", "instructions"])?;
        let name = Self::attr(n, "name")
            .ok_or_else(|| self.err(n, path, "<specification> is missing required attribute 'name'"))?
            .to_string();
        let ver_raw = Self::attr(n, "ifcVersion")
            .ok_or_else(|| self.err(n, path, "<specification> is missing required attribute 'ifcVersion'"))?;
        let mut ifc_versions = Vec::new();
        for tok in ver_raw.split_ascii_whitespace() {
            let s = Schema::from_ids_token(tok).ok_or_else(|| {
                self.err(
                    n,
                    &format!("{path}/@ifcVersion"),
                    format!("ifcVersion token '{tok}' is not one of IFC2X3, IFC4, IFC4X3_ADD2"),
                )
            })?;
            ifc_versions.push(s);
        }
        if ifc_versions.is_empty() {
            return Err(self.err(
                n,
                &format!("{path}/@ifcVersion"),
                "ifcVersion is empty (a specification must target at least one of IFC2X3, IFC4, IFC4X3_ADD2)",
            ));
        }
        let kids = self.elements(n, path)?;
        let slots = self.sequence(n, path, &kids, &[("applicability", 1, Some(1)), ("requirements", 0, Some(1))])?;

        let app = slots[0][0];
        let app_path = format!("{path}/applicability");
        self.check_attrs(app, &app_path, &["minOccurs", "maxOccurs"])?;
        let (cardinality, min_occurs, max_occurs) = self.spec_cardinality(app, &app_path)?;
        let applicability = self.applicability(app, &app_path)?;

        let (requirements, requirements_description) = match slots[1].first() {
            Some(req) => {
                let rp = format!("{path}/requirements");
                self.check_attrs(*req, &rp, &["description"])?;
                let facets = self.requirements(*req, &rp)?;
                if cardinality == SpecCardinality::Prohibited && !facets.is_empty() {
                    return Err(self.err(
                        *req,
                        &rp,
                        "a prohibited specification (minOccurs=0 maxOccurs=0) must not specify requirements",
                    ));
                }
                if let Err((i, msg)) = audit_requirement_entities(&applicability, &facets) {
                    return Err(self.err(*req, &format!("{rp}/*[{}]", i + 1), msg));
                }
                (facets, Self::attr(*req, "description").map(str::to_string))
            }
            None => (Vec::new(), None),
        };

        Ok(Spec {
            idx,
            name,
            identifier: Self::attr(n, "identifier").map(str::to_string),
            description: Self::attr(n, "description").map(str::to_string),
            instructions: Self::attr(n, "instructions").map(str::to_string),
            ifc_versions,
            cardinality,
            min_occurs,
            max_occurs,
            applicability,
            requirements,
            requirements_description,
        })
    }

    /// `applicability/@minOccurs` + `@maxOccurs` → (usage, min, max).
    ///
    /// Absent attributes take the `xs:occurs` defaults (1 / 1), which is
    /// also what IfcTester sees (xmlschema fills attribute defaults; a
    /// bare `<applicability>` is "required" there). Accepted pairs are the
    /// three of IDS UserManual/specifications.md plus the default-filled
    /// forms: min 1 with max 1|unbounded = required, min 0 with max
    /// 1|unbounded = optional, 0/0 = prohibited. Anything else (min > 1,
    /// other finite max, min > max) is rejected.
    fn spec_cardinality(&self, app: Node, path: &str) -> Result<(SpecCardinality, u32, Option<u32>), IdsError> {
        let parse = |attr: &str, v: &str| -> Result<u32, IdsError> {
            parse_non_negative_integer(v)
                .and_then(|x| u32::try_from(x).ok())
                .ok_or_else(|| {
                    self.err(
                        app,
                        &format!("{path}/@{attr}"),
                        format!(
                            "{attr} '{v}' is not a non-negative integer{}",
                            if attr == "maxOccurs" { " or 'unbounded'" } else { "" }
                        ),
                    )
                })
        };
        let min = match Self::attr(app, "minOccurs") {
            None => 1,
            Some(v) => parse("minOccurs", v)?,
        };
        let max = match Self::attr(app, "maxOccurs") {
            None => Some(1),
            Some(v) if v.trim() == "unbounded" => None,
            Some(v) => Some(parse("maxOccurs", v)?),
        };
        let card = match (min, max) {
            (1, None | Some(1)) => SpecCardinality::Required,
            (0, None | Some(1)) => SpecCardinality::Optional,
            (0, Some(0)) => SpecCardinality::Prohibited,
            (lo, hi) => {
                return Err(self.err(
                    app,
                    path,
                    format!(
                        "minOccurs={lo} maxOccurs={} is not an IDS 1.0 specification cardinality \
                         (allowed: 1/unbounded = required, 0/unbounded = optional, 0/0 = prohibited; \
                         absent attributes default to 1)",
                        hi.map_or("unbounded".to_string(), |h| h.to_string())
                    ),
                ))
            }
        };
        Ok((card, min, max))
    }

    fn applicability(&self, n: Node<'d, 'input>, path: &str) -> Result<Vec<Facet>, IdsError> {
        let kids = self.elements(n, path)?;
        let slots = self.sequence(
            n,
            path,
            &kids,
            &[
                ("entity", 0, Some(1)),
                ("partOf", 0, None),
                ("classification", 0, None),
                ("attribute", 0, None),
                ("property", 0, None),
                ("material", 0, None),
            ],
        )?;
        let mut out = Vec::new();
        for group in slots {
            for (i, f) in group.iter().enumerate() {
                let fp = format!("{path}/{}[{}]", f.tag_name().name(), i + 1);
                out.push(self.facet(*f, &fp, false)?.0);
            }
        }
        Ok(out)
    }

    /// `requirementsType` is `sequence maxOccurs=unbounded` of optional
    /// facets, i.e. any order, any count.
    fn requirements(&self, n: Node<'d, 'input>, path: &str) -> Result<Vec<Requirement>, IdsError> {
        let kids = self.elements(n, path)?;
        let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        let mut out = Vec::with_capacity(kids.len());
        for k in kids {
            let local = k.tag_name().name();
            let known = ["entity", "partOf", "classification", "attribute", "property", "material"];
            if k.tag_name().namespace() != Some(IDS_NS) || !known.contains(&local) {
                return Err(self.err(
                    k,
                    path,
                    format!(
                        "unexpected element {} in <requirements> (allowed: entity, partOf, classification, attribute, property, material)",
                        Self::describe(k)
                    ),
                ));
            }
            let c = counts.entry(local).or_insert(0);
            *c += 1;
            let fp = format!("{path}/{local}[{c}]");
            let (facet, card, instructions) = self.facet(k, &fp, true)?;
            out.push(Requirement {
                facet,
                cardinality: card,
                instructions,
            });
        }
        Ok(out)
    }

    /// Parse one facet element. `req` selects the requirements variant
    /// (which adds cardinality / uri / instructions attributes).
    fn facet(
        &self,
        n: Node<'d, 'input>,
        path: &str,
        req: bool,
    ) -> Result<(Facet, FacetCardinality, Option<String>), IdsError> {
        let local = n.tag_name().name();
        let kids = self.elements(n, path)?;
        let instructions = if req {
            Self::attr(n, "instructions").map(str::to_string)
        } else {
            None
        };
        let facet = match local {
            "entity" => {
                self.check_attrs(n, path, if req { &["instructions"] } else { &[] })?;
                Facet::Entity(self.entity_body(n, path, &kids)?)
            }
            "attribute" => {
                self.check_attrs(n, path, if req { &["cardinality", "instructions"] } else { &[] })?;
                let s = self.sequence(n, path, &kids, &[("name", 1, Some(1)), ("value", 0, Some(1))])?;
                Facet::Attribute {
                    name: self.val(s[0][0], &format!("{path}/name"))?,
                    value: self.opt_val(&s[1], &format!("{path}/value"))?,
                }
            }
            "property" => {
                self.check_attrs(
                    n,
                    path,
                    if req {
                        &["dataType", "uri", "cardinality", "instructions"]
                    } else {
                        &["dataType"]
                    },
                )?;
                let s = self.sequence(
                    n,
                    path,
                    &kids,
                    &[("propertySet", 1, Some(1)), ("baseName", 1, Some(1)), ("value", 0, Some(1))],
                )?;
                let data_type = match Self::attr(n, "dataType") {
                    None => None,
                    Some(dt) => {
                        // upperCaseName: xs:normalizedString, pattern [A-Z]+.
                        let norm: String = dt
                            .chars()
                            .map(|c| if matches!(c, '\t' | '\n' | '\r') { ' ' } else { c })
                            .collect();
                        if norm.is_empty() || !norm.chars().all(|c| c.is_ascii_uppercase()) {
                            return Err(self.err(
                                n,
                                &format!("{path}/@dataType"),
                                format!("dataType '{dt}' must be an uppercase IFC type name (XSD pattern [A-Z]+)"),
                            ));
                        }
                        Some(norm)
                    }
                };
                Facet::Property {
                    property_set: self.val(s[0][0], &format!("{path}/propertySet"))?,
                    base_name: self.val(s[1][0], &format!("{path}/baseName"))?,
                    value: self.opt_val(&s[2], &format!("{path}/value"))?,
                    data_type,
                    uri: if req { Self::attr(n, "uri").map(str::to_string) } else { None },
                }
            }
            "classification" => {
                self.check_attrs(n, path, if req { &["uri", "cardinality", "instructions"] } else { &[] })?;
                let s = self.sequence(n, path, &kids, &[("value", 0, Some(1)), ("system", 1, Some(1))])?;
                Facet::Classification {
                    value: self.opt_val(&s[0], &format!("{path}/value"))?,
                    system: self.val(s[1][0], &format!("{path}/system"))?,
                    uri: if req { Self::attr(n, "uri").map(str::to_string) } else { None },
                }
            }
            "material" => {
                self.check_attrs(n, path, if req { &["uri", "cardinality", "instructions"] } else { &[] })?;
                let s = self.sequence(n, path, &kids, &[("value", 0, Some(1))])?;
                Facet::Material {
                    value: self.opt_val(&s[0], &format!("{path}/value"))?,
                    uri: if req { Self::attr(n, "uri").map(str::to_string) } else { None },
                }
            }
            "partOf" => {
                self.check_attrs(n, path, if req { &["relation", "cardinality", "instructions"] } else { &["relation"] })?;
                let s = self.sequence(n, path, &kids, &[("entity", 1, Some(1))])?;
                let ent = s[0][0];
                let ep = format!("{path}/entity");
                self.check_attrs(ent, &ep, &[])?;
                let ent_kids = self.elements(ent, &ep)?;
                let entity = self.entity_body(ent, &ep, &ent_kids)?;
                let relation = match Self::attr(n, "relation") {
                    None => None,
                    Some(r) => Some(Relation::from_ids_token(r).ok_or_else(|| {
                        self.err(
                            n,
                            &format!("{path}/@relation"),
                            format!(
                                "relation '{r}' is not one of IFCRELAGGREGATES, IFCRELASSIGNSTOGROUP, \
                                 IFCRELCONTAINEDINSPATIALSTRUCTURE, IFCRELNESTS, \
                                 'IFCRELVOIDSELEMENT IFCRELFILLSELEMENT'"
                            ),
                        )
                    })?),
                };
                Facet::PartOf {
                    entity: Box::new(entity),
                    relation,
                }
            }
            other => {
                return Err(self.err(n, path, format!("unknown facet <{other}>")));
            }
        };
        audit_facet(&facet).map_err(|m| self.err(n, path, m))?;
        let card = if req && local != "entity" {
            match Self::attr(n, "cardinality") {
                None | Some("required") => FacetCardinality::Required,
                Some("prohibited") => FacetCardinality::Prohibited,
                Some("optional") if local != "partOf" => FacetCardinality::Optional,
                Some(c) => {
                    let allowed = if local == "partOf" {
                        "required, prohibited"
                    } else {
                        "required, optional, prohibited"
                    };
                    return Err(self.err(
                        n,
                        &format!("{path}/@cardinality"),
                        format!("cardinality '{c}' is not allowed on <{local}> (allowed: {allowed})"),
                    ));
                }
            }
        } else {
            FacetCardinality::Required
        };
        Ok((facet, card, instructions))
    }

    fn entity_body(
        &self,
        n: Node<'d, 'input>,
        path: &str,
        kids: &[Node<'d, 'input>],
    ) -> Result<EntityFacet, IdsError> {
        let s = self.sequence(n, path, kids, &[("name", 1, Some(1)), ("predefinedType", 0, Some(1))])?;
        Ok(EntityFacet {
            name: self.val(s[0][0], &format!("{path}/name"))?,
            predefined_type: self.opt_val(&s[1], &format!("{path}/predefinedType"))?,
        })
    }

    fn opt_val(&self, nodes: &[Node<'d, 'input>], path: &str) -> Result<Option<Val>, IdsError> {
        match nodes.first() {
            Some(n) => Ok(Some(self.val(*n, path)?)),
            None => Ok(None),
        }
    }

    /// `idsValue`: exactly one of `<simpleValue>` / `<xs:restriction>`.
    fn val(&self, n: Node<'d, 'input>, path: &str) -> Result<Val, IdsError> {
        self.check_attrs(n, path, &[])?;
        let kids = self.elements(n, path)?;
        if kids.len() != 1 {
            return Err(self.err(
                n,
                path,
                format!(
                    "{} must contain exactly one <simpleValue> or <xs:restriction>, found {}",
                    Self::describe(n),
                    kids.len()
                ),
            ));
        }
        let c = kids[0];
        if Self::is_ids(c, "simpleValue") {
            let sp = format!("{path}/simpleValue");
            self.check_attrs(c, &sp, &[])?;
            return Ok(Val::Simple(self.text(c, &sp)?));
        }
        if Self::is_xs(c, "restriction") {
            let rp = format!("{path}/xs:restriction");
            let r = self.restriction(c, &rp)?;
            let val = Val::Restriction(r);
            // Validate patterns and bounds now. `Unsupported` (e.g. an
            // untranslated Unicode block) is left for `compile`, which
            // attaches the spec index and honours on_unsupported="mark".
            match CompiledVal::new(&val) {
                Ok(_) | Err(IdsError::Unsupported { .. }) => {}
                Err(e) => return Err(e.at(&rp, self.line(c))),
            }
            return Ok(val);
        }
        Err(self.err(
            c,
            path,
            format!(
                "unexpected element {} (expected <simpleValue> or <xs:restriction>)",
                Self::describe(c)
            ),
        ))
    }

    fn restriction(&self, n: Node<'d, 'input>, path: &str) -> Result<Restriction, IdsError> {
        self.check_attrs(n, path, &["base"])?;
        let base = match Self::attr(n, "base") {
            // IfcTester default (facet.py Restriction.parse: "xs:string").
            None => XsdBase::String,
            Some(q) => {
                let q = q.trim();
                let (prefix, local) = match q.split_once(':') {
                    Some((p, l)) => (Some(p), l),
                    None => (None, q),
                };
                let ns = n.lookup_namespace_uri(prefix);
                if ns != Some(XS_NS) {
                    return Err(self.err(
                        n,
                        &format!("{path}/@base"),
                        format!("base '{q}' is not a type in the XML Schema namespace {XS_NS}"),
                    ));
                }
                XsdBase::from_local_name(local).ok_or_else(|| {
                    self.err(
                        n,
                        &format!("{path}/@base"),
                        format!(
                            "base xs:{local} is not an IDS 1.0 restriction base \
                             (allowed: xs:string, xs:boolean, xs:integer, xs:double, xs:date, xs:dateTime, xs:time, xs:duration)"
                        ),
                    )
                })?
            }
        };
        let mut r = Restriction {
            base,
            ..Restriction::default()
        };
        let mut n_facets = 0usize;
        for c in self.elements(n, path)? {
            if c.tag_name().namespace() != Some(XS_NS) {
                return Err(self.err(
                    c,
                    path,
                    format!("unexpected element {} inside <xs:restriction>", Self::describe(c)),
                ));
            }
            let local = c.tag_name().name();
            let fp = format!("{path}/xs:{local}");
            match local {
                "annotation" => continue,
                "enumeration" | "pattern" | "minInclusive" | "minExclusive" | "maxInclusive"
                | "maxExclusive" | "length" | "minLength" | "maxLength" => {}
                "totalDigits" | "fractionDigits" | "whiteSpace" | "assertion" | "explicitTimezone"
                | "simpleType" => {
                    return Err(IdsError::unsupported(format!("xsd-restriction:{local}")));
                }
                other => {
                    return Err(self.err(c, &fp, format!("<xs:{other}> is not an xs:restriction facet")));
                }
            }
            if !facet_allowed(base, local) {
                return Err(self.err(
                    c,
                    &fp,
                    format!(
                        "<xs:{local}> is not allowed on base xs:{} (IDS DataTypes.md: string → pattern, enumeration, lengths; \
                         boolean → pattern; numeric/temporal → pattern, enumeration, bounds)",
                        base.local_name()
                    ),
                ));
            }
            self.check_attrs(c, &fp, &["value"])?;
            for gc in self.elements(c, &fp)? {
                if !Self::is_xs(gc, "annotation") {
                    return Err(self.err(
                        gc,
                        &fp,
                        format!("unexpected element {} inside <xs:{local}>", Self::describe(gc)),
                    ));
                }
            }
            let v = Self::attr(c, "value")
                .ok_or_else(|| self.err(c, &fp, format!("<xs:{local}> is missing required attribute 'value'")))?
                .to_string();
            n_facets += 1;
            let dup = |this: &Self| this.err(c, &fp, format!("<xs:{local}> may appear at most once per restriction"));
            let len = |this: &Self, v: &str| -> Result<u32, IdsError> {
                parse_non_negative_integer(v)
                    .and_then(|x| u32::try_from(x).ok())
                    .ok_or_else(|| this.err(c, &fp, format!("<xs:{local}> value '{v}' is not a non-negative integer")))
            };
            match local {
                "enumeration" => {
                    if !lexical_ok(base, &v) {
                        return Err(self.err(
                            c,
                            &fp,
                            format!("enumeration value '{v}' is not a valid xs:{} literal", base.local_name()),
                        ));
                    }
                    r.enumeration.get_or_insert_with(Vec::new).push(v);
                }
                "pattern" => r.patterns.get_or_insert_with(Vec::new).push(v),
                "minInclusive" | "minExclusive" | "maxInclusive" | "maxExclusive" => {
                    if !lexical_ok(base, &v) {
                        return Err(self.err(
                            c,
                            &fp,
                            format!("<xs:{local}> value '{v}' is not a valid xs:{} literal", base.local_name()),
                        ));
                    }
                    let slot = match local {
                        "minInclusive" => &mut r.min_inclusive,
                        "minExclusive" => &mut r.min_exclusive,
                        "maxInclusive" => &mut r.max_inclusive,
                        _ => &mut r.max_exclusive,
                    };
                    if slot.is_some() {
                        return Err(dup(self));
                    }
                    *slot = Some(v);
                }
                _ => {
                    let x = len(self, &v)?;
                    let slot = match local {
                        "length" => &mut r.length,
                        "minLength" => &mut r.min_length,
                        _ => &mut r.max_length,
                    };
                    if slot.is_some() {
                        return Err(dup(self));
                    }
                    *slot = Some(x);
                }
            }
        }
        if n_facets == 0 {
            return Err(self.err(
                n,
                path,
                "<xs:restriction> has no facets (it would match every value); add enumeration, pattern, bounds or lengths",
            ));
        }
        Ok(r)
    }
}

/// XSD `nonNegativeInteger` lexical form (whitespace collapsed, optional `+`).
fn parse_non_negative_integer(v: &str) -> Option<u64> {
    let t = v.trim_matches(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r'));
    let t = t.strip_prefix('+').unwrap_or(t);
    if t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    t.parse::<u64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_occurs_lexical() {
        assert_eq!(parse_non_negative_integer(" +1 "), Some(1));
        assert_eq!(parse_non_negative_integer("-1"), None);
        assert_eq!(parse_non_negative_integer("unbounded"), None);
    }

    #[test]
    fn ids_declared_encoding_sniff() {
        assert_eq!(
            declared_encoding(b"<?xml version=\"1.0\" encoding='ISO-8859-1'?><ids/>").as_deref(),
            Some("ISO-8859-1")
        );
        assert_eq!(declared_encoding(b"<ids/>"), None);
    }
}
