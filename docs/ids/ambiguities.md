# IDS 1.0 ambiguity register

Design: `docs/plans/2026-09-24_ids-validation-design.md` §4 ("Ambiguity register").

**Rule:** where the IDS 1.0 text is ambiguous, ifcfast follows IfcTester. Where the text
(or a normative companion document such as `tolerance.md`) is explicit and IfcTester
diverges, ifcfast follows the text and the conformance case is registered as
`ifctester_bug` in `tests/oracle/ids_xfail.toml`.

Citations are against the pinned references:

- IfcTester **0.8.5** (PyPI), with ifcopenshell 0.8.5. Paths are relative to
  `site-packages/`.
- buildingSMART/IDS `development` @ `a67047736aa93586d723329fce3aab1b9ac056af` (the sha in
  `tests/oracle/ids_testcases.lock`). Case ids are `<folder>/<stem>` under
  `Documentation/ImplementersDocumentation/TestCases/`.

Status: the native engine implements the Entity and Attribute facets (GH #192 slice 1,
`crates/core/src/ids/{compile,candidates,attrs,eval,report}.rs`) and the Property,
Classification and Material facets (slice 2, plus `ids/graph.rs`); rows for those facets
describe shipped behaviour, pinned by `crates/core/tests/ids_eval.rs` (all 334 suite cases
agree with the filename truth: every folder but `partof` by result, `partof` as
`Unsupported`). PartOf rows stay *intended* until slice 3 lands.

## Ambiguous in the text: follow IfcTester

| # | Point | Spec text / source | IfcTester reading (source) | ifcfast | Pinning case(s) |
|---|---|---|---|---|---|
| A1 | How `predefinedType` resolves | Entity facet: "predefined type" of the element, no algorithm given | Type object first: `PredefinedType`, and if `USERDEFINED` or empty, the type's `ElementType` (or `ProcessType`). Then the occurrence: `PredefinedType`, and if `USERDEFINED` or empty, `ObjectType`. `NOTDEFINED` on the type falls through to the occurrence. `ifcopenshell/util/element.py:565-576` via `ifctester/facet.py:245-252`. A spec value of `USERDEFINED` means "is user defined", `is_userdefined_type`, `ifcopenshell/util/element.py:580` via `facet.py:246-247` | same | `entity/pass-inherited_predefined_types_should_pass`, `entity/pass-overridden_predefined_types_should_pass`, `entity/pass-a_predefined_type_may_specify_a_user_defined_element_type`, `…_object_type`, `…_process_type`, `entity/pass-userdefined_predefined_types_may_be_specified` |
| A2 | Entity matching is exact-class | Entity facet names an IFC class | No subtypes: `by_type(name, include_subtypes=False)` (`ifctester/facet.py:207`, `:222`), and `inst.is_a().upper() == self.name` (`facet.py:231`). In IFC2X3, a non-`…TYPE` name also matches an occurrence whose *type object* is `<name>TYPE` (`facet.py:233-241`), which is the IFC2X3 occurrence/type mapping table | same | `entity/invalid-subclasses_are_not_considered_as_matching`, `entity/pass-in_ifc2x3_an_airterminal_can_be_checked_by_name_via_the_type_mapping_table_1_2` |
| A3 | Boolean literal matching | `simpleValue` is a string, and booleans are compared to it | `cast_to_value` accepts exactly `"true"`/`"1"` → True and `"false"`/`"0"` → False (`ifctester/facet.py:43-47`). Anything else casts to `None` and the value check fails, so it is not treated as an invalid IDS | same for pass/fail. `TRUE` in an IDS is an `invalid-` case, so either `fail` or `IdsInvalidError` is accepted (see the last section) | `attribute/pass-attributes_with_a_boolean_true_should_pass`, `attribute/fail-booleans_must_be_specified_as_lowercase_strings_1_3`, `attribute/invalid-booleans_must_be_specified_as_lowercase_strings_2_3` |
| A4 | Numeric value vs `simpleValue` text | Value comparison against typed IFC values | For an int or float IFC value, the IDS text is cast with `float()`, so `1e3` is preserved and `42.0 == 42` (`ifctester/facet.py:39-42`). Floats are compared with `is_x` (A5), ints with `==` (`facet.py:370-380`) | same | `property/pass-real_values_are_checked_using_type_casting_1_3` … `_3_3` |
| A5 | "Empty" attribute values | Attribute facet: a value must be provided | Null, `""`, an empty aggregate, LOGICAL `UNKNOWN` and inverse attributes all count as empty and fail (`ifctester/facet.py:332-357`). Only forward attributes are read (`facet.py:306-311`) | same | `attribute/fail-attributes_with_empty_strings_always_fail` and the related `attribute/*always_fail*` cases |

