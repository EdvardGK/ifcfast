# Design: native IDS 1.0 validation in ifcfast

**Status:** approved by Ed 2026-09-24 (`ad251b1`), epic GH #192. Slice 1 in progress. Amended same day: invalid-case truth semantics (§4) after reading the suite's `scripts.md`.
**Provenance:** drafted by an opus code-architect agent from a coordinator brief; coordinator
spot-checked every [V] claim cited below against `main` @ `1db78ad` (v0.5.3). External PR #24
(jonatanjacobsson, 2026-05-31, "Native Rust IDS 1.0 validation pipeline") was an input to this
analysis and is credited for the compile-plan-validate shape and the columns-only fast path; it is
not merged (review summary in the 2026-09-24 worklog).
**Positioning:** IfcTester is the reference implementation. ifcfast IDS is the speed-first
companion, the same relationship `mesh_qto` has to ifcopenshell geometry: IfcTester is the
test-time oracle and never a runtime dependency. Primary goal is the best native engine for
ifcfast's own users, shipped on ifcfast's cadence. Upstream give-back is a periodic habit (§7).

**Evidence labels:** **[V]** read at the cited file:line. **[S]** from the IDS 1.0 spec or
IfcTester behaviour, not checked against code; every [S] item gets pinned by a conformance case in
the slice that implements it.

---

## 1. Problem and users

| User | Call | Needs |
|---|---|---|
| Agent (Python) | `ifcfast.validate_ids(ids, ifc)` / `m.validate_ids(ids)` | Tables it can filter, with reason codes; typed errors, never guesses |
| Agent (MCP) | `validate_ids(path, ids_path)` | A summary plus capped failure rows in one round trip |
| Browser (ifcfast.com) | drop an IFC and an IDS → `validateIdsJson` | Runs client-side only, single-threaded wasm |
| CI / BEP gate | `ifcfast ids MODEL.ifc SPEC.ids` | Exit code, a report in IfcTester JSON shape, and speed on 100–800 MB models where IfcTester is slow or runs out of memory |

Roadmap gate (docs/plans/2026-07-04_coordinator-staple-roadmap.md:88-89) [V]: "buildingSMART IDS
conformance suite green; rule results differential vs an equivalent Solibri ruleset on G55."

## 2. Architecture

### 2.1 Findings that drive the design [V]

| Finding | Evidence | Consequence |
|---|---|---|
| The product table is a **whitelist** | indexer.rs:29-197; misses are only counted (1038-1059) | IDS candidates **must not** come from `products`. A class that isn't whitelisted would silently fall out of applicability, and the spec would silently pass. Candidates come from EntityTable type tokens instead. |
| `predefined_type` is found by walking fields from the right (heuristic) | indexer.rs:1336-1368, 1424-1445, 1492-1504 | Not exact enough for the entity facet. IDS reads it from generated schema positions. The indexer is left alone. |
| Type objects only capture guid, entity and name | indexer.rs:1218-1231 | The IDS path reads a type's PredefinedType and ElementType itself. |
| Pset values are flattened to strings; lists joined with `", "`; no unit column | psets.rs:105-129, 636-663, 33-40 | The emitted `PsetTable` is **lossy** for IDS (list boundaries, units). It can't be the input. |
| Pset pass 1 already collects typed records and does instance-wins type inheritance | psets.rs:85-288, 349-400 | **Refactor pass 1 into a shared `PropertyGraph`.** Do not write a second parser. |
| Quantities carry `unit_step_id` | quantities.rs:37-44 | Needed for IDS unit conversion. |
| `extract_all`'s guid map includes **every** IfcRoot with a 22-char GlobalId, types included | lib.rs:500-519, 733 | Type-level pset and classification rows already exist. |
| Only the length unit is resolved | indexer.rs:324-420 | IDS needs every unit type → new `units.rs`. |
| `IfcRelNests`, `IfcRelAssignsToGroup(ByFactor)` and `IfcRelFillsElement` are not indexed; their positions are already pinned in `REL_RULES` | doc/rel_rules.rs:147-167, 188; `parse_rel`/`field_refs` at 255, 300; fixtures in tests/doc_rel_rules.rs | Reuse these positions. Don't re-derive them. |
| `EntityTable<'a>` borrows the buffer | entity_table.rs:43-66 | Keep it borrowed. The owned-buffer copy is what PR #24 got rejected for. |
| wasm keeps the source bytes | wasm analysis.rs:184-188 | The browser can build the EntityTable on demand. |
| The schema codegen pattern emits Python only | scripts/gen_schema_supertypes.py:83-113 | Extend it to also emit Rust. |

