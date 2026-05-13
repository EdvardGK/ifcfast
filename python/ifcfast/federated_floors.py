"""Federated floor synthesis — cluster storeys across discipline models.

Solibri's `Federated Floor` column synthesises one label per Z elevation
across all open discipline models in a project (e.g. ARK + RIB + RIV + RIE
all contributing to one shared label). The parser by default reports raw
`IfcBuildingStorey.Name` per model, which leaves a gap in projects where
discipline authors name the same physical level differently.

This module is **project-agnostic**. It ships only:

  - The generic 1-D elevation clusterer
  - A pluggable rule protocol (callable that maps `ClusterInfo -> label`)
  - The identity `default_rule` (most-common-name)
  - A `make_prefix_rule(prefix, overrides, idempotent_labels, ...)`
    factory that covers the common "add a building prefix + a few storey
    aliases" pattern
  - A YAML loader so projects ship their config separately

Project-specific tables (the LBK Building C name aliases, the C- prefix
convention, etc.) do **not** live here. They live with the project, as
config files passed in via `--project-config` or loaded by a CLI wrapper.

Public API::

    from ifcfast.federated_floors import (
        StoreyInfo,
        ClusterInfo,
        synthesise_federated_floors,
        default_rule,
        make_prefix_rule,
        load_rule_from_yaml,
        register_rule,
        get_rule,
    )

    # Quickest path — identity rule for files where storey names already match
    out = synthesise_federated_floors(storeys)

    # Common path — building-prefix rule with overrides for the basement zone
    rule = make_prefix_rule(
        prefix="C - ",
        overrides={"Plan U1": "Hav", "C - U1": "Hav"},
        idempotent_labels=["Hav"],
    )
    out = synthesise_federated_floors(storeys, rule=rule)

    # Config-driven path — load the rule from a project YAML
    rule = load_rule_from_yaml(Path("data/projects/lbk-building-c.yaml"))
    out = synthesise_federated_floors(storeys, rule=rule)
"""

from __future__ import annotations

from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterable, Optional


# ----------------------------------------------------------------------
# Data structures
# ----------------------------------------------------------------------


@dataclass(frozen=True)
class StoreyInfo:
    """One storey from one discipline model.

    `elevation_mm` is in millimetres — IFC files are usually authored in
    mm, and `IfcBuildingStorey.Elevation` is already in model units.
    Convert before passing if your file is in metres.
    """

    model_name: str
    storey_name: str
    elevation_mm: float
    storey_guid: Optional[str] = None


@dataclass
class ClusterInfo:
    """A cluster of storeys sharing an elevation band.

    Passed to project rules so they can decide the federated label.
    """

    representative_elevation_mm: float
    members: list[StoreyInfo] = field(default_factory=list)

    @property
    def names(self) -> list[str]:
        return [m.storey_name for m in self.members]

    @property
    def most_common_name(self) -> str:
        """Most frequent storey name in the cluster. Ties broken by
        insertion order (i.e. the first model seen wins)."""
        counts = Counter(self.names)
        if not counts:
            return ""
        # Counter.most_common is order-stable for ties in CPython 3.7+
        return counts.most_common(1)[0][0]


# ----------------------------------------------------------------------
# Clustering
# ----------------------------------------------------------------------


def _cluster_by_elevation(
    storeys: list[StoreyInfo], tolerance_mm: float
) -> list[ClusterInfo]:
    """Greedy 1-D clustering on elevation.

    Sort by elevation; start a new cluster whenever the next storey is
    more than `tolerance_mm` from the current cluster's mean elevation.
    Mean is recomputed after each addition so a cluster doesn't drift
    past tolerance even if members are added one at a time.

    O(n log n) from the sort; clustering itself is O(n).
    """
    if not storeys:
        return []

    ordered = sorted(storeys, key=lambda s: s.elevation_mm)
    clusters: list[ClusterInfo] = []
    current = ClusterInfo(representative_elevation_mm=ordered[0].elevation_mm)
    current.members.append(ordered[0])

    for s in ordered[1:]:
        # Compare against the current cluster's running mean.
        mean = sum(m.elevation_mm for m in current.members) / len(current.members)
        if abs(s.elevation_mm - mean) <= tolerance_mm:
            current.members.append(s)
        else:
            # Finalise current with its mean as representative.
            current.representative_elevation_mm = mean
            clusters.append(current)
            current = ClusterInfo(representative_elevation_mm=s.elevation_mm)
            current.members.append(s)

    # Finalise the last cluster.
    current.representative_elevation_mm = sum(
        m.elevation_mm for m in current.members
    ) / len(current.members)
    clusters.append(current)
    return clusters


