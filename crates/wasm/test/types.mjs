// Node gate for the data surfaces an IDS / model-check consumer reads:
// the four GH #183 layers, the declared type-object roster, and the
// per-product `type_guid` that joins them.
//
//   node crates/wasm/test/types.mjs
//
// Two tiers, deliberately:
//
//   * `tests/fixtures/type_objects.ifc` — in the repo, so this runs on a
//     clean checkout. Declared-vs-used is pinned by exact numbers there.
//   * every `.ifc` in `.local-samples/` — real exports, not in the repo
//     (the same convention parity.mjs uses for the Duplex). No pinned
//     numbers: the file is whatever it is. What IS asserted is the
//     invariants that must hold on any file, plus a printed per-model
//     table so a change in what a real export yields is visible in the
//     log. `IFCFAST_TYPES_EXPECT` points at a JSON file of
//     `{ "<stem>": { declared, used, psets, classifications, ... } }`
//     to turn those prints into assertions on a box that has the models.
//
// The invariants, and why each one is worth a gate:
//
//   1. Row count equals `summaryJson().tables.<name>.rows`, and every
//      row's key set equals the advertised column list. The accessor and
//      the advertisement disagreeing is worse than either being wrong.
//   2. Every non-null `type_guid` resolves in the roster. The two come
//      off the same index vectors, so a dangling one is a bug.
//   3. `typed === (type_source === 'ifctype') === (type_guid !== null)`.
//      An ObjectType string is a name with no object behind it; if
//      `type_guid` ever gets invented from a name, this catches it.
//   4. Declared ≥ used, and `typesJson()` (used NAMES) is not the same
//      number as either. That relationship is the whole reason the
//      roster is exposed.
//   5. Every row's guid resolves to SOMETHING the model surfaces — a
//      product, a type object, or a spatial/project entity. Real exports
//      hang psets on the IfcProject and the storeys, so "product or
//      type" is too narrow; a row on none of them would be a row no
//      facet could ever evaluate.

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
// Windows absolute path as the URL scheme `c:` and refuses it.
const wasmMod = await import(pathToFileURL(path.join(pkg, 'ifcfast_wasm.js')).href);
await wasmMod.default({
  module_or_path: fs.readFileSync(path.join(pkg, 'ifcfast_wasm_bg.wasm')),
});
const { IfcModel } = wasmMod;