### 2.2 Rust modules: `crates/core/src/ids/` (feature `ids`, on by default, wasm-safe)

| Module | Responsibility |
|---|---|
| `xml.rs` | Strict IDS 1.0 parse with `roxmltree` (small, no dependencies). Any unknown element or attribute, or a bad namespace, raises `IdsError::InvalidIds`. Must reject every `invalid-*` case. |
| `ir.rs` | Schema-independent IR (§2.3) |
| `compile.rs` | IR → `Plan`: resolve names against the schema tables for each `ifcVersion`, compile the regexes, compute a `Needs` bitset |
| `xsd_regex.rs` | Translate XSD regex into the Rust `regex` dialect, or raise a typed error (§2.6) |
| `restriction.rs` | Enumeration, pattern, bounds, lengths; typed comparison with the 1e-6 real tolerance [S] |
| `candidates.rs` | Seed-facet planner: builds the candidate step-id set from EntityTable type tokens |
| `attrs.rs` | Read any attribute by its generated position → `AttrValue` |
| `graph.rs` | One extra lazy pass over the EntityTable that collects only what `Needs` asks for: PropertyGraph, material records, classification chains, relation edges |
| `units.rs` (crate root) | Resolves every `UnitType`: SI prefixes, conversion-based units, `IfcDerivedUnit` via SI exponents. `resolve_length_scale` is refactored to go through it with bitwise-identical output. |
| `eval.rs` | Applicability first, then requirements with facet cardinality, then spec cardinality |
| `report.rs` | Column-major `IdsReport` (§3.2) |
| `schema_tables.rs` | **Generated**, per schema |

### 2.3 IR

```
IdsDocument { info, specs: Vec<Spec> }
Spec { idx, name, identifier, description, instructions, ifc_versions: Vec<Schema>,
       cardinality: Required|Optional|Prohibited,   // applicability min/maxOccurs
       applicability: Vec<Facet>, requirements: Vec<(Facet, FacetCard)> }
Facet = Entity{name: Val, predefined: Option<Val>}
      | Attribute{name: Val, value: Option<Val>}
      | Property{pset: Val, base_name: Val, value: Option<Val>, data_type: Option<MeasureType>}
      | Classification{system: Option<Val>, value: Option<Val>, uri: Option<String>}
      | Material{value: Option<Val>, uri: Option<String>}
      | PartOf{entity: Box<EntityFacet>, relation: Option<Relation>}
Val = Simple(String) | Restriction{base: XsdBase, enumeration, pattern, bounds, lengths}
```

### 2.4 Resolving each facet

| Facet | Resolves against | Missing today → add |
|---|---|---|
| **Entity** | EntityTable type tokens. Matching is **exact class, no subtypes** [S: IfcTester uses `include_subtypes=False`; confirmed by the entity cases]. A pattern or enumeration is matched against the distinct tokens in the file. | Generated `ENTITIES[schema]` |
| **predefinedType** | Order (amended 2026-09-24 to match `ifcopenshell.util.element.get_predefined_type`, which IfcTester uses): **type object first** — its PredefinedType, `USERDEFINED` → ElementType/ProcessType; if that yields nothing or `NOTDEFINED`, the occurrence's PredefinedType, `USERDEFINED` → ObjectType. No suite case separates the two orders; register row A9. | Generated `PREDEF_POS`, `OBJTYPE_POS`, `ELEMTYPE_POS` per entity per schema; `defines_by_type` edges (lazy) |
| **Attribute** | `attrs.rs` reads the generated position. Null, empty string, empty list and logical UNKNOWN count as absent. Enums compare as strings, numbers with tolerance, booleans map to `true`/`false`. | Generated flattened `ATTRS[schema][entity] = [(name, pos, kind)]`, inherited attributes included |
| **Property** | `PropertyGraph`: object → `(pset, kind{PropertySet, ElementQuantity}, prop, class, values: Vec<Typed{ifc_type, raw}>, unit_ref, source)`. Instance values override type values (psets.rs:303-400). **dataType** must equal the IfcValue wrapper (psets.rs:646-649) or the quantity's measure. Values are converted to SI through `units.rs`. Matching of enumerated, list and bounded values follows IfcTester [S]. | The shared pass-1 refactor; generated `MEASURE→UNITTYPE` |
| **Classification** | System = `IfcClassification.Name` via the ReferencedSource chain; value = Identification / ItemReference; uri = Location; direct `IfcClassification` associations; type inheritance (classifications.rs:71-175) | A full chain walk (the extractor records one parent ref, classifications.rs:98-101). Whether a match counts against ancestor references → follow IfcTester [S] |
| **Material** | Matches any of: material Name or Category; set name; layer, constituent or profile Name or Category; type inheritance | The public table has only `material_name` and `category` (materials.rs:30-53). IDS reads the shared pass-1 records, so **the public table does not change** |
| **PartOf** | Edges for aggregates, containment and voids from the indexer (indexer.rs:1119-1200); **nests, groups (incl. ByFactor) and fills** from `REL_RULES` positions. IDS relation values include the compound `IFCRELVOIDSELEMENT IFCRELFILLSELEMENT` [S]. Transitivity and depth follow IfcTester [S]. | Nests, groups and fills in the IDS pass; also exposed on the indexer and Model in slice 3 |

