# IDS slice 2: Property, Classification and Material semantics

What the slice-2 implementer codes against (GH #192, design §2.4/§2.5). Sources in priority
order: the suite's filename truth (buildingSMART/IDS @ `a67047736aa9`, folders `property/`,
`classification/`, `material/` and `tolerance/`, all 174 `.ids` + `.ifc` pairs read), then
IfcTester 0.8.5 where the suite says nothing, then the IDS docs at the same sha
(`UserManual/{property,classification,material}-facet.md`, `units.md`, `restrictions.md`,
`ImplementersDocumentation/{DataTypes,tolerance}.md`).

**Paths:** `facet.py` = `ifctester/facet.py`, `ids.py` = `ifctester/ids.py`, `element.py` /
`cls.py` / `unit.py` = `ifcopenshell/util/{element,classification,unit}.py` (0.8.5,
site-packages). **[V]** = read at that line. **[I]** = inferred.

**Status column:**
- **T+I**: a suite case decides it and IfcTester agrees.
- **T**: a suite case decides it, IfcTester gets it wrong (xfail in `tests/oracle/ids_xfail.toml`). The truth wins.
- **I**: no case. Follow IfcTester (register rule).
- **X**: no case, the docs are explicit and IfcTester diverges. Follow the text. These are registered as D5–D10.

Every applicability in the 174 cases is an entity facet (slice 1). **No case needs PartOf or
any other slice [V].** No case uses `uri`, a property-level `Unit`, `IfcMeasureWithUnit`, an
`IfcConversionBasedUnit`, an `IfcDerivedUnit`, or a property, classification or material facet in
applicability [V: grep over all 334 cases].

---

## 1. Property facet

### 1.1 Finding the property sets

| # | Rule | Deciding case(s) | IfcTester | Status |
|---|---|---|---|---|
| P1 | A plain `propertySet` is an exact, case-sensitive name match; a restriction is matched with `Restriction.__eq__`. The same goes for `baseName`. If no set matches → PSET_MISSING. If the set matches but the name doesn't → PROP_MISSING. | `fail-elements_with_no_properties_always_fail`, `fail-elements_with_a_matching_pset_but_no_property_also_fail`, `pass-all_matching_property_sets_must_satisfy_requirements_1_3` | `facet.py:682-695`, `:701-714` | T+I |
| P2 | Where the sets come from. **Occurrence**: `IsDefinedBy` → `IfcRelDefinesByProperties.RelatingPropertyDefinition`. This includes IfcProject/IfcContext. **Type object**: `HasPropertySets`. **IFC4+ material/profile**: `IfcMaterialDefinition.HasProperties` (`IfcMaterialProperties`) and `IfcProfileDef.HasProperties` (`IfcProfileProperties`). **IFC2X3 IfcMaterial**: every `IfcExtendedMaterialProperties` whose `Material` is the element, with its properties read from `ExtendedProperties`. IFC2X3 profiles and unnamed material-property subtypes: none. The set kinds are `IfcPropertySet`, `IfcElementQuantity` (name = quantity-set name), `IfcPreDefinedPropertySet`, material/profile properties. | `pass-project_properties_are_supported_under_ifc{2x3,4}_via_*`, `pass-material_properties_are_supported_under_ifc4_via_ifcmaterialproperties`, `pass-…_ifc2x3_via_extendedmaterialproperties`, `pass-a_name_check_will_match_any_quantity_with_any_value`, `pass-predefined_properties_are_supported_but_discouraged_1_2` | `element.py:42-156`, `:159-233`. IFC2X3 crash at `facet.py:912-913` | T+I. The IFC2X3 extended case is **T** (D4) |
| P3 | Type inheritance. An occurrence sees its type's sets (`get_type`: IFC4 `IsTypedBy[0]`, IFC2X3 the first `IfcRelDefinesByType`). An occurrence set with the same name is merged **per property**, and the occurrence value wins. A type object sees only its own sets (P19). | `pass-properties_can_be_inherited_from_the_type_1_2`, `pass-properties_can_be_overriden_by_an_occurrence_1_2` | `element.py:108-111`, `:145-150`, `:215-232`, `:614-641` | T+I for override. Per-property (not per-set) merging is **I** (A14) |
| P4 | A restriction `propertySet` that matches several sets: **every** matched set must hold a matching property, otherwise PROP_MISSING. | `fail-all_matching_property_sets_must_satisfy_requirements_2_3`, `pass-…_3_3` | `facet.py:699-721` | T+I |
| P5 | A restriction `baseName` that matches several properties: **every** matched property must satisfy dataType and value. | `fail-all_matching_properties_must_satisfy_requirements_3_3`, `fail-if_multiple_properties_are_matched__all_values_must_satisfy_requirements_2_2`, pass siblings | `facet.py:726`, `:863-901` | T+I |
| P19 | A type object in applicability is evaluated on its own sets only. Occurrence sets never flow up to the type. | `fail-properties_can_be_overriden_by_an_occurrence_2_2`, `pass-properties_can_be_inherited_from_the_type_2_2`, `fail-properties_can_be_associated_to_relevant_object_types` | `element.py:81-85`, `:186-192` | T+I |

### 1.2 Presence, empty values, unsupported kinds

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| P6 | A property counts as **absent** when its value is null (`$`), `''`, IfcLogical `.U.`, or an empty or null EnumerationValues or ListValues. Report PROP_NULL when the property entity exists, PROP_MISSING when it doesn't. `.F.` and `P0D` are values. | `fail-a_logical_unknown_is_considered_false…`, `fail-an_empty_string_is_considered_false…`, `pass-a_property_set_to_{false,true}…`, `pass-a_zero_duration_will_pass` | `facet.py:702-714`. Empty lists become `None` at `element.py:431`, `:440` | T+I. Null of the *named* property is **I**: `fail-properties_with_a_null_value_fail` actually has only a null *sibling* |
| P12 | IfcComplexProperty, IfcPhysicalComplexQuantity and IfcPropertyReferenceValue are unsupported and never satisfy a requirement. Nested properties and quantities are not reachable, and a complex quantity is not a set. | `fail-complex_properties_are_not_supported_{1,2}_2`, `fail-reference_properties_are_treated_as_objects_and_not_supported` | Complex: `facet.py:853-858`. Reference: absent from `get_properties` (`element.py:412-473`) | T+I. Cardinality treatment is **I**/open (A16) |
| P16 | `IfcPreDefinedPropertySet` attributes from index 4 on count as properties. Enum values compare as their token (`SWINGING`). | `pass-/fail-predefined_properties_are_supported_but_discouraged_{1,2}_2` | `facet.py:914-919`, `:729-731` (no dataType or unit check) | T+I for the value. dataType handling A18 |