## Explicit in the text, IfcTester diverges: follow the text

Found by the IfcTester leg of `python -m tests.oracle.ids_conformance` (2026-09-24, 22
disagreements with the filename truth). D1–D3 are registered in `tests/oracle/ids_xfail.toml`
as `ifctester_bug`, issue #193 (17 cases).

| # | Point | Spec text / source | IfcTester reading (source) | ifcfast | Cases |
|---|---|---|---|---|---|
| D1 | Real-number equality tolerance | `ImplementersDocumentation/tolerance.md`: `x == v ⇔ (v − |v|·ε − ε) < x < (v + |v|·ε + ε)`, `ε = 1e-6` (a relative **and** an absolute term) | Relative only: `is_x` rejects `value < v·(1−1e-6)` or `value > v·(1+1e-6)` (`ifctester/facet.py:53-60`), so there is no absolute term and `v = 0` needs an exact match | tolerance.md formula | the 14 `tolerance/pass-comparison_tolerance_for_floating_point_*` cases |
| D2 | Several `xs:pattern` values in one restriction | XSD 1.1 §4.3.4: pattern facets in the same derivation step are **ORed** | Every pattern must match (`ifctester/facet.py:1056-1058`, a loop that returns False on the first non-match) | OR | `restriction/pass-regex_patterns_work_in_OR_1_3`, `_2_3` |
| D3 | Optional attribute facet on a null attribute | `cardinality="optional"`: if the attribute has a value, it must match | Only an attribute *name* that does not resolve short-circuits to pass. A null forward attribute gives `values=[None]`, falls through to the empty check and fails (`ifctester/facet.py:327-357`) | pass | `attribute/pass-an_optional_attribute_passes_if_null`; the same root cause, not yet registered: `ids/pass-specification_optionality_and_facet_optionality_can_be_combined` (optional `Description` = `$`) |

| D4 | IFC2X3 material properties | Property facet on `IfcMaterial` in IFC2X3; the case authors `IFCEXTENDEDMATERIALPROPERTIES(#2,(#3),$,'Custom_Pset')` | `get_properties` reads `.Properties` for every `IfcMaterialProperties` (`ifctester/facet.py:912-913`). In IFC2X3 the subtype's list attribute is `ExtendedProperties` (schema attributes `Material, ExtendedProperties, Description, Name`), so IfcTester raises `AttributeError`. `ifcopenshell.util.element.get_psets` resolves the pset correctly (`{'Custom_Pset': {'Foo': 'Bar'}}`) | pass (`Foo` is an `IFCLABEL`) | `property/pass-material_properties_are_supported_under_ifc2x3_via_extendedmaterialproperties`, registered under #193 |

Open (needs triage): the three `invalid-` cases IfcTester passes:
`attribute/invalid-integers_cannot_be_expressed_as_floating_point_numbers_2_2`,
`property/invalid-integer_values_cannot_be_stored_with_decimal_2_4` and `_3_4`.

## `invalid-` cases: accepted outcomes

`TestCases/scripts.md` defines `invalid-` as "at least one requirement fails (invalid files
do not comply with the Audit tool, they could not be satisfied, regardless of IFC
contents)". The harness therefore accepts **`fail` or `invalid`** on an `invalid-` case,
and the rule is the same for both engines. `invalid` means IfcTester's
`IdsXmlValidationError` or ifcfast's typed `IdsInvalidError`. Only `pass` or `error` is a
disagreement.

IfcTester only rejects what fails the XSD decode (`ifctester/ids.py:56-65`). None of the
27 `invalid-` IDS files fails the XSD, so IfcTester rejects 0 outright, reports 24 as
`fail` (accepted) and 3 as `pass` (disagreements). ifcfast *may* raise `IdsInvalidError`
from a semantic audit: unknown entity or attribute names, upper-case booleans, float
literals where the dataType is an integer, patterns on numeric bases, and prohibited
specifications that carry requirements. The CLI's `rejct` column counts those rejections
for information. They are not required for agreement.

