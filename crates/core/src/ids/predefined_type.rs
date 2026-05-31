//! Resolved predefined type (IfcTester / ifcopenshell.util.element.get_predefined_type).

use super::context::ValidationContext;

/// Effective predefined type: type object's value first, then occurrence, with
/// USERDEFINED resolved to ElementType / ObjectType / ProcessType.
pub fn resolved_predefined_type(ctx: &ValidationContext, pix: u32) -> Option<String> {
    if let Some(v) = predefined_from_type_object(ctx, pix) {
        return Some(v);
    }
    resolve_predefined_pair(
        ctx.object_predefined_type
            .get(pix as usize)
            .and_then(|o| o.as_deref()),
        ctx.object_object_type
            .get(pix as usize)
            .and_then(|o| o.as_deref()),
        ctx.attrs_for_pix(pix)
            .and_then(|m| m.get("ObjectType"))
            .map(|s| s.as_str()),
    )
}

fn predefined_from_type_object(ctx: &ValidationContext, pix: u32) -> Option<String> {
    let prod_step = ctx.object_step_id.get(pix as usize).copied()?;
    let type_step = ctx
        .indexed
        .defines_by_type_product
        .iter()
        .zip(ctx.indexed.defines_by_type_type.iter())
        .find_map(|(&p, &t)| if p == prod_step { Some(t) } else { None })?;
    let type_pix = ctx
        .object_step_id
        .iter()
        .position(|&s| s == type_step)
        .map(|i| i as u32)?;
    resolve_predefined_pair(
        ctx.object_predefined_type
            .get(type_pix as usize)
            .and_then(|o| o.as_deref()),
        ctx.object_object_type
            .get(type_pix as usize)
            .and_then(|o| o.as_deref()),
        ctx.attrs_for_pix(type_pix)
            .and_then(|m| m.get("ElementType"))
            .or_else(|| ctx.attrs_for_pix(type_pix).and_then(|m| m.get("ProcessType")))
            .map(|s| s.as_str()),
    )
}

fn resolve_predefined_pair(
    predefined: Option<&str>,
    alt_type: Option<&str>,
    attr_type: Option<&str>,
) -> Option<String> {
    let Some(raw) = predefined.filter(|s| !s.is_empty()) else {
        return None;
    };
    if raw.eq_ignore_ascii_case("USERDEFINED") {
        let custom = alt_type
            .or(attr_type)
            .filter(|s| !s.is_empty() && !s.eq_ignore_ascii_case("NOTDEFINED"))?;
        return Some(custom.to_string());
    }
    if raw.eq_ignore_ascii_case("NOTDEFINED") {
        return None;
    }
    Some(raw.to_string())
}

pub fn predefined_type_matches(
    ctx: &ValidationContext,
    pix: u32,
    facet_pt: &str,
) -> bool {
    let actual = resolved_predefined_type(ctx, pix);
    actual.as_deref() == Some(facet_pt)
}
