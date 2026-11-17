# TASK-32 — Typst compile target (`.typ` via the `typst` CLI)

Status: **landed** — `crates/engine/src/typst.rs` routes `.typ`
files through `document.compile` to `typst compile --root
--diagnostic-format short`, with `--package-cache-path` pinned at
`.warraqcraft/vendor/typst-cache` (project-local `@preview`
packages) and `--package-path` wired when
`.warraqcraft/vendor/typst-packages/` exists. Verified live against
typst 0.15.1: hello.typ → PDF, bad.typ → parsed file:line diagnostics.
`WARRAQCRAFT_TYPST` env override mirrors `WARRAQCRAFT_TECTONIC`.
Recon was done against typst 0.15.1 (portable Windows binary,
MIT/Apache-2.0). Community ask (Andrés, Texpile dossier): dual
LaTeX+Typst support in one project.

## Recon findings (typst 0.15.1, verified live)

- `typst compile [--root DIR] [--diagnostic-format short]
  [--package-path DIR] [--package-cache-path DIR] <in> <out.pdf>` —
  single invocation produces the PDF; no latexmk-style rerun loop.
- `--diagnostic-format short` emits `\\?\<abs>\file.typ:LINE:COL:
  error: msg` — trivially parseable into the existing `Diagnostic`
  (strip the `\\?\` verbatim prefix, split on `:`).
- `--root` anchors absolute paths inside the project — pass the
  document's parent dir, same as the LaTeX path's `current_dir`.
- Packages: `#import "@preview/<ns>/<pkg>:<ver>"` auto-downloads into
  a cache dir on first compile. `--package-cache-path` can redirect
  that cache **into the project** (`.warraqcraft/vendor/typst-cache`)
  → project-local, committable packages with zero new code. There is
  **no offline/lock flag** in 0.15.1 — first compile still needs
  network for uncached packages; document, don't fake it.
- `--package-path DIR` accepts vendored local packages (layout =
  typst package cache shape); wire it only when
  `vendor/typst-packages/` exists — v1 ships cache-path only.
- Fonts: typst embeds its own set and searches system fonts; no
  `--font-path` needed for v1 (Craft Fonts can come later).
- No SyncTeX equivalent — PDF→source navigation stays absent for
  `.typ`; the PDF preview pane works unchanged (it renders whatever
  PDF the compile produced).
- License: MIT/Apache-2.0 — invoking the binary is clean-room-safe;
  the `typst` *crate* (library embedding) is also MIT but pulls a
  huge dep tree — **deferred**, binary-first like system TeX.

## Design

### Routing

`document.compile` inspects the target filename: `.typ` → Typst
path, everything else → existing TeX engine selection unchanged.
`% !TeX program` magic stays LaTeX-only; a `// !typst`-style hint is
NOT added v1 (extension is unambiguous enough, YAGNI).

### `crates/engine/src/typst.rs` (new, ~250 lines)

- `compile_typst(sess, params)` — mirror `compile_file_with`'s
  contract: resolve parent (reuse `packages::parent_of`-style
  absolutizing), outdir = `.warraqcraft/build`, output
  `<stem>.pdf`.
- Argv: `typst compile --root <parent> --diagnostic-format short
  [--package-cache-path <proj>/.warraqcraft/vendor/typst-cache]
  <input> <out>` — cache path added only when the dir exists or is
  creatable; `--package-path` when `vendor/typst-packages/` exists.
- Parse `short` diagnostics → `Diagnostic { file, line, column,
  message, severity }` — reuse `logrules`-adjacent parsing but as a
  small dedicated parser (typst format ≠ TeX log format).
- No bib/rerut/biber passes — typst handles everything in one pass.
- `NoTypstEngine`-style error when the binary is absent, naming the
  fix (`winget install typst` / distro package) — same honesty as
  `NoTexEngine`.
- wasm: `Err(Tool("typst compile is not supported on wasm"))`.

### Toolchain surface

- `KNOWN_TOOLS` += `"typst"` so probes/UI tool lists see it;
  `detect_engines()` unchanged (typst is not a TeX-engine choice).

### Package story (parity with TASK-31, minimal v1)

- `--package-cache-path <proj>/.warraqcraft/vendor/typst-cache`
  whenever the project opts in by having the dir (or always —
  typst creates it; always-set is simpler and keeps packages
  project-local by default → better local-first story).
  **Decision: always set it** — typst creates the dir itself;
  packages then live in the project, committable, offline after
  first fetch.
- `typst.toml`/`typst.lock`: not enforced v1 (typst has no lockfile
  yet); manifest integration deferred.
- `pkg.*` commands stay LaTeX/CTAN-scoped in v1; a `pkg.typst.*`
  namespace is a later task if demanded.

### UI/preview reality

- `.typ` opens in the same editor (plain text), Compile button works
  via `document.compile` — zero UI work for v1.
- PDF preview pane renders the typst PDF as-is.
- Problems pane shows parsed typst diagnostics with file:line —
  click-to-jump works since `Diagnostic.file/line` are populated.
- `doc.preview` (native typeset) stays LaTeX-only; typst preview =
  external compile, same as system-TeX path today.

## Non-goals (v1)

- No `typst` crate embedding (dep-tree cost, revisit if binary UX
  proves limiting).
- No Typst→LaTeX or LaTeX→Typst conversion.
- No `typst watch` long-running mode (compile-on-demand matches the
  existing pipeline).
- No typst-specific completion/outline beyond what the generic
  structure scan already gives (typst headings are `= Title` —
  `texparse` doesn't read them; outline for .typ is a later task).
- No `@preview` registry browsing UI — packages arrive via
  `#import` in source, cache-path keeps them local.

## Commit split

- `TASK-32a` — toolchain probe + `typst.rs` skeleton + `document.compile`
  routing + diagnostics parser + unit tests (short-format parsing,
  argv construction, missing-binary error).
- `TASK-32b` — package-cache-path wiring + live smoke on this
  machine's typst 0.15.1 (hello.pdf + bad.typ diagnostics verified
  manually already).
- `TASK-32c` — ROADMAP/log + spec status update.

## Risks

- Diagnostic `short` format stability across typst versions — parser
  must degrade to "unparsed line" not panic (hostile-input rule).
- `--package-cache-path` semantics on old typst versions (<0.12?) —
  gate on `typst --version` ≥ 0.12 or just document minimum version.
- typst binary absence is the common case early on — error must be
  as helpful as `NoTexEngine`'s.