## Slice 1 (Entity + Attribute) decisions, 2026-09-24

Rows A6–A13 follow the same rule (ambiguous → IfcTester). R1–R3 record where the slice-1
brief or the design doc said one thing and the suite pinned another; the suite won. Open
questions are marked **open**.

| # | Point | IfcTester reading (source) | ifcfast | Pinning case(s) |
|---|---|---|---|---|
| A6 | IFC2X3 `<NAME>TYPE` rule with a restriction entity name | `Entity.__call__` calls `self.name.endswith("TYPE")` (`ifctester/facet.py:234-238`); on a `Restriction` that raises `AttributeError` | The mapping rule applies to plain names only; a restriction name matches the occurrence class exactly | none (**open**: no suite case) |
| A7 | Candidates when applicability has no entity facet | `Attribute.filter` (`facet.py:277-303`): every entity declaring an attribute of that name, `by_type(..., include_subtypes=True)`. A name restriction matching two attributes declared at different levels collects a record twice | Every record whose class has a matching attribute (inherited included), deduplicated | none (**open**: no attribute-first case in the suite) |
| A8 | Several `IfcRelDefinesByType` on one occurrence (invalid IFC) | `get_type`: IFC4 `IsTypedBy[0]`, IFC2X3 the first `IfcRelDefinesByType` in `IsDefinedBy` (`ifcopenshell/util/element.py:629-641`); inverse order is ifcopenshell's | The first rel in file order | none |
| A9 | `predefinedType` resolution order when BOTH type and occurrence carry a concrete value | Type first (A1; `util/element.py:565-576`) | Type first, as IfcTester. Design §2.4 lists the occurrence first; the suite cases (`entity/pass-inherited_predefined_types_should_pass`, `…overridden…`) are consistent with both orders | **open**: no case with a concrete value on both. The design text should be amended to "type first" or a case added |
| A10 | Optional attribute facet on `''`, an empty list, or LOGICAL `.U.` | Fails (`FALSEY`, `facet.py:332-357`); only a non-resolvable name short-circuits to pass | Only null (`$`) or a derived slot (`*`) passes an optional facet (D3); `''`, `()` and `.U.` are written values and fail | `attribute/fail-an_optional_attribute_fails_if_empty` (`''`); `()` and `.U.` under optional: **open** |
| A11 | Value checks on references, lists and typed selects | Entity instances fail outright; a tuple never equals a cast IDS string (`facet.py:359-385`) | Always `ATTR_VALUE_MISMATCH` at run time; at compile time, a value on an attribute whose every resolution is object / list / select is `IdsInvalidError` | `attribute/invalid-value_checks_always_fail_for_{objects,lists,selects}` |
| A12 | An attribute-name restriction that matches no attribute of the applicability entity | `values=[]` → `NOVALUE` fail (`facet.py:306-330`) | Same (evaluated, not rejected). A plain unknown name is `IdsInvalidError` | `attribute/invalid-invalid_attribute_names_always_fail` (plain name) |
| A13 | File schemas `IFC4X3`, `IFC4X3_ADD1`, `IFC4X3_TC1` | `check_ifc_version`: exact `schema_identifier in ifcVersion` (`ids.py:278-280`); only matters when filtering | All map to the IFC4X3_ADD2 tables (ifcopenshell 0.8.5 resolves `IFC4X3` to ADD2); other schemas are an error | none |
| R1 | `ifcVersion` vs the file schema | `Ids.validate(..., should_filter_version=False)` by default (`ids.py:167-180`, `:282-284`): every spec is validated whatever its `ifcVersion` | Same by default. `filter_ifc_version=True` opts into `status="skipped_ifc_version"`. The slice brief asked to skip mismatches by default; that fails 10 `ids/` cases (IFC4 files under `ifcVersion="IFC2X3"`) | `ids/pass-specification_version_is_purely_metadata_and_does_not_impact_pass_or_fail_result` and the other 9 `ids/` cases |
| R2 | `predefinedType` literal outside the entity's enumeration | Compared as a user-defined type (ObjectType / ElementType / ProcessType) | Legal; never `IdsInvalidError`. The slice brief asked to reject it; that would fail user-defined cases. `partof/invalid-a_group_predefined_type_must_match_exactly_1_2` (`BUNNARY` vs `BUNNY`) fails on its own once PartOf lands | `entity/pass-a_predefined_type_may_specify_a_user_defined_object_type` (`WALDO`), `entity/fail-a_predefined_type_from_an_enumeration_must_be_uppercase` (must be `fail`, not invalid) |
| R3 | Attribute-name validation scope | No audit (only XSD decode) | A plain attribute name must be an explicit, non-derived attribute of the applicability entity (of some schema entity when there is none); inverse and derived names are `IdsInvalidError`. Applied to applicability and requirement facets alike | `attribute/invalid-{invalid_attribute_names,inverse_attributes,derived_attributes}_*` |

