# Issue #7 — Breaking change: federated_floors module — lbk_rule / LBK_OVERRIDES removed

_Originally filed: 2026-05-11 · status at close: CLOSED_

_Imported from `EdvardGK/ifc-workbench#7` when ifcfast was extracted as a standalone repo._

---

Heads-up on a breaking change in `bc1076d` (on `fastparse-v3-native-rust-tier1`).

## What's gone

`ifc_workbench/fastparse/federated_floors.py` previously shipped:

```python
from ifc_workbench.fastparse.federated_floors import lbk_rule, LBK_OVERRIDES
register_rule("LBK", lbk_rule)   # registered at import
```

These are removed. The module is now project-agnostic — LBK was scaffolding that grew during validation, and Skiplum Building C has no business in a parser meant for any IFC.

## What replaces it

```python
from ifc_workbench.fastparse.federated_floors import (
    make_prefix_rule,       # build a rule from inline config
    load_rule_from_yaml,    # build a rule from a project YAML
    default_rule,           # identity / most-common-name
    drop_leading_zero,      # the generic normalisation helper
)

# Inline equivalent of the old lbk_rule:
rule = make_prefix_rule(
    prefix="C - ",
    overrides={"Plan U1": "Hav", "Grunn U1": "Hav", "C - U1": "Hav", ...},
    idempotent_labels=["Hav"],
    apply_drop_leading_zero=True,
)

# Or load from the shipped project YAML:
rule = load_rule_from_yaml(Path("data/projects/lbk-building-c.yaml"))

# Use either with the synthesizer:
out = synthesise_federated_floors(storeys, rule=rule)
```

## What also changed

`scripts/validate_against_ito.py`:
- `--project LBK`  →  `--project-config data/projects/lbk-building-c.yaml`
- Without `--project-config`, identity rule is used (storey names pass through unchanged; federation still happens via elevation clustering)

The full migration is documented in `bc1076d` commit message.

## Why an issue rather than just a comment

Wanted this discoverable for anyone (including future-you) who searches the repo history for "LBK_OVERRIDES" or "lbk_rule" wondering what happened to them.

Closing this immediately — informational only.