# ----------------------------------------------------------------------
# Generic rules
# ----------------------------------------------------------------------


def default_rule(cluster: ClusterInfo) -> str:
    """Identity rule — return the most common storey name in the cluster."""
    return cluster.most_common_name


def drop_leading_zero(name: str) -> str:
    """`Plan 01` -> `Plan 1`, `Plan 010` -> `Plan 10`. Leaves words alone.

    Also handles alphanumeric tokens like `U01` -> `U1`. This is generic
    "human written number with optional leading zero" cleanup.
    """
    out: list[str] = []
    parts = name.split()
    for p in parts:
        if p.isdigit():
            out.append(str(int(p)))
        elif len(p) >= 2 and p[0] == "0" and p[1:].isdigit():
            # bare numeric token with leading zero (rare; covered by isdigit)
            out.append(str(int(p)))
        elif (
            len(p) >= 2
            and p[0].isalpha()
            and any(c.isdigit() for c in p)
        ):
            # token like "U01" -> "U1"
            i = 0
            while i < len(p) and p[i].isalpha():
                i += 1
            prefix, num = p[:i], p[i:]
            if num.isdigit():
                out.append(f"{prefix}{int(num)}")
            else:
                out.append(p)
        else:
            out.append(p)
    return " ".join(out)


def make_prefix_rule(
    *,
    prefix: str = "",
    overrides: Optional[dict[str, str]] = None,
    idempotent_labels: Optional[Iterable[str]] = None,
    apply_drop_leading_zero: bool = True,
) -> Callable[[ClusterInfo], str]:
    """Build a project rule from a small config bundle.

    Resolution order, applied per cluster:

    1. If **any** member name (or its drop-leading-zero normalisation) is
       in `overrides`, return the override value. Surfaces ITO-style
       federation where different discipline authors use different names
       for the same physical level.
    2. Take the most-common name in the cluster; normalise it.
    3. If the normalised name is already in `idempotent_labels` or
       already starts with `prefix`, return it as-is (don't double-prefix).
    4. Return `prefix + normalised`.

    Args:
        prefix: building / zone label prefix, e.g. `"C - "`. Empty string
            means identity-of-most-common-name.
        overrides: explicit storey-name -> federated-label aliases. Keys
            are checked against each cluster member's raw and normalised
            name; the first match wins.
        idempotent_labels: final-form labels that the rule must not
            prefix again. Typical: a list like `["Hav"]` for sea-level /
            ground labels that don't follow the prefix scheme.
        apply_drop_leading_zero: turn `Plan 01` -> `Plan 1` before
            prefixing. Default True since most authoring tools emit
            zero-padded numbers and most ITO conventions strip them.
    """
    overrides = dict(overrides or {})
    idempotents = set(idempotent_labels or ())

    def _normalise(name: str) -> str:
        return drop_leading_zero(name) if apply_drop_leading_zero else name

    def rule(cluster: ClusterInfo) -> str:
        # 1. Any cluster member matching an override resolves the cluster.
        for member in cluster.names:
            if member in overrides:
                return overrides[member]
            n = _normalise(member)
            if n in overrides:
                return overrides[n]

        # 2. Most-common name, normalised.
        base = cluster.most_common_name
        normalised = _normalise(base)

        # 3. Idempotency: don't re-prefix something already in final form.
        if normalised in idempotents:
            return normalised
        if prefix and normalised.startswith(prefix):
            return normalised

        # 4. Apply the prefix.
        return f"{prefix}{normalised}"

    return rule