Also resolved by slice 1: D3's second case,
`ids/pass-specification_optionality_and_facet_optionality_can_be_combined`, is green in
ifcfast (it needs no separate registration; IfcTester's `ifctester_bug` label covers it once
the harness runs both legs).

Label parity (`expected`, `applicability_label`, `requirement_labels`): the English
templates are ported verbatim (`facet.py:123-158`, `:182-197`, `:260-275`). A restriction
prints as IfcTester's `str(options)` dict, but with a fixed key order (enumeration, pattern,
bounds, lengths) where IfcTester keeps document order (`facet.py:1007-1022`). **open**,
gated in slice 4 by `to_ifctester_json` equality.


## Slice 2 (Property, Classification, Material) decisions, 2026-09-25

Research pass for slice 2 (`docs/ids/facet-semantics-slice2.md`, which has the full rule → case
→ line table). All rows below are **intended** until slice 2 lands. Paths as above, plus
`element.py` / `cls.py` / `unit.py` = `ifcopenshell/util/{element,classification,unit}.py`
0.8.5. None of these points is decided by a suite case. Shipped 2026-09-25 as decided in the
last section (A14, A16, A26, D8 as the coordinator ruled; the rest as the `ifcfast` column says). **open** = the recommendation departs
from IfcTester, or the IfcTester reading is an artifact, so it needs a sign-off.

