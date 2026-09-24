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
`crates/core/src/ids/{compile,candidates,attrs,eval,report}.rs`); rows for those facets
describe shipped behaviour, pinned by `crates/core/tests/ids_eval.rs` (all 334 suite cases
agree with the filename truth: slice-1 folders by result, the rest as `Unsupported`). Rows
for other facets stay *intended* until their slice lands.

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

