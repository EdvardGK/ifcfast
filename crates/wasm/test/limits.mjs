// Node gate for the `.ifczip` zip-bomb guard in the browser build (GH #175).
//
//   node crates/wasm/test/limits.mjs
//
// ifcfast.com parses dropped files in the visitor's tab. The site's
// 300 MB drop cap checks the COMPRESSED size, which is exactly what a
// zip bomb defeats: ~65 KB of deflate expands to 64 MB of `'0'`, and a
// slightly larger one takes the tab down. The bound therefore has to
// live at the decompression choke-point (`source::decompress_ifczip`),
// which is the same code the native wheel runs.
//
// Archives are hand-built here rather than committed as fixtures — a
// bomb fixture is a large file by definition, and hand-building lets us
// forge a LYING uncompressed-size header, which no ZIP writer will emit
// and which is precisely the case the streaming cap exists for.

import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
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

// ---------------------------------------------------------------------
// minimal ZIP writer (so we can lie in the headers)
// ---------------------------------------------------------------------

let CRC_TABLE = null;
function crc32(buf) {
  if (!CRC_TABLE) {
    CRC_TABLE = new Int32Array(256);
    for (let n = 0; n < 256; n++) {
      let c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      CRC_TABLE[n] = c;
    }
  }
  let crc = -1;
  for (let i = 0; i < buf.length; i++) crc = (crc >>> 8) ^ CRC_TABLE[(crc ^ buf[i]) & 0xff];
  return (crc ^ -1) >>> 0;
}

/** entries: [{ name, data, declaredSize? }] — `declaredSize` forges the
 *  uncompressed-size field in both headers. */
function makeZip(entries) {
  const local = [];
  const central = [];
  let offset = 0;
  for (const e of entries) {
    const name = Buffer.from(e.name, 'utf8');
    const comp = zlib.deflateRawSync(e.data, { level: 9 });
    const crc = crc32(e.data);
    const declared = e.declaredSize ?? e.data.length;

    const lfh = Buffer.alloc(30);
    lfh.writeUInt32LE(0x04034b50, 0);
    lfh.writeUInt16LE(20, 4);
    lfh.writeUInt16LE(8, 8); // deflate
    lfh.writeUInt32LE(crc, 14);
    lfh.writeUInt32LE(comp.length, 18);
    lfh.writeUInt32LE(declared, 22);
    lfh.writeUInt16LE(name.length, 26);
    local.push(lfh, name, comp);

    const cdh = Buffer.alloc(46);
    cdh.writeUInt32LE(0x02014b50, 0);
    cdh.writeUInt16LE(20, 4);
    cdh.writeUInt16LE(20, 6);
    cdh.writeUInt16LE(8, 10); // deflate
    cdh.writeUInt32LE(crc, 16);
    cdh.writeUInt32LE(comp.length, 20);
    cdh.writeUInt32LE(declared, 24);
    cdh.writeUInt16LE(name.length, 28);
    cdh.writeUInt32LE(offset, 42);
    central.push(cdh, name);

    offset += lfh.length + name.length + comp.length;
  }
  const cd = Buffer.concat(central);
  const eocd = Buffer.alloc(22);
  eocd.writeUInt32LE(0x06054b50, 0);
  eocd.writeUInt16LE(entries.length, 8);
  eocd.writeUInt16LE(entries.length, 10);
  eocd.writeUInt32LE(cd.length, 12);
  eocd.writeUInt32LE(offset, 16);
  return Buffer.concat([...local, cd, eocd]);
}

// ---------------------------------------------------------------------
// checks
// ---------------------------------------------------------------------

const results = [];
function check(label, ok, detail = '') {
  results.push({ label, ok });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${label}${detail ? ` — ${detail}` : ''}`);
}

/** Run fromBytes and return the thrown message, or null if it succeeded. */
function throwsWith(bytes) {
  let m = null;
  try {
    m = IfcModel.fromBytes(bytes, 'drop.ifczip');
  } catch (e) {
    return e?.message ?? String(e);
  }
  m.free();
  return null;
}

const ZEROS = Buffer.alloc(64 * 1024 * 1024, 0x30); // 64 MiB of '0'

// 1. Honest bomb: ~65 KB packed, 64 MiB inflated (~1000x).
{
  const zip = makeZip([{ name: 'bomb.ifc', data: ZEROS }]);
  const msg = throwsWith(zip);
  check(
    'zip bomb (64 MiB of one byte) is refused',
    msg !== null &&
      msg.includes('bomb.ifc') &&
      msg.includes('expansion-ratio') &&
      msg.includes('max_expansion_ratio'),
    msg === null ? 'fromBytes SUCCEEDED — the tab would be the victim' : msg,
  );
  console.log(
    `INFO  packed ${zip.length} B → declared ${ZEROS.length} B (${(ZEROS.length / zip.length).toFixed(0)}x); ` +
    'the site\'s 300 MB drop cap sees only the packed number',
  );
}

// 2. Lying header: declares 1 KB, inflates to 64 MiB. The declared-size
//    pre-check is defeated on purpose; the streaming cap must hold.
{
  const zip = makeZip([{ name: 'liar.ifc', data: ZEROS, declaredSize: 1024 }]);
  const msg = throwsWith(zip);
  check(
    'a member that LIES about its uncompressed size is still refused',
    msg !== null && msg.includes('liar.ifc') && msg.includes('.ifczip:'),
    msg === null ? 'fromBytes SUCCEEDED on a forged header' : msg,
  );
}

// 3. Container walk bounds.
{
  const many = [];
  for (let i = 0; i < 5000; i++) many.push({ name: `m${i}.ifc`, data: Buffer.from('x') });
  const msg = throwsWith(makeZip(many));
  check(
    'a 5000-member archive is refused before the directory walk',
    msg !== null && msg.includes('max_members'),
    msg ?? 'fromBytes SUCCEEDED',
  );
}
{
  const msg = throwsWith(makeZip([{ name: `${'a'.repeat(2000)}.ifc`, data: Buffer.from('x') }]));
  check(
    'a 2 KB member name is refused',
    msg !== null && msg.includes('max_name_len'),
    msg ?? 'fromBytes SUCCEEDED',
  );
}

// 4. The guard is invisible to a genuine archive.
{
  const step = fs.readFileSync(path.join(repo, 'tests/fixtures/minimal.ifc'));
  const zip = makeZip([{ name: 'minimal.ifc', data: step }]);
  let ok = false;
  let detail = '';
  try {
    const m = IfcModel.fromBytes(zip, 'minimal.ifczip');
    const s = JSON.parse(m.summaryJson());
    ok = s.schema === 'IFC4' && s.products > 0;
    detail = `schema=${s.schema} products=${s.products} ratio=${(step.length / zip.length).toFixed(2)}x`;
    m.free();
  } catch (e) {
    detail = e?.message ?? String(e);
  }
  check('a real .ifczip still opens at the default limits', ok, detail);
}

const failed = results.filter((r) => !r.ok);
console.log(`\n${results.length - failed.length}/${results.length} checks passed`);
process.exit(failed.length ? 1 : 0);