### 1.3 dataType

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| P7 | `dataType` must equal the IFC class of the value wrapper, compared case-insensitively. SingleValue: `NominalValue`. Enumerated/List: IfcTester checks the first element (A17). Bounded: the last present of Upper, Lower, SetPoint (A17). Quantity: the declared type of attribute 3 (table below). Mismatch → PROP_DATATYPE_MISMATCH. **No subtype or measure widening** (`IFCLABEL` ≠ `IFCTEXT`). | `fail-measures_are_used_to_specify_an_ifc_data_type_1_2`, `fail-quantities_must_also_match_the_appropriate_measure`, `pass-a_name_check_will_match_any_quantity_with_any_value` | `facet.py:733-738`, `:752-758`, `:774-778`, `:784-788`, `:803-811` | T+I |
| P11a | Table values: only the columns (Defining, Defined) whose first value's class equals `dataType` supply values. **No dataType, or no matching column → PROP_DATATYPE_MISMATCH** in IfcTester. | `pass-any_matching_value_in_a_table_property_will_pass_{1,2}_3` | `facet.py:825-852` | T+I with a dataType. Without one: **X** (D9) |

Quantity → dataType (identical in IFC2X3, IFC4 and IFC4X3_ADD2 [V: `schema_by_name`]):

| Quantity | dataType | Quantity | dataType |
|---|---|---|---|
| IfcQuantityLength | IFCLENGTHMEASURE | IfcQuantityCount | IFCCOUNTMEASURE |
| IfcQuantityArea | IFCAREAMEASURE | IfcQuantityWeight | IFCMASSMEASURE |
| IfcQuantityVolume | IFCVOLUMEMEASURE | IfcQuantityTime | IFCTIMEMEASURE |
| IfcQuantityNumber (4X3 only) | IFCNUMERICMEASURE | | |

