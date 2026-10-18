# WarraqCraft — product description

*Named after the* warrāq *(ورّاق) — the paper-makers, copyists and
bookbinders of the medieval Islamic manuscript trade. The craft of the
printed page, a thousand years before TeX.*

Clean-room, Rust-native LaTeX authoring and typesetting environment — the
LaTeX-class member of the ArtCraft family. Think *Overleaf-class UX on the
desktop*: a project tree, a focused editor, live PDF preview, version
history, and first-class reference-manager integration — with no server
account and no cloud dependency.

Reference target: Overleaf's editor surface (see `docs/reference/` notes;
screenshots of the public product used only as UX reference, never for
assets or code).

**Status: alpha.** Working today: the egui studio (LocalLeaf-class
three-pane layout — file tree, guttered syntax-highlighted editor, PDF
preview — with light/dark themes), compile through a bundled `tectonic`
(Track B-lite, no TeX install; `scripts/fetch-tectonic.ps1` drops the
binary in `bin/`) or any system TeX on PATH, log-driven reruns and
bibtex/biber passes, SyncTeX click-through both directions
(Ctrl+click source → PDF highlight, click PDF → caret jump), an opt-in
5-second-idle auto-compile, project-wide multi-file awareness, local
history, a multilingual RTL-aware UI, and a template gallery with
user-source import. Not yet: in-process typesetter (B/A′). See
[`ROADMAP.md`](ROADMAP.md) for the milestone plan.

## What it is

A user writes LaTeX; WarraqCraft keeps a real PDF preview one keystroke away.
Everything that makes Overleaf comfortable is in scope — and the UI goes
further: Overleaf's chrome is a decade old; WarraqCraft targets a **modern,
themeable design language** from day one.

- **Themes, not a toggle** — a real theme system (`theme::Tokens`, the
  same convention as the siblings): light, dark, high-contrast, and
  accent variants; per-pane theming (editor can be dark while the PDF
  stays paper-white); system-theme follow on Windows/macOS. Theme
  switching re-colors instantly — no restart.
- **Modern surface** — dockable/resizable panes, tab bar, command
  palette (Ctrl-Shift-P for every command id — free because everything
  is already a command), notification toasts, settings UI instead of
  config files, smooth scroll and cursor animation where egui allows.
- **Editor chrome** — the modern affordances: minimap, gutter with
  error/warning markers, inline diagnostics (squiggles over the
  offending source, not just a log pane), folding, breadcrumbs.
- **Typography & RTL in the UI itself** — UI fonts come from
  `craft-fonts`; the app's own chrome renders Arabic correctly
  (menus, dialogs, palette) — a themed Arabic UI is part of the
  product, matching the arabdoc story.

- **Project model** — a folder is a project. File tree with `.tex`, `.bib`,
  `.sty`, `figures/`; multiple files per project; `main.tex` detection.
- **Editor** — syntax highlighting for LaTeX/BibTeX, symbol palette,
  math-mode helpers, bracket matching, comment folding, breadcrumbs
  (chapter → section → subsection), equation preview on hover, editor
  tabs, focus mode, editing/reviewing modes.
- **PDF preview** — split view (editor | PDF), editor-only, PDF-only,
  pop-out window, zoom, presentation mode. **SyncTeX-style forward and
  inverse search**: click a line, jump to the PDF; Ctrl+click the PDF,
  jump back to source. This is the feature that makes a TeX IDE feel real.
- **Background compile** — debounced recompile on save/idle, error log
  parsed into a clickable list (file:line → editor jump), warning vs
  error vs overfull-hbox triage.
- **History** — per-project version timeline (git-backed), labeled
  snapshots ("before journal submission"), diff view between any two
  snapshots, one-click restore. Mirrors Overleaf's History panel but
  local and inspectable.
