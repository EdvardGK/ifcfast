//! Attribute mutation on the owned STEP document (GH #133) — the last
//! write axis: psets, names, placements, with the same
//! byte-identical-elsewhere guarantee as subset/hotswap.
//!
//! ## Batch-first
//!
//! One [`mutate`] call applies a whole list of ops against one open
//! [`Doc`] and emits once. Agents batch hundreds of mutations; per-op
//! re-emission of a large file would be absurd. Ops apply in list order,
//! each seeing the cumulative state (translate-then-rotate composes in
//! that order).
//!
//! ## Copy-on-write semantics (the crux)
//!
//! An op expresses *per-element* intent — "set THIS wall's FireRating" —
//! so shared substructure is never edited in place:
//!
//! - **Psets**: if the pset applies to more than one element (its rel
//!   anchors several objects, or several rels share the pset), the
//!   element is spliced out of the shared rel and a cloned pset (fresh
//!   GlobalId) is attached via its own rel. Only then is the property
//!   touched.
//! - **Properties**: an `IfcPropertySingleValue` referenced by more than
//!   one container is replaced by a fresh record; an unshared one is
//!   value-spliced in place (minimal diff — the common per-instance-pset
//!   case).
//! - **Placements**: the `IfcAxis2Placement3D` under the element's
//!   `IfcLocalPlacement` is always replaced by a freshly minted one —
//!   points and directions are exactly the records real files share, and
//!   a wrong sharing judgement there silently relocates *other*
//!   elements. Old geometry is reclaimed by orphan GC afterwards; a
//!   shared point/direction keeps a positive refcount and survives.
//!   Do not "optimize" this into an in-place edit behind a refcount
//!   check — leaving the old records untouched and letting GC decide is
//!   the load-bearing safety property. A shared `IfcLocalPlacement`
//!   (several products, one placement) is CoW-cloned first.
//!
//! ## Atomicity and failure semantics
//!
//! All-or-nothing: any op failure aborts the whole batch with no output
//! (emission only happens at the end). Failures are *collected* — the
//! error reports every failing op, not just the first, so an agent fixes
//! a 300-op batch in one round trip instead of one error at a time.
//!
//! ## Frames and units
//!
//! `translate` deltas are in the placement-parent's frame, in the file's
//! native length unit — the same contract as hotswap's local-frame mesh.
//! `rotate` composes an axis-angle rotation (axis in the parent frame)
//! on top of the existing axes, about the element's own location, so a
//! pre-tilted element stays tilted.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::refs::{forward_refs, RecordSource};
use super::rel_rules::{field_span, RelField};
use super::step_fmt::{encode_string, fmt_real};
use super::Doc;
use crate::guid::GuidMinter;
use crate::lexer::{
    parse_field, parse_record_span, parse_ref_list, scan_ref_tokens, split_top_level_args, Field,
};

/// A new value for a property. `Null` writes `$` (unset).
#[derive(Debug, Clone, PartialEq)]
pub enum PropValue {
    Str(String),
    Real(f64),
    Int(i64),
    Bool(bool),
    Null,
}

/// One mutation. `guid` always names the element the intent is about.
#[derive(Debug, Clone)]
pub enum MutateOp {
    /// Set `Name` and/or `Description` on a rooted entity.
    Rename {
        guid: String,
        name: Option<String>,
        description: Option<String>,
    },
    /// Set (or add) a property on the named pset of the element.
    /// `ifc_type` (e.g. `IFCTEXT`, `IFCLENGTHMEASURE`) is required when
    /// adding a new property and optional (retype) when replacing.
    SetProperty {
        guid: String,
        pset: String,
        prop: String,
        value: PropValue,
        ifc_type: Option<String>,
    },
    /// Move the element by `delta` (placement-parent frame, native units).
    Translate { guid: String, delta: [f64; 3] },
    /// Rotate the element about its own location by `degrees` around
    /// `axis` (parent frame; not required to be unit length).
    Rotate {
        guid: String,
        axis: [f64; 3],
        degrees: f64,
    },
}

/// Summary of a mutate pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MutateStats {
    pub ops_applied: usize,
    pub renamed: usize,
    pub props_set: usize,
    pub props_added: usize,
    /// Replacements where the value's wrapper type changed (explicit
    /// `ifc_type` differing from the authored one).
    pub props_retyped: usize,
    /// Psets cloned copy-on-write because they applied to >1 element.
    pub psets_cloned: usize,
    /// `IfcRelDefinesByProperties` cloned when the element was spliced
    /// out of a shared rel.
    pub rels_cloned: usize,
    /// `IfcLocalPlacement`s cloned because several products shared one.
    pub placements_cloned: usize,
    pub translated: usize,
    pub rotated: usize,
    pub records_minted: usize,
    pub records_gc: usize,
    pub records_out: usize,
}

/// All failures across the batch, each `(op_index, message)`. The batch
/// is atomic: if this is returned, nothing was emitted.
#[derive(Debug, Clone)]
pub struct MutateError {
    pub failures: Vec<(usize, String)>,
}