### 1.4 Value matching

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| P8 | Single value. A **string** IFC value is compared exactly and case-sensitively, after STEP decoding (`\X2\…\X0\`, `''`). A **number** is compared with the IDS text parsed by Python `float()`: reals under tolerance (T1), integers exactly. A **boolean** is compared with `true/false/1/0` only. A restriction is matched with `Restriction.__eq__`: enumeration any-of, pattern (strings only), bounds exact, lengths. | `pass-real_values_are_checked_using_type_casting_{1,2,3}_3`, `pass-integer_values_…_1_4`, `pass-/fail-booleans_…`, `pass-/fail-specifying_a_value_performs_a_case_sensitive_match_*`, `pass-a_number_specified_as_a_string_is_treated_as_a_string`, `pass-non_ascii_characters_are_treated_without_encoding`, `pass-only_specifically_formatted_numbers_are_allowed_{3,4}_4` | `facet.py:36-50`, `:863-901`, `:1045-1083` | T+I (tolerance: see §4) |
| P17 | IfcDate, IfcDuration and IfcIdentifier are plain strings. No date or duration normalisation, no truncation. | `fail-/pass-dates_are_treated_as_strings_*`, `fail-/pass-durations_…`, `fail-ids_does_not_handle_string_truncation…` | `facet.py:865-870` | T+I |
| P9 | Enumerated and List values pass when **any** element matches a simple value. For enumerated values the *selected* `EnumerationValues` count, not the `IfcPropertyEnumeration` domain. | `pass-any_matching_value_in_a_list_property_will_pass_{1,2}_3`, `fail-…_3_3`, `pass-any_matching_value_in_an_enumerated_property_will_pass_{1,2}_3`, `fail-no_matching_value_in_an_enumerated_property_will_fail_3_3` | `facet.py:871-877` | T+I |
| P10 | Bounded values pass when **any** of {Upper, Lower, SetPoint} equals the simple value. The bounds are not treated as a range: 2 against [1,5] with set point 3 **fails**. This contradicts the bounded table in `property-facet.md`, and the suite wins. | `pass-any_matching_value_in_a_bounded_property_will_pass_{1,2,3}_4`, `fail-…_4_4` | `facet.py:801-824`, `:871-877` | T+I |
| P11 | Table: **any** value of the dataType-matching columns (P11a). | `pass-…table…_{1,2}_3`, `fail-…table…_3_3` | `facet.py:825-852` | T+I |
| P9r | A **restriction** against a multi-valued property. IfcTester passes when any element matches. `property-facet.md` says a range restriction must hold for *all* IFC values. | none | `facet.py:878-884` | **X** (D8), open |
| P9t | List and enumerated elements. IfcTester compares floats exactly (`in`, `==`). tolerance.md applies ε to every double. | none | `facet.py:873-874`, `:880`, `:1050-1052` | **X** (D7) |

### 1.5 Units

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| P13 | IDS numbers are SI. Convert the IFC value to SI before comparing. The unit is the property's own (`Unit`; `EnumerationReference.Unit`; table `DefiningUnit`/`DefinedUnit`; quantity `Unit`), otherwise the project unit for the measure's unit type. The unit type comes from the measure name: strip `Ifc`, `Measure`, `Non`, `Positive`, `Negative`, upper-case, add `UNIT`. `IfcNumericMeasure` → `USERDEFINED`. Reals, integers, labels and counts have no unit type. The only pinned path is a project `IfcSIUnit` with the MILLI prefix, on single, bounded and table values. | `pass-/fail-unit_conversions_shall_take_place_to_ids_nominated_standard_units_*`, bounded `_1..3_4`, table `_2_3`, `pass-measures_are_used_to_specify_an_ifc_data_type_2_2` (TIMEUNIT s) | `unit.py:456-502`, `:505-545`, `:565-585`, `:647-679`. Single value `facet.py:740-750`; quantity `:760-768`; list `:789-800`; bounded `:812-823`; table `:827-846` | T+I for SI prefixes |
| P13a | Mass is in **kg**: the IfcSIUnitName is GRAM, so KILO GRAM → ×1. IfcTester outputs kg only for single values and grams elsewhere. | none | `facet.py:743` vs `:762-768` | **X** (D5) |
| P13b | Enumerated measures are converted. IfcTester never converts them. | none | `facet.py:769-778` | **X** (D5) |
| P13c | Conversion-based units go through `ConversionFactor` (`IfcMeasureWithUnit`: ValueComponent × SI factor of UnitComponent, recursively). IfcTester looks up the unit *name* in an approximate table and ignores the factor (`pound` 0.454, where units.md gives 0.45359237). | none | `unit.py:661-662`, `:208` | **X** (D6) |
| P13d | Temperature is in K with an offset (units.md: 20 °C → 293.15). IfcTester applies no offset. | none | `unit.py:647-679` | **X** (D6) |
| P13e | Derived units: product of the elements' SI factors raised to their exponents. IfcTester skips unnamed units on single values and raises `AttributeError` elsewhere. | none | `facet.py:741-742`, `:762` | **X** (D6) |
| P13f | A measure value compared against a value whose unit cannot be resolved (no property Unit and no project unit of that type). IfcTester compares the raw number. | none | `unit.py:499-502` | open (A26) |

### 1.6 Cardinality, applicability, metadata

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| P14 | **optional**: a missing set or an absent property passes. A present property must still satisfy dataType and value. | `pass-an_optional_facet_always_passes_regardless_of_outcome_{1,2}_2` | `facet.py:692-694`, `:716-718` | T+I. The early return with a restriction set is A15 |
| P15 | **prohibited** = NOT(the required outcome, with dataType and value). "prohibited + value" therefore fails only when the property exists and matches. | `fail-a_prohibited_facet_returns_the_opposite_of_a_required_facet` (no value) | `facet.py:903-904` | T+I. With a value: **I** (A25) |
| P18 | An IDS literal that does not fit the dataType's XSD base (`42.`, `42,3`, `FALSE`) is `IdsInvalidError` at audit. Already shipped: `crates/core/src/ids/audit.rs:44`, `:132`. | `invalid-*` (6) | IfcTester passes `_2_4` and `_3_4` | T for `invalid-integer_values_cannot_be_stored_with_decimal_{2,3}_4` |
| P20 | Applicability-side property facet: the same `__call__`, with cardinality forced to required, so type inheritance applies. When it is the **first** applicability facet the seed is `IfcObjectDefinition` in IFC2X3, and IfcObjectDefinition + IfcMaterialDefinition + IfcProfileDef in IFC4+ (subtypes included). | none | `facet.py:668-679`, `ids.py:290-300` | I (A24) |
| P21 | `uri` and `instructions` are metadata and never checked. | none | never read in `__call__` | text + IfcTester agree |

---

## 2. Classification facet

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| C1 | No classification at all → CLASS_MISSING. | `fail-systems_should_match_exactly_2_5`, `fail-restrictions_can_be_used_for_systems_1_2`, `fail-a_classification_facet_with_no_data…_1_2` | `cls.py:24-58`, `facet.py:426-432` | T+I |
| C2 | `system` (required by the XSD) matches `IfcClassification.Name` of the chain root: follow `ReferencedSource` up to the first IfcClassification (unbounded, guard against cycles). A reference with no root has no system. A pattern is allowed. | `pass-systems_should_match_exactly_{3,4,5}_5`, `fail-a_required_classification_system_fails_if_no_match`, `pass-restrictions_can_be_used_for_systems_2_2` | `cls.py:61-69`, `facet.py:440-445` | T+I |
| C3 | A direct `IfcRelAssociatesClassification` to an **IfcClassification**, with no reference, counts. Its system is its own Name and its value is null. | `pass-systems_should_match_exactly_1_5` (IfcProject), `fail-an_optional_classification_value_fails_if_no_match` | `cls.py:67-68`, `facet.py:435` | T+I |
| C4 | `value` matches `Identification` (IFC4+) or `ItemReference` (IFC2X3) **exactly**. No prefix matching: `1` does not match `11`. Patterns are allowed. | `pass-both_system_and_value_must_match…_1_2`, `fail-…_2_2`, `pass-restrictions_can_be_used_for_values_{1,2}_3`, `pass-values_should_match_exactly_if_lightweight_classifications_are_used` | `facet.py:434-438` | T+I |
| C5 | **Ancestor references count.** The value is tested against the leaf and every ancestor `IfcClassificationReference` (`2` matches leaf `22` whose parent is `2`). The root IfcClassification is excluded. | `pass-values_match_subreferences_if_full_classifications_are_used…`, `fail-restrictions_can_be_used_for_values_3_3` | `cls.py:72-79`, `facet.py:420-424` | T+I |
| C6 | Type inheritance for IfcObject occurrences. The type's references are grouped by system, and an occurrence reference of the **same** system replaces that group. Other systems' type references stay. | `pass-occurrences_override_the_type_classification_per_system_{1,3}_3`, `fail-…_2_3` | `cls.py:37-57` | T+I for "different systems both apply". Same-system replacement is **I** (A20) |
| C6a | Value and system are checked **independently**: any reference value, then any reference's system. Doc wording suggests the same reference. | none separates | `facet.py:434-445` | I (A19) |
| C7 | **optional**: no classification passes. A present one must match. | `pass-an_optional_classification_value_passes_if_{null,specified}` (both have an unclassified wall), `fail-an_optional_classification_value_fails_if_no_match` | `facet.py:429-431` | T+I |
| C8 | **prohibited** = NOT(required outcome). With a value: "must not have that value". | `fail-a_prohibited_classification_reference_returns…`, `fail-a_prohibited_facet_returns…` | `facet.py:447-448` | T+I |
| C9 | Non-rooted resources (IfcMaterial, …) are classified through `HasExternalReferences` → `IfcExternalReferenceRelationship.RelatingReference`, with no type inheritance. | `pass-non_rooted_resources_that_have_external_classification_references_should_also_pass` | `cls.py:32-36` | T+I. Non-classification external references: A21 |
| C10 | `uri` (Location) is not checked. Docs: "not subject to IDS checking". | none | not read | text + IfcTester agree |
| C11 | Applicability-first seed: `IfcObjectDefinition` only, so non-rooted resources are not candidates. | none | `facet.py:412-417` | I (A24) |

Reason order follows IfcTester: presence → value → system (`facet.py:429-445`).

---

## 3. Material facet

| # | Rule | Case(s) | IfcTester | Status |
|---|---|---|---|---|
| M1 | Material presence is the first `IfcRelAssociatesMaterial` of the element, otherwise its type's. A usage resolves to its set: `IfcMaterialLayerSetUsage.ForLayerSet`, `IfcMaterialProfileSetUsage.ForProfileSet`. An empty set is still a material. No value → pass if present, MATERIAL_MISSING if not. | `pass-elements_with_any_material_will_pass_an_empty_material_facet`, `fail-elements_without_a_material_always_fail` | `element.py:704-741`, `facet.py:947-955` | T+I |
| M2 | Candidate strings (value = any-of, exact, or a restriction). **IfcMaterial**: Name, Category. **IfcMaterialList**: each member's Name, Category. **IfcMaterialLayerSet**: LayerSetName; for each layer its Name, Category, Material.Name, Material.Category. **IfcMaterialProfileSet**: Name; for each profile its Name, Category, Material.Name, Material.Category. **IfcMaterialConstituentSet**: Name; for each constituent its Name, Category, Material.Name, Material.Category. Descriptions never count. IFC2X3 attributes that don't exist are simply absent. | 20 `pass-a_…`/`pass-any_…_will_pass_a_value_check` cases, `fail-material_with_no_data…`, `fail-a_material_list_with_no_data…`, `fail-a_constituent_set_with_no_data…` | `facet.py:957-995` | T+I |
| M3 | Type inheritance is **all or nothing**: the occurrence's own association replaces the type's, with no union. | `pass-occurrences_can_inherit_materials_from_their_types`, `pass-occurrences_can_override_materials_from_their_types` (union would pass too) | `element.py:738-741` | T+I for inheritance. No-union is **I** (A22) |
| M4 | Directly associated IfcMaterialLayer / Profile / Constituent (IFC4 `IfcMaterialDefinition`), and a layer with a null Material. | none | `values` unbound / `AttributeError` at `facet.py:957-986` | I-crash → A23 |
| M5 | **optional**: no material passes. A present material must match. | `pass-an_optional_material_passes_if_{null,specified}`, `fail-an_optional_material_fails_if_no_value_matches` | `facet.py:952-954` | T+I |
| M6 | **prohibited** = NOT(required outcome). | `fail-a_prohibited_facet_returns_the_opposite_of_a_required_facet` | `facet.py:997-998` | T+I |
| M7 | `uri` is not checked. Applicability-first seed: `IfcObjectDefinition`. | none | `facet.py:939-944` | text+I / I (A24) |

---

## 4. Tolerance folder (36 cases)

Every case is an `IFCREAL` single value on an IfcWall (no units apply) [V].

| Group | n | Requirement | IfcTester |
|---|---|---|---|
| `pass-comparison_tolerance_for_floating_point_{zero,one,negative_one,positive_low_number,negative_low_number,positive_high_number,negative_high_number}_{lower,upper}_bound` | 14 | T1: the actual sits **exactly on** the tolerance.md table edge, `v ± (|v|·1e-6 + 1e-6)` (e.g. 1 → 1.000002, 0 → ±0.000001, -1e6 → -1000001.000001). It must pass | **all 14 fail**: `is_x` is relative-only (`facet.py:53-60`). Registered (D1) |
| `fail-…` same 14 names | 14 | T1: the actual is just outside the edge (e.g. 1.0000021, ±0.0000011). It must fail | agrees |
| `pass-comparison_tolerance_for_floating_point_range_{greater,lower}_than_zero_{inclusive,exclusive}` | 4 | T2: bounds are exact, with no ε: `>0` passes 1e-7, `≥0` passes 0, `<0` passes -1e-7, `≤0` passes 0 | agrees (`facet.py:1069-1080`) |
| `fail-…range…` | 4 | T2: `>0` fails 0, `≥0` fails -1e-7, `<0` fails 0, `≤0` fails 1e-8. These are within ε, so ε must **not** be applied to bounds | agrees |

**T1 needs a closed interval widened by one ulp.** This comes from a Python f64 simulation of the 28
point cases, which reproduces Rust's correctly-rounded `str::parse` used by `lexer.rs:833-835`:

| Formula | Cases wrong |
|---|---|
| strict `lo < x < hi` (tolerance.md text) | 12 of 14 pass cases |
| closed `v−tol ≤ x ≤ v+tol`, `tol = |v|·ε+ε`. **This is shipped `real_eq`, `crates/core/src/ids/restriction.rs:236-242`** | 2: `pass-…negative_low_number_upper_bound`, `pass-…positive_low_number_lower_bound` |
| `|x−v| ≤ tol` | 8 |
| exact rational arithmetic on the parsed f64 | 4 |
| closed interval, `lo`/`hi` each moved 1 ulp outward (`next_down`/`next_up`) | **0** (1–5 ulps all give 0) |

So `real_eq` must widen by one ulp, or its first property-facet run fails 2 suite cases. Its unit
test (`restriction.rs:363-389`) doesn't carry those two rows. Add them. The ε also applies to
enumeration values of a double restriction (tolerance.md: "doubles in ids:simpleValue and
xs:restriction"; ranges are excluded). `restriction.rs` already does this (D7).

---

## 5. Reason codes

| Failure shape | Code | IfcTester reason (`facet.py`) | Pinning case |
|---|---|---|---|
| No set of that name | PSET_MISSING | NOPSET `:695` | `fail-elements_with_no_properties_always_fail`, `fail-material_properties_that_are_absent_*`, `fail-project_properties_that_are_absent_*`, `fail-complex_…_2_2` |
| Set present, no matching property | PROP_MISSING | NOVALUE `:720` | `fail-elements_with_a_matching_pset_but_no_property…`, `fail-properties_with_a_null_value_fail`, `fail-all_matching_property_sets…_2_3` |
| Property present, value null / `''` / `.U.` / empty list | PROP_NULL | NOVALUE `:720`, `:771`, `:781` | `fail-an_empty_string…`, `fail-a_logical_unknown…` |
| Complex property, complex quantity, reference value | **PROP_UNSUPPORTED (new)** | NOVALUE `:858` / absent | `fail-complex_properties_are_not_supported_1_2`, `fail-reference_properties_…` |
| Wrapper or measure ≠ dataType; table with no matching column | PROP_DATATYPE_MISMATCH | DATATYPE `:737`, `:757`, `:850` | `fail-measures_…_1_2`, `fail-quantities_must_also_match…` |
| Value mismatch (any P8–P11) | PROP_VALUE_MISMATCH | VALUE `:869-900` | 14 `fail-comparison_tolerance_*`, the list/bounded/table/enum fails, `fail-dates_…`, `fail-booleans_…_1_3` |
| A measure's unit can't be resolved and a value comparison needs it | **PROP_UNIT_UNRESOLVED (new, proposed)** | none (compares raw) | none (A26) |
| No classification | CLASS_MISSING | NOVALUE `:432` | C1 cases |
| No reference or ancestor value matches | CLASS_VALUE_MISMATCH | VALUE `:438` | `fail-both_system_and_value…_2_2`, `fail-occurrences_override…_2_3`, `fail-an_optional_classification_value_fails_if_no_match`, `fail-restrictions_can_be_used_for_values_3_3` |
| Value OK, no root system matches | CLASS_SYSTEM_MISMATCH | SYSTEM `:445` | `fail-a_required_classification_system_fails_if_no_match` |
| No material | MATERIAL_MISSING | NOVALUE `:955` | `fail-elements_without_a_material_always_fail` |
| No candidate string matches | MATERIAL_VALUE_MISMATCH | VALUE `:995` | `fail-material_with_no_data…`, list and constituent fails, `fail-an_optional_material_fails…` |
| Prohibited facet satisfied | PROHIBITED_PRESENT | PROHIBITED `:904`, `:448`, `:998` | the three `fail-a_prohibited_*` |

Adding the two new codes means touching `report.rs::reason`, `python/ifcfast/ids.py:68-73`
`REASON_CODES`, design §3.2, and AGENTS.md (a public-contract rule). `value_source` = `type`
when the matched value came from the type's set, reference or material (P3, C6, M3), otherwise
`instance`.

---

## 6. IfcTester label templates (verbatim)

Substitution (`facet.py:123-158`, already ported): take the first template whose every `{param}`
is non-None, and print each value with `str()` (a restriction prints as its `options` dict).
Requirement + `optional`: `shall→may`, `Shall→May`, `must→may`. Requirement + `prohibited`: the
prohibited templates. Applicability under a prohibited spec (`maxOccurs=0`): the prohibited
templates. Any requirement under a prohibited spec: `"The requirement is not applicable"`.

**Property** (`facet.py:654-665`; parameter order propertySet, baseName, value, `@dataType`…; dataType never appears)

| Clause | Templates, in order |
|---|---|
| applicability | `Elements with {baseName} data of {value} in the dataset {propertySet}` · `Elements with {baseName} data in the dataset {propertySet}` |
| requirement | `{baseName} data shall be {value} and in the dataset {propertySet}` · `{baseName} data shall be provided in the dataset {propertySet}` |
| prohibited | `{baseName} data shall not be {value} and in the dataset {propertySet}` · `{baseName} data shall not be provided in the dataset {propertySet}` |

**Classification** (`facet.py:394-408`; order value, system)

| Clause | Templates |
|---|---|
| applicability | `Data having a {system} reference of {value}` · `Data classified using {system}` · `Data classified as {value}` |
| requirement | `Shall have a {system} reference of {value}` · `Shall be classified using {system}` · `Shall be classified as {value}` |
| prohibited | `Shall not have a {system} reference of {value}` · `Shall not be classified using {system}` · `Shall not be classified as {value}` |

**Material** (`facet.py:925-936`)

| Clause | Templates |
|---|---|
| applicability | `All data with a {value} material` · `All data with a material` |
| requirement | `Shall have a material of {value}` · `Shall have a material` |
| prohibited | `Shall not have a material of {value}` · `Shall not have a material` |

Failure reasons (`Result.to_string`), for `to_ifctester_json`:

| Facet | Reason → text |
|---|---|
| Property `:1150-1167` | NOPSET `The required property set does not exist` · NOVALUE `The property set does not contain the required property` · DATATYPE `The property's data type "{actual}" does not match the required data type of "{dataType}"` · VALUE, list of length 1: `The property value "{actual[0]}" does not match the requirements`; longer list: `The property values "{actual}" do not match the requirements`; scalar: `The property value "{actual}" does not match the requirements` · PROHIBITED `The property should not have met the requirement` |
| Classification `:1126-1135` | NOVALUE `The entity has no classification` · VALUE `The references "{actual}" do not match the requirements` · SYSTEM `The systems "{actual}" do not match the requirements` · PROHIBITED `The classification should not have met the requirement` |
| Material `:1170-1179` | NOVALUE `The entity has no material` · VALUE `The material names and categories of "{actual}" does not match the requirement` · PROHIBITED `The material should not have met the requirement` |

`{actual}` is a Python repr. A DATATYPE `actual` is the CamelCase class (`IfcMassMeasure`) and
`dataType` is as written. For Material VALUE it is a `set` repr, whose order depends on the hash
seed (A28).

---

## 7. Open ambiguities

Appended to `docs/ids/ambiguities.md` as A14–A28 (IfcTester reading, or a stated deviation) and
D5–D10 (text explicit, no case, IfcTester diverges). The ones that change behaviour against
IfcTester on real files, and so need Ed's eye: **A14** (dataType checked against the source
property), **A16** (unsupported = absent), **A26** (unresolved unit = per-element failure), and
**D8** (range restriction = all values).

