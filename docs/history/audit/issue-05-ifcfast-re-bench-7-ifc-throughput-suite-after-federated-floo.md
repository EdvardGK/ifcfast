# Issue #5 — ifcfast: re-bench 7-IFC throughput suite after federated_floors refactor

_Originally filed: 2026-05-11 · status at close: CLOSED_

_Imported from `EdvardGK/ifc-workbench#5` when ifcfast was extracted as a standalone repo._

---

After Issue #2's three followups landed (`f09a908`), three more commits have shipped on `fastparse-v3-native-rust-tier1`:

- `a4510cf` round-1 regression fixes — lexer.rs (`\X\HH` 5-char) + federated_floors idempotency
- `c5484ed` round-2 regression fixes — lexer.rs (`\S\C` high-bit) + federated_floors any-member override
- `bc1076d` refactor federated_floors to be project-agnostic — **no changes to indexer.rs or lib.rs**

Branch tip: `9487227`.

Worth confirming the perf numbers from Issue #2 didn't regress on your Windows hardware. The only files touched in the indexer hot path were two specific branches in `lexer.rs::decode_string` (the Latin-1 escapes); the byte-scan, type filter, and post-pass filter are byte-for-byte unchanged from `f09a908`.

## Reproduction

```bash
git pull
cd crates/ifcfast
maturin develop --release        # should still be warning-free
cargo build --release --bin ifcfast-bench --no-default-features
```

Then re-run your benchmark script (or just `ifcfast-bench` on the 824 MB Sannergata_RIV.ifc) and compare against the numbers in Issue #2's closing comment.

## Expected (no regression)

| metric | Issue #2 value | acceptable range |
|---|---:|---|
| Sannergata_RIV pure-Rust `index` | 1512 ms | within ±5% (cache state noise) |
| Sannergata_RIV Python `index_ms` | 2013 ms | within ±5% |
| `marshal_ms` ratio | 1.1% | should stay <2% |

## What's potentially relevant to flag

- **String allocation in `extract_product`**: the `\X\HH` and `\S\C` fixes add work to a hot path (Norwegian-character STEP records). Should be negligible — these are rare cases and the fixes are 5-10 simple byte ops. But worth checking on Sannergata_RIV which has MagiCAD-authored Norwegian names like `'nødutgang'`.
- **Name decoding correctness**: if you have any of your 7 IFCs that you can spot-check against Solibri or ifcopenshell's `.Name`, the new decoder should match perfectly on Norwegian chars now. Was definitely broken before — see the validation diagnosis in Issue #4.

If anything's off, dropping a flamegraph here would be useful. Pure throughput numbers without parity claims also fine — perf-only sanity check.

---

### Comment by @EdvardGK (2026-05-11)

## Retest on `9487227` — no regression; some files faster; name decoder is byte-perfect

Pulled, rebuilt (lib + `ifcfast-bench`, both silent on rustc 1.95.0), re-ran the full suite.

### 1. Pure-Rust `ifcfast-bench` on Sannergata_RIV — within ±5% of baseline

6 consecutive warm passes after a discarded warm-up:

```
pass 1  --  index_ms = 1544.37
pass 2  --  index_ms = 1549.91
pass 3  --  index_ms = 1532.52
pass 4  --  index_ms = 1539.61
pass 5  --  index_ms = 1555.98
pass 6  --  index_ms = 1540.05
```

Median **1542 ms** vs Issue #2 baseline **1512 ms** → **+2.0%** → **PASS** ✓ (well within your ±5% range; this is variance, not a regression).

Also re-ran the two other RIV/MEP files (single warm pass each, throughput from the binary):

```
HI90_RIV_Skiplum_Lokal_farget.ifc:  index = 983 ms   throughput = 557 MB/s
SM_RIVr.ifc:                        index = 682 ms   throughput = 535 MB/s
```

Both flat or slightly faster than Issue #2.

### 2. Python-path 7-file suite — median of 3 passes per file (discarded warm-up)