impl std::fmt::Display for MutateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} op(s) failed:", self.failures.len())?;
        for (i, msg) in &self.failures {
            write!(f, " [op {i}] {msg};")?;
        }
        Ok(())
    }
}

impl std::error::Error for MutateError {}

/// Apply `ops` to `doc` and emit the mutated document. `seed` makes
/// GlobalId minting reproducible (tests); `None` (default) salts it.
pub fn mutate(
    doc: &Doc,
    ops: &[MutateOp],
    seed: Option<u64>,
) -> Result<(Vec<u8>, MutateStats), MutateError> {
    let mut ed = Editor::new(doc, seed);
    let mut failures: Vec<(usize, String)> = Vec::new();

    for (i, op) in ops.iter().enumerate() {
        let res = match op {
            MutateOp::Rename {
                guid,
                name,
                description,
            } => ed.apply_rename(guid, name.as_deref(), description.as_deref()),
            MutateOp::SetProperty {
                guid,
                pset,
                prop,
                value,
                ifc_type,
            } => ed.apply_set_property(guid, pset, prop, value, ifc_type.as_deref()),
            MutateOp::Translate { guid, delta } => ed.apply_translate(guid, *delta),
            MutateOp::Rotate {
                guid,
                axis,
                degrees,
            } => ed.apply_rotate(guid, *axis, *degrees),
        };
        if let Err(msg) = res {
            failures.push((i, msg));
        }
    }

    if !failures.is_empty() {
        return Err(MutateError { failures });
    }

    ed.stats.ops_applied = ops.len();
    ed.gc();
    let (bytes, records_out) = ed.emit();
    ed.stats.records_out = records_out;
    Ok((bytes, ed.stats))
}

// ----------------------------------------------------------------------
// Editor — the pending-state accumulator every op routes through
// ----------------------------------------------------------------------

/// Mutable overlay over a [`Doc`]: pending overrides of existing records,
/// freshly minted records, a maintained reference graph (so sharing
/// checks are O(1) instead of a full scan per op), and the GC candidate
/// list. `current_bytes` is THE one accessor — every traversal, field
/// read, and splice goes through it, so op N always sees op N-1's edits.
struct Editor<'d> {
    doc: &'d Doc,
    /// Overrides of existing records (id present in `doc`).
    pending: HashMap<u64, Vec<u8>>,
    /// New records, id > `doc.max_id()`; BTreeMap so emit order is
    /// deterministic regardless of op order.
    minted: BTreeMap<u64, Vec<u8>>,
    /// Records removed by GC.
    removed: HashSet<u64>,
    next_id: u64,
    /// Current outbound refs per record — updated on every write.
    out_refs: HashMap<u64, Vec<u64>>,
    /// Current inbound refcount per record — derived from `out_refs`,
    /// maintained incrementally. THE sharing oracle.
    in_count: HashMap<u64, u32>,
    /// Roots detached by placement/property replacement; GC candidates.
    detached: Vec<u64>,
    /// GlobalId (field 0 string) → step id, first occurrence; plus the
    /// full set of taken guids for mint-collision checks.
    guid_to_id: HashMap<String, u64>,
    taken_guids: HashSet<String>,
    minter: GuidMinter,
    stats: MutateStats,
}

impl RecordSource for Editor<'_> {
    fn current_bytes(&self, id: u64) -> Option<&[u8]> {
        if self.removed.contains(&id) {
            return None;
        }
        self.pending
            .get(&id)
            .map(|v| v.as_slice())
            .or_else(|| self.minted.get(&id).map(|v| v.as_slice()))
            .or_else(|| self.doc.record_bytes(id))
    }
}