- **Insert/Format helpers** — figure wizard (file → `\includegraphics`
  with caption and label), table wizard, citation insertion (`\cite{}` with
  live key completion from the project's .bib files), cross-reference
  completion (`\ref{}` sees every `\label{}` in the project), list/
  section/format commands under menus — matching the Insert/Format
  menu structure in the reference screenshots.
- **Packages** — curated built-in set to start (article/report/book
  classes, amsmath, graphicx, hyperref, geometry, booktabs, float,
  multirow, enumitem, amssymb, titlesec) **plus the arabdoc suite**;
  later a package registry.

## Reference-management integration (headline feature)

Overleaf's integration panel (Zotero, Mendeley, ReadCube, GitHub,
Dropbox) is the model — but WarraqCraft's is better for local-first users:

- **Zotero** — live integration via the local HTTP API
  (`localhost:23119`) and **Better BibTeX** auto-export: point the
  project at the exported `.bib`, watch it for changes, recompile
  citations automatically. One-click "export this paper's keys to Zotero".
- **Mendeley / Zotero / EndNote import** — BibTeX, RIS and CSL-JSON
  importers; drag a library export into the project.
- **EndNote** — import only (`.enl` is a proprietary binary database;
  document the BibTeX/RIS export path rather than parsing it).
- **Citation picker** — fuzzy-searchable overlay over every project key
  (author/title/year), inserts the right `\cite` variant (`\cite`,
  `\parencite`, `\textcite` per package).
- **Bibliography backends** — BibTeX-style sorting/formatting plus a
  CSL (Citation Style Language) path so users can reformat a
  bibliography by dropping in a `.csl` file — the same styles Zotero
  and Mendeley ship, thousands of journals covered.

## Arabic & RTL — the built-in advantage

No LaTeX IDE treats Arabic as a first-class citizen. WarraqCraft does,
because the family already owns the hard parts:

