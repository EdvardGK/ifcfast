// Node gate for GH #194 in the browser build: clipping bodies are the
// element's own shape in every mode, and the wasm build (no `csg`, no
// Manifold) clips them with the pure-Rust route.
//
//   node crates/wasm/test/clip194.mjs
//
// Fixtures are Revit 2025 IFC2X3 walls (feet) pinned against
// ifcopenshell with opening subtraction disabled:
//   * tests/fixtures/clip_single_pbhs_194.ifc — one
//     IfcPolygonalBoundedHalfSpace clip → 37.7596 m³ (unclipped 38.9135)
//   * tests/fixtures/clip_chain3_194.ifc — three chained clips → 23.8721 m³
//   * tests/fixtures/clip_unresolvable_opening_194.ifc — a clip that
//     cannot be applied (GH #211): counter 1, volume flagged unreliable

import fs from 'node:fs';
import path from 'node:path';
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

const WALL = '1G_I00WlX2MBQekqB04dQj';
const cases = [
  ['clip_single_pbhs_194.ifc', 37.7596],
  ['clip_chain3_194.ifc', 23.8721],
];

let pass = 0;
let fail = 0;
for (const [name, want] of cases) {
  const bytes = fs.readFileSync(path.join(repo, 'tests/fixtures', name));
  const m = IfcModel.fromBytes(bytes, name);
  const graph = JSON.parse(m.graphJson());
  const row = graph.products.find((p) => p.guid === WALL);
  const got = row ? row.m3_direct : undefined;
  const bySource = JSON.parse(m.bySourceJson());
  const leaked = Object.keys(bySource).filter((k) => k.includes('halfspace'));
  const ok =
    typeof got === 'number' && Math.abs(got - want) / want < 1e-3 && leaked.length === 0;
  if (ok) {
    pass += 1;
    console.log(`PASS  ${name}: wall m3_direct ${got} (ifcopenshell ${want})`);
  } else {
    fail += 1;
    console.log(
      `FAIL  ${name}: wall m3_direct ${got} vs ifcopenshell ${want}; halfspace tags ${JSON.stringify(leaked)}`,
    );
  }
}
// GH #211: the unresolvable clip. The host stays unclipped, so the browser
// build must say so: the counter in statsJson, and the class's volume
// split out of the reliable total in qtoJson.
{
  const name = 'clip_unresolvable_opening_194.ifc';
  const bytes = fs.readFileSync(path.join(repo, 'tests/fixtures', name));
  const m = IfcModel.fromBytes(bytes, name);
  const stats = JSON.parse(m.statsJson());
  const rows = JSON.parse(m.qtoJson()).rows;
  const bad = rows.filter((r) => r.products_clip_unapplied > 0);
  const r = bad[0];
  const ok =
    stats.halfspace_clip_unapplied === 1 &&
    stats.halfspace_clip_manifold === 0 &&
    bad.length === 1 &&
    r.volume_unreliable_m3 > 0 &&
    Math.abs(r.volume_reliable_m3 + r.volume_unreliable_m3 - r.volume_m3) < 1e-9;
  if (ok) {
    pass += 1;
    console.log(
      `PASS  ${name}: halfspace_clip_unapplied=1, ${r.entity} volume_unreliable_m3=${r.volume_unreliable_m3}`,
    );
  } else {
    fail += 1;
    console.log(
      `FAIL  ${name}: stats ${JSON.stringify(stats)}; unreliable rows ${JSON.stringify(bad)}`,
    );
  }
  // A clean model reports nothing unreliable.
  const clean = IfcModel.fromBytes(
    fs.readFileSync(path.join(repo, 'tests/fixtures/clip_single_pbhs_194.ifc')),
    'clip_single_pbhs_194.ifc',
  );
  const cs = JSON.parse(clean.statsJson());
  const cr = JSON.parse(clean.qtoJson()).rows;
  const cok =
    cs.halfspace_clip_unapplied === 0 &&
    cr.every((x) => x.products_clip_unapplied === 0 && !(x.volume_unreliable_m3 > 0));
  if (cok) {
    pass += 1;
    console.log('PASS  clip_single_pbhs_194.ifc: no unapplied clip, volume_unreliable_m3 = 0');
  } else {
    fail += 1;
    console.log(`FAIL  clean model flagged: ${JSON.stringify(cs)}`);
  }
}

console.log(`\n${pass}/${pass + fail} checks passed`);
process.exit(fail === 0 ? 0 : 1);
