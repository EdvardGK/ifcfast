// Node gate for `IfcModel.validateIds` (GH #192 slice 4).
//
//   IFCFAST_WASM_FEATURES=ids crates/wasm/build.sh     # builds + runs this
//   node crates/wasm/test/ids_parity.mjs
//
// Two halves:
//
//   1. Self-contained: `tests/fixtures/minimal.ifc` against an inline IDS
//      — the report is IfcTester-shaped, a failing spec reports its
//      reason, a malformed IDS throws `IdsInvalidError: …`.
//   2. wasm = wheel: every case of the buildingSMART IDS suite through
//      `validateIds` and through the wheel's
//      `ifcfast.validate_ids(...).to_ifctester_json()`, compared after
//      dropping `date` / `filepath` / `filename` (run-dependent; the
//      browser has a file name, not a path). Errors must match by class.
//      Needs the suite (`python scripts/fetch_ids_testcases.py`) and a
//      Python with ifcfast built from this tree (`IFCFAST_PYTHON`,
//      default `<repo>/.venv/bin/python`); skipped with a note otherwise.
//
// A module built without the `ids` feature has no `validateIds`: the
// whole file is skipped with a note (that is the default site bundle).

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../..');
const pkg = path.resolve(here, '../pkg');

if (!fs.existsSync(path.join(pkg, 'ifcfast_wasm.js'))) {
  console.error(`pkg/ not built — run ${path.relative(repo, path.join(here, '../build.sh'))}`);
  process.exit(1);
}

const wasmMod = await import(path.join(pkg, 'ifcfast_wasm.js'));
await wasmMod.default({
  module_or_path: fs.readFileSync(path.join(pkg, 'ifcfast_wasm_bg.wasm')),
});
const { IfcModel } = wasmMod;

if (typeof IfcModel.prototype.validateIds !== 'function') {
  console.log('SKIP  ids_parity — module built without the `ids` feature (IFCFAST_WASM_FEATURES=ids to include it)');
  process.exit(0);
}

let failures = 0;
let checks = 0;
function check(name, ok, detail = '') {
  checks += 1;
  if (ok) {
    console.log(`PASS  ${name}`);
  } else {
    failures += 1;
    console.log(`FAIL  ${name}${detail ? `\n        ${detail}` : ''}`);
  }
}

// ---------------------------------------------------------------------
// 1. self-contained
// ---------------------------------------------------------------------
const ids = (specs) => `<?xml version="1.0" encoding="utf-8"?>
<ids xmlns="http://standards.buildingsmart.org/IDS" xmlns:xs="http://www.w3.org/2001/XMLSchema">
<info><title>wasm gate</title></info><specifications>${specs}</specifications></ids>`;
const WALL_TAG = `<specification name="walls tagged" ifcVersion="IFC4"><applicability minOccurs="1" maxOccurs="unbounded"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity></applicability><requirements><attribute><name><simpleValue>Tag</simpleValue></name><value><simpleValue>no-such-tag</simpleValue></value></attribute></requirements></specification>`;
const WALLS = `<specification name="walls exist" ifcVersion="IFC4"><applicability minOccurs="1" maxOccurs="unbounded"><entity><name><simpleValue>IFCWALL</simpleValue></name></entity></applicability></specification>`;

const minimal = fs.readFileSync(path.join(repo, 'tests/fixtures/minimal.ifc'));
const m = IfcModel.fromBytes(minimal, 'minimal.ifc');
const rep = JSON.parse(m.validateIds(ids(WALL_TAG + WALLS)));
check('report is IfcTester-shaped', ['title', 'date', 'filepath', 'filename', 'specifications', 'status',
  'total_specifications', 'percent_checks_pass'].every((k) => k in rep), Object.keys(rep).join(','));
check('filename is the fromBytes name', rep.filename === 'minimal.ifc', rep.filename);
check('two specs, overall fail', rep.specifications.length === 2 && rep.status === false);
const s0 = rep.specifications[0];
const f0 = s0.requirements[0].failed_entities[0];
check('failing attribute carries the IfcTester reason',
  f0 && /^The attribute value "(.*)" does not match the requirement$|^The attribute value "(.*)" is empty$|^The required attribute did not exist$/.test(f0.reason),
  JSON.stringify(f0));