**Supertypes:** no supertype walk is needed for matching, because matching is exact. The generated
table is still used for: telling type objects from occurrences, flattening the attribute table, and
validating IDS names against the target schema (`ALL_ENTITIES`, gen_schema_supertypes.py:104-113 [V]).

**Codegen:** extend `scripts/gen_schema_supertypes.py`, or add `gen_schema_tables.py`, to write
`crates/core/src/ids/schema_tables.rs` next to the Python module. Everything comes from
`ifcopenshell.schema_by_name(...)` at generation time, pinned to ifcopenshell 0.8.5
(pyproject.toml:34 [V]). Schemas: IFC2X3, IFC4, IFC4X3 (script line 38 [V]). The generated file
carries a header naming the pinned ifcopenshell version, and CI checks it for drift.

### 2.5 Units and dataTypes

- IDS values are written in SI [S]. IFC values are converted to SI using the property's own `Unit`
  when present, otherwise the project assignment for the measure's unit type.
- If a spec needs a unit that can't be resolved, `IdsError::UnresolvedUnit{unit_type}` is raised.
  Never assume SI (same policy as GH #149, indexer.rs:216-228 [V]).
- dataType names are uppercase (`IFCLENGTHMEASURE`) and compared case-insensitively. A dataType
  that doesn't exist in the target schema is an invalid IDS.

### 2.6 XSD regex → Rust `regex`: risks

| XSD behaviour | Handling |
|---|---|
| Whole value must match (implicit anchoring) | Wrap as `^(?:…)$` |
| `^` and `$` are **literal** characters | Escape them |
| `.` excludes `\n` and `\r` | Rewrite to `[^\n\r]` |
| Class subtraction `[a-z-[aeiou]]` | Translate when both sides are simple, otherwise `Unsupported("xsd-regex:class-subtraction")` |
| `\i \c \I \C` (XML name characters) | Expand to explicit NameStartChar / NameChar classes |
| `\p{IsBasicLatin}` block escapes | Generated block table, or `Unsupported` naming the block |
| `\d \w` are Unicode | `regex` is Unicode by default. Use `regex` (not `regex-lite`) in wasm too; record the size increase. |
| Features XSD lacks (lazy quantifiers, lookaround, backrefs) | Reject as an invalid pattern |

## 3. Public surface

### 3.1 API

```python
ifcfast.validate_ids(ids, ifc, *, on_unsupported="raise", filter_ifc_version=False) -> IdsReport
Model.validate_ids(ids, *, on_unsupported="raise", filter_ifc_version=False) -> IdsReport
# filter_ifc_version defaults to False: the suite's ids/ folder pairs IFC4 files with
# ifcVersion="IFC2X3" specs and expects real results (IfcTester: should_filter_version=False).
# True gives those specs status="skipped_ifc_version".
# ids: path | XML str | bytes | list thereof (one EntityTable build per call)
```

- `IdsReport` is a NamedTuple `(specs, elements, failures)` of DataFrames, with `.ok`,
  `.to_parquet(dir)` and `.to_ifctester_json()`.
- Each call stands alone. No compiled plan is cached on the Model (rules out PR #24's stale-session bug).
- `on_unsupported="mark"` gives the spec row `status="unsupported"` with the feature named and
  emits no element rows. Explicit and labelled, so not a fallback.

### 3.2 Tables

**`specs`**, one row per spec:

| col | dtype |
|---|---|
| spec_index | int32 |
| name, identifier, description, instructions | string |
| ifc_versions | string |
| cardinality | category {required, optional, prohibited} |
| status | category {pass, fail, skipped_ifc_version, unsupported} |
| reason_code | category (spec-level: SPEC_NO_APPLICABLE, SPEC_PROHIBITED_APPLICABLE) |
| unsupported_feature | string\|null |
| applicable, passed, failed | int64 |
| applicability_label, requirement_labels | string / list<string> |

**`elements`**, one row per spec × applicable element:

| col | dtype |
|---|---|
| spec_index | int32 |
| step_id | int64 |
| guid | string\|null (the entity facet may target a non-IfcRoot entity) |
| entity | string (Title case) |
| predefined_type, name, description, tag | string\|null |
| type_step_id | int64\|null |
| status | category {pass, fail} |
| n_failed | int16 |

**`failures`**, one row per spec × element × failing requirement:

| col | dtype |
|---|---|
| spec_index, step_id, guid | as above |
| requirement_index | int16 |
| facet_type | category |
| facet_cardinality | category |
| reason_code | category |
| expected | string (IfcTester-style label) |
| actual | string\|null |
| value_source | category {instance, type, null} |

**Reason codes:** `ENTITY_MISMATCH, PREDEFINED_MISMATCH, ATTR_MISSING, ATTR_VALUE_MISMATCH,
PSET_MISSING, PROP_MISSING, PROP_NULL, PROP_UNSUPPORTED, PROP_DATATYPE_MISMATCH, PROP_VALUE_MISMATCH, CLASS_MISSING,
CLASS_SYSTEM_MISMATCH, CLASS_VALUE_MISMATCH, MATERIAL_MISSING, MATERIAL_VALUE_MISMATCH,
PARTOF_MISSING, PARTOF_ENTITY_MISMATCH, PROHIBITED_PRESENT, SPEC_NO_APPLICABLE,
SPEC_PROHIBITED_APPLICABLE`.

`PROP_UNSUPPORTED` (added 2026-09-25, ambiguity register A16): a complex property or quantity, or a
reference value, matched a required property facet. IDS 1.0 cannot check these kinds, so they count
as absent (`optional` and `prohibited` pass). A unit that cannot be resolved is not a reason code: it is
`IdsError::UnresolvedUnit` routed through `on_unsupported` (A26; `mark` → `status="unsupported"`,
`unsupported_feature="unit:<UNITTYPE>"`).

### 3.3 IfcTester interop: `to_ifctester_json()`

- Reproduces `ifctester.reporter.Json` exactly [S, from IfcTester source]: top-level title, date,
  filepath, filename, status, `total_*` / `percent_*` counters (incl. the `"N/A"` convention),
  `specifications`; per specification name, description, instructions, status, is_skipped,
  is_ifc_version, applicable counts, `applicable_entities`, cardinality, `applicability`,
  `requirements`; per requirement facet_type, label, value, description, status,
  `passed_entities` / `failed_entities`, totals, percent; per entity class, predefined_type, name,
  description, id, global_id, tag, reason.
- `passed_entities` for a requirement is rebuilt as applicable elements minus that requirement's
  failures, which makes the conversion lossless.
- IfcTester's live `element` / `element_type` handles are replaced by `id` and `type_id`.
- Port IfcTester's English facet `to_string` templates so `label` and `reason` match word for word.
- Fields we have that IfcTester doesn't (`reason_code`, `actual`, `value_source`, `spec_index`) go
  under an `"ifcfast"` key on each entity.
- Gate: JSON equality with IfcTester's output across the conformance suite, `ifcfast` keys and
  timestamps stripped.

### 3.4 Other surfaces

| Surface | Shape |
|---|---|
| CLI | `ifcfast ids MODEL SPEC.ids [SPEC2.ids…] [--json] [--out DIR] [--ifctester-json PATH] [--on-unsupported raise\|mark]`. Exit codes: 0 all pass, 1 any fail, 2 error. |
| MCP | `validate_ids(path, ids_path, only_failures=True, limit=200)`: full specs table, capped failure rows, `truncated`. Follows the `limit` convention (AGENTS.md:117-125 [V]). |
| wasm | `IfcModel.validateIdsJson(idsXml: string) -> string` returning `{specs, elements, failures}` as row objects. Builds the EntityTable from retained `source` (analysis.rs:188 [V]). |
| Errors | `IdsInvalidError` (malformed IDS; `.path`/`.line`), `IdsUnsupportedError` (`.feature`, `.spec_index`; message points to IfcTester as the reference), `IdsUnitError`. All subclass `IfcfastError`. A truncated parse is refused as today (lib.rs:529 [V]). |

## 4. Correctness strategy

| Layer | Design |
|---|---|
| **Suite source** | `scripts/fetch_ids_testcases.py` downloads `Documentation/ImplementersDocumentation/TestCases` at a **pinned commit SHA**, checks a committed sha256 manifest, stores under `~/.cache/ifcfast/ids-testcases/<sha>/`. CI caches that directory. Checksum mismatch fails loudly. Not a submodule (repo is large, carries docs we don't need). Not vendored (licence CC BY-ND 4.0 allows verbatim copies with attribution, but keeping third-party-licensed files out of the MIT tree and the sdist is cleaner). |
| **Truth** | From the filename. `pass-*` / `fail-*` = expected overall result. `invalid-*`: the suite's `scripts.md` defines it as "at least one requirement fails (invalid files do not comply with the Audit tool, they could not be satisfied regardless of IFC contents)", so the accepted outcomes are `fail` **or** `IdsInvalidError`; a `pass` is the bug. Policy: raise `IdsInvalidError` for audit violations detectable in the IDS alone (unknown entity/attribute name for the target schema, derived/inverse attribute, lowercase entity, uppercase boolean literal, float literal on an integer base, restriction base not matching the dataType's base, prohibited spec carrying requirements). A predefinedType literal outside the entity's enum is NOT rejected: the suite requires `WALDO`-style user-defined values to pass and a lowercase enum literal to fail rather than be invalid; everything else validates and fails. The harness reports a `strict` column (how many invalid cases were rejected outright) as information. Suite pinned at buildingSMART/IDS `development` @ `a670477` (334 cases: 187 pass, 120 fail, 27 invalid). |
| **Harness** | `tests/oracle/ids_conformance.py` runs **ifcfast and IfcTester** on every case. Both agree with truth → green. Only IfcTester wrong → `ifctester_bug`. Both wrong → triage `ifcfast_bug` or `test_case_drift`. Only ifcfast wrong → `ifcfast_bug`, blocks. Reuses `Classification` / `Collector` from tests/oracle/report.py:50-74, 138-186 [V], adds the two labels. IfcTester is a dev-extra next to ifcopenshell 0.8.5 (pyproject.toml:30-39 [V]), imported only inside the test (conftest.py:34-39 pattern [V]). |
| **Parse differential** | Every `.ids` is parsed by both engines, each emits canonical IR JSON, compared. Keeps our XML semantics from diverging silently. |
| **Known failures** | `tests/oracle/ids_xfail.toml`, one entry per case `{case, label, issue="#NNN", note}`. `strict=True`: an xfail that starts passing turns red. Only `ifctester_bug` and `test_case_drift` count as benign. |
| **Ambiguity register** | `docs/ids/ambiguities.md`: for each ambiguous point, the spec text, IfcTester's reading (with source line), our behaviour (same), the case that pins it. Rule: where the spec is ambiguous, we follow IfcTester. |
| **Second gate: G55 vs Solibri** | A G55 IDS (from the BEP; Ed provides or signs off) imported as a Solibri ruleset. Solibri results exported as GUID-keyed truth to `scratch/g55/ids_truth/` (client data, never in the repo). `tests/oracle/ids_solibri.py` measures per-element agreement; every miss attributed, as in the clash harness. IfcTester as third voice only on models ifcopenshell can open in RAM, one at a time (Omarchy memory). |

## 5. Performance

| Aspect | Design |
|---|---|
| Fast path | Specs made only of Entity and Attribute facets: one EntityTable build plus a type-token scan (no argument splitting). Attributes read only from candidate records via `EntityTable::get` (entity_table.rs:197 [V]). No extractor pass. |
| Lazy passes | `Needs` decides which records the single `graph.rs` pass looks at. Replaces running four extractors that each iterate the whole table (lib.rs:743-754 [V]). |
| Planner | Start from the cheapest applicability facet (usually entity), filter with the others. Property-first applicability starts from RelDefinesByProperties targets, following IfcTester's per-facet `filter` [S]. |
| Several IDS files | One call takes a list, so EntityTable and graph are built once. |
| Parallelism | rayon over (spec × candidate chunk) on native behind the existing rayon feature; serial in wasm (wasm lib.rs:20-21 [V]). Deterministic ordering: sort by (spec_index, step_id). |
| Large files | Mmapped, borrowed EntityTable ≈ 40 bytes per entity (entity_table.rs:10-12 [V]). PropertyGraph emits rows only for candidates. No full-file copy. |
| wasm | Adds `roxmltree` + `regex`. Budget: record the size increase against the current 1.07 MB in slice 4; file an issue if > 400 KB. |
| Target | Benchmark table (ifcfast vs IfcTester wall-clock and RSS) on G55 in slice 5, `--release` builds only. |

## 6. Delivery plan

| # | Slice | Files | Gate | Days |
|---|---|---|---|---|
| 1 | **Core + fast path + harness** | `scripts/gen_schema_tables.py`, `ids/{xml,ir,compile,xsd_regex,restriction,candidates,attrs,eval,report,schema_tables}.rs`, core Cargo.toml (roxmltree, regex), lib.rs `_core.validate_ids`, `python/ifcfast/ids.py`, `scripts/fetch_ids_testcases.py`, `tests/oracle/ids_conformance.py`, `ids_xfail.toml`, `docs/ids/ambiguities.md` | entity, attribute, restriction and ids folders green; zero unattributed `ifcfast_bug`; parse differential clean; cargo tests | 4–5 |
| 2 | **Property, Classification, Material + units** | `extractors/property_graph.rs` (pass 1 refactored out of psets.rs and quantities.rs), `units.rs` (indexer.rs:324-420 routed through it), `ids/graph.rs` | property, classification, material and tolerance folders green; **PsetTable, QuantityTable and unit_scale bitwise-identical** on the corpus; existing oracle tests unchanged | 4–5 |
| 3 | **PartOf + relation capture** | `ids/graph.rs` edges via `doc::rel_rules`; indexer.rs gains nests, group-membership and fills columns; model.py `m.nests`, `m.groups` | partof folder green; **full suite green**; **`_CACHE_SCHEMA_VERSION` 34 → 35** (header.py:790 [V]) | 2 |
| 4 | **Surfaces + interop** | cli.py `ids`, mcp_server.py `validate_ids`, `to_ifctester_json`, wasm lib.rs/analysis.rs `validateIdsJson`, `crates/wasm/test/ids_parity.mjs` | IfcTester JSON equality on the suite; wasm = Python on the suite; wasm size recorded | 2–3 |
| 5 | **G55 differential + benchmark** | `tests/oracle/ids_solibri.py`, scratch truth files | per-element agreement with every miss attributed; benchmark table | 2–3 (+ Ed's Solibri session) |

Total: **14–18 days.** Only slice 3 bumps the cache. Validation results are never cached (IDS input is arbitrary).

**AGENTS.md changes (each in the same change as its code):** decision tree "check a model against
an IDS"; new section `## IDS validation (m.validate_ids)` (API, three tables, reason codes, errors,
`on_unsupported`, IfcTester relationship); MCP tool list (AGENTS.md:86-96); CLI quick reference
(1759+); browser section (757+); graph accessors `m.nests` / `m.groups` (slice 3); cache version
note; "What ifcfast does NOT do" (1580+): unsupported XSD regex constructs.

## 7. What we give back (secondary, at release time)

When a release is cut, look through the `ifctester_bug` and `test_case_drift` entries in
`ids_xfail.toml`. Any that are verified and reproducible go upstream as minimal fixtures:
buildingSMART/IDS TestCases for drift, IfcOpenShell issues for IfcTester bugs. A habit, not a
deliverable; no slice.

## 8. Rejected alternatives

| Alternative | Why rejected |
|---|---|
| Wrap IfcTester or ifcopenshell at runtime | Breaks the oracle-only rule; loses the speed and wasm reasons for the feature; PR #24's `auto` mode silently opened with ifcopenshell. |
| A pandas engine in Python over the extractor tables | Duplicates logic; tables are lossy (joined lists, no units); can't reach wasm. |
| Merging PR #24 as it stands | Vacuous tests, personal paths, owned-buffer copy, stale-session cache, silent fallback. Its rule logic is the only part worth reading, and it is re-derived here on main's API. |
| Building candidates from `products` | The whitelist makes a missing class silently pass a spec. |
| Reusing the emitted `PsetTable` for the property facet | Values are strings with no unit. The shared pass-1 `PropertyGraph` avoids both a lossy input and a second parser. |
| Supertype-inclusive entity matching | Contradicts IDS 1.0 and IfcTester (exact class) [S]. |
| Full XSD validation of the IDS in Rust | No mature crate. Strict structural parse + `invalid-*` cases + parse differential covers the same risk. |
| `regex-lite` in wasm | No Unicode classes; would diverge silently from native. |
| Git submodule for the suite | Heavy; licence argues for keeping it outside the tree. Pinned fetch with checksums is lighter. |
