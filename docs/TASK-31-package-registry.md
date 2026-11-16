# TASK-31 — Package registry, manifest and install (`texpkg` L3)

Status: **landed** — 31a registry, 31b manifest+vendor, 31c
`pkg.list`/`pkg.audit`/`pkg.resolve`/`pkg.search`, 31d `pkg.install`
(tlmgr→miktex/mpm, CTAN `miktex`/`texlive` name-mapping fallback, kpsewhich
verified), 31e `pkg.vendor` (CTAN `.tds.zip` → full TDS tree in
`vendor/texmf`, sha256+URL provenance), 31f logs-sheet Install action
(worker thread, 16-language status, auto-recompile). Compile exposes
the vendor tree via `TEXINPUTS` (+ `TEXMFHOME` on web2c engines).
Verified live on MiKTeX: `pkg.vendor` fetched siunitx's `/install/`
archive (full TDS tree, sha256+URL provenance), vendored `.sty`
shadows the system copy, and a doc compiled against it. `pkg.install`
was exercised against `miktex packages install`/`mpm` with
kpsewhich verification across `.sty`/`.cls`/`.tex`/`.def`/`.cfg`
(plain-TeX packages have no `.sty`). Remaining limits: packages
without a CTAN `install` zip still need a system package manager;
vendored *fonts* resolve only on web2c/kpathsea engines (MiKTeX
ignores `TEXMFHOME`); engines without kpathsea (Tectonic's fixed
bundle) cannot see the vendor tree at all; no Typst `@preview`
packages yet.
Inspired by the Texpile gap analysis (`analysis/texpile/DOSSIER.md`):
Texpile has no package management at all — this is an open lane.

## What already exists (verified in tree)

- `crates/texpkg` — static completion catalog (`&'static` tables, zero
  deps). Each `CommandSpec` carries a `package` field; `packages()`
  already yields the distinct package list. This is the seed of the
  registry, not the registry.
- `crates/texparse/src/structure.rs` — already extracts
  `\usepackage`/`\RequirePackage` package names per file.
- `crates/engine/src/logrules.rs` — `missing-file` diagnostics on
  `File `X.sty' not found` already populate `diag.package` via
  `suggested_package()`. The UX hook exists; nothing acts on it yet.
- `crates/engine/src/toolchain.rs` — probes `tectonic`, `xelatex`,
  `lualatex`, `pdflatex`, `bibtex`, `biber`, `latexmk`, `synctex`, `git`
  with well-known-dir fallback. Extend `KNOWN_TOOLS` for package
  managers (`tlmgr`, `miktex`, `mpm`).
- `crates/engine/src/lib.rs` — compile already injects `BIBINPUTS`/
  `BSTINPUTS` for the source dir; `TEXINPUTS` follows the same pattern.
- `.warraqcraft/` project dir already holds `ai/` state and the build
  dir — the manifest lives there.

## Design

### 1. `texpkg` registry (L3, zero-dep preserved)

Grow the catalog into a package registry without breaking its
`&'static`/no-dependency contract:

```rust
pub struct PackageInfo {
    pub name: &'static str,        // CTAN package name
    pub files: &'static [&'static str], // .sty/.cls it provides
    pub summary: &'static str,     // one-line, authored from CTAN abstracts
    pub catalog_range: (usize, usize), // slice of CATALOG for completion
}

pub fn registry() -> &'static [PackageInfo];
pub fn resolve(sty_or_cls: &str) -> Option<&'static PackageInfo>; // "amsmath" -> amsmath
pub fn search(query: &str) -> impl Iterator<Item = &'static PackageInfo>;
```

Seed from the `package` field already in `CATALOG`, plus a curated
top-up (amsmath, graphicx, hyperref, biblatex, polyglossia, arabdoc,
ctable, booktabs, siunitx, tikz/pgf, fontspec, microtype, geometry,
setspace, enumitem, cleveref, natbib, glossaries, tcolorbox, minted…).
Summaries authored from CTAN abstracts — never copied from other
editors' package lists.

Registry stays data-only. **No network in `texpkg`.**

### 2. Manifest + vendor dir

- `.warraqcraft/packages.toml`:
  ```toml
  [packages]
  amsmath = { source = "ctan", version = "2.17" }   # pinned when known
  mypkg    = { source = "vendor", path = "vendor/mypkg" }
  ```
- `vendor/` holds fetched packages as a TDS tree under `vendor/texmf/`,
  committed by the user (local-first: the project must compile offline
  once vendored).
- Compile sets two env vars when `vendor/texmf/` exists:
  `TEXINPUTS=<root>/tex//` (recursive `.sty`/`.cls` resolution —
  honored by MiKTeX *and* web2c; verified live) and
  `TEXMFHOME=<root>` (whole-TDS incl. fonts — web2c/kpathsea only;
  MiKTeX ignores it since its file DB covers registered roots only).
  Existing values are preserved. Engines without kpathsea (Tectonic)
  ignore both; system TeX is required for vendoring.