| # | Point | IfcTester reading (source) | ifcfast | Pinning case(s) |
|---|---|---|---|---|
| A14 | Occurrence and type sets with the same name | Merged **per property**: the type dict is updated with the occurrence's (`element.py:145-150`, `:215-232`). The pset `id` becomes the occurrence set's, so the dataType and unit loop only walks the occurrence set's properties (`facet.py:723-726`). A value inherited from the type is never dataType-checked or unit-converted | Merge per property, as IfcTester. dataType and units are checked against the property entity that **supplied** the value (**open**: departs from IfcTester in this corner) | `pass-properties_can_be_overriden_by_an_occurrence_1_2` is consistent with both merges |
| A15 | `optional` + a restriction `propertySet` that matches several sets | Returns pass at the first matched set that lacks the property, even if a later set has a wrong value (`facet.py:716-718`) | Evaluate every matched set. Absent properties are skipped; every present one must satisfy (**open**) | none |
| A16 | Cardinality of unsupported kinds | A reference value is invisible, i.e. absent (`element.py:412-473` has no branch). A complex property or quantity is present but fails with NOVALUE (`facet.py:853-858`), so `optional` fails on it and `prohibited` passes | Both count as **absent**: required → `PROP_UNSUPPORTED`, optional → pass, prohibited → pass (**open**) | `fail-complex_properties_are_not_supported_1_2`, `fail-reference_properties_are_treated_as_objects_and_not_supported` (required only) |
| A17 | dataType of multi-valued properties | List and enumerated: the first element only (`facet.py:774`, `:784`). Bounded: the last present of Upper, Lower, SetPoint (`:803-808`) | Every present element must carry the dataType (identical for homogeneous data) | none |
| A18 | dataType and units on `IfcPreDefinedPropertySet` attributes | Neither is checked: the fake property objects skip every branch (`facet.py:729-731`, `:914-919`) | dataType = the attribute's declared type name (`IFCDOORPANELOPERATIONENUM`). Measure attributes are converted like single values | `pass-predefined_properties_are_supported_but_discouraged_1_2` passes either way |
| A19 | Classification value and system on the same reference? | Independent: value against any reference (ancestors included), system against any reference's root (`facet.py:434-445`) | Same as IfcTester. `classification-facet.md` ("the classification ETIM must have the value …") reads as same-reference, but no case separates them | none |
| A20 | An occurrence reference in the same system as a type reference | The type's references of that system are replaced (`cls.py:46-57`) | Same | `…per_system_{1,2,3}_3` use two different systems |
| A21 | Non-classification external references, IFC2X3 material classification, reference cycles | `IfcLibraryReference` etc. on a non-rooted resource raises `AttributeError` in `get_classification` (`cls.py:61-69`, it reads `ReferencedSource`). IFC2X3 `IfcMaterialClassificationRelationship` is not read (`cls.py:32-36`). A `ReferencedSource` cycle loops forever (`cls.py:72-79`) | Only `IfcClassificationReference` / `IfcClassification` count. IFC2X3 material classification: none (as IfcTester). The chain walk is cycle-guarded | none |
| A22 | Occurrence material plus type material | The occurrence's first `IfcRelAssociatesMaterial` wins outright, with no union with the type (`element.py:728-741`) | Same | `pass-occurrences_can_override_materials_from_their_types` would also pass under a union |
| A23 | Directly associated `IfcMaterialLayer` / `IfcMaterialProfile` / `IfcMaterialConstituent` (IFC4), a layer or profile whose `Material` is null | `values` is unbound (`UnboundLocalError`), or `AttributeError` (`facet.py:957-986`) | Treat it as a one-item set: its Name and Category plus its material's Name and Category. Skip null materials | none |
| A24 | Candidates when the **first** applicability facet is property / classification / material | Property: `IfcObjectDefinition`, plus `IfcMaterialDefinition` + `IfcProfileDef` in IFC4+ (`facet.py:668-679`). Classification and material: `IfcObjectDefinition` (`:412-417`, `:939-944`). Subtypes included | Same | none |
| A25 | Configurations the facet docs call "not allowed": `value` without `dataType`, prohibited with `dataType` or `value`, optional without either | Evaluated normally. Prohibited = NOT(required outcome incl. value) (`facet.py:903-904`) | Evaluate as IfcTester. Not `IdsInvalidError` (no `invalid-` case covers them) | none |
| A26 | A measure value whose unit can't be resolved (no property `Unit`, no project unit of that type) and that needs a value comparison | Compares the raw number, which assumes SI (`unit.py:499-502` returns None) | Per-element failure `PROP_UNIT_UNRESOLVED` (new code). Design §2.5's run-level `IdsError::UnresolvedUnit` would abort a whole report over one property (**open**) | none |
| A27 | IfcTester artifacts not replicated | A baseName restriction can match the pset dict's `"id"` key (`facet.py:712-713`, `element.py:294`). LOGICAL `.U.` counts as a value on the restriction path (`facet.py:711-714`). `StopIteration` when a `.U.` comes from a type set (`:703-707`). `UnboundLocalError` on a bounded value with no bounds (`:808`) | None of these | none |
| A28 | Material failure text | `actual` is a Python `set`, so its repr order depends on `PYTHONHASHSEED` (`facet.py:995`, `:1176`) | Sorted, deduplicated, `None` dropped. `actual` of `MATERIAL_VALUE_MISMATCH` is excluded from the slice-4 parity gate | none |

Explicit in the text, IfcTester diverges, **no case**: follow the text.