---

## 8. Case inventory (174; outside the §1–7 word budget)

Rule ids refer to §1–§4. The IfcTester column comes from the xfail registry plus source reading.
`inv` = `invalid-`, and `fail` or `IdsInvalidError` is accepted. No case needs PartOf or another slice.
`pass-non_ascii_characters_are_treated_without_encoding` depends on the STEP string decoder
(`\X2\`), which is core parser work, not a slice.

### property/ (82)

| Case | Exp | Rule pinned | IfcTester |
|---|---|---|---|
| `fail-a_logical_unknown_is_considered_false_and_will_not_pass` | fail | P6 LOGICAL .U. = absent | agrees |
| `fail-a_prohibited_facet_returns_the_opposite_of_a_required_facet` | fail | P15 prohibited, prop present | agrees |
| `fail-all_matching_properties_must_satisfy_requirements_3_3` | fail | P5 Foobaz=y fails value x | agrees |
| `fail-all_matching_property_sets_must_satisfy_requirements_2_3` | fail | P4 Foo_Baz lacks Foo | agrees |
| `fail-an_empty_string_is_considered_false_and_will_not_pass` | fail | P6 '' = absent | agrees |
| `fail-any_matching_value_in_a_bounded_property_will_pass_4_4` | fail | P10 2 ∉ {5,1,3} (not a range test) | agrees |
| `fail-any_matching_value_in_a_list_property_will_pass_3_3` | fail | P9 Z ∉ list | agrees |
| `fail-any_matching_value_in_a_table_property_will_pass_3_3` | fail | P11 Y ∉ IFCLABEL column | agrees |
| `fail-booleans_must_be_specified_as_lowercase_strings_1_3` | fail | P8 true ≠ .F. | agrees |
| `fail-complex_properties_are_not_supported_1_2` | fail | P12 complex quantity named Foo | agrees |
| `fail-complex_properties_are_not_supported_2_2` | fail | P12/P1 complex quantity is not a pset | agrees |
| `fail-dates_are_treated_as_strings_2_2` | fail | P17 '2022-01-01+00:00' ≠ '2022-01-01' | agrees |
| `fail-durations_are_treated_as_strings_1_2` | fail | P17 P2D ≠ PT16H | agrees |
| `fail-elements_with_a_matching_pset_but_no_property_also_fail` | fail | P1 prop absent | agrees |
| `fail-elements_with_no_properties_always_fail` | fail | P1 pset absent | agrees |
| `fail-ids_does_not_handle_string_truncation_such_as_for_identifiers` | fail | P17 exact, no truncation | agrees |
| `fail-if_multiple_properties_are_matched__all_values_must_satisfy_requirements_2_2` | fail | P5 z ∉ enum{x,y} | agrees |
| `fail-material_properties_that_are_absent_fail_under_ifc2x3` | fail | P2 IFC2X3 material, no props | agrees |
| `fail-material_properties_that_are_absent_fail_under_ifc4` | fail | P2 IFC4 material, no props | agrees |
| `fail-measures_are_used_to_specify_an_ifc_data_type_1_2` | fail | P7 IFCMASSMEASURE ≠ IFCTIMEMEASURE | agrees |
| `fail-no_matching_value_in_an_enumerated_property_will_fail_3_3` | fail | P9 NEW ∉ selected values (not the enumeration) | agrees |
| `fail-predefined_properties_are_supported_but_discouraged_2_2` | fail | P16 SWINGING ≠ SWONGING | agrees |
| `fail-project_properties_that_are_absent_fail_under_ifc2x3_via_ifcobject` | fail | P2 project, no pset | agrees |
| `fail-project_properties_that_are_absent_fail_under_ifc4_via_ifccontext` | fail | P2 project, no pset | agrees |
| `fail-properties_can_be_associated_to_relevant_object_types` | fail | P19 type w/o pset + FOOBAR fails pattern | agrees |
| `fail-properties_can_be_overriden_by_an_occurrence_2_2` | fail | P19 type evaluated on own pset (Baz) | agrees |
| `fail-properties_with_a_null_value_fail` | fail | P1/P6 Foo absent (only a null sibling) | agrees |
| `fail-quantities_must_also_match_the_appropriate_measure` | fail | P7 IfcQuantityLength ≠ IFCAREAMEASURE | agrees |
| `fail-reference_properties_are_treated_as_objects_and_not_supported` | fail | P12 reference value | agrees |
| `fail-specifying_a_value_fails_against_different_values` | fail | P8 Baz ≠ Bar | agrees |
| `fail-specifying_a_value_performs_a_case_sensitive_match_2_2` | fail | P8 bar ≠ Bar | agrees |
| `fail-unit_conversions_shall_take_place_to_ids_nominated_standard_units_1_2` | fail | P13 2 mm = 0.002 ≠ 2 | agrees |
| `invalid-booleans_must_be_specified_as_lowercase_strings_3_3` | inv | P18 FALSE | agrees (fail) |
| `invalid-integer_values_are_checked_using_type_casting_4_4` | inv | P18 42.3 on IFCINTEGER | agrees (fail) |
| `invalid-integer_values_cannot_be_stored_with_decimal_2_4` | inv | P18 42. on IFCINTEGER | **wrong** (xfail #193) |
| `invalid-integer_values_cannot_be_stored_with_decimal_3_4` | inv | P18 42.0 on IFCINTEGER | **wrong** (xfail #193) |
| `invalid-only_specifically_formatted_numbers_are_allowed_1_4` | inv | P18 42,3 | agrees (fail) |
| `invalid-only_specifically_formatted_numbers_are_allowed_2_4` | inv | P18 123,4.5 | agrees (fail) |
| `pass-a_name_check_will_match_any_property_with_any_string_value` | pass | P1 no value = presence only | agrees |
| `pass-a_name_check_will_match_any_quantity_with_any_value` | pass | P1/P7 quantity as property | agrees |
| `pass-a_number_specified_as_a_string_is_treated_as_a_string` | pass | P8 IFCLABEL '1' = '1' | agrees |
| `pass-a_property_set_to_false_is_still_considered_a_value_and_will_pass_a_name_check` | pass | P6 .F. is a value | agrees |
| `pass-a_property_set_to_true_will_pass_a_name_check` | pass | P6 .T. is a value | agrees |
| `pass-a_required_facet_checks_all_parameters_as_normal` | pass | P1 baseline | agrees |
| `pass-a_zero_duration_will_pass` | pass | P6 P0D is a value | agrees |
| `pass-all_matching_properties_must_satisfy_requirements_1_3` | pass | P5 one match | agrees |
| `pass-all_matching_properties_must_satisfy_requirements_2_3` | pass | P5 both x | agrees |
| `pass-all_matching_property_sets_must_satisfy_requirements_1_3` | pass | P4 one pset | agrees |
| `pass-all_matching_property_sets_must_satisfy_requirements_3_3` | pass | P4 both psets carry Foo | agrees |
| `pass-an_optional_facet_always_passes_regardless_of_outcome_1_2` | pass | P14 optional, present | agrees |
| `pass-an_optional_facet_always_passes_regardless_of_outcome_2_2` | pass | P14 optional, absent | agrees |
| `pass-any_matching_value_in_a_bounded_property_will_pass_1_4` | pass | P10+P13 lower 1000 mm = 1 | agrees |
| `pass-any_matching_value_in_a_bounded_property_will_pass_2_4` | pass | P10+P13 upper 5000 mm = 5 | agrees |
| `pass-any_matching_value_in_a_bounded_property_will_pass_3_4` | pass | P10+P13 setpoint 3000 mm = 3 | agrees |
| `pass-any_matching_value_in_a_list_property_will_pass_1_3` | pass | P9 X ∈ list | agrees |
| `pass-any_matching_value_in_a_list_property_will_pass_2_3` | pass | P9 Y ∈ list | agrees |
| `pass-any_matching_value_in_a_table_property_will_pass_1_3` | pass | P11 defining column | agrees |
| `pass-any_matching_value_in_a_table_property_will_pass_2_3` | pass | P11+P13 defined column 1000 mm = 1 | agrees |
| `pass-any_matching_value_in_an_enumerated_property_will_pass_1_3` | pass | P9 EXISTING ∈ values | agrees |
| `pass-any_matching_value_in_an_enumerated_property_will_pass_2_3` | pass | P9 DEMOLISH ∈ values | agrees |
| `pass-booleans_must_be_specified_as_lowercase_strings_2_3` | pass | P8 false = .F. | agrees |
| `pass-dates_are_treated_as_strings_1_2` | pass | P17 date string equal | agrees |
| `pass-durations_are_treated_as_strings_2_2` | pass | P17 duration string equal | agrees |
| `pass-if_multiple_properties_are_matched__all_values_must_satisfy_requirements_1_2` | pass | P5 x,y ∈ enum | agrees |
| `pass-integer_values_are_checked_using_type_casting_1_4` | pass | P8 42 = IFCINTEGER(42) | agrees |
| `pass-material_properties_are_supported_under_ifc2x3_via_extendedmaterialproperties` | pass | P2 IfcExtendedMaterialProperties | **wrong** (xfail #193) |
| `pass-material_properties_are_supported_under_ifc4_via_ifcmaterialproperties` | pass | P2 IfcMaterialProperties | agrees |
| `pass-measures_are_used_to_specify_an_ifc_data_type_2_2` | pass | P7+P13 IFCTIMEMEASURE, s | agrees |
| `pass-non_ascii_characters_are_treated_without_encoding` | pass | P8 STEP \X2\ + '' decoded before compare | agrees |
| `pass-only_specifically_formatted_numbers_are_allowed_3_4` | pass | P8 1.2345e3 | agrees |
| `pass-only_specifically_formatted_numbers_are_allowed_4_4` | pass | P8 1.2345E3 | agrees |
| `pass-predefined_properties_are_supported_but_discouraged_1_2` | pass | P16 IfcDoorPanelProperties.PanelOperation | agrees |
| `pass-project_properties_are_supported_under_ifc2x3_via_ifcobject` | pass | P2 IfcProject psets | agrees |
| `pass-project_properties_are_supported_under_ifc4_via_ifccontext` | pass | P2 IfcProject psets | agrees |
| `pass-properties_can_be_inherited_from_the_type_1_2` | pass | P3 occurrence inherits | agrees |
| `pass-properties_can_be_inherited_from_the_type_2_2` | pass | P19 type has own pset | agrees |
| `pass-properties_can_be_overriden_by_an_occurrence_1_2` | pass | P3 occurrence Bar wins over type Baz | agrees |
| `pass-real_values_are_checked_using_type_casting_1_3` | pass | P8 42 = 42. | agrees |
| `pass-real_values_are_checked_using_type_casting_2_3` | pass | P8 42.0 = 42. | agrees |
| `pass-real_values_are_checked_using_type_casting_3_3` | pass | P8 42.3 = 42.3 | agrees |
| `pass-specifying_a_value_performs_a_case_sensitive_match_1_2` | pass | P8 Bar = Bar | agrees |
| `pass-unit_conversions_shall_take_place_to_ids_nominated_standard_units_2_2` | pass | P13 2000 mm = 2 | agrees |

### classification/ (27)

| Case | Exp | Rule pinned | IfcTester |
|---|---|---|---|
| `fail-a_classification_facet_with_no_data_matches_any_classification_1_2` | fail | C1 wall unclassified | agrees |
| `fail-a_prohibited_classification_reference_returns_the_opposite_of_a_required_facet` | fail | C8 prohibited, 1/Foobar present | agrees |
| `fail-a_prohibited_facet_returns_the_opposite_of_a_required_facet` | fail | C8 prohibited, system present | agrees |
| `fail-a_required_classification_system_fails_if_no_match` | fail | C2 Foobar ≠ Foobar1 | agrees |
| `fail-an_optional_classification_value_fails_if_no_match` | fail | C7 optional, direct IfcClassification present, value None | agrees |
| `fail-both_system_and_value_must_match__all__not_any__if_specified_2_2` | fail | C4 11 ≠ 1 (no prefix match) | agrees |
| `fail-occurrences_override_the_type_classification_per_system_2_3` | fail | C6 22 on neither occ nor type | agrees |
| `fail-restrictions_can_be_used_for_systems_1_2` | fail | C1 wall unclassified | agrees |
| `fail-restrictions_can_be_used_for_values_3_3` | fail | C4/C5 pattern 1.* vs {22,2} | agrees |
| `fail-systems_should_match_exactly_2_5` | fail | C1 wall unclassified | agrees |
| `pass-a_classification_facet_with_no_data_matches_any_classification_2_2` | pass | C2 system only | agrees |
| `pass-a_required_facet_checks_all_parameters_as_normal` | pass | C2 baseline | agrees |
| `pass-an_optional_classification_value_passes_if_null` | pass | C7 optional, wall unclassified | agrees |
| `pass-an_optional_classification_value_passes_if_specified` | pass | C7 optional, wall unclassified | agrees |
| `pass-both_system_and_value_must_match__all__not_any__if_specified_1_2` | pass | C4 1/Foobar | agrees |
| `pass-non_rooted_resources_that_have_external_classification_references_should_also_pass` | pass | C9 IfcMaterial via IfcExternalReferenceRelationship | agrees |
| `pass-occurrences_override_the_type_classification_per_system_1_3` | pass | C6 occurrence ref 11 | agrees |
| `pass-occurrences_override_the_type_classification_per_system_3_3` | pass | C6 type ref X (other system) inherited | agrees |
| `pass-restrictions_can_be_used_for_systems_2_2` | pass | C2 pattern Foo.* | agrees |
| `pass-restrictions_can_be_used_for_values_1_3` | pass | C4 pattern 1.* vs 1 | agrees |
| `pass-restrictions_can_be_used_for_values_2_3` | pass | C4 pattern 1.* vs 11 | agrees |
| `pass-systems_should_match_exactly_1_5` | pass | C3 IfcProject, direct IfcClassification | agrees |
| `pass-systems_should_match_exactly_3_5` | pass | C2 slab | agrees |
| `pass-systems_should_match_exactly_4_5` | pass | C2 column | agrees |
| `pass-systems_should_match_exactly_5_5` | pass | C2/C5 beam, 2-level chain to Foobar | agrees |
| `pass-values_match_subreferences_if_full_classifications_are_used__e_g__ef_25_10_should_match_ef_25_10_25__ef_25_10_30__etc_` | pass | C5 parent 2 of leaf 22 counts | agrees |
| `pass-values_should_match_exactly_if_lightweight_classifications_are_used` | pass | C4 1 = 1 | agrees |

### material/ (29)

| Case | Exp | Rule pinned | IfcTester |
|---|---|---|---|
| `fail-a_constituent_set_with_no_data_will_fail_a_value_check` | fail | M2 empty constituent set, value Foo | agrees |
| `fail-a_material_list_with_no_data_will_fail_a_value_check` | fail | M2 list {Concrete,CONCRETE} vs Foo | agrees |
| `fail-a_prohibited_facet_returns_the_opposite_of_a_required_facet` | fail | M6 prohibited, material present | agrees |
| `fail-an_optional_material_fails_if_no_value_matches` | fail | M5 optional, present, No match | agrees |
| `fail-elements_without_a_material_always_fail` | fail | M1 no material | agrees |
| `fail-material_with_no_data_will_fail_a_value_check` | fail | M2 Unnamed ≠ Foo | agrees |
| `pass-a_layer_set_name_will_pass_a_value_check` | pass | M2 LayerSetName | agrees |
| `pass-a_material_category_may_pass_the_value_check` | pass | M2 IfcMaterial.Category | agrees |
| `pass-a_material_name_may_pass_the_value_check` | pass | M2 IfcMaterial.Name | agrees |
| `pass-a_required_facet_checks_all_parameters_as_normal` | pass | M1 baseline | agrees |
| `pass-an_optional_material_passes_if_null` | pass | M5 optional, absent | agrees |
| `pass-an_optional_material_passes_if_specified` | pass | M5 optional, match | agrees |
| `pass-any_constituent_category_in_a_constituent_set_will_pass_a_value_check` | pass | M2 constituent Category | agrees |
| `pass-any_constituent_name_in_a_constituent_set_will_pass_a_value_check` | pass | M2 constituent Name | agrees |
| `pass-any_layer_category_in_a_layer_set_will_pass_a_value_check` | pass | M2 layer Category | agrees |
| `pass-any_layer_name_in_a_layer_set_will_pass_a_value_check` | pass | M2 layer Name | agrees |
| `pass-any_material_category_in_a_constituent_set_will_pass_a_value_check` | pass | M2 constituent material Category | agrees |
| `pass-any_material_category_in_a_layer_set_will_pass_a_value_check` | pass | M2 layer material Category | agrees |
| `pass-any_material_category_in_a_list_will_pass_a_value_check` | pass | M2 list member Category | agrees |
| `pass-any_material_category_in_a_profile_set_will_pass_a_value_check` | pass | M2 profile material Category | agrees |
| `pass-any_material_name_in_a_constituent_set_will_pass_a_value_check` | pass | M2 constituent material Name | agrees |
| `pass-any_material_name_in_a_layer_set_will_pass_a_value_check` | pass | M2 layer material Name | agrees |
| `pass-any_material_name_in_a_list_will_pass_a_value_check` | pass | M2 list member Name | agrees |
| `pass-any_material_name_in_a_profile_set_will_pass_a_value_check` | pass | M2 profile material Name | agrees |
| `pass-any_profile_category_in_a_profile_set_will_pass_a_value_check` | pass | M2 profile Category | agrees |
| `pass-any_profile_name_in_a_profile_set_will_pass_a_value_check` | pass | M2 profile Name | agrees |
| `pass-elements_with_any_material_will_pass_an_empty_material_facet` | pass | M1 presence | agrees |
| `pass-occurrences_can_inherit_materials_from_their_types` | pass | M3 type material inherited | agrees |
| `pass-occurrences_can_override_materials_from_their_types` | pass | M3 occurrence Foo (union would also pass) | agrees |

### tolerance/ (36)

| Case | Exp | Rule pinned | IfcTester |
|---|---|---|---|
| `fail-comparison_tolerance_for_floating_point_negative_high_number_lower_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_negative_high_number_upper_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_negative_low_number_lower_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_negative_low_number_upper_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_negative_one_lower_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_negative_one_upper_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_one_lower_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_one_upper_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_positive_high_number_lower_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_positive_high_number_upper_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_positive_low_number_lower_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_positive_low_number_upper_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_range_greater_than_zero_exclusive` | fail | T2 bound exact, no tolerance | agrees |
| `fail-comparison_tolerance_for_floating_point_range_greater_than_zero_inclusive` | fail | T2 bound exact, no tolerance | agrees |
| `fail-comparison_tolerance_for_floating_point_range_lower_than_zero_exclusive` | fail | T2 bound exact, no tolerance | agrees |
| `fail-comparison_tolerance_for_floating_point_range_lower_than_zero_inclusive` | fail | T2 bound exact, no tolerance | agrees |
| `fail-comparison_tolerance_for_floating_point_zero_lower_bound` | fail | T1 point tolerance, just outside | agrees |
| `fail-comparison_tolerance_for_floating_point_zero_upper_bound` | fail | T1 point tolerance, just outside | agrees |
| `pass-comparison_tolerance_for_floating_point_negative_high_number_lower_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_negative_high_number_upper_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_negative_low_number_lower_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_negative_low_number_upper_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_negative_one_lower_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_negative_one_upper_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_one_lower_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_one_upper_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_positive_high_number_lower_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_positive_high_number_upper_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_positive_low_number_lower_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_positive_low_number_upper_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_range_greater_than_zero_exclusive` | pass | T2 bound exact, no tolerance | agrees |
| `pass-comparison_tolerance_for_floating_point_range_greater_than_zero_inclusive` | pass | T2 bound exact, no tolerance | agrees |
| `pass-comparison_tolerance_for_floating_point_range_lower_than_zero_exclusive` | pass | T2 bound exact, no tolerance | agrees |
| `pass-comparison_tolerance_for_floating_point_range_lower_than_zero_inclusive` | pass | T2 bound exact, no tolerance | agrees |
| `pass-comparison_tolerance_for_floating_point_zero_lower_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
| `pass-comparison_tolerance_for_floating_point_zero_upper_bound` | pass | T1 point tolerance, inside, on edge | **wrong** (xfail #193) |
