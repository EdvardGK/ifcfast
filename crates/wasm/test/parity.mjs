// Node smoke + parity gate for `ifcfast-wasm` (GH #172).
//
//   node crates/wasm/test/parity.mjs
//
// Loads `crates/wasm/pkg/` (build it with `crates/wasm/build.sh` first),
// runs `IfcModel.fromBytes` on the Duplex sample, and diffs the four JSON
// surfaces key-by-key against the sidecars the Python generator baked
// into `ifcfast-site/public/sample/`.
//
// Ignored keys, and why:
//   * summary.path / summary.parse_seconds  — inputs, not outputs.
//   * types manifest generated_with / glb / bytes — no per-type mini-glbs
//     in v1 (the contract says so).
//   * graph.spaces / buildings / sites / projects ORDER — the core keeps
//     those in a `HashMap`, so the reference file's order is whatever one
//     Python process happened to iterate. Verified: two runs of
//     `_core.index_ifc` on the same file give different orders. The
//     contents are compared exactly; only the order is set-insensitive.
//
// The GLB is checked for node + GUID-material counts. See the report at
// the bottom for why it cannot equal the baked `duplex.glb` in v1.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../..');
const pkg = path.resolve(here, '../pkg');
const SAMPLES = path.join(repo, '.local-samples');
const SITE = '/home/edkjo/workspace/inbox/ifcfast-site/public/sample';

if (!fs.existsSync(path.join(pkg, 'ifcfast_wasm.js'))) {
  console.error(`pkg/ not built — run ${path.relative(repo, path.join(here, '../build.sh'))}`);
  process.exit(1);
}

const wasmMod = await import(path.join(pkg, 'ifcfast_wasm.js'));
await wasmMod.default({
  module_or_path: fs.readFileSync(path.join(pkg, 'ifcfast_wasm_bg.wasm')),
});
const { IfcModel } = wasmMod;

// ---------------------------------------------------------------------
// deep diff
// ---------------------------------------------------------------------

function isObj(v) {
  return v !== null && typeof v === 'object' && !Array.isArray(v);
}

/** Collect up to `cap` differences as `path: expected != actual` lines. */
function diff(expected, actual, opts = {}, prefix = '', out = []) {
  const cap = opts.cap ?? 40;
  if (out.length >= cap) return out;
  const ignore = opts.ignore ?? new Set();
  if (ignore.has(prefix)) return out;

  if (Array.isArray(expected) && Array.isArray(actual)) {
    if (expected.length !== actual.length) {
      out.push(`${prefix}: length ${expected.length} != ${actual.length}`);
      return out;
    }
    for (let i = 0; i < expected.length; i++) {
      diff(expected[i], actual[i], opts, `${prefix}[${i}]`, out);
      if (out.length >= cap) return out;
    }
    return out;
  }
  if (isObj(expected) && isObj(actual)) {
    const keys = new Set([...Object.keys(expected), ...Object.keys(actual)]);
    for (const k of [...keys].sort()) {
      const p = prefix ? `${prefix}.${k}` : k;
      if (ignore.has(p)) continue;
      if (!(k in expected)) { out.push(`${p}: missing in expected (actual ${JSON.stringify(actual[k])})`); continue; }
      if (!(k in actual)) { out.push(`${p}: missing in actual (expected ${JSON.stringify(expected[k])})`); continue; }
      diff(expected[k], actual[k], opts, p, out);
      if (out.length >= cap) return out;
    }
    return out;
  }
  if (typeof expected === 'number' && typeof actual === 'number') {
    if (!Object.is(expected, actual)) out.push(`${prefix}: ${expected} != ${actual}`);
    return out;
  }
  if (expected !== actual) {
    out.push(`${prefix}: ${JSON.stringify(expected)} != ${JSON.stringify(actual)}`);
  }
  return out;
}

const results = [];
function check(label, diffs) {
  const ok = diffs.length === 0;
  results.push({ label, ok, diffs });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  for (const d of diffs.slice(0, 20)) console.log(`        ${d}`);
  if (diffs.length > 20) console.log(`        … ${diffs.length - 20} more`);
}

// ---------------------------------------------------------------------
// glb reader
// ---------------------------------------------------------------------

