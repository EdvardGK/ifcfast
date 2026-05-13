# Issue #1 — ifcfast: clean up two unused-code warnings (indexer.rs:11, lexer.rs:395)

_Originally filed: 2026-05-11 · status at close: CLOSED_

_Imported from `EdvardGK/ifc-workbench#1` when ifcfast was extracted as a standalone repo._

---

## Two unused-code warnings in `crates/ifcfast`

`cargo build --release` against `crates/ifcfast` on `fastparse-v3-native-rust-tier1` (rustc 1.95.0, MSVC) emits two warnings. They don't affect output, but easy to clean up.

### 1. Unused `self` import — `src/indexer.rs:11`

```
warning: unused import: `self`
  --> src\indexer.rs:11:5
   |
11 |     self, data_section_start, endsec_position, for_each_record, parse_field, parse_ref_list,
   |     ^^^^
   |
   = note: `#[warn(unused_imports)]` (part of `#[warn(unused)]`) on by default
```

Drop the leading `self,` from the use-list.

### 2. Dead `Other` variant field — `src/lexer.rs:395`

```
warning: field `0` is never read
   --> src\lexer.rs:395:11
    |
395 |     Other(&'a [u8]),
    |     ----- ^^^^^^^^
    |     |
    |     field in this variant
```

Either remove `Other`, change it to `Other(())`, or `#[allow(dead_code)]` it if you want to keep the byte slice around for future use.

### Environment

- Branch: `fastparse-v3-native-rust-tier1` @ HEAD
- Toolchain: stable-x86_64-pc-windows-msvc, rustc 1.95.0, cargo 1.95.0
- Build via `maturin develop --release` with pyo3 0.22.6, memmap2 0.9.10, memchr 2.8.0

---

### Comment by @EdvardGK (2026-05-11)

Fixed in `f09a908` on `fastparse-v3-native-rust-tier1`.

- `src/indexer.rs:11` — dropped the unused `self,` from the `use` list.
- `src/lexer.rs:395` — kept `Field::Other(&'a [u8])` (the raw bytes are useful when debugging unrecognised fields) and silenced the warning with `#[allow(dead_code)]` on the variant + a one-liner comment explaining why.

`cargo build --release` and `maturin develop --release` are now both warning-free on this branch (rustc 1.95.0). Safe to close once you pull and rebuild.

---

### Comment by @EdvardGK (2026-05-11)

Verified on Windows by EdvardGK — warning-free build confirmed. Closing.
