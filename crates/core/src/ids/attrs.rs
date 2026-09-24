//! Read any explicit attribute of a STEP record by its generated schema
//! position (design §2.4, "Attribute" row).
//!
//! Decoding reuses the crate's STEP helpers only — `lexer::split_top_level_args`
//! for argument splitting, `lexer::parse_field` (and through it
//! `lexer::decode_string`, which resolves `\X2\`/`\X4\`/`\X\`/`\S\`
//! escapes) for values. There is no second tokenizer here.
//!
//! The decoded [`AttrValue`] keeps the distinctions the IDS attribute
//! facet needs and a plain [`Actual`] cannot carry: null vs derived vs
//! logical UNKNOWN (all "absent"), entity references and typed select
//! values (present, but never value-comparable), and lists.

use super::restriction::{py_float_repr, Actual};
use super::schema_tables::AttrKind;
use super::IdsError;
use crate::entity_table::EntityTable;
use crate::lexer::{parse_field, split_top_level_args, Field};

/// One decoded STEP attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum AttrValue {
    /// `$`
    Null,
    /// `*` (a slot redeclared as DERIVE in a subtype).
    Derived,
    /// `.U.` on a LOGICAL.
    Unknown,
    Str(String),
    /// An enumeration literal without its dots (`.SOLIDWALL.` → `SOLIDWALL`).
    Enum(String),
    Int(i64),
    Real(f64),
    Bool(bool),
    List(Vec<AttrValue>),
    /// `#id`
    Ref(u64),
    /// A typed select value, e.g. `IFCNORMALISEDRATIOMEASURE(0.5)`.
    Typed {
        type_name: String,
        value: Box<AttrValue>,
    },
}

impl AttrValue {
    /// IfcTester's "empty" test for attribute values (`ifctester/facet.py:332-357`):
    /// `None` (null or derived), `""`, an empty aggregate, or LOGICAL
    /// UNKNOWN. Empty values count as absent.
    pub fn is_empty(&self) -> bool {
        match self {
            AttrValue::Null | AttrValue::Derived | AttrValue::Unknown => true,
            AttrValue::Str(s) => s.is_empty(),
            AttrValue::List(v) => v.is_empty(),
            _ => false,
        }
    }

    /// `$` or `*`: no value was written at all (as opposed to an empty
    /// string, an empty list or UNKNOWN, which are written values).
    pub fn is_null(&self) -> bool {
        matches!(self, AttrValue::Null | AttrValue::Derived)
    }

    /// The comparable scalar, if this value can be compared against an
    /// IDS value at all. References, typed select values, lists and the
    /// absent forms return `None`: IfcTester fails every value check on
    /// them (`facet.py:359-385`: entity instances fail outright, tuples
    /// never equal a string or a cast of one).
    pub fn to_actual(&self) -> Option<Actual> {
        match self {
            AttrValue::Str(s) | AttrValue::Enum(s) => Some(Actual::Str(s.clone())),
            AttrValue::Int(i) => Some(Actual::Int(*i)),
            AttrValue::Real(x) => Some(Actual::Num(*x)),
            AttrValue::Bool(b) => Some(Actual::Bool(*b)),
            _ => None,
        }
    }

    /// The value as its string, when it is a string or enumeration.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            AttrValue::Str(s) | AttrValue::Enum(s) => Some(s),
            _ => None,
        }
    }

    /// Python `str(value)` of what ifcopenshell returns for this value,
    /// used for the `actual` column (IfcTester's `reason["actual"]`).
    /// References print as `#id` (IfcTester prints the whole instance).
    pub fn py_str(&self) -> String {
        match self {
            AttrValue::Null | AttrValue::Derived => "None".into(),
            AttrValue::Unknown => "UNKNOWN".into(),
            AttrValue::Str(s) | AttrValue::Enum(s) => s.clone(),
            AttrValue::Int(i) => i.to_string(),
            AttrValue::Real(x) => py_float_repr(*x),
            AttrValue::Bool(b) => if *b { "True" } else { "False" }.into(),
            AttrValue::Ref(id) => format!("#{id}"),
            AttrValue::Typed { type_name, value } => format!("{type_name}({})", value.py_repr()),
            AttrValue::List(items) => {
                let inner: Vec<String> = items.iter().map(AttrValue::py_repr).collect();
                if inner.len() == 1 {
                    format!("({},)", inner[0])
                } else {
                    format!("({})", inner.join(", "))
                }
            }
        }
    }

    /// Python `repr(value)` (strings quoted), for list members.
    fn py_repr(&self) -> String {
        match self {
            AttrValue::Str(s) | AttrValue::Enum(s) => py_repr_str(s),
            other => other.py_str(),
        }
    }
}

/// Python `repr(str)`: single quotes unless the text contains `'` and no
/// `"`; backslash, the quote and control characters escaped.
pub(crate) fn py_repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

/// One record split into its arguments, with its class resolved to the
/// schema's canonical (uppercase, `'static`) name.
#[derive(Debug)]
pub struct Record<'a> {
    pub id: u64,
    pub class: &'static str,
    pub args: Vec<&'a [u8]>,
}

