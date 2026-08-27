# Third-Party Licenses

Kuznor includes and depends on third-party open-source software.

Kuznor itself is licensed under the **GNU General Public License v3.0 only (GPL-3.0-only)**.

Third-party components retain their original licenses. Nothing in the Kuznor license replaces, relicenses, or removes the obligations of those third-party licenses.

This document provides a practical license inventory for the dependencies used by Kuznor v0.1.0 Alpha.

> This file is informational and should not be treated as legal advice.  
> For redistribution, always preserve the original license and notice files required by each dependency.

---

## Main Rust dependencies

| Component | License |
|---|---|
| anyhow | MIT OR Apache-2.0 |
| chrono | MIT OR Apache-2.0 |
| crossbeam-channel | MIT OR Apache-2.0 |
| eframe | MIT OR Apache-2.0 |
| egui | MIT OR Apache-2.0 |
| hex | MIT OR Apache-2.0 |
| lopdf | MIT |
| reqwest | MIT OR Apache-2.0 |
| rfd | MIT |
| rusqlite | MIT |
| serde | MIT OR Apache-2.0 |
| serde_json | MIT OR Apache-2.0 |
| sha2 | MIT OR Apache-2.0 |
| tempfile | MIT OR Apache-2.0 |
| winres | MIT |

---

## Additional license families present in transitive dependencies

The dependency graph for Kuznor also includes packages under the following license families and combinations:

- Apache-2.0
- MIT
- BSD-2-Clause
- BSD-3-Clause
- ISC
- Zlib
- BSL-1.0
- CC0-1.0
- 0BSD
- Unlicense
- Unicode-3.0
- OFL-1.1
- Ubuntu-font-1.0
- Apache-2.0 WITH LLVM-exception
- combinations such as `Apache-2.0 OR MIT`

No AGPL-licensed dependency was reported by the project's current `cargo license` inventory.

---

## Components requiring special notice attention

### encoding_rs

Reported license expression:

```text
(Apache-2.0 OR MIT) AND BSD-3-Clause
```

When redistributing binaries that include this dependency, preserve the applicable license and attribution notices.

### epaint_default_fonts

Reported license expression:

```text
(Apache-2.0 OR MIT) AND OFL-1.1 AND Ubuntu-font-1.0
```

This package includes font assets with their own licenses.

Redistributions of Kuznor that include these font assets should preserve the corresponding Open Font License and Ubuntu Font License notices as required by those licenses.

### unicode-ident and ICU/Unicode-related crates

Some Unicode-related dependencies are reported under:

```text
Unicode-3.0
```

or combinations that include Unicode-3.0.

Preserve the applicable Unicode license notice in redistributions where required.

---

## SQLite

Kuznor uses:

```text
rusqlite
libsqlite3-sys
```

The Rust wrappers retain their own licenses.

Kuznor currently enables:

```toml
rusqlite = { version = "0.33", features = ["bundled"] }
```

This causes SQLite to be built as part of the application rather than relying on a separately installed system SQLite library.

SQLite itself is generally distributed as public-domain software. The Rust wrapper crates remain under their respective licenses.

---

## llama.cpp

Kuznor uses **llama.cpp** as the local inference backend.

llama.cpp is third-party software and retains its own license.

If a Kuznor Windows portable package redistributes llama.cpp binaries, the corresponding llama.cpp license and notices must be included with that distribution.

Kuznor's GPL-3.0-only license does not replace the llama.cpp license.

---

## AI models and GGUF files

AI models are **not licensed under Kuznor's GPL license** merely because they are used with Kuznor.

Each model retains the license published by its original author or distributor.

Examples of models tested during development include:

- Gemma 3 4B Instruct
- Qwen3 4B
- Qwen3 1.7B
- nomic-embed-text-v1.5

Users and distributors are responsible for reviewing the license of each model before use or redistribution.

Kuznor does not automatically download or redistribute GGUF models in v0.1.0 Alpha.

---

## Windows resources

Kuznor may use `winres` or equivalent Windows resource tooling to embed application metadata and icons.

Third-party Windows libraries or runtime components, if packaged in a future release, must retain their original license and notice files.

---

## Full dependency inventory

The exact dependency graph may change between releases.

For the current checkout, a license inventory can be regenerated with:

```bash
cargo license
```

For stricter automated license policy checks, the project may also use:

```bash
cargo deny check licenses
```

The generated output from these tools should be reviewed before each public binary release.

---

## Redistribution checklist

Before publishing a Kuznor binary release:

1. Include the Kuznor `LICENSE` file.
2. Include this `THIRD_PARTY_LICENSES.md` file.
3. Preserve required license and attribution notices from redistributed dependencies.
4. Include llama.cpp's license if llama.cpp binaries are bundled.
5. Preserve font-license notices for bundled font assets.
6. Do not claim third-party GGUF models are licensed under GPLv3.
7. Review the final packaged files, not only Cargo dependencies.
8. Re-run the dependency-license inventory for every release.

---

## Kuznor license

Kuznor source code is intended to be distributed under:

```text
GPL-3.0-only
```

Third-party components retain their respective licenses.

For the complete Kuznor license terms, see:

```text
LICENSE
```