function glbJson(bytes) {
  const dv = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  if (dv.getUint32(0, true) !== 0x46546c67) throw new Error('not a GLB');
  const jsonLen = dv.getUint32(12, true);
  return JSON.parse(Buffer.from(bytes.buffer, bytes.byteOffset + 20, jsonLen).toString('utf8'));
}

// ---------------------------------------------------------------------
// Duplex parity
// ---------------------------------------------------------------------

const duplexPath = path.join(SAMPLES, 'Duplex_A_20110907.ifc');
const duplexBytes = fs.readFileSync(duplexPath);

const tParse0 = performance.now();
const m = IfcModel.fromBytes(duplexBytes, 'Duplex_A_20110907.ifc');
const tParse = performance.now() - tParse0;

// v2 (GH #172): fromBytes is parse + index + extractors only. The first
// geometry-derived surface pulls in the batch mesh pass — graphJson()
// here — and every number it produces is v1's, unchanged.
//
// summaryJson() is deliberately NOT one of those surfaces: it is what the
// drop zone shows the instant parsing finishes, so it never meshes, and
// its `drift` / `segments` tables read `rows: 0, loaded: false` until
// geometry has run. Hence it is read AFTER graphJson() here; the check
// itself is unchanged.
const tMesh0 = performance.now();
const graph = JSON.parse(m.graphJson());
const tMesh = performance.now() - tMesh0;

const summary = JSON.parse(m.summaryJson());
const qto = JSON.parse(m.qtoJson());
const types = JSON.parse(m.typesJson());
const stats = JSON.parse(m.statsJson());
const bySource = JSON.parse(m.bySourceJson());

const tGlb0 = performance.now();
const glb = m.toGlb(true, false);
const tGlb = performance.now() - tGlb0;

const refSummary = JSON.parse(fs.readFileSync(path.join(SITE, 'duplex.summary.json'), 'utf8'));
const refGraph = JSON.parse(fs.readFileSync(path.join(SITE, 'duplex.graph.json'), 'utf8'));
const refQto = JSON.parse(fs.readFileSync(path.join(SITE, 'duplex.qto.json'), 'utf8'));
const refTypes = JSON.parse(fs.readFileSync(path.join(SITE, 'types/manifest.json'), 'utf8'));

check(
  'summary.json',
  diff(refSummary, summary, { ignore: new Set(['path', 'parse_seconds']) }),
);

// Order-insensitive collections (HashMap iteration on the reference side).
const byKey = (arr, k) => Object.fromEntries(arr.map((r) => [r[k], r]));
check(
  'graph.json',
  diff(refGraph, graph, {
    ignore: new Set(['spaces', 'buildings', 'sites', 'projects']),
    cap: 60,
  }),
);
for (const [field, key] of [['spaces', 'guid'], ['buildings', 'guid'], ['sites', 'guid'], ['projects', 'guid']]) {
  check(`graph.${field} (order-insensitive)`, diff(byKey(refGraph[field], key), byKey(graph[field], key), {}, `graph.${field}`));
}

check('qto.json', diff(refQto, qto, {}, '', []));

const stripTypes = (o) => ({
  source: o.source,
  types: o.types.map(({ slug, type_name, entity, count, guid }) => ({ slug, type_name, entity, count, guid })),
});
check('types/manifest.json', diff(stripTypes(refTypes), stripTypes(types)));

// ---- GLB shape ------------------------------------------------------
//
// The contract's gate is "227 nodes / 426 GUID-named materials, like the
// shipped duplex.glb". v1 lands at 216 / 415 and the whole 11-node delta
// is attributed, not waved away:
//
//   the sidecar generator carves a space-free subset and exports THAT.
//   `subset()` pulls 11 IfcSpace products back in as dependencies of the
//   kept elements, so the reference glb carries 11 space nodes the script
//   was trying to remove. Our node set is a strict SUBSET of the
//   reference's — asserted below — and equals the intended space-free set.
//
// cut_openings contributes no node-count difference: it suppresses the
// 50 opening products (we drop the same 50) and changes host *geometry*.
// The holes themselves are the real v1 gap and they are invisible to
// these counters.
const g = glbJson(glb);
const guidMats = g.materials.filter((mat) => !String(mat.name ?? '#').startsWith('#')).length;
console.log(
  `INFO  glb: ${g.nodes.length} nodes, ${g.materials.length} materials ` +
  `(${guidMats} GUID-named), ${g.meshes.length} meshes, ${glb.length} bytes`,
);