- **`arabdoc` bundled** — the maintainer's own CTAN package
  ([`macros/xetex/latex/arabdoc`](https://ctan.org/tex-archive/macros/xetex/latex/arabdoc),
  LPPL): `arabdoc-setup` collapses the fragile 15–25-line bidi/
  polyglossia/fontspec preamble into one `\usepackage`;
  `arabdoc-article/book/thesis/report` classes, `arabdoc-glossary`
  and `arabdoc-bib` (Arabic biblatex styles). Ships inside WarraqCraft
  and is the reference for the app's RTL diagnostics — the editor can
  *catch* the classic `bidi loaded after X` ordering bug, not just
  report it.
- **Bidi rendering in the app itself** — the family already ships RTL
  display logic (`bidi::display_rtl` in the siblings' i18n), so Arabic
  source *and* preview read correctly everywhere in the UI.
- **Fonts** — Amiri / Scheherazade / Lateef / Amiri Quran via the
  shared `craft-fonts` convention (never vendored into a repo).
- **Islamic texts** — the maintainer already owns the whole stack:
  `quran-uthmani` (in-house package — all 114 surahs in proper
  Uthmani rasm, on-demand surah loading), `hadith` (in-house package
  v1.1.0 — the Seven Canonical Books, ~36,400 hadiths with tashkeel,
  English translations and grades, on-demand chapter loading), plus
  the CTAN `quran` package ecosystem (translations into 15+
  languages). WarraqCraft ships these as first-class builtins, not
  optional downloads.
- **Template intelligence** — the editor knows the arabdoc option set
  (`amiri`/`scheherazade`, `maghrib`/`arabicdigits`, `gregorian`/
  `islamic`, `legal-sections`) and autocompletes it.

## Template gallery — content is the moat

Overleaf's gallery is a large part of why people choose it. WarraqCraft's
answer is three-layered:

1. **Bundled starter library** — original, source-only templates covering
   English, Arabic, Persian, Urdu, French, Italian, German, Spanish,
   Portuguese, Turkish, Latin, Russian, Chinese, Japanese, Korean and
   Hindi. A larger curated collection can join once its license has been
   reviewed and its assets packaged.
2. **User/community imports** — an explicit importer accepts a template
   file or folder, copies only text sources (`.tex`, `.sty`, `.cls`,
   `.bib`, …), skips fonts, PDFs, binaries and build output, and records
   the caller-provided license/provenance. The CC-BY-NC Arabic template
   skill assets are therefore not bundled; users may import sources they
   are entitled to use, and their license obligations remain with them.
3. **Overleaf-gallery compatible** — gallery/CTAN templates are plain
   LaTeX source; WarraqCraft's template loader consumes the same
   "folder of .tex/.sty + metadata" shape, so users can drop in a
   downloaded Overleaf template and it just opens.

## Family conventions it inherits

- **Everything is a command** — `document.compile`, `document.open`,
  `cite.insert`, `figure.insert`, `project.snapshot`, `history.diff`,
  `bibliography.import`… reachable identically from CLI, the TCP control
  channel and MCP (same shape as MatCraft's `engine`/`mcp` crates).
- **Rust only**, no handwritten JS/TS, never break a future wasm target.
- **No panics in shipped code**; untrusted `.tex`/`.bib` input is fuzzed
  (LaTeX input is famously adversarial — `\write18`-style constructs are
  always disabled).
- **`warraqcraft-cli`** — headless: `warraqcraft-cli run main.tex`,
  `warraqcraft-cli --cmd 'document.compile {}'`, `functions`, `mcp`.
- **Thin UI** — egui studio over the command layer; JSON-lines TCP
  control channel (`engine.execute`, `ui.state`, `ui.render`,
  `ui.screenshot`, `ping`) like every sibling.
- **Packaging** — WiX MSI + portable zip, `C:\Program Files\WarraqCraft`;
  entry in the `artcraft-cli` skill and a numbered demo project.

## The hard problem: the typesetting engine (open decision)

The family convention is clean-room-from-scratch, but TeX is special:

| Option | License notes | Trade |
| --- | --- | --- |
| **A. Own TeX-subset typesetter** | clean-room, consistent with family | Huge: line breaking (Knuth–Plass is published), boxes/glue, math layout (appendix G is published). Years, not months — but we control everything (incremental retypeset, human-readable errors, clean wasm), and never touch GPL. |
| **B. Vendor the Tectonic core** | MIT — Tectonic already excised XeTeX's GPL/LGPL parts (kpathsea, poppler) and de-web2c'd the engine for Rust | Real Unicode TeX fast, **already Rust-hosted**, already handles Arabic via fontspec/bidi — exactly what arabdoc needs. Still a borrowed engine: no incremental typesetting, cryptic-error surface unchanged. **pdfTeX, LuaTeX, BibTeX, biber stay banned (GPL).** |
| **C. Wrap system TeX Live / MiKTeX** | zero shipped code | Nothing works out of the box without a 5 GB distro; fine as an *escape hatch* toggle ("use system TeX"), bad as the only path. **Now working** (M0.6): `document.compile` probes MiKTeX/TeX Live, full bibtex passes, hayro-rasterized preview. |
| **D. Build on `texlang`/`boxworks` (jamespfennell/warraqcraft)** | MIT | Existing "LLVM-style" modular TeX reimplementation in Rust: `texlang` (tokenizer, catcodes, real macro expansion, registers, conditionals — benchmarks ~5% slower than pdfTeX), `boxworks`+`-knuthplass`+`-hyphenate` (boxes/glue, line breaking), `tfm`, `dvi`. WASM-proven (play.warraqcraft.dev), `FileSystem`-trait embedding. **Gaps (verified by spike):** no e-TeX primitives so `latex.ltx` can't run, no shipout pipeline, no math typesetting — it executes TeX, doesn't yet emit pages. |

Spike result (validated by embedding, ~50 lines, git-dep pinned):
`texlang` + `texlang-stdlib` run real TeX semantics from our own process —
`\def`/#param macros, `\count`/`\advance`/`\the`, `\ifnum`/`\ifcase`,
`\csname`, `\input` through a swappable `FileSystem` trait — with
positioned caret diagnostics. One real hazard: macro expansion recurses
on the host stack with no depth bound (`\def\a{\a}\a` overflows), so any
embedded VM must run on a bounded worker thread.

Recommended path: **C is live now** (system TeX compiles + previews
today), **B as the shipping engine** (embedded Tectonic core — the product
works from a single MSI, no distro install), and **A becomes A′ — own
typesetter built on the texlang/boxworks foundation** rather than from
scratch: incremental retypesetting, real file:line diagnostics and clean
wasm are things a wrapped engine can never do, and the MIT license lets us
depend instead of copy. The differentiation analysis says the moat is the
family pipeline + RTL + templates, not engine ownership, so B/C buy the
product while A′ matures. Knuth–Plass line breaking and math typesetting
are published algorithms — clean-room is legitimate; the "TeX" name is
not claimed (name-restriction), product stays "WarraqCraft, LaTeX-compatible".

Note: "warraqcraft" (crate, repo, warraqcraft.dev) is already the JF project's
name — a product rename decision is deferred to before public v1.

Bibliography: implement BibTeX-semantics ourselves (the .bst stack
language is small) **or** go straight to a citeproc-style CSL pipeline —
decide with the user; CSL gives every reference-manager style for free.

## Crate sketch (mirrors MatCraft's layering)

```text
crates/texparse   — .tex lexer/parser, .bib parser, SyncTeX data, L0
crates/typeset    — boxes/glue/line-breaking/math layout, L1
crates/texpdf     — PDF writer (fonts, embedded subsets, outlines), L2
crates/texbib     — BibTeX/CSL-JSON/RIS import, .bst or CSL rendering, L2
crates/texpkg     — package/class registry + loader, L3
crates/engine     — command registry + Session guard, L5
crates/mcp        — JSON-RPC stdio server, L6
crates/ui-egui    — studio UI, L6
apps/warraqcraft-cli — headless CLI, L7
apps/warraqcraft     — studio app + TCP control server, L7
```

## Screen-by-screen scope (from the Overleaf reference)

| Screen | WarraqCraft equivalent |
| --- | --- |
| Project editor (file tree / editor / PDF) | Studio main window — three-dock layout, resizable |
| Projects dashboard | Launcher/home view: recent projects, new-from-template, search, tags |
| History panel | Snapshot timeline + diff + restore (git-backed) |
| Layout options | Split / editor-only / PDF-only / pop-out, focus mode |
| File/Edit/Insert/View/Format/Help menus | Same structure; Insert covers figure/table/citation/link/cross-ref/abstract/keywords/title wizards |
| Editing/Reviewing mode | Tracked-changes-lite: review mode adds margin comments + highlights edits |
| Integrations | GitHub/Git clone-sync, Zotero live API, Better BibTeX watch, Mendeley/EndNote/ReadCube import |

## Explicitly out of scope (v1)

- Real-time collaboration (no server; git is the collaboration story)
- Cloud compile, spell-check-as-a-service, paywalled templates
- Full LaTeX3/expl3 surface on day one — curate the common 80%
- Parsing proprietary library databases directly (`.enl` etc.)

## Open questions for discussion

1. Engine: recommendation is now **B (embedded Tectonic core) shipping,
   C optional, A as the R&D track** — confirm before scaffolding.
2. Bibliography backend: own .bst interpreter vs CSL pipeline vs both?
3. History: bundle a tiny git backend vs own snapshot format?
4. Editor: egui `TextEdit` (consistent, limited) vs a richer editor
   widget crate — syntax highlighting is needed regardless.
5. Name: **decision deferred to before public v1.** "warraqcraft" is taken
   (jamespfennell's project — crates.io, GitHub, warraqcraft.dev), separate
   from the TeX-trademark naming rule ("WarraqCraft" is a distinct product
   name, not claiming to be TeX — same convention as "MATLAB-class"
   wording). A rename also avoids `warraqcraft.exe` colliding with their
   `warraqcraft` CLI if we depend on their crates.
6. Template-library licensing: `Templates/` is already LPPL-1.3c —
   clean to ship. The `arabic-template` collection's skill wrapper is
   CC-BY-NC-4.0, but the maintainer owns it — relicensing the bundled
   selection is a decision, not a blocker.