### 3. Commands (engine → CLI/control/MCP parity, per AGENTS.md)

| Command | Job |
| --- | --- |
| `pkg.list` | merged view: requested (`\usepackage` scan) vs resolved (kpsewhich/system) vs vendored |
| `pkg.resolve <name>` | registry lookup → CTAN name, provided files, install hint |
| `pkg.audit` | `\usepackage` set minus installed minus vendored → missing list |
| `pkg.install <name>` | install path below |
| `pkg.vendor <name>` | fetch package files into `vendor/` + manifest entry |

### 4. `pkg.install` — two paths, probed in order

1. **System package manager** (preferred — gets updates, fonts, docs):
   - TeX Live: `tlmgr install <pkg>` (tlmgr maps CTAN name → collection;
     for single packages `tlmgr install <ctan-name>` works directly)
   - MiKTeX: `miktex packages install <pkg>` (legacy `mpm --install=` kept as fallback) — or rely on MiKTeX's existing
     auto-install-on-missing (observed in Texpile recon: MiKTeX built
     the lualatex format unprompted; `--enable-installer` behavior may
     already cover .sty on demand — **verified live**: removing a
     package and re-querying it resurrects it via on-the-fly install)
   - Tectonic: no package manager — falls through to vendor path
2. **Vendor fetch** (fallback + tectonic path):
   - CTAN JSON API `https://www.ctan.org/json/pkg/<name>` for metadata;
     file list → download from a CTAN mirror into `vendor/<name>/`.
   - **Open verification:** confirm the API's file enumeration covers
     all runtime files (.sty/.cls/.def/.cfg/shipped .tex inputs), not
     just docs. If incomplete, restrict v1 to packages whose file list
     is fully enumerable, or fetch via the mirror's package tarball.
   - All fetches recorded in manifest with source URL + sha256.

### 5. UI hook (small)

- `missing-file` diagnostics ending in `.sty`/`.cls` already carry
  `diag.package` → surface an "Install package" action in the Problems
  pane → `pkg.install`. Defer if UI work grows; CLI/MCP path ships first.

## Explicit non-goals (v1)

- No TeX Live bundling or downloading (B-lite stays: system TeX +
  bundled Tectonic).
- No `tlmgr`/`mpm` *updates* of the whole distro — install-only.
- No Typst `@preview` packages yet (Track A prerequisite).
- No GUI package browser — `pkg.list`/`search` via CLI/MCP is enough.
- No automatic install on compile — always explicit user action
  (security: `.tex` is hostile input per AGENTS.md).

## Risks / honest unknowns

- **CTAN API coverage** — SPIKED 2026-11-13: `ctan.org/json/pkg/<name>`
  returns `ctan.path`, `miktex`/`texlive` names, and — for most
  packages — `install: "/macros/latex/.../<pkg>.tds.zip"`. **The zip
  lives under `mirrors.ctan.org/install/`**, not the tex-archive root
  (the bare path 404s — found via the pkg page's download link).
  Archives come in two layouts: legacy `texmf-dist/`-rooted and
  current already-TDS-rooted; extraction normalizes both and keeps
  only standard TDS top-level categories. **Gap:** some
  packages (biblatex, enumitem, cleveref) have no `install` zip — v1
  falls back to system-manager install or reports vendor-unsupported.
  `TEXINPUTS=<vendor>/texmf/tex//` covers `tex/**` on all kpathsea
  engines; `TEXMFHOME=<vendor>/texmf` additionally exposes fonts/doc
  on web2c engines (MiKTeX ignores it — its file DB covers registered
  roots only, so vendored fonts are a web2c-only feature for now).
- **Name mapping** — `\usepackage{foo}` ↔ CTAN `foo` is usually 1:1 but
  not always (e.g. `biblatex` vs collection names); registry `resolve`
  absorbs the common cases, `pkg.resolve` reports uncertainty.
- **MiKTeX auto-install** confirmed live: `kpsewhich` on a missing
  package triggers on-the-fly installation, so `pkg.install`'s
  already-present check often short-circuits — the manager path is
  the fallback for engines/configs without it.
- **License/provenance** — vendored files keep their CTAN licenses;
  manifest records source; `NOTICE` may need a line for shipped
  vendored content if any template vendors packages.

## Suggested commit split (one task id per commit)

- `TASK-31a` — `texpkg` registry types + seed data + `resolve`/`search`,
  unit tests (sorted-table invariant like catalog's).
- `TASK-31b` — `.warraqcraft/packages.toml` + `vendor/` + `TEXINPUTS`/
  `TEXMFHOME` injection in compile prep.
- `TASK-31c` — `pkg.list`/`pkg.audit`/`pkg.resolve` commands.
- `TASK-31d` — `pkg.install` system-manager path (tlmgr/miktex probe).
- `TASK-31e` — `pkg.vendor` CTAN fetch path + manifest provenance.
- `TASK-31f` — Problems-pane "Install" action (optional, UI pass).
