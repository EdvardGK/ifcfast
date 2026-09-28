# IDS slice 3: PartOf semantics

What the slice-3 implementation codes against (GH #192, design §2.4 PartOf row). Sources in
priority order: the suite's filename truth (buildingSMART/IDS @ `a67047736aa9`, folder
`partof/`, all 34 `.ids` + `.ifc` pairs read), then IfcTester 0.8.5 where the suite says
nothing, then `UserManual/partof-facet.md` (read from a local clone at `35f8c16`, not the
pinned sha; the facet text there is short and has not been cited for a pass/fail decision).

**Paths:** `facet.py` = `ifctester/facet.py`, `element.py` = `ifcopenshell/util/element.py`
(0.8.5, site-packages). **[V]** = read at that line. **[I]** = inferred.

**Status column** (as slice 2):
- **T+I**: a suite case decides it and IfcTester agrees.
- **T**: a suite case decides it, IfcTester gets it wrong. (None in this folder: IfcTester
  agrees with the filename on all 34 cases [V: conformance run 2026-09-28].)
- **I**: no case. Follow IfcTester (register rule).
- **X**: no case, the docs are explicit and IfcTester diverges. Follow the text. (None.)

Suite coverage [V: grep over all 334 cases]: every partOf is a requirement with an explicit
`relation` (12 × IFCRELAGGREGATES, 9 × IFCRELCONTAINEDINSPATIALSTRUCTURE, 7 × IFCRELNESTS,
6 × IFCRELASSIGNSTOGROUP). No case omits the relation, uses the compound
`IFCRELVOIDSELEMENT IFCRELFILLSELEMENT`, puts partOf in applicability, or uses partOf outside
`partof/`. Those paths are **I**, pinned by `crates/core/tests/ids_eval.rs`
`ids_part_of_relations_reason_codes_and_labels` on `fixtures/ids/partof_relations.ifc`, whose
12 specs were cross-checked against IfcTester (same status, same reason, same label).

Code: `crates/core/src/ids/graph.rs` `RelData` (edges), `eval.rs` `eval_part_of`
(`climb` / `direct` / `default_parent`), `compile.rs` `CPartOf`, `report.rs` labels,
`candidates.rs` seed.

---

## 1. Which relation, how far

The related object is always read through the inverse attribute ifcopenshell exposes, and
ifcopenshell takes that inverse's **first** member. Inverse members come back in **file
order** [V: two-relation probe, `HasAssignments` of `#1` listed `[#20, #10, #30]` for that
text order], so `RelData` keeps the first relation in file order per object (A39).

| # | Rule | Deciding case(s) | IfcTester | Status |
|---|---|---|---|---|
| R1 | **IFCRELAGGREGATES** is transitive through aggregation only: climb `Decomposes[0]` while it is an `IfcRelAggregates`. The **first** ancestor whose class matches the entity name decides: its predefinedType must then match, and the climb stops either way. Containment is never crossed. | `pass-an_aggregate_entity_may_pass_any_ancestral_whole_passes`, `pass-/fail-the_containment_can_be_indirect_{1,2}_2` (beam → space → building by aggregation passes; beam contained in a space that is aggregated into the building fails), `pass-/fail-an_aggregate_may_specify_the_entity_of_the_whole_{1,2}_2`, `…_predefined_type_of_the_whole_{1,2}_2`, `fail-a_non_aggregated_element_fails_an_aggregate_relationship`, `fail-the_aggregated_whole_fails_an_aggregate_relationship`, `pass-the_aggregated_part_passes_an_aggregate_relationship` | `facet.py:523-544`; `element.py:1324` `get_aggregate` | T+I |
| R2 | **IFCRELNESTS** is transitive through nesting only (`Nests[0]` in IFC4+, `Decomposes[0]` when it is an `IfcRelNests` in IFC2X3), same first-match rule. | `pass-nesting_may_be_indirect`, `pass-any_nested_part_passes_a_nest_relationship`, `fail-any_nested_whole_fails_a_nest_relationship`, `pass-/fail-the_nest_entity_must_match_exactly_{1,2}_2`, `pass-/fail-the_nest_predefined_type_must_match_exactly_{1,2}_2` | `facet.py:577-598`; `element.py:1349` `get_nest` | T+I |
| R3 | **IFCRELCONTAINEDINSPATIALSTRUCTURE** is direct: `ContainedInStructure[0].RelatingStructure`, no climb. Only classes that carry that inverse have a container (A40). | `pass-/fail-any_contained_element_passes_a_containment_relationship_{2,1}_2`, `fail-the_container_itself_always_fails`, `pass-/fail-the_container_entity_must_match_exactly_{2,1}_2`, `pass-/fail-the_container_predefined_type_must_match_exactly_{2,1}_2`, `pass-/fail-the_container_must_be_related_using_specified_relation_{1,2}_2` | `facet.py:563-576`; `element.py:1041` `get_container(should_get_direct=True)` | T+I; the class gate is **I** |
| R4 | **IFCRELASSIGNSTOGROUP** is direct: the first `IfcRelAssignsToGroup` of `HasAssignments` (subtype `…ByFactor` included), no climb through groups of groups. | `pass-a_grouped_element_passes_a_group_relationship`, `fail-a_non_grouped_element_fails_a_group_relationship`, `pass-/fail-a_group_entity_must_match_exactly_{2,1}_2`, `pass-a_group_predefined_type_must_match_exactly_2_2`, `invalid-…_1_2` | `facet.py:545-562` (`rel.is_a("IfcRelAssignsToGroup")` includes ByFactor) | T+I; ByFactor and "first group only" are **I** |
| R5 | **IFCRELVOIDSELEMENT IFCRELFILLSELEMENT** (the one compound token) is direct: an `IfcOpeningElement` (subtypes included) → its voided element; anything else → the opening it fills → that opening's voided element. | none | `facet.py:599-618`; `element.py:1286`, `:1305` | I |
| R6 | **No relation**: climb ifcopenshell `get_parent` (direct container, else aggregate, else nest, else filled opening, else voided element), and IfcTester's fallback to the first group, until a class matches (first match decides, as R1). | none | `facet.py:505-522`, `:624-631`; `element.py:1237` | I |
| R7 | `IFCRELVOIDSELEMENT` or `IFCRELFILLSELEMENT` alone is not in the XSD `relations` enumeration: `IdsInvalidError` at parse (IfcTester: `IdsXmlValidationError`). | none | XSD | I (both engines agree) |