impl<'d> Editor<'d> {
    fn new(doc: &'d Doc, seed: Option<u64>) -> Editor<'d> {
        // One up-front scan builds the reference graph and the guid maps.
        let mut out_refs: HashMap<u64, Vec<u64>> = HashMap::with_capacity(doc.len());
        let mut in_count: HashMap<u64, u32> = HashMap::with_capacity(doc.len());
        let mut guid_to_id: HashMap<String, u64> = HashMap::new();
        let mut taken_guids: HashSet<String> = HashSet::new();
        for (id, i) in doc.records() {
            let span = &doc.buf()[doc.record_span(i)];
            let refs = forward_refs(doc, id);
            for &r in &refs {
                *in_count.entry(r).or_insert(0) += 1;
            }
            out_refs.insert(id, refs);
            if let Some((_id, _ty, args)) = parse_record_span(span) {
                let split = split_top_level_args(args);
                // Same rooted-shape guard as Doc::resolve_guids (GH #132
                // item 4): a property Name or a material must not resolve
                // as a GlobalId.
                if !super::looks_rooted(&split) {
                    continue;
                }
                if let Some(first) = split.first() {
                    if let Some(g) = crate::lexer::decode_string(first) {
                        guid_to_id.entry(g.clone()).or_insert(id);
                        taken_guids.insert(g);
                    }
                }
            }
        }
        Editor {
            doc,
            pending: HashMap::new(),
            minted: BTreeMap::new(),
            removed: HashSet::new(),
            next_id: doc.max_id(),
            out_refs,
            in_count,
            detached: Vec::new(),
            guid_to_id,
            taken_guids,
            minter: GuidMinter::new(seed),
            stats: MutateStats::default(),
        }
    }

    // ---- graph-maintaining write primitives ---------------------------

    /// Install `bytes` as record `id`'s current bytes, updating the
    /// reference graph incrementally (diff old refs vs new).
    fn set_bytes(&mut self, id: u64, bytes: Vec<u8>) {
        let mut new_refs = scan_ref_tokens(&bytes);
        if !new_refs.is_empty() {
            new_refs.remove(0); // leading token is the record's own id
        }
        let old_refs = self.out_refs.get(&id).cloned().unwrap_or_default();
        for r in &old_refs {
            if let Some(c) = self.in_count.get_mut(r) {
                *c = c.saturating_sub(1);
            }
        }
        for r in &new_refs {
            *self.in_count.entry(*r).or_insert(0) += 1;
        }
        self.out_refs.insert(id, new_refs);
        if self.doc.contains(id) {
            self.pending.insert(id, bytes);
        } else {
            self.minted.insert(id, bytes);
        }
    }

    fn alloc_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Mint a new record from a full `#id=TYPE(...);` body.
    fn mint(&mut self, id: u64, body: String) {
        debug_assert!(body.starts_with(&format!("#{id}=")));
        self.stats.records_minted += 1;
        self.set_bytes(id, body.into_bytes());
    }

    fn fresh_guid(&mut self) -> String {
        let g = self.minter.mint(&self.taken_guids);
        self.taken_guids.insert(g.clone());
        g
    }

    /// The elements the pset/placement intent addresses: `guid` → step id.
    fn resolve(&self, guid: &str) -> Result<u64, String> {
        self.guid_to_id
            .get(guid)
            .copied()
            .ok_or_else(|| format!("unknown GlobalId: {guid}"))
    }

    /// Replace positional field `index` of record `id` with `repl`.
    fn splice_field(&mut self, id: u64, index: usize, repl: &[u8]) -> Result<(), String> {
        let span = self
            .current_bytes(id)
            .ok_or_else(|| format!("#{id} absent"))?;
        let range = field_span(
            span,
            RelField {
                index,
                is_set: false,
            },
        )
        .ok_or_else(|| format!("#{id} has no field @{index}"))?;
        let mut out = span.to_vec();
        out.splice(range, repl.iter().copied());
        self.set_bytes(id, out);
        Ok(())
    }

    /// Clone record `src` as a fresh record, substituting the given
    /// `(field_index, replacement_bytes)` pairs. Returns the new id.
    /// Inter-arg whitespace is normalized; arg bytes are verbatim.
    fn clone_record(&mut self, src: u64, subs: &[(usize, Vec<u8>)]) -> Result<u64, String> {
        let span = self
            .current_bytes(src)
            .ok_or_else(|| format!("#{src} absent"))?
            .to_vec();
        let (_id, ty, args) =
            parse_record_span(&span).ok_or_else(|| format!("#{src} unparseable"))?;
        let split = split_top_level_args(args);
        let ty = std::str::from_utf8(ty).map_err(|_| format!("#{src} bad type token"))?;
        let new_id = self.alloc_id();
        let mut body = format!("#{new_id}={ty}(");
        for (i, raw) in split.iter().enumerate() {
            if i > 0 {
                body.push(',');
            }
            match subs.iter().find(|(idx, _)| *idx == i) {
                Some((_, repl)) => body.push_str(
                    std::str::from_utf8(repl).map_err(|_| "non-UTF8 substitution".to_string())?,
                ),
                None => body.push_str(
                    std::str::from_utf8(raw).map_err(|_| format!("#{src} non-UTF8 arg"))?,
                ),
            }
        }
        body.push_str(");\n");
        self.mint(new_id, body);
        Ok(new_id)
    }

    // ---- field readers (all via current_bytes) ------------------------

    fn type_of(&self, id: u64) -> Option<Vec<u8>> {
        let span = self.current_bytes(id)?;
        let (_id, ty, _args) = parse_record_span(span)?;
        Some(ty.to_ascii_uppercase())
    }

    fn field_raw(&self, id: u64, index: usize) -> Option<Vec<u8>> {
        let span = self.current_bytes(id)?;
        let (_id, _ty, args) = parse_record_span(span)?;
        split_top_level_args(args).get(index).map(|r| r.to_vec())
    }

    fn field_string(&self, id: u64, index: usize) -> Option<String> {
        match parse_field(&self.field_raw(id, index)?) {
            Field::String(s) => Some(s),
            _ => None,
        }
    }

    fn field_ref(&self, id: u64, index: usize) -> Option<u64> {
        match parse_field(&self.field_raw(id, index)?) {
            Field::Ref(r) => Some(r),
            _ => None,
        }
    }

    fn field_ref_list(&self, id: u64, index: usize) -> Vec<u64> {
        match self.field_raw(id, index).as_deref().map(parse_field) {
            Some(Field::List(body)) => parse_ref_list(body),
            _ => Vec::new(),
        }
    }

    /// Every record id currently live: source order, then minted order.
    fn all_ids(&self) -> Vec<u64> {
        self.doc
            .ids()
            .iter()
            .copied()
            .chain(self.minted.keys().copied())
            .filter(|id| !self.removed.contains(id))
            .collect()
    }

    // ---- op: rename ----------------------------------------------------

    fn apply_rename(
        &mut self,
        guid: &str,
        name: Option<&str>,
        description: Option<&str>,
    ) -> Result<(), String> {
        if name.is_none() && description.is_none() {
            return Err("rename: at least one of name/description required".into());
        }
        let id = self.resolve(guid)?;
        // Rooted entities carry GlobalId@0, OwnerHistory@1, Name@2,
        // Description@3. resolve() already proved field 0 is a string.
        if let Some(n) = name {
            let enc = encode_string(n).map_err(|e| format!("rename name: {e}"))?;
            self.splice_field(id, 2, enc.as_bytes())?;
        }
        if let Some(d) = description {
            let enc = encode_string(d).map_err(|e| format!("rename description: {e}"))?;
            self.splice_field(id, 3, enc.as_bytes())?;
        }
        self.stats.renamed += 1;
        Ok(())
    }

    // ---- op: set_property ----------------------------------------------

    fn apply_set_property(
        &mut self,
        guid: &str,
        pset_name: &str,
        prop_name: &str,
        value: &PropValue,
        ifc_type: Option<&str>,
    ) -> Result<(), String> {
        let elem = self.resolve(guid)?;

        // Locate (rel, pset): every IfcRelDefinesByProperties anchoring the
        // element whose RelatingPropertyDefinition@5 reaches an IFCPROPERTYSET
        // named `pset_name`. The type guard is mandatory: IfcElementQuantity
        // is reachable through the same field with a DIFFERENT layout
        // (Quantities@5, not HasProperties@4) — matching by name alone would
        // corrupt an IfcQuantity record.
        let mut matches: Vec<(u64, u64, Vec<u64>)> = Vec::new(); // (rel, pset, anchor)
        let mut quantity_hit = false;
        for id in self.all_ids() {
            if self.type_of(id).as_deref() != Some(b"IFCRELDEFINESBYPROPERTIES") {
                continue;
            }
            let anchor = self.field_ref_list(id, 4);
            if !anchor.contains(&elem) {
                continue;
            }
            // Field 5 may be a bare #ref or (IFC4) an inline select
            // aggregate `(#a,#b)` — scan tokens covers both.
            let pds = match self.field_raw(id, 5) {
                Some(raw) => scan_ref_tokens(&raw),
                None => continue,
            };
            for pd in pds {
                let named = || self.field_string(pd, 2).as_deref() == Some(pset_name);
                match self.type_of(pd).as_deref() {
                    Some(b"IFCPROPERTYSET") if named() => {
                        matches.push((id, pd, anchor.clone()));
                    }
                    Some(b"IFCELEMENTQUANTITY") if named() => {
                        quantity_hit = true;
                    }
                    _ => {}
                }
            }
        }
        let (rel, pset, anchor) = match matches.len() {
            0 if quantity_hit => {
                return Err(format!(
                    "'{pset_name}' on {guid} is an IfcElementQuantity, not an IfcPropertySet — \
                     quantity mutation is not supported"
                ));
            }
            0 => {
                return Err(format!(
                    "element {guid} has no IfcPropertySet named '{pset_name}'"
                ));
            }
            1 => matches.remove(0),
            n => {
                return Err(format!(
                    "element {guid} has {n} psets named '{pset_name}' (rels {:?}) — ambiguous",
                    matches.iter().map(|(r, _, _)| r).collect::<Vec<_>>()
                ));
            }
        };

        // Copy-on-write when the pset applies to more than one element:
        // through a multi-member rel anchor, or through several rels
        // sharing the pset record.
        let pset_shared = self.in_count.get(&pset).copied().unwrap_or(0) > 1;
        let anchor_multi = anchor.len() > 1;
        let target_pset = if anchor_multi || pset_shared {
            // Clone the pset with a fresh GlobalId; same properties.
            let g = self.fresh_guid();
            let enc = encode_string(&g).expect("guid is ASCII");
            let new_pset = self.clone_record(pset, &[(0, enc.into_bytes())])?;
            self.stats.psets_cloned += 1;

            if anchor_multi {
                // Splice the element out of the shared rel's anchor…
                let survivors: Vec<u64> = anchor.iter().copied().filter(|a| *a != elem).collect();
                let mut list = String::from("(");
                for (i, s) in survivors.iter().enumerate() {
                    if i > 0 {
                        list.push(',');
                    }
                    list.push_str(&format!("#{s}"));
                }
                list.push(')');
                // …and mint this element its own rel pointing at the clone.
                // Field 5 may aggregate several property definitions (IFC4
                // select set) — keep the others, repoint only our pset.
                let old_pds = self
                    .field_raw(rel, 5)
                    .ok_or_else(|| format!("#{rel} lost field @5"))?;
                let new_pds = replace_ref_token(&old_pds, pset, new_pset);
                self.splice_field(rel, 4, list.as_bytes())?;
                let rg = self.fresh_guid();
                let renc = encode_string(&rg).expect("guid is ASCII");
                self.clone_record(
                    rel,
                    &[
                        (0, renc.into_bytes()),
                        (4, format!("(#{elem})").into_bytes()),
                        (5, new_pds),
                    ],
                )?;
                self.stats.rels_cloned += 1;
            } else {
                // Anchor is just us; repoint our rel's @5 at the clone.
                let raw = self
                    .field_raw(rel, 5)
                    .ok_or_else(|| format!("#{rel} lost field @5"))?;
                let repl = replace_ref_token(&raw, pset, new_pset);
                self.splice_field(rel, 5, &repl)?;
                self.detached.push(pset); // may orphan if all sharers move off
            }
            new_pset
        } else {
            pset
        };

        // Find the property inside the (possibly cloned) pset.
        let props = self.field_ref_list(target_pset, 4);
        let mut found: Vec<u64> = Vec::new();
        let mut wrong_kind: Option<Vec<u8>> = None;
        for p in &props {
            if self.field_string(*p, 0).as_deref() == Some(prop_name) {
                match self.type_of(*p) {
                    Some(t) if t == b"IFCPROPERTYSINGLEVALUE" => found.push(*p),
                    Some(t) => wrong_kind = Some(t),
                    None => {}
                }
            }
        }
        if found.len() > 1 {
            return Err(format!(
                "pset '{pset_name}' holds {} properties named '{prop_name}' — ambiguous",
                found.len()
            ));
        }
        if found.is_empty() {
            if let Some(t) = wrong_kind {
                return Err(format!(
                    "property '{prop_name}' in '{pset_name}' is {} — only \
                     IfcPropertySingleValue can be set",
                    String::from_utf8_lossy(&t)
                ));
            }
            // ADD a new property.
            let nominal = build_nominal(value, ifc_type, None)
                .map_err(|e| format!("add '{prop_name}': {e}"))?;
            let name_enc = encode_string(prop_name).map_err(|e| format!("prop name: {e}"))?;
            let pid = self.alloc_id();
            self.mint(
                pid,
                format!("#{pid}=IFCPROPERTYSINGLEVALUE({name_enc},$,{nominal},$);\n"),
            );
            // Append to HasProperties@4.
            let raw = self
                .field_raw(target_pset, 4)
                .ok_or_else(|| format!("#{target_pset} has no HasProperties"))?;
            let list = append_to_ref_list(&raw, pid)?;
            self.splice_field(target_pset, 4, &list)?;
            self.stats.props_added += 1;
            return Ok(());
        }

        // REPLACE the value of the existing IfcPropertySingleValue.
        let prop = found[0];
        let old_nominal = self
            .field_raw(prop, 2)
            .ok_or_else(|| format!("#{prop} has no NominalValue field"))?;
        let (nominal, retyped) = {
            let existing = parse_nominal(&old_nominal);
            let n = build_nominal(value, ifc_type, existing.as_ref())
                .map_err(|e| format!("set '{prop_name}': {e}"))?;
            let rt = match (&existing, ifc_type) {
                (Some(ex), Some(t)) => !ex.wrapper.eq_ignore_ascii_case(t),
                _ => false,
            };
            (n, rt)
        };
        if self.in_count.get(&prop).copied().unwrap_or(0) > 1 {
            // Shared property (several psets reference it): mint a
            // replacement and repoint only our pset.
            let new_prop = self.clone_record(prop, &[(2, nominal.into_bytes())])?;
            let raw = self
                .field_raw(target_pset, 4)
                .ok_or_else(|| format!("#{target_pset} has no HasProperties"))?;
            let repl = replace_ref_token(&raw, prop, new_prop);
            self.splice_field(target_pset, 4, &repl)?;
            self.detached.push(prop);
        } else {
            self.splice_field(prop, 2, nominal.as_bytes())?;
        }
        self.stats.props_set += 1;
        if retyped {
            self.stats.props_retyped += 1;
        }
        Ok(())
    }

    // ---- ops: translate / rotate ----------------------------------------

    /// Resolve the element's `IfcLocalPlacement`, CoW-cloning it if other
    /// records share it, and return `(local_placement, axis2placement3d)`.
    fn placement_of(&mut self, guid: &str) -> Result<(u64, u64), String> {
        let elem = self.resolve(guid)?;
        // IfcProduct: ObjectPlacement@5.
        let lp = self
            .field_ref(elem, 5)
            .ok_or_else(|| format!("element {guid} has no ObjectPlacement"))?;
        match self.type_of(lp).as_deref() {
            Some(b"IFCLOCALPLACEMENT") => {}
            Some(t) => {
                return Err(format!(
                    "element {guid} placement is {} — only IfcLocalPlacement is supported",
                    String::from_utf8_lossy(t)
                ));
            }
            None => return Err(format!("element {guid} placement #{lp} absent")),
        }
        let lp = if self.in_count.get(&lp).copied().unwrap_or(0) > 1 {
            // Several records share this placement (products, or child
            // placements chaining through it): give this element its own.
            let clone = self.clone_record(lp, &[])?;
            self.splice_field(elem, 5, format!("#{clone}").as_bytes())?;
            self.stats.placements_cloned += 1;
            clone
        } else {
            lp
        };
        let a2p = self
            .field_ref(lp, 1)
            .ok_or_else(|| format!("placement #{lp} has no RelativePlacement"))?;
        match self.type_of(a2p).as_deref() {
            Some(b"IFCAXIS2PLACEMENT3D") => Ok((lp, a2p)),
            Some(t) => Err(format!(
                "element {guid} RelativePlacement is {} — only IfcAxis2Placement3D is supported",
                String::from_utf8_lossy(t)
            )),
            None => Err(format!("RelativePlacement #{a2p} absent")),
        }
    }

    /// Read an `IfcAxis2Placement3D` as `(location, axis, refdir)` with
    /// IFC defaults applied for `$` axis/refdir.
    #[allow(clippy::type_complexity)]
    fn read_a2p(&self, a2p: u64) -> Result<([f64; 3], [f64; 3], [f64; 3]), String> {
        let loc_ref = self
            .field_ref(a2p, 0)
            .ok_or_else(|| format!("#{a2p} has no Location"))?;
        let loc = self.read_xyz(loc_ref, "IfcCartesianPoint")?;
        let axis = match self.field_ref(a2p, 1) {
            Some(d) => self.read_xyz(d, "IfcDirection")?,
            None => [0.0, 0.0, 1.0],
        };
        let refdir = match self.field_ref(a2p, 2) {
            Some(d) => self.read_xyz(d, "IfcDirection")?,
            None => [1.0, 0.0, 0.0],
        };
        Ok((loc, axis, refdir))
    }

    /// Field 0 of a point/direction as `[x, y, z]` (z = 0 for 2D).
    fn read_xyz(&self, id: u64, what: &str) -> Result<[f64; 3], String> {
        let raw = self
            .field_raw(id, 0)
            .ok_or_else(|| format!("{what} #{id} has no coordinate list"))?;
        let body = match parse_field(&raw) {
            Field::List(b) => b.to_vec(),
            _ => return Err(format!("{what} #{id} field 0 is not a list")),
        };
        let mut out = [0.0f64; 3];
        let parts = split_top_level_args(&body);
        if parts.is_empty() || parts.len() > 3 {
            return Err(format!("{what} #{id} has {} coordinates", parts.len()));
        }
        for (i, p) in parts.iter().enumerate() {
            match parse_field(p) {
                Field::Number(n) => out[i] = n,
                _ => return Err(format!("{what} #{id} coordinate {i} is not a number")),
            }
        }
        Ok(out)
    }

    fn apply_translate(&mut self, guid: &str, delta: [f64; 3]) -> Result<(), String> {
        if !delta.iter().all(|c| c.is_finite()) {
            return Err(format!("translate: non-finite delta {delta:?}"));
        }
        let (lp, a2p) = self.placement_of(guid)?;
        let (loc, _axis, _refdir) = self.read_a2p(a2p)?;
        let new_loc = [loc[0] + delta[0], loc[1] + delta[1], loc[2] + delta[2]];

        // Mint a new point + a new A2P3D that reuses the old axis/refdir
        // fields verbatim (shared directions stay shared — we never touch
        // them). The old A2P3D is detached; GC reclaims what it uniquely
        // owned.
        let pid = self.alloc_id();
        self.mint(
            pid,
            format!(
                "#{pid}=IFCCARTESIANPOINT(({},{},{}));\n",
                fmt_real(new_loc[0]),
                fmt_real(new_loc[1]),
                fmt_real(new_loc[2])
            ),
        );
        let new_a2p = self.clone_record(a2p, &[(0, format!("#{pid}").into_bytes())])?;
        self.splice_field(lp, 1, format!("#{new_a2p}").as_bytes())?;
        self.detached.push(a2p);
        self.stats.translated += 1;
        Ok(())
    }

    fn apply_rotate(&mut self, guid: &str, axis: [f64; 3], degrees: f64) -> Result<(), String> {
        if !degrees.is_finite() {
            return Err("rotate: non-finite angle".into());
        }
        let len = (axis[0] * axis[0] + axis[1] * axis[1] + axis[2] * axis[2]).sqrt();
        if !len.is_finite() || len < 1e-12 {
            return Err(format!("rotate: degenerate axis {axis:?}"));
        }
        let u = [axis[0] / len, axis[1] / len, axis[2] / len];
        let (lp, a2p) = self.placement_of(guid)?;
        let (_loc, old_axis, old_refdir) = self.read_a2p(a2p)?;

        let r = degrees.to_radians();
        let new_axis = rotate_about(u, r, old_axis);
        let new_refdir = rotate_about(u, r, old_refdir);

        // Location is unchanged (rotation about the element's own origin);
        // axis/refdir become freshly minted IfcDirections. Minting (never
        // editing a possibly-shared IfcDirection in place) is the safety
        // property — see the module docs.
        let ax_id = self.alloc_id();
        self.mint(
            ax_id,
            format!(
                "#{ax_id}=IFCDIRECTION(({},{},{}));\n",
                fmt_real(new_axis[0]),
                fmt_real(new_axis[1]),
                fmt_real(new_axis[2])
            ),
        );
        let rd_id = self.alloc_id();
        self.mint(
            rd_id,
            format!(
                "#{rd_id}=IFCDIRECTION(({},{},{}));\n",
                fmt_real(new_refdir[0]),
                fmt_real(new_refdir[1]),
                fmt_real(new_refdir[2])
            ),
        );
        let new_a2p = self.clone_record(
            a2p,
            &[
                (1, format!("#{ax_id}").into_bytes()),
                (2, format!("#{rd_id}").into_bytes()),
            ],
        )?;
        self.splice_field(lp, 1, format!("#{new_a2p}").as_bytes())?;
        self.detached.push(a2p);
        self.stats.rotated += 1;
        Ok(())
    }

    // ---- GC + emit -------------------------------------------------------

    /// Reclaim detached records whose inbound refcount reached zero,
    /// cascading through their children. The maintained `in_count` already
    /// reflects every splice, so this is a plain peel — no special-casing
    /// of which record was overridden (unlike hotswap's single-override
    /// shortcut).
    fn gc(&mut self) {
        let mut work: Vec<u64> = std::mem::take(&mut self.detached);
        while let Some(c) = work.pop() {
            if self.removed.contains(&c) || self.current_bytes(c).is_none() {
                continue;
            }
            if self.in_count.get(&c).copied().unwrap_or(0) != 0 {
                continue; // still referenced by a live record
            }
            let children = self.out_refs.get(&c).cloned().unwrap_or_default();
            self.removed.insert(c);
            self.stats.records_gc += 1;
            for child in children {
                if let Some(n) = self.in_count.get_mut(&child) {
                    *n = n.saturating_sub(1);
                }
                work.push(child);
            }
        }
    }

    /// Header + kept records (source order, overrides applied) + minted
    /// records (id order) + trailer.
    fn emit(&self) -> (Vec<u8>, usize) {
        let buf = self.doc.buf();
        let mut out = Vec::with_capacity(buf.len() + 1024);
        out.extend_from_slice(&buf[..self.doc.prefix_end()]);

        let mut records_out = 0usize;
        for (id, i) in self.doc.records() {
            if self.removed.contains(&id) {
                continue;
            }
            match self.pending.get(&id) {
                Some(bytes) => out.extend_from_slice(bytes),
                None => out.extend_from_slice(&buf[self.doc.record_span(i)]),
            }
            records_out += 1;
        }
        for (id, bytes) in &self.minted {
            if self.removed.contains(id) {
                continue;
            }
            out.extend_from_slice(bytes);
            records_out += 1;
        }

        out.extend_from_slice(&buf[self.doc.endsec()..]);
        (out, records_out)
    }
}

// ----------------------------------------------------------------------
// Pure helpers
// ----------------------------------------------------------------------

/// Rodrigues rotation of `v` about unit axis `u` by `theta` radians.
fn rotate_about(u: [f64; 3], theta: f64, v: [f64; 3]) -> [f64; 3] {
    let (s, c) = theta.sin_cos();
    let dot = u[0] * v[0] + u[1] * v[1] + u[2] * v[2];
    let cross = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    [
        v[0] * c + cross[0] * s + u[0] * dot * (1.0 - c),
        v[1] * c + cross[1] * s + u[1] * dot * (1.0 - c),
        v[2] * c + cross[2] * s + u[2] * dot * (1.0 - c),
    ]
}

/// Replace the token `#old` with `#new` inside raw field bytes, respecting
/// token boundaries (`#12` must not match inside `#123`).
fn replace_ref_token(raw: &[u8], old: u64, new: u64) -> Vec<u8> {
    let needle = format!("#{old}");
    let nb = needle.as_bytes();
    let mut out = Vec::with_capacity(raw.len() + 8);
    let mut i = 0;
    while i < raw.len() {
        if raw[i..].starts_with(nb) {
            let after = raw.get(i + nb.len());
            let boundary = !matches!(after, Some(b) if b.is_ascii_digit());
            if boundary {
                out.extend_from_slice(format!("#{new}").as_bytes());
                i += nb.len();
                continue;
            }
        }
        out.push(raw[i]);
        i += 1;
    }
    out
}

/// Append `#id` to a raw `(...)` ref-list field. `()` → `(#id)`.
fn append_to_ref_list(raw: &[u8], id: u64) -> Result<Vec<u8>, String> {
    let trimmed: Vec<u8> = raw.to_vec();
    let close = trimmed
        .iter()
        .rposition(|&b| b == b')')
        .ok_or_else(|| "field is not a list".to_string())?;
    let empty = trimmed[..close]
        .iter()
        .skip_while(|&&b| b != b'(')
        .skip(1)
        .all(|b| b.is_ascii_whitespace());
    let mut out = trimmed[..close].to_vec();
    if !empty {
        out.push(b',');
    }
    out.extend_from_slice(format!("#{id}").as_bytes());
    out.extend_from_slice(&trimmed[close..]);
    Ok(out)
}

/// A parsed `NominalValue`: wrapper type name + the content kind.
struct Nominal {
    wrapper: String,
    kind: NominalKind,
}

#[derive(PartialEq, Clone, Copy, Debug)]
enum NominalKind {
    Str,
    Number,
    Boolish,
    Null,
}

/// Parse an authored `NominalValue` field: `IFCLABEL('x')`, `$`, …
fn parse_nominal(raw: &[u8]) -> Option<Nominal> {
    let t: Vec<u8> = raw
        .iter()
        .copied()
        .skip_while(|b| b.is_ascii_whitespace())
        .collect();
    if t.is_empty() || t == b"$" {
        return Some(Nominal {
            wrapper: String::new(),
            kind: NominalKind::Null,
        });
    }
    // Typed wrapper: NAME(inner)
    let open = t.iter().position(|&b| b == b'(')?;
    let name = std::str::from_utf8(&t[..open]).ok()?.trim().to_string();
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return None;
    }
    let close = t.iter().rposition(|&b| b == b')')?;
    let inner: Vec<u8> = t[open + 1..close]
        .iter()
        .copied()
        .skip_while(|b| b.is_ascii_whitespace())
        .collect();
    let kind = match inner.first() {
        Some(b'\'') => NominalKind::Str,
        Some(b'.') => NominalKind::Boolish,
        Some(b'-') | Some(b'+') | Some(b'0'..=b'9') => NominalKind::Number,
        _ => return None,
    };
    Some(Nominal {
        wrapper: name,
        kind,
    })
}

