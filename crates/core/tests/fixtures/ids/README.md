# IDS conformance fixtures (third-party, verbatim)

These 27 `.ids` files are **unmodified copies** of buildingSMART IDS
conformance test cases, from
<https://github.com/buildingSMART/IDS/tree/a67047736aa93586d723329fce3aab1b9ac056af/Documentation/ImplementersDocumentation/TestCases>
(commit `a67047736aa93586d723329fce3aab1b9ac056af`, same sha as
`tests/oracle/ids_testcases.lock`; every file's sha256 matches the lock).

Licence: (c) buildingSMART International Ltd., Creative Commons
Attribution-NoDerivatives 4.0 International (CC BY-ND 4.0),
<http://creativecommons.org/licenses/by-nd/4.0/>. The files are not
modified; this README is the attribution. They are NOT covered by the
repository's MIT licence.

Only a handful are vendored, as parse fixtures for
`crates/core/tests/ids_xml.rs` (the `.ifc` halves are not needed). The
full suite is fetched at the pinned sha by `scripts/fetch_ids_testcases.py`
into `~/.cache/ifcfast/ids-testcases/<sha>/`; `ids_xml.rs` also walks
that directory (or `$IFCFAST_IDS_TESTCASES`) when it exists.

## ifcfast's own fixtures

`props_units.ifc` is NOT from the buildingSMART suite: it is written for
ifcfast (MIT, like the rest of the repository) as the fixture for
`extractors::property_graph` and `units` (GH #192 slice 2): list,
bounded, enumerated, table and complex properties, a property with its
own `Unit`, a type-inherited pset shadowed by the instance, a quantity
with its own `Unit`, an IFC2X3 `IfcExtendedMaterialProperties`, and a
unit assignment with SI, conversion-based, derived, monetary and
offset (°C) units.