const results = [];
function check(label, diffs) {
  const ok = diffs.length === 0;
  results.push({ label, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}`);
  for (const d of diffs.slice(0, 10)) console.log(`        ${d}`);
  if (diffs.length > 10) console.log(`        … ${diffs.length - 10} more`);
}

/** Rows vs summary metadata, and key set vs advertised columns. */
function layerShape(label, rows, meta) {
  const d = [];
  if (!Array.isArray(rows)) return [`${label}: not an array`];
  if (rows.length !== meta.rows) d.push(`${label}: ${rows.length} rows != summary ${meta.rows}`);
  const want = [...meta.columns].sort().join(',');
  const bad = rows.findIndex((r) => Object.keys(r).sort().join(',') !== want);
  if (bad >= 0) d.push(`${label}[${bad}]: columns ${Object.keys(rows[bad]).sort().join(',')} != ${want}`);
  return d;
}

/** Everything both tiers assert, plus the counts they print. */
function probe(file, name) {
  const m = IfcModel.fromBytes(fs.readFileSync(file), name);
  const summary = JSON.parse(m.summaryJson());
  const layers = {
    psets: JSON.parse(m.psetsJson()),
    quantities: JSON.parse(m.quantitiesJson()),
    materials: JSON.parse(m.materialsJson()),
    classifications: JSON.parse(m.classificationsJson()),
    type_objects: JSON.parse(m.typeObjectsJson()),
  };
  const sizes = {
    psets: m.psetsJson().length,
    quantities: m.quantitiesJson().length,
    materials: m.materialsJson().length,
    classifications: m.classificationsJson().length,
    type_objects: m.typeObjectsJson().length,
  };
  // graphJson meshes on first call — the only expensive line here.
  const graph = JSON.parse(m.graphJson());
  const usedNames = JSON.parse(m.typesJson()).types;
  m.free();

  const d = [];
  for (const [label, rows] of Object.entries(layers)) {
    d.push(...layerShape(label, rows, summary.tables[label]));
  }

  const declared = new Set(layers.type_objects.map((t) => t.guid));
  const used = new Set(graph.products.map((p) => p.type_guid).filter(Boolean));
  for (const g of used) if (!declared.has(g)) d.push(`type_guid ${g} is not in the roster`);

  let inconsistent = 0;
  for (const p of graph.products) {
    const byGuid = p.type_guid !== null && p.type_guid !== undefined;
    if (byGuid !== (p.type_source === 'ifctype') || byGuid !== p.typed) inconsistent++;
  }
  if (inconsistent) d.push(`${inconsistent} products where typed / type_source / type_guid disagree`);
  if (used.size > declared.size) d.push(`used ${used.size} > declared ${declared.size}`);

  // A facet row that joins to nothing cannot be evaluated. The join
  // space is NOT just products: the extractors walk every
  // IfcRelDefinesByProperties / IfcRelAssociatesClassification in the
  // file, and real exports hang psets on the spatial and project
  // entities too — KNM_ARK puts `NOAV_Oppdrag` (Oppdragsnummer,
  // Oppdragsnavn) on the IfcProject, KNM_RIB on its site, building and
  // eight storeys. Those are legitimate rows about entities the product
  // whitelist deliberately excludes, so a consumer joining on
  // `graph.products` alone silently drops them. Counted and printed
  // rather than failed; what fails is a guid that matches NOTHING the
  // model surfaces.
  const spatial = new Set([
    ...graph.storeys.map((s) => s.guid),
    ...graph.spaces.map((s) => s.guid),
    ...graph.buildings.map((b) => b.guid),
    ...graph.sites.map((s) => s.guid),
    ...graph.projects.map((p) => p.guid),
  ]);
  const joinable = new Set([...graph.products.map((p) => p.guid), ...declared, ...spatial]);
  const nonProduct = {};
  for (const layer of ['psets', 'classifications', 'quantities', 'materials']) {
    const rows = layers[layer];
    const orphan = rows.filter((r) => !joinable.has(r.guid));
    if (orphan.length) {
      d.push(`${layer}: ${orphan.length} rows whose guid is on nothing this model surfaces (${orphan[0].guid})`);
    }
    nonProduct[layer] = rows.filter((r) => spatial.has(r.guid)).length;
  }

  const counts = {
    schema: summary.schema,
    products: graph.products.length,
    psets: layers.psets.length,
    pset_names: new Set(layers.psets.map((r) => r.pset_name)).size,
    quantities: layers.quantities.length,
    classifications: layers.classifications.length,
    declared_types: declared.size,
    used_types: used.size,
    unused_types: declared.size - used.size,
    used_type_names: usedNames.length,
    typed_products: graph.products.filter((p) => p.type_guid).length,
    // Rows about a spatial / project entity rather than a product.
    psets_on_spatial: nonProduct.psets,
    classifications_on_spatial: nonProduct.classifications,
  };
  return { d, counts, sizes, layers, graph };
}

// ---- tier 1: the in-repo fixture ------------------------------------
{
  const { d, counts, layers, graph } = probe(
    path.join(repo, 'tests/fixtures/type_objects.ifc'),
    'type_objects.ifc',
  );
  const pin = (label, got, want) => {
    if (got !== want) d.push(`${label}: ${got} != ${want}`);
  };
  pin('declared_types', counts.declared_types, 3);
  pin('used_types', counts.used_types, 1);
  pin('unused_types', counts.unused_types, 2);
  pin('typed_products', counts.typed_products, 3);
  // Two names over occurrences, and one of them is a bare ObjectType
  // with no type object at all — neither number is the roster's 3.
  pin('used_type_names', counts.used_type_names, 2);
  const w4 = graph.products.find((p) => p.name === 'Wall-004');
  pin('Wall-004.type_source', w4.type_source, 'objecttype');
  pin('Wall-004.type_name', w4.type_name, 'Yttervegg 250');
  pin('Wall-004.type_guid', w4.type_guid, null);
  // Two declared types share the name 'Ubrukt': a name is not a key.
  const ubrukt = layers.type_objects.filter((t) => t.name === 'Ubrukt');
  pin('duplicate type names stay distinct rows', ubrukt.length, 2);
  check('type_objects.ifc — declared vs used', d);
}

// ---- tier 2: real exports, when present ------------------------------
const expectPath = process.env.IFCFAST_TYPES_EXPECT;
const expected = expectPath ? JSON.parse(fs.readFileSync(expectPath, 'utf8')) : null;

const samples = fs.existsSync(SAMPLES)
  ? fs.readdirSync(SAMPLES).filter((f) => f.toLowerCase().endsWith('.ifc')).sort()
  : [];
if (!samples.length) {
  console.log('SKIP  real models — put .ifc files in .local-samples/ (not in the repo)');
}
for (const f of samples) {
  const stem = path.basename(f, path.extname(f));
  const t0 = performance.now();
  const { d, counts, sizes } = probe(path.join(SAMPLES, f), f);
  const ms = performance.now() - t0;
  const kb = (n) => `${(n / 1024).toFixed(0)} KiB`;
  console.log(
    `INFO  ${stem}: ${counts.schema} · ${counts.products} products · ` +
    `psets ${counts.psets} (${kb(sizes.psets)}, ${counts.pset_names} pset names) · ` +
    `quantities ${counts.quantities} · classifications ${counts.classifications} (${kb(sizes.classifications)}) · ` +
    `types ${counts.declared_types} declared / ${counts.used_types} used / ` +
    `${counts.unused_types} unused / ${counts.used_type_names} used names (${kb(sizes.type_objects)}) · ` +
    `${counts.typed_products} typed products · ` +
    `${counts.psets_on_spatial}/${counts.classifications_on_spatial} pset/classification rows on spatial entities · ` +
    `${ms.toFixed(0)} ms`,
  );
  if (expected?.[stem]) {
    for (const [k, want] of Object.entries(expected[stem])) {
      if (counts[k] !== want) d.push(`${k}: ${counts[k]} != expected ${want}`);
    }
  }
  check(`${f} — data surfaces`, d);
}

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
process.exit(failed.length ? 1 : 0);