# ----------------------------------------------------------------------
# YAML loader for project configs
# ----------------------------------------------------------------------


def load_rule_from_yaml(path: Path | str) -> Callable[[ClusterInfo], str]:
    """Build a rule callable from a YAML project config.

    Expected schema (all keys optional unless noted)::

        prefix: "C - "
        overrides:
          Plan U1: Hav
          C - U1: Hav
        idempotent_labels:
          - Hav
        apply_drop_leading_zero: true

    Returns the same shape as `make_prefix_rule(**config)`.
    """
    import yaml  # local import — yaml isn't a hard dep elsewhere

    p = Path(path).expanduser().resolve()
    with p.open("r", encoding="utf-8") as f:
        cfg: dict[str, Any] = yaml.safe_load(f) or {}

    return make_prefix_rule(
        prefix=cfg.get("prefix", ""),
        overrides=cfg.get("overrides"),
        idempotent_labels=cfg.get("idempotent_labels"),
        apply_drop_leading_zero=cfg.get("apply_drop_leading_zero", True),
    )


# ----------------------------------------------------------------------
# Rule registry (kept for callers that prefer named-project lookup)
# ----------------------------------------------------------------------


_RULES: dict[str, Callable[[ClusterInfo], str]] = {
    "default": default_rule,
}


def register_rule(project: str, rule: Callable[[ClusterInfo], str]) -> None:
    """Register a project-specific rule under `project`. Overwrites if exists."""
    _RULES[project] = rule


def get_rule(project: str) -> Callable[[ClusterInfo], str]:
    """Look up a registered project rule. Raises KeyError if not registered.

    Use `default_rule` directly for the identity behaviour, or
    `load_rule_from_yaml(path)` to build a rule from a config file.
    """
    try:
        return _RULES[project]
    except KeyError as exc:
        raise KeyError(
            f"No federated-floor rule registered for project {project!r}. "
            f"Known: {sorted(_RULES)}"
        ) from exc


def known_rules() -> list[str]:
    return sorted(_RULES)


# ----------------------------------------------------------------------
# Public entry point
# ----------------------------------------------------------------------


def synthesise_federated_floors(
    storeys: list[StoreyInfo],
    *,
    tolerance_mm: float = 100.0,
    rule: Optional[Callable[[ClusterInfo], str]] = None,
) -> dict[tuple[str, str], str]:
    """Cluster `storeys` by elevation and apply a rule.

    Args:
        storeys: list of `StoreyInfo` across one or more discipline models.
        tolerance_mm: max elevation spread (mm) for storeys to merge into
            one cluster. Default 100 mm matches Solibri's federation
            tolerance observed on the LBK project; other projects may
            want a tighter or looser band.
        rule: callable that maps a `ClusterInfo` to a federated label.
            Defaults to `default_rule` (most common name in the cluster).
            Use `make_prefix_rule(...)` or `load_rule_from_yaml(...)` for
            project-specific behaviour.

    Returns:
        `{(model_name, storey_name): federated_label}` for every input
        storey. If the same (model, storey) pair appears multiple times
        in `storeys` it will only have one entry — the last-clustered one
        wins (clusters are stable so this is deterministic).
    """
    if rule is None:
        rule = default_rule

    clusters = _cluster_by_elevation(storeys, tolerance_mm)
    out: dict[tuple[str, str], str] = {}
    for cluster in clusters:
        label = rule(cluster)
        for member in cluster.members:
            out[(member.model_name, member.storey_name)] = label
    return out


__all__ = [
    "StoreyInfo",
    "ClusterInfo",
    "synthesise_federated_floors",
    "default_rule",
    "make_prefix_rule",
    "load_rule_from_yaml",
    "drop_leading_zero",
    "register_rule",
    "get_rule",
    "known_rules",
]