| # | Point | Spec text / source | IfcTester reading (source) | ifcfast |
|---|---|---|---|---|
| D5 | Unit conversion outside single values | `units.md`: values "need to be converted to the default unit before comparison"; mass is kg | Enumerated values are never converted (`facet.py:769-778`). Mass is converted with output prefix KILO only on single values; quantities, lists, bounded and tables come out in **grams** (`:743` vs `:762-768`, `:792-798`, `:814-822`, `:837-845`) | Convert every numeric kind to SI; mass in kg |
| D6 | Conversion-based, temperature and derived units | `units.md` examples: 1 lbs = 0.45359237 kg, 20 °C = 293.15 K | Converts by unit *name* through an approximate table, ignoring `ConversionFactor` (`unit.py:661-662`; `pound` = 0.454 at `:208`). No temperature offset (`si_offsets` is unused, `:223`). Unnamed derived units are skipped on single values (`facet.py:741-742`) and raise `AttributeError` on the other kinds | `IfcConversionBasedUnit` via `ConversionFactor` (`IfcMeasureWithUnit`, recursive). Celsius/Fahrenheit with offset. Derived units = product of element factors^exponent |
| D7 | Tolerance on list / enumerated elements and on restriction enumerations | `tolerance.md`: ε applies to "doubles in ids:simpleValue and xs:restriction"; only ranges are exempt | Exact float equality: `cast_value not in value` (`facet.py:873-874`), `v == self.value` (`:880`), `Restriction.__eq__` enumeration (`:1050-1052`) | Tolerant equality everywhere except bounds (`restriction.rs` already does this) |
| D8 | A range restriction against a list / enumerated / bounded / table property | `property-facet.md` "Supported types of properties": "If the IDS value is a restriction (with minExclusive,maxExclusive,minInclusive,maxInclusive), all IFC values should respect the range" | Any element matching is enough (`facet.py:878-884`) | **All** values must satisfy when the restriction has a bound facet. Enumeration and pattern restrictions stay any-of. **open** (the same doc's bounded *simple-value* table is refuted by `property/fail-any_matching_value_in_a_bounded_property_will_pass_4_4`, which lowers the doc's authority on this section) |
| D9 | A table property checked without `dataType` | `property-facet.md`: `baseName` "must exist … and have a non-empty value"; dataType is optional | No column matches `None`, so it fails as DATATYPE (`facet.py:833`, `:848-851`) | A non-empty table passes a name-only check. With a `value` but no `dataType`, all columns are candidates |
| D10 | A bounded value with no Upper, Lower or SetPoint | `property-facet.md` bounded table: "at least one of the lower and upper bounds is required" | Crash (A27) | Counts as empty → `PROP_NULL` |

Pre-existing implementation note found by this pass (not an ambiguity): the shipped
`real_eq` (`crates/core/src/ids/restriction.rs:236-242`, closed interval `v ± (|v|ε+ε)`) fails
`tolerance/pass-comparison_tolerance_for_floating_point_{negative_low_number_upper,positive_low_number_lower}_bound`
in f64. Moving each edge outward by one ulp passes all 28 point cases (method in the slice-2 doc §4).

## Coordinator decisions on the slice-2 open rows, 2026-09-25

Truth order stays: suite filename truth > IDS docs > IfcTester. Recorded here so the
implementer codes against decisions, not options. All reversible.

| Row | Decision | Why |
|---|---|---|
| A14 | Check dataType and unit against the property entity that **supplied** the value, type-inherited or not. | A value is a value regardless of source; skipping the check for inherited values would let a wrong-typed type property pass silently. Departs from IfcTester in a corner no case pins. |
| A16 | Complex and reference properties both count as **absent**: required → `PROP_UNSUPPORTED` (new reason code, row-level and labelled), optional → pass, prohibited → pass. | One rule for both kinds; the failure names what we cannot check instead of pretending a NOVALUE. |
| A26 | **Keep the design**: an unresolvable unit that a comparison needs is `IdsError::UnresolvedUnit`, routed through the existing `on_unsupported` machinery — `raise` → `IdsUnitError`; `mark` → that spec gets `status="unsupported"`, `unsupported_feature="unit:<UNITTYPE>"`, no element rows. No `PROP_UNIT_UNRESOLVED` code. | A per-element *fail* for "we could not determine" fabricates a compliance result and flips `rep.ok` for a reason unrelated to the model. Marking the spec keeps the rest of the report intact without inventing an outcome. |
| D8 | A restriction **with bound facets** (min/max inclusive/exclusive) must hold for **all** values of a multi-valued property; enumeration and pattern restrictions stay any-of. | The docs are explicit and no case contradicts them; IfcTester's any-of is a gap, not a reading. |
| real_eq | Widen each tolerance edge outward by **1 ulp** (`next_down` / `next_up`) so `tol = |v|·1e-6 + 1e-6` is inclusive in f64; add the 14 tolerance point cases as unit rows. | Two suite pass cases fail on the exact-edge f64 comparison; the rule is inclusive by intent. |
| PROP_UNSUPPORTED | Added to the reason-code list (report.rs, `python/ifcfast/ids.py` REASON_CODES, design §3.2, AGENTS.md). | Needed by A16. |