impl<'a> Record<'a> {
    /// Split record `id`. `class` must be the canonical name of its type
    /// token (the caller resolved it against the schema tables).
    pub fn load(
        table: &'a EntityTable,
        id: u64,
        class: &'static str,
    ) -> Result<Record<'a>, IdsError> {
        let (_, args) = table.get(id).ok_or_else(|| IdsError::IfcInput {
            msg: format!("record #{id} is referenced but not present in the DATA section"),
        })?;
        Ok(Record {
            id,
            class,
            args: split_top_level_args(args),
        })
    }

    /// Decode the argument at schema position `pos` of kind `kind`.
    pub fn attr(&self, pos: u16, kind: AttrKind) -> Result<AttrValue, IdsError> {
        let raw = self
            .args
            .get(pos as usize)
            .ok_or_else(|| IdsError::IfcInput {
                msg: format!(
                "record #{} ({}) has {} arguments; the schema puts an attribute at position {pos}",
                self.id,
                self.class,
                self.args.len()
            ),
            })?;
        parse_value(raw, Some(kind)).map_err(|m| IdsError::IfcInput {
            msg: format!("record #{} ({}), argument {pos}: {m}", self.id, self.class),
        })
    }
}

/// `read_attr(table, step_id, pos)`: decode one attribute of one record
/// without keeping the split. Prefer [`Record`] when reading several
/// attributes of the same record.
pub fn read_attr(
    table: &EntityTable,
    step_id: u64,
    pos: u16,
    kind: AttrKind,
) -> Result<AttrValue, IdsError> {
    let (ty, args) = table.get(step_id).ok_or_else(|| IdsError::IfcInput {
        msg: format!("record #{step_id} is not present in the DATA section"),
    })?;
    let fields = split_top_level_args(args);
    let ty = String::from_utf8_lossy(ty);
    let raw = fields.get(pos as usize).ok_or_else(|| IdsError::IfcInput {
        msg: format!(
            "record #{step_id} ({ty}) has {} arguments; the schema puts an attribute at position {pos}",
            fields.len()
        ),
    })?;
    parse_value(raw, Some(kind)).map_err(|m| IdsError::IfcInput {
        msg: format!("record #{step_id} ({ty}), argument {pos}: {m}"),
    })
}

/// Decode one STEP argument. `kind` (from the schema) decides between an
/// integer and a real, and whether `.T.`/`.F.`/`.U.` are booleans;
/// without it (list members, typed values) the token shape decides.
pub fn parse_value(raw: &[u8], kind: Option<AttrKind>) -> Result<AttrValue, String> {
    Ok(match parse_field(raw) {
        Field::Null => AttrValue::Null,
        Field::Star => AttrValue::Derived,
        Field::String(s) => AttrValue::Str(s),
        Field::Ref(id) => AttrValue::Ref(id),
        Field::Enum(e) => {
            let lit = std::str::from_utf8(e)
                .map_err(|_| "enumeration literal is not UTF-8".to_string())?;
            let boolish = matches!(kind, None | Some(AttrKind::Bool) | Some(AttrKind::Logical));
            match lit {
                "T" if boolish => AttrValue::Bool(true),
                "F" if boolish => AttrValue::Bool(false),
                "U" if boolish => AttrValue::Unknown,
                _ => AttrValue::Enum(lit.to_string()),
            }
        }
        Field::Number(x) => {
            let text = std::str::from_utf8(crate::lexer::trim_ws(raw)).unwrap_or("");
            let int_shaped = !text.contains(['.', 'e', 'E']);
            match kind {
                Some(AttrKind::Real) => AttrValue::Real(x),
                Some(AttrKind::Int) => match text.parse::<i64>() {
                    Ok(i) => AttrValue::Int(i),
                    Err(_) => return Err(format!("'{text}' is not an INTEGER")),
                },
                _ if int_shaped => match text.parse::<i64>() {
                    Ok(i) => AttrValue::Int(i),
                    Err(_) => AttrValue::Real(x),
                },
                _ => AttrValue::Real(x),
            }
        }
        Field::List(body) => {
            if crate::lexer::trim_ws(body).is_empty() {
                AttrValue::List(Vec::new())
            } else {
                let items = split_top_level_args(body)
                    .into_iter()
                    .map(|f| parse_value(f, None))
                    .collect::<Result<Vec<_>, _>>()?;
                AttrValue::List(items)
            }
        }
        Field::Other(raw) => parse_other(raw)?,
    })
}

