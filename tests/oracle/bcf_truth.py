"""Solibri BCF exports as clash ground truth.

A Solibri coordination round is exported as a BCF 2.x zip: one folder per
topic, each holding ``markup.bcf`` (Header/File model refs with the models'
*internal* STEP timestamps, Topic metadata) and one or more viewpoints
(``*.bcfv``) whose ``Components/Selection`` lists the clashing elements'
IfcGuids.

Truth semantics used by :mod:`tests.oracle.clash_sweep`:

- a viewpoint whose Selection has exactly TWO IfcGuids is a **clean pair**
  — the two elements Solibri reported as clashing;
- a topic whose viewpoints select more than two elements is a **grouped
  issue** — matched if ANY unordered pair from its selection set clashes.

Solibri reports are *triaged subsets* of the full checking result, so BCF
truth supports a recall gate (everything Solibri reported must be found),
never a precision gate.
"""

from __future__ import annotations

import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from xml.etree import ElementTree as ET

Pair = frozenset  # frozenset of exactly two guids


@dataclass
class Topic:
    topic_guid: str
    title: str
    status: str
    topic_type: str
    creation_date: str
    description: str
    viewpoint_guids: list[list[str]] = field(default_factory=list)

    @property
    def rule(self) -> str:
        """Solibri rule tag = first line of the description.

        TMK exports carry the checking-rule name there ("10.1. RIE -
        RIVv", "RIVv", "2.3. ARK Div - RIE", or free text for manual
        comments). Recall must be judged per rule: only clash rules
        imply geometric contact; clearance rules imply a tolerance
        band; free-text topics are human comments, not engine truth.
        """
        return (self.description or "").splitlines()[0].strip() if self.description else ""

    @property
    def selection_guids(self) -> set[str]:
        return {g for vp in self.viewpoint_guids for g in vp}

    def clean_pairs(self) -> set[Pair]:
        """Unordered guid pairs from viewpoints that select exactly two."""
        return {frozenset(vp) for vp in self.viewpoint_guids if len(set(vp)) == 2}

    def all_candidate_pairs(self) -> set[Pair]:
        """Every unordered pair within the topic's full selection set."""
        guids = sorted(self.selection_guids)
        return {
            frozenset((a, b))
            for i, a in enumerate(guids)
            for b in guids[i + 1 :]
        }


@dataclass
class BcfTruth:
    path: Path
    topics: list[Topic]
    models: list[dict]  # {filename, date, ifc_project, occurrences}

    def clean_pairs(self) -> set[Pair]:
        out: set[Pair] = set()
        for t in self.topics:
            out |= t.clean_pairs()
        return out


def _parse_markup(xml_bytes: bytes):
    root = ET.fromstring(xml_bytes)
    header_files = []
    header = root.find("Header")
    if header is not None:
        for f in header.findall("File"):
            header_files.append(
                {
                    "filename": f.findtext("Filename"),
                    "date": f.findtext("Date"),
                    "ifc_project": f.get("IfcProject"),
                }
            )
    topic_el = root.find("Topic")
    if topic_el is None:
        raise ValueError("markup.bcf without a <Topic> element")
    topic = Topic(
        topic_guid=topic_el.get("Guid") or "",
        title=topic_el.findtext("Title") or "",
        status=topic_el.get("TopicStatus") or "",
        topic_type=topic_el.get("TopicType") or "",
        creation_date=topic_el.findtext("CreationDate") or "",
        description=topic_el.findtext("Description") or "",
    )
    viewpoint_refs = [
        vp.findtext("Viewpoint")
        for vp in root.findall("Viewpoints")
        if vp.findtext("Viewpoint")
    ]
    return header_files, topic, viewpoint_refs


def _parse_viewpoint(xml_bytes: bytes) -> list[str]:
    root = ET.fromstring(xml_bytes)
    components = root.find("Components")
    if components is None:
        return []
    selection = components.find("Selection")
    if selection is None:
        return []
    return [
        c.get("IfcGuid") for c in selection.findall("Component") if c.get("IfcGuid")
    ]


def load_bcf(path: Path | str) -> BcfTruth:
    """Parse one Solibri BCF zip into a :class:`BcfTruth`."""
    path = Path(path)
    z = zipfile.ZipFile(path)
    names = set(z.namelist())
    topic_dirs = sorted({n.split("/")[0] for n in names if n.endswith("/markup.bcf")})
    if not topic_dirs:
        raise ValueError(f"{path}: no topics (not a BCF zip?)")

    model_counts: dict[tuple, int] = {}
    topics: list[Topic] = []
    for tdir in topic_dirs:
        header_files, topic, viewpoint_refs = _parse_markup(z.read(f"{tdir}/markup.bcf"))
        for hf in header_files:
            key = (hf["filename"], hf["date"], hf["ifc_project"])
            model_counts[key] = model_counts.get(key, 0) + 1
        if not viewpoint_refs and f"{tdir}/viewpoint.bcfv" in names:
            viewpoint_refs = ["viewpoint.bcfv"]
        for vp_name in viewpoint_refs:
            vp_path = f"{tdir}/{vp_name}"
            if vp_path in names:
                guids = _parse_viewpoint(z.read(vp_path))
                if guids:
                    topic.viewpoint_guids.append(guids)
        topics.append(topic)

    models = [
        {"filename": k[0], "date": k[1], "ifc_project": k[2], "occurrences": v}
        for k, v in sorted(model_counts.items(), key=lambda kv: -kv[1])
    ]
    return BcfTruth(path=path, topics=topics, models=models)
