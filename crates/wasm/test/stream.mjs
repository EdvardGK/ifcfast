// Streaming-geometry gate for `ifcfast-wasm` (GH #172 v2).
//
//   node crates/wasm/test/stream.mjs
//
// Drives `IfcModel.streamMeshes()` on the Duplex sample and holds it to
// the contract in `docs/plans/2026-09-07_wasm-client-side.md`:
//
//   * batches arrive incrementally and cover every drawable product once;
//   * the merged per-batch buffers are internally consistent (`v0`/`vn`
//     span `positions`, `i0`/`in` span `indices`, indices are batch-local
//     and in range);
//   * the numbers are v1's — `qtoJson()` after a stream is byte-identical
//     to `qtoJson()` on a fresh model that took the batch path, and the
//     per-product `m3`/`m2` match `graphJson()` key-for-key;
//   * `rgba` is the glTF writer's colour cascade, not a second one.
//
// Timing is reported, not gated: wall clock on this box is not a contract.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, '../../..');
const pkg = path.resolve(here, '../pkg');
const SAMPLES = path.join(repo, '.local-samples');

if (!fs.existsSync(path.join(pkg, 'ifcfast_wasm.js'))) {
  console.error(`pkg/ not built — run ${path.relative(repo, path.join(here, '../build.sh'))}`);
  process.exit(1);
}

// `pathToFileURL`, not the bare path: Node's ESM loader reads a
// Windows absolute path as the URL scheme `c:` and refuses it, so on
// Windows this gate could not run at all.
const wasmMod = await import(pathToFileURL(path.join(pkg, 'ifcfast_wasm.js')).href);
await wasmMod.default({
  module_or_path: fs.readFileSync(path.join(pkg, 'ifcfast_wasm_bg.wasm')),
});
const { IfcModel } = wasmMod;

