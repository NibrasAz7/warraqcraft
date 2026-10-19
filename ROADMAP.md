# WarraqCraft roadmap

**Stage: M0 — scaffold and parsing core.** The crate sketch and product
description are in `README.md`. Open decisions (engine option B vs A′ —
A′ now means building on MIT-licensed `texlang`/`boxworks` from
jamespfennell/warraqcraft, bibliography backend, history backend, editor
widget) are tracked there — nothing in M0 depends on them.

## Milestones

| Id | Scope | Status |
| --- | --- | --- |
| M0 | Repo scaffold (workspace, lints, xtask, AGENTS) + `texparse` (.tex lexer, structure scan, .bib parser) + `warraqcraft-cli parse/bib/check` | done |
| M1 | Engine decision landed; `texpkg` curated package set; minimal compile pipeline (`document.compile`) | in progress — system-TeX + bundled tectonic (B-lite) done; `texpkg` open |
| M2 | `engine` command registry + Session; CLI `--cmd` surface | in progress — Session + execute() landed; `--cmd`/control next |
| M3 | `ui-egui` studio: project tree, editor, PDF preview, SyncTeX nav | in progress — IDE shell, editor, PDF preview, SyncTeX click-through and project-wide outline/refs done |
| M4 | History (snapshots/diff/restore), reference-manager integration (Zotero/CSL), arabdoc + template gallery | in progress — snapshots/history and multilingual source-only template gallery landed; reference-manager integration open |
| M5 | Ship readiness: settings, print/export, untitled/autosave, platform packaging and docs | in progress |

## Progress log

- 2026-10-14 — product description written (`README.md`); repo scaffold +
  `texparse` started.
- M0.5 — minimal egui studio: editor, live outline/issues, open/save,
  theme toggle.
- M0.6 — `crates/engine` (Session + `execute()`, first commands) and a
  working PDF preview: system-TeX compile (`xelatex`/`lualatex`/`pdflatex`
  on PATH, `.warraqcraft/` build dir, no shell escape, bibtex pass when a
  bibliography exists), hayro rasterizes pages in the preview pane.
  `warraqcraft-cli compile` added. Repo published to `NibrasAz7/warraqcraft`.
- M0.7 — reconnaissance: observation-level REA/Ghidra survey of an
  installed TeXstudio (feature/subsystem inventory — BuildManager command
  constants, LatexDocument/LatexParser IDE model, QSynctex, BibTeXDialog;
  no implementation copied) and analysis of jamespfennell/warraqcraft (MIT),
  validated by a spike that embedded `texlang` via git dep and ran real
  TeX semantics (def/registers/ifs/csname/input, caret diagnostics; one
  gap: unbounded expansion stack → worker thread required). Plan updated:
  C live, B shipping, A′ = own typesetter on texlang/boxworks.
