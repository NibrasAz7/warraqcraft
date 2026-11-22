<!-- markdownlint-disable MD033 MD041 -->
<p align="center">
  <img src="website/logo.svg" width="128" height="128" alt="WarraqCraft icon: a cream pen nib on a navy tile with a gold accent">
</p>

<h1 align="center">WarraqCraft</h1>

<p align="center">
  <b>A LaTeX studio for the desktop; open source, local-first, built in pure Rust.</b><br>
  Overleaf-class editing comfort — project awareness, real compilation, SyncTeX,<br>
  citations, history — without a server account or cloud dependency.<br>
  <b>The typesetting engine under the writing tools</b> — LaTeX-grade PDF output<br>
  and BibTeX/CSL-JSON citations on your own machine.<br>
  <i>Named after the</i> warrāq <i>(ورّاق), the paper-makers and copyists of the medieval manuscript trade.</i><br>
  macOS · Windows · Linux · BSD · the web
</p>

<p align="center">
  <img alt="License: MIT OR Apache-2.0" src="https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-17233b?style=flat-square">
  <img alt="Written in Rust" src="https://img.shields.io/badge/written%20in-Rust-c99a2c?style=flat-square&logo=rust&logoColor=white">
  <img alt="Runs on macOS, Windows, Linux, BSD and the web" src="https://img.shields.io/badge/runs%20on-macOS%20%C2%B7%20Windows%20%C2%B7%20Linux%20%C2%B7%20BSD%20%C2%B7%20web-17233b?style=flat-square">
  <img alt="Agent-drivable over MCP" src="https://img.shields.io/badge/agents-MCP%20%C2%B7%20control-c99a2c?style=flat-square">
  <img alt="Status: alpha" src="https://img.shields.io/badge/status-alpha-orange?style=flat-square">
  <br>
  <a href="https://remade.wiki/projects/warraqcraft/"><img alt="Listed on Remade" src="https://remade.wiki/badges/listed-on-remade.svg"></a>
</p>

<p align="center">
  <a href="#quick-start">Quick start</a> ·
  <a href="#highlights">Highlights</a> ·
  <a href="#feature-overview">Features</a> ·
  <a href="#the-layout">Layout</a> ·
  <a href="#status">Status</a> ·
  <a href="#license">License</a>
</p>

---

## Quick start

```console
cargo run -p warraqcraft                                   # the studio
cargo run -p warraqcraft-cli -- check paper.tex            # structural issues (exit 1 if any)
cargo run -p warraqcraft-cli -- compile paper.tex          # system-TeX compile → .warraqcraft/
cargo run -p warraqcraft-cli -- parse paper.tex            # document structure outline
cargo run -p warraqcraft-cli -- mcp                        # MCP server on stdio
```

Compilation uses a bundled Tectonic when present, or a detected system XeLaTeX/LuaLaTeX/pdfLaTeX.

## Highlights

<table>
<tr>
<td width="33%" valign="top">

### A real project editor
File tree, syntax highlighting, folding, command/environment completion, live diagnostics and cross-file navigation through `\input`/`\include` graphs.

</td>
<td width="33%" valign="top">

### Compile and navigate
Tectonic or system TeX, hayro-rendered PDF preview, clickable error log, and **two-way SyncTeX** — click the source, land on the PDF; click the PDF, land on the line.

</td>
<td width="33%" valign="top">

### Multilingual by design
16 document languages and a bidirectional-aware UI — Arabic-script text renders correctly through run-level bidi and font fallback, with 144 original templates.

</td>
</tr>
</table>

## Feature overview

- **Compile and preview** — bundled Tectonic subprocess when present, or a detected system XeLaTeX/LuaLaTeX/pdfLaTeX; hayro-rendered PDF preview; clickable error/warning log; two-way SyncTeX navigation.
- **Editing helpers** — slim formatting toolbar, section and math insertion, common environments, categorized Greek/operator/relation/arrow symbol palette, line operations and occurrence mirrors.
- **Documents and recovery** — untitled buffers protected by dirty-state guards; autosave of text, path and cursor offered on restart; external disk changes surface an explicit reload/overwrite choice, never silent data loss.
- **Git collaboration** — repository detection, branch/status display, initialize/pull/push/commit, conflict counts and guarded branch commands; credentials stay with the user's Git tooling.
- **Bibliography and references** — project-wide `\label`/`\ref` completion, `.bib` parsing, inline `\cite` key completion, searchable cite dialog and bibliography backend detection; `cite.fetch` resolves DOI/arXiv/PMID records, `bib.import` folds a reference manager's CSL-JSON (Zotero) or RIS export into a project `.bib` with deduplication, `bib.audit` flags duplicates, and the library pane attaches PDFs per cite key. Deliberately **not a reference manager** — Zotero already covers storage, free — WarraqCraft cites from the library you already keep and differentiates on typesetting and authoring.
- **Templates** — 144 original source-only starters (article, beamer, letter, report, book, thesis, CV, homework, poster × 16 document languages) plus an explicit user/community import that records provenance.
- **History and tools** — `.warraqcraft/` snapshot history, named user tools, math preview, document statistics, Hunspell spell-checking, printing and PDF export.
- **Multilingual UI** — English fallback plus Arabic, Persian, Urdu, French, Italian, German, Spanish, Portuguese, Turkish, Latin, Russian, Mandarin, Japanese, Korean and Hindi.
- **Automation surfaces** — the same engine commands from the GUI, `warraqcraft-cli`, the opt-in TCP control channel and the opt-in MCP server.

## The layout

Layered like the sibling craft apps; nothing below L6 touches egui/winit:

| Layer | Crate               | Job                                                            |
| ----- | ------------------- | -------------------------------------------------------------- |
| L0    | `texparse`          | `.tex` lexer/structure scan, `.bib` parser, SyncTeX data        |
| L1    | `typeset`           | boxes/glue, Knuth–Plass line breaking, math layout              |
| L2    | `texpdf`, `texbib`  | PDF writer; BibTeX/CSL-JSON/RIS import & render                 |
| L3    | `texpkg`            | package/class registry + loader (arabdoc built in)              |
| L5    | `engine`            | command registry + Session guard                                |
| L6    | `mcp`, `ui-egui`    | JSON-RPC server; egui studio UI                                  |
| L7    | `apps/*`            | `warraqcraft` studio + control server, `warraqcraft-cli`        |

`cargo xtask layers` enforces the stack. Everything is a command: `document.compile`, `cite.insert`, `figure.insert`, `project.snapshot`, `history.diff` — reachable identically from the GUI, CLI, control channel and MCP.

## Status

**Alpha.** The desktop studio already supports multi-file LaTeX projects, real compilation, PDF preview, SyncTeX navigation, templates, citations, local history, autosave recovery, spell-checking and a 16-language bidirectional-aware UI. The in-process ArtCraft-tradition typesetter remains a research track. `ROADMAP.md` tracks what lands next.

WarraqCraft is **LaTeX-compatible**; it does not claim to be TeX.

## License

MIT OR Apache-2.0. Clean-room: semantics come from public sources only — The TeXbook's published algorithms, LaTeX2e/3 public documentation and CTAN package docs. No GPL/LGPL code (pdfTeX, LuaTeX, BibTeX, biber stay banned); Tectonic's MIT core is the one allowed engine dependency; `\write18`-style shell escape is always disabled.