/// Typed select values (`IFCLABEL('x')`) and binaries (`"0FF"`).
fn parse_other(raw: &[u8]) -> Result<AttrValue, String> {
    if raw.first() == Some(&b'"') && raw.last() == Some(&b'"') && raw.len() >= 2 {
        let hex = std::str::from_utf8(&raw[1..raw.len() - 1])
            .map_err(|_| "binary is not ASCII".to_string())?;
        return Ok(AttrValue::Str(hex.to_string()));
    }
    let open = raw.iter().position(|&b| b == b'(');
    match open {
        Some(o) if o > 0 && raw.last() == Some(&b')') && raw[0].is_ascii_alphabetic() => {
            let name = std::str::from_utf8(&raw[..o])
                .map_err(|_| "typed value name is not ASCII".to_string())?
                .trim()
                .to_ascii_uppercase();
            let inner = &raw[o + 1..raw.len() - 1];
            Ok(AttrValue::Typed {
                type_name: name,
                value: Box::new(parse_value(inner, None)?),
            })
        }
        _ => Err(format!(
            "cannot decode STEP value '{}'",
            String::from_utf8_lossy(&raw[..raw.len().min(80)])
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pv(s: &str, k: Option<AttrKind>) -> AttrValue {
        parse_value(s.as_bytes(), k).unwrap_or_else(|e| panic!("{e}"))
    }

    #[test]
    fn ids_attrs_decode_scalars() {
        assert_eq!(pv("$", None), AttrValue::Null);
        assert_eq!(pv("*", None), AttrValue::Derived);
        assert_eq!(
            pv("'Foo'", Some(AttrKind::String)),
            AttrValue::Str("Foo".into())
        );
        assert_eq!(
            pv("''", Some(AttrKind::String)),
            AttrValue::Str(String::new())
        );
        assert_eq!(
            pv(".SOLIDWALL.", Some(AttrKind::Enum)),
            AttrValue::Enum("SOLIDWALL".into())
        );
        assert_eq!(pv(".T.", Some(AttrKind::Bool)), AttrValue::Bool(true));
        assert_eq!(pv(".F.", Some(AttrKind::Logical)), AttrValue::Bool(false));
        assert_eq!(pv(".U.", Some(AttrKind::Logical)), AttrValue::Unknown);
        assert_eq!(pv(".T.", Some(AttrKind::Enum)), AttrValue::Enum("T".into()));
        assert_eq!(pv("42", Some(AttrKind::Int)), AttrValue::Int(42));
        assert_eq!(pv("42.", Some(AttrKind::Real)), AttrValue::Real(42.0));
        assert_eq!(pv("0.", None), AttrValue::Real(0.0));
        assert_eq!(pv("7", None), AttrValue::Int(7));
        assert_eq!(pv("#12", Some(AttrKind::Ref)), AttrValue::Ref(12));
        assert!(parse_value(b"4.2", Some(AttrKind::Int)).is_err());
    }

    #[test]
    fn ids_attrs_decode_unicode_escapes() {
        let v = pv(
            r"'\X2\266B\X0\Don''t\X2\00C4\X0\rgerh\X2\00F4\X0\tel\X2\040A04350442\X0\'",
            None,
        );
        assert_eq!(v, AttrValue::Str("♫Don'tÄrgerhôtelЊет".into()));
    }

    #[test]
    fn ids_attrs_decode_lists_and_typed() {
        assert_eq!(pv("()", Some(AttrKind::List)), AttrValue::List(vec![]));
        assert_eq!(
            pv("(0.,1.,2.)", Some(AttrKind::List)),
            AttrValue::List(vec![
                AttrValue::Real(0.0),
                AttrValue::Real(1.0),
                AttrValue::Real(2.0)
            ])
        );
        let t = pv("IFCNORMALISEDRATIOMEASURE(0.5)", Some(AttrKind::Select));
        assert_eq!(
            t,
            AttrValue::Typed {
                type_name: "IFCNORMALISEDRATIOMEASURE".into(),
                value: Box::new(AttrValue::Real(0.5))
            }
        );
        assert_eq!(t.py_str(), "IFCNORMALISEDRATIOMEASURE(0.5)");
        assert!(parse_value(b"@@", None).is_err());
    }

    #[test]
    fn ids_attrs_emptiness_matches_ifctester() {
        for v in [
            AttrValue::Null,
            AttrValue::Derived,
            AttrValue::Unknown,
            AttrValue::Str(String::new()),
            AttrValue::List(vec![]),
        ] {
            assert!(v.is_empty(), "{v:?}");
        }
        for v in [
            AttrValue::Bool(false),
            AttrValue::Real(0.0),
            AttrValue::Int(0),
            AttrValue::Str("P0D".into()),
            AttrValue::Ref(1),
        ] {
            assert!(!v.is_empty(), "{v:?}");
        }
        assert!(AttrValue::Null.is_null() && !AttrValue::Str(String::new()).is_null());
    }

    #[test]
    fn ids_attrs_py_str() {
        assert_eq!(AttrValue::Real(42.0).py_str(), "42.0");
        assert_eq!(AttrValue::Bool(true).py_str(), "True");
        assert_eq!(AttrValue::Null.py_str(), "None");
        assert_eq!(
            AttrValue::List(vec![AttrValue::Str("a".into())]).py_str(),
            "('a',)"
        );
        assert_eq!(py_repr_str("don't"), "\"don't\"");
    }
}