- M0.8 — LocalLeaf recon (MIT, github.com/leoaliperti/LocalLeaf — a
  shipped local-first Overleaf-style editor, `warraqcraft-core` Rust core +
  Tauri/Vue/CodeMirror/PDF.js frontend). Technique-level adoptions into
  `engine`: log-driven rerun loop (cross-refs resolve without a
  bibliography now), artifact-driven bibliography (`.bcf`→biber,
  `.aux \bibdata`→bibtex, `BIBINPUTS`/`BSTINPUTS`), and a `synctex`
  module wrapping the distro `synctex` CLI for source↔PDF jumps
  (verified on MiKTeX both directions). Their MCP surface
  (compile/read/write/list-engines) informs M2. Name note: "warraqcraft"
  is now taken twice (JF crate + LocalLeaf's `warraqcraft-core`).
- M3.1 — LocalLeaf-style IDE shell: dark palette, file-tree sidebar with
  main-doc star, guttered lexer-driven syntax highlighting, PDF preview
  toolbar (pager, zoom, fit), togglable outline + logs panels, status
  strip, engine picker on Recompile.
- M3.2 — SyncTeX click-through wired both directions (Ctrl+click source
  → PDF highlight + scroll; PDF click → caret, opens target file) with
  pt↔px mapping per page; opt-in auto-compile after 5 s idle (attempt-
  stamped, no failure hot-loop).
- M1.1 — B-lite: bundled `bin/tectonic.exe` (MIT, subprocess) detected
  before system TeX — zero-install compile incl. its internal
  bibtex/rerun loop; one `--keep-logs --synctex` invocation. Light/dark
  themes via `theme::Palette` + toolbar toggle.
- M0.9 — TeXstudio feature survey (REA/Ghidra, observational) →
  `docs/TEXSTUDIO-SURVEY.md` merge plan: magic comments, user tools,
  `.cwl`-style curated completion, folding, templates, log marks,
  inline math preview scheduled across M2–M4+.
- M3.3 — Painted sun/moon theme icon (pure Painter geometry, no font
  glyphs) replacing the text toggle.
- M1.2 — craft-libs adoption: `craft-atomic` crash-safe saves (tmp→
  fsync→rename) in Session::save + new-file dialog; `craft-quit`
  platform-correct quit hook; compile/tree polling moved into eframe
  `logic` so minimized windows keep working. Pinned rev `457a078`.
- M2/M3 Wave-1 — TeXstudio surface, clean-room (all four streams
  parallel-built, disjoint crates):
  * texparse `magic.rs` — `% !TeX program/root/encoding/bibliography/
    spellcheck/jobname` + `% !BIB program` header directives,
    `engine_hint()` normalization; engine consumes program→engine
    pick, root→master-file compile, BIB→bib-tool override; `magic.get`.
  * `texpkg` (new L3 crate) — curated 271-entry command/environment/
    package catalog (own data, not copied `.cwl`).
  * engine `tools.rs` — `user_tool.set/list/run/clear`: fixed-argv
    spawn (no shell), `{file}/{dir}/{stem}/{basename}` placeholders,
    60 s timeout + kill, 256 KiB drained output cap. UI loads
    `warraqcraft-tools.json` per project; demo file in examples/.
  * ui-egui — full block folding (heading/env regions, derived folded
    buffer, source-coordinate remapping); line ops Ctrl+D/Alt+↑↓/
    Ctrl+//Ctrl+Shift+K/Ctrl+Shift+Bksp; compile errors→gutter dots+
    tooltips; completion popup (\cmd ≥2 chars, \begin/\end envs, ↑↓
    Enter/Tab Esc); magic directives in status bar.
- M4 Wave-2 — document workflow features (parallel streams again):
  * texparse `graph.rs` — file-level deps via the lexer (\input,
    \include, graphics, bib, class, packages), extension-aware
    resolution, cycle-safe BFS `build`, root detection; engine
    `project.graph` exposes it.
  * engine `templates.rs` + `history.rs` — `template.list/read/new`
    over a shipped `templates/` dir (article, report, letter, beamer,
    arabdoc polyglossia+xelatex); `project.snapshot` local history
    (`.warraqcraft/snapshots/<ts>/`, atomic restore, confirm-gated).
  * ui-egui `cite.rs` — insert-cite dialog (Ctrl+Shift+C / cite
    button): project .bib scan via parse_bib, filter+keyboard nav,
    merges into existing `\cite{…}`; template picker in the tree
    toolbar; History window (snapshot now / two-click restore);
    craft-settings persistence — theme, autocompile, engine, zoom,
    panel visibility survive restarts (memory-stub on wasm).
- M5 Wave-3 — editor intelligence + system features:
  * texparse `stats.rs` — `DocStats`/`analyze` (words, chars, lines,
    commands, math/env/label/cite counts), lexer-accurate.
  * engine `preview.rs` + `stats.rs` — `math.preview` (scratch compile
    through the real pipeline, magic-comment preamble, serialized) and
    `text.stats` (delegates to texparse::analyze).
  * ui-egui `spellcheck.rs` — spellbook (MPL-2.0) Hunspell checker,
    dicts/ via fetch-dict.ps1; skips commands/comments/math/envs/
    ref-args/verb; wavy underlines + right-click suggestions.
  * ui-egui `mathprev.rs` — caret-in-math → worker compile → cached
    texture preview floating bottom-right + span tint.
  * ui-egui `mirrors.rs` + `ai.rs` — Ctrl+M occurrence-mirror editing
    (bounded multi-caret), BYO-endpoint AI panel (craft-ai, native-only;
    token never persisted); 🖨 print via craft-print; Stats window.
- M2 — control surface (headless reachability for all engine commands):
  * `crates/mcp` (new, L6) — opt-in stdio MCP server (NDJSON JSON-RPC,
    no port): initialize/ping/tools·list/tools·call/resources; tools map
    1:1 to Session::execute; read-only warraqcraft://doc/* resources;
    >4 MiB lines capped. `warraqcraft-cli mcp` launches it.
  * `warraqcraft-cli cmd`/`--cmd` — generic engine passthrough
    (--file/--params/--check) + `commands` listing.
  * `--control FILE` — opt-in loopback TCP channel on the GUI: 128-bit
    token in an owner-only control file (staged write, no symlink
    follow), per-request-token NDJSON requests drained through
    logic(), oneshot replies, 64 in-flight cap. Client:
    `warraqcraft-cli ui --control FILE <cmd> [--params]`.
- M3.4 — project-wide awareness: engine `project.structure` walks the
  `% !TeX root`-aware `\input` graph and merges each file's headings,
  labels, cite keys and unresolved deps (open file parsed from the live
  buffer; `.sty`/`.cls` distro deps filtered out). UI: FILE OUTLINE is
  now the project outline — per-file headers, a missing-files warning,
  and cross-file click→open+jump; `\ref{`/`\eqref{`/… completion offers
  project labels tagged with their defining file (completion items are
  now owned `CompletionItem`s so label keys can carry dynamic meta).
  Exposed as `project_structure` on CLI/control/MCP.
- M5 Wave-4 — ship-readiness workbench:
  * Engine/UI: `document.new` creates a dirty untitled buffer; status
    messages, shortcut help, settings, export and recovery strings are
    localized through the 16-language catalog.
  * UI: LaTeX formatting toolbar plus categorized Greek/arrow/operator/
    relation symbol palette; selection-aware insertion preserves caret
    position.
  * Recovery: source/path/cursor autosave in preferences, restart
    recovery prompt, and autosave cleared after a successful save.
  * Diagnostics: file-attributed errors/warnings feed cross-file jumps;
    project `.bib` keys now feed inline cite completion.
  * Platform/docs: three-OS CI and release workflow, Windows/POSIX
    packaging scripts, WASM xtask gate, collaboration and AI-harness
    plans, README/user-guide refresh.
- M5 Wave-5 — agent automation + reusable family shell:
  * `ai.rs` adds provider presets/probing, automated local Ollama setup
    and worker-thread asks; `engine::ai` exposes `ai.providers`,
    `ai.probe` and `ai.ask` over CLI/control/MCP. `docs/AI-HARNESS.md`
    maps the follow-on memory/task/system-prompt harness.
  * `docs/APP-SKELETON.md` records the reusable ArtCraft architecture —
    crate/layer layout, Session command contract, CLI/TCP/MCP parity,
    prefs/recovery, i18n, templates/history, AI and packaging gates —
    for PhotoCraft, VectorCraft, PDFCraft and future siblings.
- M6 Wave-1 — Git-native collaboration + disk safety:
  * `engine::repo` adds bounded fixed-argv `repo.status/init/pull/push/
    commit/branches/conflicts/history`; dirty buffers and stale disk
    state guard mutating operations, and credentials are redacted.
  * `document.disk_state` plus open/save stamps detect external edits;
    `document.save` refuses silent overwrite, `overwrite: true` is the
    explicit confirmation, and `document.new/open` require
    `discard: true` before replacing a dirty buffer.
  * UI adds a localized Git status/window and changed-on-disk modal with
    reload / save-over / keep-editing choices across the 16-language
    catalog; the same commands are exposed through CLI/control/MCP.
- M6 Wave-2 — guided Git collaboration UX:
  * `repo.remote` adds/updates a sharing remote without storing
    credentials in preferences; init can now collect an optional remote
    before creating `.git`.
  * `repo.pull` exposes explicit `ff-only` or `rebase` strategy, while
    `repo.status` returns bounded branch, change, conflict and recent-
    history payloads for one consistent UI/CLI/control/MCP contract.
  * The localized repository window now offers connect-remote guidance,
    branch picker/create, upstream-on-push, changed/conflicted file lists
    and recent commits.
- M6 Wave-3 — agent integration contract:
  * `docs/AGENT-INTEGRATIONS.md` documents the fresh-session CLI,
    live-session control channel and persistent MCP surfaces, including
    setup recipes for Devin, Claude Code, Cursor and generic MCP clients.
  * The guide pins the safety expectations for dirty/stale buffers,
    explicit destructive parameters, full-buffer `source.set`, bounded
    subprocesses, credential redaction and CI-only headless usage.