const results = [];
function check(label, ok, detail = '') {
  results.push({ label, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${detail ? `  — ${detail}` : ''}`);
}

function summarise() {
  const failed = results.filter((r) => !r.ok);
  console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
  process.exit(failed.length ? 1 : 0);
}

function glbJson(b) {
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  if (dv.getUint32(0, true) !== 0x46546c67) throw new Error('not a GLB');
  const jsonLen = dv.getUint32(12, true);
  return JSON.parse(Buffer.from(b.buffer, b.byteOffset + 20, jsonLen).toString('utf8'));
}

// ---------------------------------------------------------------------
// far-origin precision (GH #188) — in-repo fixture, always runs
// ---------------------------------------------------------------------
//
// `far_origin_duct_mm.ifc` is a millimetre model on a Norwegian NTM site
// placement (x ~ 9.2e7 mm, y ~ 1.25e9 mm), where the f32 ulp is 8 mm and
// 128 mm. Two IfcDuctSegments share one extruded annulus, outer
// r = 200 mm. Streamed in the World frame that circle came back with a
// 105 mm radius spread; the Local bake + f64 reposition holds it under
// 0.05 mm. The Duplex gate below cannot catch this — near-origin models
// have a zero shift and every frame agrees.

const FAR_NAME = 'far_origin_duct_mm.ifc';
const farBytes = fs.readFileSync(path.join(repo, 'tests/fixtures', FAR_NAME));
// Placement chain, metres: site (92200000, 1247000000, 0) mm + duct
// (319899.3, 315537.2, 127250) mm; duct B 2000 mm east.
const FAR_AXIS = {
  '1VluSltEX0WftNcGXN68O9': [92519.8993, 1247315.5372],
  '1VluSltEX0WftNcGXN68P0': [92521.8993, 1247315.5372],
};

const far = IfcModel.fromBytes(farBytes, FAR_NAME);
const farBatches = [];
far.streamMeshes(8, (metaJson, positions, indices, _p) => {
  farBatches.push({ meta: JSON.parse(metaJson), positions, indices });
});
const farShift = JSON.parse(far.streamShiftJson());

check(
  'far-origin: streamShiftJson() reports the georeference (not [0,0,0])',
  Array.isArray(farShift) && farShift.length === 3
    && Math.abs(farShift[0] - 92519.899) < 1e-6
    && Math.abs(farShift[1] - 1247315.537) < 1e-6,
  JSON.stringify(farShift),
);
check(
  'far-origin: shiftJson() is the same value under the neutral name',
  far.shiftJson() === far.streamShiftJson(),
  far.shiftJson(),
);

let farErr = '';
let farSeen = 0;
let worstSpread = 0;
for (const b of farBatches) {
  for (const row of b.meta) {
    const axis = FAR_AXIS[row.guid];
    if (!axis) { farErr ||= `unexpected guid ${row.guid}`; continue; }
    let rMin = Infinity;
    let rMax = -Infinity;
    let outer = 0;
    for (let v = row.v0; v < row.v0 + row.vn; v++) {
      const x = b.positions[v * 3] + farShift[0];
      const y = b.positions[v * 3 + 1] + farShift[1];
      const r = Math.hypot(x - axis[0], y - axis[1]) * 1000;
      if (r > 199.5) { outer++; rMin = Math.min(rMin, r); rMax = Math.max(rMax, r); }
    }
    if (outer < 32) farErr ||= `${row.guid}: only ${outer} outer-loop vertices`;
    const spread = rMax - rMin;
    worstSpread = Math.max(worstSpread, spread);
    if (!(spread < 0.05)) {
      farErr ||= `${row.guid}: outer radius spread ${spread.toFixed(4)} mm (mean ~${rMax.toFixed(4)})`;
    }
    farSeen++;
  }
}
if (farSeen !== 2) farErr ||= `${farSeen} duct segments streamed, expected 2`;
check(
  'far-origin: streamed Ø400 duct stays round (radius spread < 0.05 mm)',
  !farErr,
  farErr || `worst spread ${worstSpread.toFixed(5)} mm over ${farSeen} ducts`,
);

// GH #188 review item 1: the same file with a `$`-placement product
// FIRST. A `$` placement resolves to identity, so that product anchors
// at the origin; a shift pinned from the first EMITTED product would
// read [0,0,0] here and quantise every duct after it.
const unplaced = IfcModel.fromBytes(
  fs.readFileSync(path.join(repo, 'tests/fixtures', 'far_origin_unplaced_first_mm.ifc')),
  'far_origin_unplaced_first_mm.ifc',
);
const unplacedMeta = [];
unplaced.streamMeshes(8, (metaJson, positions, _i, _p) => {
  for (const row of JSON.parse(metaJson)) unplacedMeta.push({ row, positions });
});
const unplacedShift = JSON.parse(unplaced.streamShiftJson());
check(
  'far-origin: an unplaced FIRST product does not zero the shift',
  Math.abs(unplacedShift[0] - 92519.899) < 1e-6
    && Math.abs(unplacedShift[1] - 1247315.537) < 1e-6,
  `${JSON.stringify(unplacedShift)} over ${unplacedMeta.length} products, first = ${unplacedMeta[0]?.row.guid}`,
);
let unplacedErr = '';
if (unplacedMeta[0]?.row.guid !== '0UnplacedFirstProd00__') {
  unplacedErr = `first streamed product is ${unplacedMeta[0]?.row.guid}, fixture has lost its teeth`;
}
for (const { row, positions } of unplacedMeta) {
  const axis = FAR_AXIS[row.guid];
  if (!axis) continue;
  let rMin = Infinity;
  let rMax = -Infinity;
  for (let v = row.v0; v < row.v0 + row.vn; v++) {
    const r = Math.hypot(
      positions[v * 3] + unplacedShift[0] - axis[0],
      positions[v * 3 + 1] + unplacedShift[1] - axis[1],
    ) * 1000;
    if (r > 199.5) { rMin = Math.min(rMin, r); rMax = Math.max(rMax, r); }
  }
  if (!(rMax - rMin < 0.05)) unplacedErr ||= `${row.guid}: spread ${(rMax - rMin).toFixed(4)} mm`;
}
check('far-origin: ducts stay round behind the unplaced product', !unplacedErr, unplacedErr);
unplaced.free();

const farGlb = glbJson(far.toGlb(true, true));
const farExtras = ((farGlb.asset.extras ?? {}).ifcfast ?? {}).global_shift;
check(
  'far-origin: toGlb() self-describes the same shift in asset.extras',
  Array.isArray(farExtras) && farExtras.length === 3
    && farExtras.every((v, i) => Math.abs(v - farShift[i]) < 1e-9),
  JSON.stringify(farExtras),
);
far.free();

// ---------------------------------------------------------------------
// the Duplex stream contract — needs a sample that is not in the repo
// ---------------------------------------------------------------------

const NAME = 'Duplex_A_20110907.ifc';
const DUPLEX = path.join(SAMPLES, NAME);
if (!fs.existsSync(DUPLEX)) {
  console.log(`SKIP  Duplex stream contract — ${path.relative(repo, DUPLEX)} not present`);
  summarise();
}
const bytes = fs.readFileSync(DUPLEX);
const BATCH = 64;

// ---------------------------------------------------------------------
// reference: a fresh v1-style model (batch mesh pass, no streaming)
// ---------------------------------------------------------------------

const v1 = IfcModel.fromBytes(bytes, NAME);
const tV1Mesh0 = performance.now();
const v1Qto = v1.qtoJson();
const tV1Mesh = performance.now() - tV1Mesh0;
const v1Graph = JSON.parse(v1.graphJson());
const v1Stats = JSON.parse(v1.statsJson());
const v1Glb = glbJson(v1.toGlb(true, false));

// ---------------------------------------------------------------------
// the stream
// ---------------------------------------------------------------------

const tFrom0 = performance.now();
const m = IfcModel.fromBytes(bytes, NAME);
const tFrom = performance.now() - tFrom0;

// summaryJson() must work — and be honest — before any geometry runs.
const preSummary = JSON.parse(m.summaryJson());
check(
  'summaryJson() before geometry: identity present, mesh tables unloaded',
  preSummary.products > 0
    && preSummary.schema !== null
    && preSummary.tables.drift.loaded === false
    && preSummary.tables.drift.rows === 0
    && preSummary.tables.segments.loaded === false,
  `products=${preSummary.products} schema=${preSummary.schema}`,
);

const batches = [];
let cbTime = 0;
const tStream0 = performance.now();
m.streamMeshes(BATCH, (metaJson, positions, indices, progressJson) => {
  const c0 = performance.now();
  batches.push({
    meta: JSON.parse(metaJson),
    positions,
    indices,
    progress: JSON.parse(progressJson),
  });
  cbTime += performance.now() - c0;
});
const tStream = performance.now() - tStream0;

const shift = JSON.parse(m.streamShiftJson());

// ---- batch shape ----------------------------------------------------

check('batches >= 3', batches.length >= 3, `${batches.length} batches @ ${BATCH} products`);

let batchShapeErr = '';
let totalMeta = 0;
for (const [bi, b] of batches.entries()) {
  let vCursor = 0;
  let iCursor = 0;
  let maxIndex = -1;
  for (const p of b.meta) {
    if (p.v0 !== vCursor) { batchShapeErr ||= `batch ${bi}: v0 ${p.v0} != running ${vCursor}`; }
    if (p.i0 !== iCursor) { batchShapeErr ||= `batch ${bi}: i0 ${p.i0} != running ${iCursor}`; }
    vCursor += p.vn;
    iCursor += p.in;
  }
  if (b.positions.length !== vCursor * 3) {
    batchShapeErr ||= `batch ${bi}: positions ${b.positions.length} != 3 * ${vCursor}`;
  }
  if (b.indices.length !== iCursor) {
    batchShapeErr ||= `batch ${bi}: indices ${b.indices.length} != ${iCursor}`;
  }
  for (let k = 0; k < b.indices.length; k++) if (b.indices[k] > maxIndex) maxIndex = b.indices[k];
  if (b.indices.length && maxIndex >= b.positions.length / 3) {
    batchShapeErr ||= `batch ${bi}: max index ${maxIndex} >= vertex count ${b.positions.length / 3}`;
  }
  if (b.meta.length > BATCH) batchShapeErr ||= `batch ${bi}: ${b.meta.length} products > ${BATCH}`;
  totalMeta += b.meta.length;
}
check('per-batch buffers self-consistent (v0/vn/i0/in, indices in range)', !batchShapeErr, batchShapeErr);

check(
  'batch sizes: all full but the tail',
  batches.slice(0, -1).every((b) => b.meta.length === BATCH),
  batches.map((b) => b.meta.length).join('+'),
);

const sumVn = batches.reduce((a, b) => a + b.meta.reduce((x, p) => x + p.vn, 0), 0);
const sumPositions = batches.reduce((a, b) => a + b.positions.length, 0);
check('concatenated positions length == 3 * sum(vn)', sumPositions === sumVn * 3, `${sumPositions} vs ${sumVn * 3}`);

const sumTri = batches.reduce((a, b) => a + b.meta.reduce((x, p) => x + p.tri, 0), 0);
const sumIdx = batches.reduce((a, b) => a + b.indices.length, 0);
check('sum(tri) == sum(in) / 3', sumTri === sumIdx / 3, `${sumTri} vs ${sumIdx / 3}`);

// ---- guid coverage --------------------------------------------------

const guids = new Map();
for (const b of batches) for (const p of b.meta) guids.set(p.guid, (guids.get(p.guid) ?? 0) + 1);
const dupes = [...guids.entries()].filter(([, n]) => n > 1);
check('every product guid appears exactly once', dupes.length === 0 && guids.size === totalMeta,
  `${guids.size} distinct / ${totalMeta} meta rows, ${dupes.length} repeated`);

// ---- triangle accounting vs the engine counters ---------------------
//
// The contract line was "sum(tri) == statsJson().triangles". It cannot be,
// and v1 does not satisfy it either: `MeshStats.triangles` is the raw
// tessellation count, while every measure (v1's drift rows, the qto
// `triangles` column, and these batches) is taken AFTER the synthetic
// half-space stand-in slabs are stripped (GH #66). The invariant that
// actually holds — asserted here — is that the stream's triangles equal
// v1's post-strip total exactly, and that the gap to the engine counter is
// the same gap v1 has.
const streamStats = JSON.parse(m.statsJson());
const v1QtoObj = JSON.parse(v1Qto);
const v1PostStrip = v1QtoObj.rows.reduce((a, r) => a + r.triangles, 0);
const v1Gap = v1Stats.triangles - v1PostStrip;
check('sum(tri) == v1 post-strip triangle total', sumTri === v1PostStrip, `${sumTri} vs ${v1PostStrip}`);
check(
  'stream vs engine counter gap == v1 gap (stripped cutter slabs, GH #66)',
  streamStats.triangles - sumTri === v1Gap && streamStats.triangles === v1Stats.triangles,
  `engine ${streamStats.triangles}, emitted ${sumTri}, gap ${v1Gap}`,
);

// ---- progress -------------------------------------------------------

let progressErr = '';
let prevSeen = 0;
for (const [bi, b] of batches.entries()) {
  if (b.progress.total !== preSummary.products) progressErr ||= `batch ${bi}: total ${b.progress.total} != ${preSummary.products}`;
  if (b.progress.seen < prevSeen) progressErr ||= `batch ${bi}: seen went backwards`;
  if (b.progress.meshed > b.progress.seen) progressErr ||= `batch ${bi}: meshed > seen`;
  prevSeen = b.progress.seen;
}
const last = batches[batches.length - 1].progress;
if (last.meshed !== totalMeta) progressErr ||= `final meshed ${last.meshed} != ${totalMeta} emitted`;
if (last.seen !== streamStats.products_meshed) progressErr ||= `final seen ${last.seen} != products_meshed ${streamStats.products_meshed}`;
check('progress monotonic and closes on the engine counters', !progressErr, progressErr
  || `seen ${last.seen}, meshed ${last.meshed}, total ${last.total}`);

// ---- the numbers are v1's ------------------------------------------

check('qtoJson() after streaming is byte-identical to v1', m.qtoJson() === v1Qto,
  `${m.qtoJson().length} vs ${v1Qto.length} bytes`);

const streamGraph = JSON.parse(m.graphJson());
const v1M = new Map(v1Graph.products.map((p) => [p.guid, p]));
let graphErr = '';
if (streamGraph.products.length !== v1Graph.products.length) {
  graphErr = `product count ${streamGraph.products.length} != ${v1Graph.products.length}`;
} else {
  for (const p of streamGraph.products) {
    const r = v1M.get(p.guid);
    if (!r) { graphErr ||= `guid ${p.guid} missing from v1 graph`; continue; }
    if (!Object.is(p.m3, r.m3)) graphErr ||= `${p.guid}.m3 ${p.m3} != ${r.m3}`;
    if (!Object.is(p.m2, r.m2)) graphErr ||= `${p.guid}.m2 ${p.m2} != ${r.m2}`;
    if (!Object.is(p.lm, r.lm)) graphErr ||= `${p.guid}.lm ${p.lm} != ${r.lm}`;
  }
}
check('graphJson() m3/m2/lm identical to v1, key-for-key', !graphErr, graphErr
  || `${streamGraph.products.length} products`);

// The streamed per-product m3/m2 must be the SAME numbers, not merely
// close: they come from the same drift row the graph joins.
let metaErr = '';
for (const b of batches) {
  for (const p of b.meta) {
    const r = v1M.get(p.guid);
    if (!r) { metaErr ||= `${p.guid} not in graph`; continue; }
    if (!Object.is(p.m3, r.m3_direct)) metaErr ||= `${p.guid}.m3 ${p.m3} != graph m3_direct ${r.m3_direct}`;
    if (!Object.is(p.m2, r.m2_direct)) metaErr ||= `${p.guid}.m2 ${p.m2} != graph m2_direct ${r.m2_direct}`;
    if (!Array.isArray(p.rgba) || p.rgba.length !== 4 || p.rgba.some((c) => !(c >= 0 && c <= 1))) {
      metaErr ||= `${p.guid}.rgba not 4 channels in [0,1]: ${JSON.stringify(p.rgba)}`;
    }
  }
}
check('batch meta m3/m2 == graph m3_direct/m2_direct, rgba well-formed', !metaErr, metaErr);

// ---- rgba is the glTF writer's cascade ------------------------------
//
// With per_product_materials the writer names a product's FIRST material
// exactly by its guid, so a single-primitive product's baseColorFactor is
// `resolve_product_color` for that product. Multi-primitive products
// resolve per SEGMENT (`resolve_segment_color`), which can legitimately
// differ on the first segment — those are counted, not gated.
const matByName = new Map(v1Glb.materials.map((mat) => [mat.name, mat]));
const primCount = new Map();
for (const node of v1Glb.nodes) {
  const guid = (node.extras ?? {}).guid;
  if (guid === undefined || node.mesh === undefined) continue;
  primCount.set(guid, v1Glb.meshes[node.mesh].primitives.length);
}
let rgbaChecked = 0;
let rgbaMismatch = 0;
const rgbaExamples = [];
for (const b of batches) {
  for (const p of b.meta) {
    const mat = matByName.get(p.guid);
    if (!mat || primCount.get(p.guid) !== 1) continue;
    const ref = mat.pbrMetallicRoughness.baseColorFactor;
    rgbaChecked++;
    if (ref.some((c, i) => Math.abs(c - p.rgba[i]) > 1e-6)) {
      rgbaMismatch++;
      if (rgbaExamples.length < 3) rgbaExamples.push(`${p.guid}: ${JSON.stringify(p.rgba)} != ${JSON.stringify(ref)}`);
    }
  }
}
check(
  'rgba == glTF baseColorFactor on single-primitive products',
  rgbaMismatch === 0,
  rgbaMismatch ? rgbaExamples.join('; ') : `${rgbaChecked} products cross-checked`,
);

// ---- global shift ---------------------------------------------------

check(
  'streamShiftJson() is [sx, sy, sz] metres (Duplex is near-origin ⇒ zero)',
  Array.isArray(shift) && shift.length === 3 && shift.every((v) => v === 0),
  JSON.stringify(shift),
);

// ---- toGlb still works after a stream -------------------------------

const glbAfter = glbJson(m.toGlb(true, false));
check('toGlb() after streaming re-runs the batch pass and matches v1',
  glbAfter.nodes.length === v1Glb.nodes.length && glbAfter.materials.length === v1Glb.materials.length,
  `${glbAfter.nodes.length} nodes / ${glbAfter.materials.length} materials`);

// ---------------------------------------------------------------------
// timings — reported, not gated
// ---------------------------------------------------------------------

// Per-callback overhead: the same stream at two batch sizes with a
// do-nothing callback. The tessellation work is identical in both, so the
// difference divided by the extra callbacks is the marshalling cost of one
// batch boundary (two typed-array copies + the JS call + the meta JSON).
function noopStream(perBatch) {
  const mm = IfcModel.fromBytes(bytes, NAME);
  let n = 0;
  const t0 = performance.now();
  mm.streamMeshes(perBatch, () => { n++; });
  const t = performance.now() - t0;
  mm.free();
  return { t, n };
}
noopStream(BATCH); // warm
const coarse = noopStream(BATCH);
const fine = noopStream(1);
const perCb = (fine.t - coarse.t) / (fine.n - coarse.n);

console.log(
  `\nTIMING duplex stream: fromBytes ${tFrom.toFixed(1)} ms (parse+index+extractors, no mesh), ` +
  `streamMeshes(${BATCH}) ${tStream.toFixed(1)} ms over ${batches.length} batches ` +
  `[engine mesh ${streamStats.mesh_ms.toFixed(1)} ms, entity table ${streamStats.entity_table_ms.toFixed(1)} ms]`,
);
console.log(
  `TIMING callback: ${cbTime.toFixed(1)} ms total inside this test's JS callback ` +
  `(${(cbTime / batches.length).toFixed(2)} ms/batch — JSON.parse + retain, ` +
  `${(100 * cbTime / tStream).toFixed(1)}% of the stream)`,
);
console.log(
  `TIMING boundary: no-op cb at ${BATCH}/batch ${coarse.t.toFixed(1)} ms over ${coarse.n} batches, ` +
  `at 1/batch ${fine.t.toFixed(1)} ms over ${fine.n} batches ` +
  `⇒ ${(perCb * 1000).toFixed(0)} µs per batch boundary (2 typed-array copies + call + meta JSON)`,
);
console.log(
  `TIMING v1 reference: batch mesh (first qtoJson) ${tV1Mesh.toFixed(1)} ms; ` +
  `${sumTri} triangles emitted, ${sumVn} vertices, ${totalMeta} drawable products`,
);

m.free();
v1.free();

summarise();
