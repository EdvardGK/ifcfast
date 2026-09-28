"""GH #192 slice 4: `IdsReport.to_ifctester_json()`, `ifcfast ids`, and the
`validate_ids` MCP tool, end to end through the wheel on the bundled
minimal fixture (one IfcWall, #30). Word-for-word parity with IfcTester is
the oracle's job (`tests/oracle/ids_json_parity.py`); this pins the shape
and the surfaces.
"""
from __future__ import annotations

import json

import pytest

import ifcfast
from ifcfast.cli import EXIT_IDS_NOT_SATISFIED, main

IFC = str(ifcfast.example_path())


def _ids(specs: str) -> str:
    return (
        '<?xml version="1.0" encoding="utf-8"?>'
        '<ids xmlns="http://standards.buildingsmart.org/IDS" '
        'xmlns:xs="http://www.w3.org/2001/XMLSchema">'
        f"<info><title>T</title></info><specifications>{specs}</specifications></ids>"
    )


WALLS = (
    '<specification name="walls exist" ifcVersion="IFC4"><applicability minOccurs="1" '
    'maxOccurs="unbounded"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity>'
    "</applicability></specification>"
)
WALL_TAG = (
    '<specification name="walls tagged" ifcVersion="IFC4"><applicability minOccurs="1" '
    'maxOccurs="unbounded"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity>'
    "</applicability><requirements><attribute><name><simpleValue>Name</simpleValue></name>"
    "<value><simpleValue>no-such-name</simpleValue></value></attribute></requirements>"
    "</specification>"
)


def test_to_ifctester_json_shape():
    rep = ifcfast.validate_ids(_ids(WALLS + WALL_TAG), IFC)
    assert not rep.ok
    specs, elements, failures = rep  # still a 3-tuple
    doc = rep.to_ifctester_json()
    assert doc["title"] == "T"
    assert doc["filepath"] == IFC
    assert doc["status"] is False
    assert doc["total_specifications"] == 2 and doc["total_specifications_pass"] == 1
    s0, s1 = doc["specifications"]
    assert s0["status"] is True and s0["applicability"] == ["All IFCWALL data"]
    assert s0["requirements"] == [] and s0["ifcfast_status"] == "pass"
    r = s1["requirements"][0]
    assert r["facet_type"] == "Attribute" and r["label"] == "Name" and r["value"] == "no-such-name"
    assert r["metadata"] == {
        "name": {"simpleValue": "Name"},
        "value": {"simpleValue": "no-such-name"},
        "@cardinality": "required",
    }
    (fe,) = r["failed_entities"]
    assert fe["id"] == 30 and fe["class"] == "IfcWall"
    assert fe["reason"].startswith('The attribute value "') and fe["reason"].endswith(
        '" does not match the requirement'
    )
    assert fe["ifcfast_reason_code"] == "ATTR_VALUE_MISMATCH"
    assert fe["element"].startswith("#30=IfcWall(")
    assert r["percent_pass"] == 0 and s1["percent_checks_pass"] == 0


def test_to_ifctester_json_multi_document_needs_index():
    m = ifcfast.open(IFC, use_cache=False, write_cache=False)
    rep = m.validate_ids([_ids(WALLS), _ids(WALL_TAG)])
    with pytest.raises(ValueError, match="ids_index"):
        rep.to_ifctester_json()
    assert rep.to_ifctester_json(ids_index=0)["status"] is True
    assert rep.to_ifctester_json(ids_index=1)["status"] is False
    with pytest.raises(IndexError):
        rep.to_ifctester_json(ids_index=2)


def test_to_ifctester_json_bytes_input_has_no_filepath():
    with open(IFC, "rb") as fh:
        rep = ifcfast.validate_ids(_ids(WALLS), fh.read())
    doc = rep.to_ifctester_json()
    assert doc["filepath"] is None and doc["filename"] is None


def test_cli_ids_exit_codes_and_outputs(tmp_path, capsys):
    ok = tmp_path / "ok.ids"
    ok.write_text(_ids(WALLS), encoding="utf-8")
    bad = tmp_path / "bad.ids"
    bad.write_text(_ids(WALL_TAG), encoding="utf-8")
    assert main(["ids", str(ok), IFC]) == 0
    assert "[PASS] (1/1) walls exist" in capsys.readouterr().out

    out = tmp_path / "r.json"
    pq = tmp_path / "pq"
    rc = main(["ids", str(ok), str(bad), IFC, "--json", str(out), "--parquet", str(pq)])
    assert rc == EXIT_IDS_NOT_SATISFIED == 3
    docs = json.loads(out.read_text(encoding="utf-8"))
    assert isinstance(docs, list) and [d["status"] for d in docs] == [True, False]
    assert (pq / "failures.parquet").is_file()
    assert "[FAIL]" in capsys.readouterr().out  # text still printed with --json OUT

    assert main(["ids", str(ok), IFC, "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["status"] is True

    broken = tmp_path / "broken.ids"
    broken.write_text("<ids>nope</ids>", encoding="utf-8")
    assert main(["ids", str(broken), IFC]) == 1
    assert "invalid IDS" in capsys.readouterr().err


def test_mcp_validate_ids_tool():
    pytest.importorskip("mcp")
    from ifcfast import mcp_server

    res = mcp_server.validate_ids("example", _ids(WALLS + WALL_TAG), failures=5)
    json.dumps(res)  # JSON-safe for the transport
    assert res["ok"] is False and res["n_failures"] == 1
    assert [s["status"] for s in res["specs"]] == ["pass", "fail"]
    assert res["failures"][0]["reason_code"] == "ATTR_VALUE_MISMATCH"
    assert mcp_server.validate_ids("example", [_ids(WALLS)], failures=0)["failures"] == []