## Slice 2 implementation rows, 2026-09-25

Points the implementation had to decide that the rows above leave open. No suite case pins any
of them (all 174 slice-2 cases agree either way). Code paths are `crates/core/src/ids/`.

| # | Point | IfcTester reading (source) | ifcfast | Pinning |
|---|---|---|---|---|
| A29 | A measure with no unit type (`IFCMONETARYMEASURE`, `IFCREAL`, `IFCCOUNTMEASURE`, `IFCNUMERICMEASURE`) whose property carries a `Unit` | `get_property_unit` returns the unit and `convert` runs on it (`facet.py:740-750`, `unit.py:456-502`); a monetary unit has no SI name and the value comes back unchanged | No conversion: a value is converted only when its measure has a unit type (`schema_tables.rs` `MEASURE_UNIT_TYPE`, hand-mapped rows name why). Currency has no SI unit, so nothing is assumed | `eval.rs` `to_si`; none |
| A30 | IFC2X3 "predefined" sets (`IfcDoorPanelProperties`, `IfcDoorLiningProperties`, … are direct `IfcPropertySetDefinition` subtypes; IFC2X3 has no `IfcPreDefinedPropertySet`) | `get_properties` has no branch for them and returns `None`, so the loop at `facet.py:726` raises `TypeError` | Not read: only IFC4+ `IfcPreDefinedPropertySet` subtypes (the generated `PREDEF_PSET_ATTR_TYPE` table is empty for IFC2X3), so a spec naming one gets `PSET_MISSING` | `graph.rs` `predefined_set`; none |
| A31 | Quantity classes the shared `PropertyGraph` has no reader for (IFC4X3 `IfcQuantityNumber`, spec §1.3 → `IFCNUMERICMEASURE`) | Read through attribute 3 like every `IfcPhysicalSimpleQuantity` (`facet.py:752-768`) | `PROP_UNSUPPORTED` (the graph records them as `UnhandledQuantity` without a value, and the public QuantityTable's `unhandled:` marker rows must not change). **Follow-up**: give the graph a value slot for them | `eval.rs` `extract`; none |
| A32 | dataType of a table column | The first member's class (`facet.py:829-833`) | Every present member of the column must carry the dataType (the A17 rule applied per column); identical for homogeneous data | `eval.rs` `check_prop`; none |
| A33 | `IfcRelDefinesByProperties` whose related object is a type object | `get_psets` reads a type's `HasPropertySets` only (`element.py:186-192`) | Read as well (the relation is what the file declares). Occurrence sets still never flow up to the type (P19) | `graph.rs` `PropData::build`; none |
| A34 | Type used for inheritance when an occurrence has several `IfcRelDefinesByType` (invalid IFC) | First (`element.py:629-641`) | First in file order (A8), for properties, classifications and materials alike. The public tables keep their last-wins map; they differ only on such invalid files | `eval.rs` `Ctx::build_type_map`; none |
| A35 | `actual` of `PROP_DATATYPE_MISMATCH` | The CamelCase class, `IfcText` (`facet.py:737`) | The STEP token as written, `IFCTEXT` (the schema tables carry no CamelCase type names). **open** for the slice-4 `to_ifctester_json` parity gate | `eval.rs` `check_prop`; none |
| A36 | A required facet whose name restriction matches only unsupported and null properties | NOVALUE either way | `PROP_UNSUPPORTED` wins over `PROP_NULL`: the actionable fact is that IDS cannot check the kind | `eval.rs` `eval_property`; none |
| A37 | A list, enumerated, bounded or table member that is not comparable (a nested list or reference inside an `IfcValue` list) | Compared as a tuple and never equal (`facet.py:871-884`) | Never matches; under a bounded restriction (D8, all-of) it fails the check | `eval.rs` `check_prop`; none |
| A38 | A property `dataType` that names an IFC4-only IfcValue / measure type in an IFC2X3 file (e.g. `IFCDATE`) | Not checked (no audit beyond the XSD, `ids.py:56-65`); the value simply never matches | `IdsInvalidError` at compile when the name is an IfcValue / measure type of some schema but not the file's (design §2.5). Enumeration dataTypes are only checked against `DataTypes.md` (the tables carry no full type list) | `compile.rs` `resolve_data_type`; `ids_compile_data_facets_and_datatypes` |