check('failed entity names the wall', f0 && f0.class === 'IfcWall' && f0.id === 30, JSON.stringify(f0));
check('existence-only spec passes', rep.specifications[1].status === true);
let threw = '';
try {
  m.validateIds('<ids>not an ids</ids>');
} catch (e) {
  threw = String(e.message ?? e);
}
check('malformed IDS throws IdsInvalidError', threw.startsWith('IdsInvalidError'), threw);
let badOpt = '';
try {
  m.validateIds(ids(WALLS), 'sometimes');
} catch (e) {
  badOpt = String(e.message ?? e);
}
check('bad onUnsupported is refused', badOpt.includes('onUnsupported'), badOpt);

// ---------------------------------------------------------------------
// 2. wasm = wheel on the IDS suite
// ---------------------------------------------------------------------
function suiteRoot() {
  if (process.env.IFCFAST_IDS_TESTCASES) return process.env.IFCFAST_IDS_TESTCASES;
  const lock = JSON.parse(fs.readFileSync(path.join(repo, 'tests/oracle/ids_testcases.lock'), 'utf8'));
  return path.join(os.homedir(), '.cache/ifcfast/ids-testcases', lock.sha);
}

const root = suiteRoot();
const py = process.env.IFCFAST_PYTHON ?? path.join(repo, '.venv/bin/python');
const WHEEL = `
import json, sys
from pathlib import Path
import ifcfast
root = Path(sys.argv[1]); out = {}
for ids in sorted(root.glob("*/*.ids")):
    ifc = ids.with_suffix(".ifc")
    if not ifc.exists():
        continue
    key = f"{ids.parent.name}/{ids.stem}"
    try:
        out[key] = {"json": ifcfast.validate_ids(str(ids), str(ifc)).to_ifctester_json()}
    except Exception as e:
        out[key] = {"error": type(e).__name__}
json.dump(out, sys.stdout)
`;

const canon = (v) => {
  if (Array.isArray(v)) return v.map(canon);
  if (v && typeof v === 'object') {
    const o = {};
    for (const k of Object.keys(v).sort()) o[k] = canon(v[k]);
    return o;
  }
  return v;
};
const strip = (doc) => {
  const { date, filepath, filename, ...rest } = doc;
  return JSON.stringify(canon(rest));
};

if (!fs.existsSync(root)) {
  console.log(`SKIP  wasm = wheel — IDS suite not fetched (${root}); python scripts/fetch_ids_testcases.py`);
} else if (!fs.existsSync(py)) {
  console.log(`SKIP  wasm = wheel — no Python at ${py} (set IFCFAST_PYTHON)`);
} else {
  const t0 = performance.now();
  const r = spawnSync(py, ['-c', WHEEL, root], { encoding: 'utf8', maxBuffer: 1 << 30 });
  if (r.status !== 0) {
    check('wheel leg ran', false, (r.stderr || '').split('\n').slice(-4).join(' | '));
  } else {
    const wheel = JSON.parse(r.stdout);
    const t1 = performance.now();
    let same = 0;
    const bad = [];
    for (const [key, want] of Object.entries(wheel)) {
      const idsText = fs.readFileSync(path.join(root, `${key}.ids`), 'utf8');
      const ifcBytes = fs.readFileSync(path.join(root, `${key}.ifc`));
      let got;
      try {
        const model = IfcModel.fromBytes(ifcBytes, `${path.basename(key)}.ifc`);
        got = { json: JSON.parse(model.validateIds(idsText)) };
        model.free();
      } catch (e) {
        got = { error: String(e.message ?? e).split(':')[0] };
      }
      const ok = want.error !== undefined
        ? got.error === want.error
        : got.json !== undefined && strip(got.json) === strip(want.json);
      if (ok) same += 1;
      else bad.push(`${key}: wheel=${want.error ?? 'json'} wasm=${got.error ?? 'json'}`);
    }
    const n = Object.keys(wheel).length;
    check(`wasm = wheel on the IDS suite (${same}/${n} cases identical)`, bad.length === 0, bad.slice(0, 8).join('\n        '));
    console.log(`TIMING wheel leg ${(t1 - t0).toFixed(0)} ms, wasm leg ${(performance.now() - t1).toFixed(0)} ms (${n} cases)`);
  }
}

console.log(`\n${checks - failures}/${checks} checks passed`);
process.exit(failures ? 1 : 0);