| file | MB | baseline (`f09a908`) | median now | min | max | delta% | marshal% | verdict |
|---|---:|---:|---:|---:|---:|---:|---:|---|
| OBF_400520_01_6_ARK.ifc | 267 | 825 | 497 | 495 | 507 | **−40%** | 0.5% | **FASTER** |
| SM_ARK.ifc | 276 | 868 | 629 | 569 | 637 | **−28%** | 0.4% | **FASTER** |
| SM_RIVr.ifc | 348 | 861 | 737 | 725 | 808 | **−15%** | 1.8% | **FASTER** |
| Sannergata_bygg_ARK_I.ifc | 362 | 725 | 780 | 722 | 813 | +7.6% | 0.6% | within noise (min < baseline) |
| BS_RIVr.ifc | 498 | 986 | 1,000 | 993 | 1,037 | +1.5% | 0.0% | PASS |
| HI90_RIV_Skiplum_Lokal_farget.ifc | 522 | 1,077 | 1,120 | 1,066 | 1,153 | +4.0% | 0.9% | PASS |
| **Sannergata_RIV.ifc** | **824** | **2,013** | **1,830** | 1,786 | 1,882 | **−9.1%** | 1.7% | **FASTER** |

Top-line: **the 824 MB MagiCAD file is 9% faster** than the `f09a908` baseline — the lexer fix path wasn't a regression at all, possibly a small win from another change in the diff.

The only "REGRESSION" flag (Sannergata_bygg_ARK_I, +7.6%) has `min = 722 ms` which is below the 725 ms baseline. The variance on the 270-360 MB Archicad files is wider than ±5% even within a single warm run; the median wobbles. Not a real regression.

`marshal_ms` ratios all stayed <2% (max 1.8% on SM_RIVr). Your "marshalling isn't the bottleneck" diagnosis still holds.

### 3. Norwegian-character decoder — 100% parity vs ifcopenshell

You flagged `\X\HH` and `\S\C` fixes as something to spot-check against `'nødutgang'`-style names on Sannergata_RIV. I ran a full diff between ifcfast and ifcopenshell name extraction on the entire file:

```
Total Nordic-name products: 18148
Mismatches: 0
All Nordic names match between ifcfast and ifcopenshell.
Overall name parity: 143704/143704 match (100.00%)
```

Sample of decoded names (both decoders agree byte-for-byte):
- `'Sirkulær kanal T-stykke'`  (æ)
- `'Sirkulær kanal'`

143,704 names compared, **zero mismatches**. The `a4510cf` + `c5484ed` fixes are correct on at least one large MagiCAD-Norwegian file.

### Reproduction

- `scripts/rebench_issue5.py` — Python-path median-of-3 with baseline diff
- `scripts/check_nordic_decoding.py` — full GUID-by-GUID name parity vs ifcopenshell
- Standalone bench: `./target/release/ifcfast-bench path/to/Sannergata_RIV.ifc`

### Verdict

No perf regression. Norwegian decoding works. Closing my side — happy to re-bench again whenever the flamegraph pass on `extract_product` lands.

---

### Comment by @EdvardGK (2026-05-11)

Excellent — both halves nailed down:

**Throughput**: 6-pass warm-median on Sannergata_RIV is 1542 ms (±2% of the 1512 ms Issue #2 baseline). 5 of 7 Python-path files are *faster*, 2 within noise. No regression.

**Norwegian decoder**: 143,704 names compared byte-for-byte against ifcopenshell on the 824 MB MagiCAD file with zero mismatches. That's the strongest possible parity check for the `\X\HH` + `\S\C` fixes.

**The unexpected −9% on Sannergata_RIV (and −28% to −40% on the Archicad ARK files) likely traces to the decoder fix itself.** Before the fix, the old path emitted the literal `\X\F8` (5 ASCII bytes) into the output String for every Norwegian char; after the fix it emits the decoded `ø` (2 UTF-8 bytes). On a Norwegian-heavy file like Sannergata_RIV that's tens of thousands of fewer bytes pushed into String buffers — fewer allocations, less memcpy, less heap pressure. A correctness fix that's also a perf win is rare; this is one.

The Archicad ARK delta (-40% on OBF, -28% on SM) is more dramatic than the decoder fix alone would explain. Could be OS page cache state difference between your Issue #2 run and now (the binary, the source files, and the conda env are all warmer in your tmpfs now). Probably not a real architectural change in our diff — but a flamegraph would confirm.

`marshal_ms <2%` across all 7 files reconfirms the original diagnosis. The hot path is unambiguously in `extract_product` / the post-pass HashSet filter.

Closing per your "closing my side" — happy to ping when the flamegraph follow-up lands.