/// Build the new `NominalValue` bytes for `value`.
///
/// Wrapper resolution: explicit `ifc_type` wins; otherwise the authored
/// wrapper is preserved when the value kind matches it; otherwise fail
/// loud — silently retyping a property corrupts downstream consumers,
/// and guessing a wrapper for a NEW property is lossy (a Python float
/// could be `IFCREAL`, `IFCLENGTHMEASURE`, `IFCAREAMEASURE`, …).
fn build_nominal(
    value: &PropValue,
    ifc_type: Option<&str>,
    existing: Option<&Nominal>,
) -> Result<String, String> {
    if matches!(value, PropValue::Null) {
        return Ok("$".to_string());
    }
    let value_kind = match value {
        PropValue::Str(_) => NominalKind::Str,
        PropValue::Real(_) | PropValue::Int(_) => NominalKind::Number,
        PropValue::Bool(_) => NominalKind::Boolish,
        PropValue::Null => unreachable!(),
    };
    let wrapper: String = match ifc_type {
        Some(t) => {
            let t = t.trim().to_ascii_uppercase();
            if !t.starts_with("IFC") || !t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                return Err(format!("ifc_type '{t}' is not an IFC type name"));
            }
            t
        }
        None => match existing {
            Some(ex) if ex.kind == value_kind => ex.wrapper.clone(),
            Some(ex) if ex.kind == NominalKind::Null => {
                return Err(
                    "authored value is $ (no wrapper to preserve) — pass ifc_type explicitly"
                        .to_string(),
                );
            }
            Some(ex) => {
                return Err(format!(
                    "value kind {:?} does not match authored wrapper {}({:?}) — pass ifc_type \
                     explicitly to retype",
                    value_kind, ex.wrapper, ex.kind
                ));
            }
            None => {
                return Err(
                    "new property needs an explicit ifc_type (e.g. IFCLABEL, IFCLENGTHMEASURE)"
                        .to_string(),
                );
            }
        },
    };
    let inner = match value {
        PropValue::Str(s) => encode_string(s)?,
        PropValue::Real(r) => {
            if !r.is_finite() {
                return Err(format!("non-finite value {r}"));
            }
            fmt_real(*r)
        }
        // An integer keeps integer syntax only inside a genuinely
        // INTEGER-derived wrapper; every other numeric wrapper is
        // REAL-derived and STEP requires the decimal point there.
        PropValue::Int(n) => {
            if wrapper_is_integer(&wrapper) {
                format!("{n}")
            } else {
                fmt_real(*n as f64)
            }
        }
        PropValue::Bool(b) => {
            if *b {
                ".T.".to_string()
            } else {
                ".F.".to_string()
            }
        }
        PropValue::Null => unreachable!(),
    };
    Ok(format!("{wrapper}({inner})"))
}

/// Whether an IFC simple-type wrapper takes INTEGER syntax (no decimal
/// point). Everything else numeric is REAL-derived and needs the point.
fn wrapper_is_integer(wrapper: &str) -> bool {
    matches!(
        wrapper,
        "IFCINTEGER"
            | "IFCCOUNTMEASURE"
            | "IFCTIMESTAMP"
            | "IFCDAYINMONTHNUMBER"
            | "IFCDAYINWEEKNUMBER"
            | "IFCMONTHINYEARNUMBER"
            | "IFCINTEGERCOUNTRATEMEASURE"
    )
}