const refGlb = glbJson(fs.readFileSync(path.join(SITE, 'duplex.glb')));
const refGuids = new Set(refGlb.nodes.map((n) => (n.extras ?? {}).guid).filter(Boolean));
const ourGuids = new Set(g.nodes.map((n) => (n.extras ?? {}).guid).filter(Boolean));
const entityOf = Object.fromEntries(refGraph.products.map((p) => [p.guid, p.entity]));
const extra = [...ourGuids].filter((x) => !refGuids.has(x));
const missing = [...refGuids].filter((x) => !ourGuids.has(x));
const missingEntities = [...new Set(missing.map((x) => entityOf[x]))];

const glbDiffs = [];
// Hard gate: we must never emit a product the reference glb does not have.
if (extra.length) glbDiffs.push(`nodes present here but not in duplex.glb: ${extra.length} (${[...new Set(extra.map((x) => entityOf[x]))].join(', ')})`);
// Hard gate: the only thing we may be missing is the subset's space leak.
if (missingEntities.length && (missingEntities.length !== 1 || missingEntities[0] !== 'IfcSpace')) {
  glbDiffs.push(`missing non-space nodes vs duplex.glb: ${missingEntities.join(', ')}`);
}
// Regression pin on the v1 numbers themselves.
if (g.nodes.length !== 216) glbDiffs.push(`nodes: expected 216 (= 227 - 11 leaked IfcSpace), got ${g.nodes.length}`);
if (guidMats !== 415) glbDiffs.push(`GUID-named materials: expected 415 (= 426 - 11), got ${guidMats}`);
check('duplex.glb node/material accounting', glbDiffs);
console.log(
  `INFO  glb vs duplex.glb: +${extra.length} / -${missing.length} nodes ` +
  `(missing are all ${missingEntities.join(', ') || 'n/a'}); ` +
  'contract gate 227/426 not reachable in v1 — see the comment above.',
);

// ---- fixtures -------------------------------------------------------
function smoke(label, file, name) {
  const bytes = fs.readFileSync(file);
  const mm = IfcModel.fromBytes(bytes, name);
  const s = JSON.parse(mm.summaryJson());
  const gl = mm.toGlb(true, true);
  console.log(
    `INFO  ${label}: schema=${s.schema} products=${s.products} ` +
    `drift=${s.tables.drift.rows} glb=${gl.length}B`,
  );
  mm.free();
}

smoke('minimal.ifc', path.join(repo, 'tests/fixtures/minimal.ifc'), 'minimal.ifc');

const zipCandidates = [
  path.join(repo, 'tests/fixtures'),
  path.join(repo, 'scratch/pytest-tmp'),
];
let zip = null;
for (const dir of zipCandidates) {
  if (!fs.existsSync(dir)) continue;
  const stack = [dir];
  while (stack.length && !zip) {
    const d = stack.pop();
    for (const e of fs.readdirSync(d, { withFileTypes: true })) {
      const p = path.join(d, e.name);
      if (e.isDirectory()) stack.push(p);
      else if (e.name.endsWith('.ifczip')) { zip = p; break; }
    }
  }
  if (zip) break;
}
if (zip) smoke(`ifczip (${path.basename(zip)})`, zip, path.basename(zip));
else console.log('INFO  no .ifczip fixture found — magic-byte dispatch not exercised');

// ---- timings --------------------------------------------------------
console.log(
  `\nTIMING duplex: fromBytes (parse+index+extractors) ${tParse.toFixed(1)} ms, ` +
  `on-demand batch mesh (first graphJson) ${tMesh.toFixed(1)} ms ` +
  `[engine mesh ${stats.mesh_ms.toFixed(1)} ms, entity table ${stats.entity_table_ms.toFixed(1)} ms], ` +
  `toGlb ${tGlb.toFixed(1)} ms, glb ${glb.length} B`,
);
console.log(`STATS  ${JSON.stringify(stats)}`);
console.log(`BYSRC  ${JSON.stringify(bySource)}`);
console.log(
  `PKG    ifcfast_wasm_bg.wasm ${fs.statSync(path.join(pkg, 'ifcfast_wasm_bg.wasm')).size} bytes`,
);

m.free();

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
process.exit(failed.length ? 1 : 0);