`partof-facet.md` says a named relation is "evaluated (recursively)" for every kind. IfcTester
climbs only aggregation, nesting and the no-relation walk; containment, groups and
voids/fills are one step. The suite pins aggregation (R1) and nesting (R2) only, so the
recursion of the other three is **I**, registered as A46 (open).

## 2. Matching the related object

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| E1 | The nested entity facet is matched against the RELATED object (the whole, container, host, group, voided element), never the element itself. Class match is exact, no subtypes; a restriction name matches class names. | `fail-the_container_entity_must_match_exactly_1_2` (IFCSITE ≠ the space container), `fail-a_group_entity_must_match_exactly_1_2` (IFCGROUP ≠ IFCINVENTORY), `fail-the_nest_entity_must_match_exactly_1_2` | `facet.py:511`, `:533`, `:555`, `:569`, `:587`, `:611` (`is_a().upper() == self.name` / `!=`) | T+I |
| E2 | predefinedType of the related object resolves as the entity facet's (type object first, `USERDEFINED` → `ObjectType` / `ElementType`), then compares as a plain value. The entity facet's `USERDEFINED` query (A1) does not apply here. | `pass-a_group_predefined_type_must_match_exactly_2_2` (`.USERDEFINED.` + ObjectType `BUNNY`), `fail-the_container_predefined_type_must_match_exactly_1_2`, `pass-/fail-the_nest_predefined_type_…` | `element.py:547` `get_predefined_type`; `facet.py:513`, `:535`, `:559`, `:573`, `:589`, `:615` | T+I (the missing USERDEFINED query is **I**) |
| E3 | Group branch only: predefinedType is checked even when the class already failed, and its reason replaces the class reason. Containment and voids/fills check it only after a class match. Pass/fail is the same either way; only `actual` differs. | none | `facet.py:554-562` vs `:569-576`, `:611-618` | I |
| E4 | An unknown nested entity name is an invalid IDS (as for an entity facet). | none in `partof/` | IfcTester does not check | I (same rule as slice 1's entity names) |

## 3. Cardinality and applicability

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| C1 | `required`: pass iff a related object matches. `prohibited`: the negation (`PROHIBITED_PRESENT`, `actual` = the matched class). | `pass-a_required_facet_checks_all_parameters_as_normal`, `fail-a_prohibited_facet_returns_the_opposite_of_a_required_facet` | `facet.py:620-622` | T+I |
| C2 | `optional` is not allowed on `<partOf>` by the IDS 1.0 XSD; the parser refuses it (`IdsInvalidError`). | none | XSD | I |
| C3 | partOf as the FIRST applicability facet seeds every `IfcObjectDefinition` (A41). | none | `facet.py:477-482` (`list(ifc_file)`) | I |

## 4. Reason codes and labels

| IfcTester reason | ifcfast `reason_code` | `actual` |
|---|---|---|
| `NOVALUE` (no related object of that relation) | `PARTOF_MISSING` | null |
| `ENTITY` with an empty ancestor list (no-relation walk found no parent) | `PARTOF_MISSING` (A43) | null |
| `ENTITY` (transitive branches) | `PARTOF_ENTITY_MISMATCH` | Python list of the classes climbed, the one that matched by name suffixed `.<predefinedType>` (`"['IFCELEMENTASSEMBLY.TRUSS']"`) |
| `ENTITY` (direct branches) | `PARTOF_ENTITY_MISMATCH` | the related object's class (`IFCSPACE`) |
| `PREDEFINEDTYPE` | `PARTOF_ENTITY_MISMATCH` (A43) | the related object's predefinedType, `None` spelled out |
| `PROHIBITED` | `PROHIBITED_PRESENT` | the matched class |

`facet_type` is `part_of`, `value_source` is `instance` when a related object exists.

Labels port `facet.py:452-475` (parameter order name, predefinedType, relation). With no
relation every template needs `{relation}`, so the label is IfcTester's
`This facet cannot be interpreted` (A42). Optional templates (`must` → `may`) are unreachable
(C2).

## 5. Case inventory (34)

| Case | Exp | Rule pinned | IfcTester |
|---|---|---|---|
| `fail-a_group_entity_must_match_exactly_1_2` | fail | R4/E1 IFCGROUP ≠ IFCINVENTORY | agrees |
| `fail-a_non_aggregated_element_fails_an_aggregate_relationship` | fail | R1 no aggregate | agrees |
| `fail-a_non_grouped_element_fails_a_group_relationship` | fail | R4 no group | agrees |
| `fail-a_prohibited_facet_returns_the_opposite_of_a_required_facet` | fail | C1 prohibited | agrees |
| `fail-an_aggregate_may_specify_the_entity_of_the_whole_2_2` | fail | R1/E1 whole is not IFCWALL | agrees |
| `fail-an_aggregate_may_specify_the_predefined_type_of_the_whole_2_2` | fail | R1/E2 SLABRADOR ≠ BASESLAB | agrees |
| `fail-any_contained_element_passes_a_containment_relationship_1_2` | fail | R3 not contained | agrees |
| `fail-any_nested_whole_fails_a_nest_relationship` | fail | R2 the host is not nested | agrees |
| `fail-the_aggregated_whole_fails_an_aggregate_relationship` | fail | R1 the whole has no aggregate | agrees |
| `fail-the_container_entity_must_match_exactly_1_2` | fail | R3/E1 container class | agrees |
| `fail-the_container_itself_always_fails` | fail | R3 the space is not contained | agrees |
| `fail-the_container_must_be_related_using_specified_relation_2_2` | fail | R3 aggregated into the space, not contained | agrees |
| `fail-the_container_predefined_type_must_match_exactly_1_2` | fail | R3/E2 WARREN | agrees |
| `fail-the_containment_can_be_indirect_2_2` | fail | R1 aggregation does not cross containment | agrees |
| `fail-the_nest_entity_must_match_exactly_1_2` | fail | R2/E1 | agrees |
| `fail-the_nest_predefined_type_must_match_exactly_1_2` | fail | R2/E2 LITTERBOX | agrees |
| `invalid-a_group_predefined_type_must_match_exactly_1_2` | inv | R4/E2 BUNNARY ≠ BUNNY (reached as `fail`) | agrees (fail) |
| `pass-a_group_entity_must_match_exactly_2_2` | pass | R4/E1 | agrees |
| `pass-a_group_predefined_type_must_match_exactly_2_2` | pass | R4/E2 USERDEFINED → ObjectType | agrees |
| `pass-a_grouped_element_passes_a_group_relationship` | pass | R4 | agrees |
| `pass-a_required_facet_checks_all_parameters_as_normal` | pass | C1 | agrees |
| `pass-an_aggregate_entity_may_pass_any_ancestral_whole_passes` | pass | R1 transitive | agrees |
| `pass-an_aggregate_may_specify_the_entity_of_the_whole_1_2` | pass | R1/E1 | agrees |
| `pass-an_aggregate_may_specify_the_predefined_type_of_the_whole_1_2` | pass | R1/E2 | agrees |
| `pass-any_contained_element_passes_a_containment_relationship_2_2` | pass | R3 | agrees |
| `pass-any_nested_part_passes_a_nest_relationship` | pass | R2 | agrees |
| `pass-nesting_may_be_indirect` | pass | R2 transitive | agrees |
| `pass-the_aggregated_part_passes_an_aggregate_relationship` | pass | R1 | agrees |
| `pass-the_container_entity_must_match_exactly_2_2` | pass | R3/E1 | agrees |
| `pass-the_container_must_be_related_using_specified_relation_1_2` | pass | R3 | agrees |
| `pass-the_container_predefined_type_must_match_exactly_2_2` | pass | R3/E2 | agrees |
| `pass-the_containment_can_be_indirect_1_2` | pass | R1 transitive | agrees |
| `pass-the_nest_entity_must_match_exactly_2_2` | pass | R2/E1 | agrees |
| `pass-the_nest_predefined_type_must_match_exactly_2_2` | pass | R2/E2 | agrees |
