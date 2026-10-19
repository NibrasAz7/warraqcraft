# WarraqCraft

[![Listed on Remade](https://remade.wiki/badges/listed-on-remade.svg)](https://remade.wiki/projects/warraqcraft/)

*Named after the* warrāq *(ورّاق)* — the paper-makers, copyists and
bookbinders of the medieval Islamic manuscript trade.

WarraqCraft is a clean-room, Rust-native, local-first LaTeX authoring and
typesetting studio in the ArtCraft family. The goal is Overleaf-class
editing comfort on the desktop — project awareness, a focused editor,
real PDF compilation and preview, local history and agent-friendly
automation — without a server account or cloud dependency.

WarraqCraft is **LaTeX-compatible**; it does not claim to be TeX.

## Status

**Alpha.** The desktop studio already supports multi-file LaTeX projects,
real compilation, PDF preview, SyncTeX navigation, templates, citations,
local history, autosave recovery, spell-checking and a 16-language
bidirectional-aware UI. The in-process ArtCraft typesetter remains a
research track.

## Feature overview

- **Project editor** — file tree, guttered syntax highlighting, folding,
  command/environment completion, project outline, live diagnostics and
  cross-file navigation through `\input`/`\include` graphs.
- **Compile and preview** — bundled Tectonic subprocess when present,
  or a detected system XeLaTeX/LuaLaTeX/pdfLaTeX; hayro-rendered PDF
  preview; clickable error/warning log; two-way SyncTeX navigation.
- **Editing helpers** — slim formatting toolbar, section and math
  insertion, common environments, categorized Greek/operator/relation/
  arrow symbol palette, line operations and occurrence mirrors.
- **Documents and recovery** — new documents start as untitled unsaved
  buffers; dirty state is protected; the latest text, path and cursor are
  autosaved to preferences and offered on restart. External disk changes
  surface an explicit reload/overwrite choice instead of silent data loss.
- **Git collaboration** — repository detection, branch/status display,
  initialize/pull/push/commit actions, conflict counts and guarded branch
  commands; credentials stay with the user's Git tooling.
- **Bibliography and references** — project-wide `\label`/`\ref`
  completion, `.bib` parsing, inline `\cite` key completion, a searchable
  cite dialog and bibliography backend detection.
- **Templates** — original source-only starter templates for all 16
  supported UI/document languages, plus an explicit user/community
  template import that copies text sources only and records provenance.
- **History and tools** — `.warraqcraft/` snapshot history, named user
  tools, math preview, document statistics, Hunspell spell-checking,
  printing and PDF export.
- **Multilingual UI** — English fallback plus Arabic, Persian, Urdu,
  French, Italian, German, Spanish, Portuguese, Turkish, Latin, Russian,
  Mandarin Chinese, Japanese, Korean and Hindi. Arabic-script UI text is
  rendered through run-level bidirectional display and font fallback.
- **Automation surfaces** — the same engine commands are available from
  the GUI, `warraqcraft-cli`, the opt-in TCP control channel and the
  opt-in MCP server for agents such as Devin, Claude Code, Cursor and
  other tool clients.

## Supported platforms

| Platform | State |
| --- | --- |
| Windows | Release zip built and smoke-tested locally via `scripts/package.ps1` |
| Linux | CI target; release tarball and `.deb` via `scripts/package.sh` |
| macOS | CI target; release `.app` bundle and `.dmg` via `scripts/package.sh` |
| WASM | Lower parsing/catalog crates are checked with `cargo xtask wasm` |

The repository CI tests Linux, Windows and macOS on every push/pull
request; the release workflow produces all three platform artifact sets.
Release smoke-test criteria and the current Windows result are recorded in
[`docs/RELEASE-QA.md`](docs/RELEASE-QA.md).

## Install and run

### Prerequisites

- A stable Rust toolchain.
- A TeX engine: either a system XeLaTeX, LuaLaTeX or pdfLaTeX on `PATH`,
  or Tectonic fetched into `bin/`:

```powershell
scripts/fetch-tectonic.ps1
```

Optional extras:

```powershell
scripts/fetch-dict.ps1    # en_US Hunspell dictionary into dicts/
scripts/fetch-fonts.ps1   # shared craft-fonts checkout/discovery helper
```

### Run from source

```powershell
cargo run -p warraqcraft -- path\to\main.tex
```

Run without a path to start with the launcher/untitled workflow.

### Package locally

```powershell
pwsh scripts/package.ps1 -Version 0.1.0
```

```sh
./scripts/package.sh 0.1.0
```

The scripts stage the two binaries, licences, notices and templates and
write release archives under `dist/`.

## Command and agent surfaces

Every user-facing operation is registered on the engine command surface.

```text
warraqcraft-cli commands
warraqcraft-cli cmd document.compile --file main.tex
warraqcraft-cli cmd document.new --params '{}'
warraqcraft-cli cmd project.structure --file main.tex
warraqcraft-cli mcp
```

The GUI can also expose an authenticated loopback control channel when
started explicitly with `--control FILE`; agents then use
`warraqcraft-cli ui --control FILE ...`. Neither MCP nor TCP control is
enabled by default.

Client setup, session lifetimes, safety rules and copyable recipes are in
[`docs/AGENT-INTEGRATIONS.md`](docs/AGENT-INTEGRATIONS.md).

## Templates

Bundled template sources live in
[`templates/`](templates/) and on GitHub in the
[WarraqCraft template source tree](https://github.com/NibrasAz7/warraqcraft/tree/main/templates).

The gallery supports user/community folder imports. Imported templates
must be sources the user is entitled to use: the importer copies text
source formats only, skips PDFs/fonts/binaries/build output, and stores
caller-supplied provenance and licence metadata. The Arabic template
skill assets are CC-BY-NC-4.0 and are therefore not bundled into this
MIT/Apache-2.0 repository.

## Collaboration direction

WarraqCraft remains local-first by default. The collaboration plan keeps
three paths separate:

1. **File-based collaboration now** — users can place a project in Git,
   a synced folder or any existing version-control workflow.
2. **Git-aware project integration** — implemented through `repo.status`,
   `repo.init`, `repo.remote`, `repo.pull`, `repo.push`, `repo.commit`,
   `repo.branches`, `repo.conflicts` and `repo.history`. Operations are
   explicit, guarded against dirty/stale buffers, and preserve atomic
   saves; credentials remain with Git rather than WarraqCraft preferences.
3. **Optional Overleaf-like session sync** — a later opt-in service or
   self-hosted relay could coordinate live sessions and shared cursors.
   It must never become required for editing or compiling.

The design notes live in [`docs/COLLABORATION.md`](docs/COLLABORATION.md).

## AI direction

The AI assist panel is provider-based: Ollama gets a one-click local
setup flow (detect or install the CLI, start the service, pull the
selected model and probe the endpoint), while OpenAI, OpenRouter and
custom OpenAI-compatible endpoints only need endpoint/model plus a
session-only token. Headless agents can use `ai.providers`, `ai.probe`
and `ai.ask` through the CLI/control/MCP command surface.

The full AI harness is still planned separately: memory, task system,
system prompt, command permissions, orchestration and safety
boundaries. The application remains fully useful with AI disabled.

The harness plan lives in [`docs/AI-HARNESS.md`](docs/AI-HARNESS.md).

## Clean-room and licensing rules

- Behaviour and semantics come from public documentation, published
  algorithms and user-owned files only.
- No GPL/LGPL engine or editor code is copied. pdfTeX, LuaTeX, BibTeX,
  biber and kpathsea are not embedded; system TeX is only invoked as an
  optional external process.
- `\write18`-style shell escape is always disabled.
- Fonts are provided through the shared `craft-fonts` convention and are
  not vendored into this repository.
- See [`NOTICE`](NOTICE) for third-party notices and template/import
  provenance handling.

## Repository layout

```text
crates/texparse   — LaTeX/BibTeX parsing, project graphs, statistics
crates/texpkg     — curated command/environment/package catalog
crates/engine     — Session, command registry, compile/history/templates
crates/mcp        — opt-in JSON-RPC MCP server
crates/ui-egui    — egui studio, editor chrome, preview and dialogs
apps/warraqcraft  — desktop studio executable
apps/warraqcraft-cli — headless CLI/control/MCP entry point
templates/        — original source-only starter templates
scripts/          — font/dictionary/engine helpers and packaging
xtask/            — fmt, clippy, test, layer and WASM gates
```

## Development

```powershell
cargo xtask ci       # fmt, clippy -D warnings, tests, layering
cargo xtask wasm     # wasm32 compatibility for the supported crates
```

`ROADMAP.md` tracks milestones and the progress log. `AGENTS.md` is the
authoritative contributor/agent contract.
[`docs/APP-SKELETON.md`](docs/APP-SKELETON.md) documents the reusable
ArtCraft application shell for future siblings such as PhotoCraft,
VectorCraft and PDFCraft.
