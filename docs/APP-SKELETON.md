# ArtCraft application skeleton

This is the reusable recipe behind WarraqCraft. Use it for sibling
applications such as PhotoCraft (raster images), VectorCraft (vector
graphics), PDFCraft (PDF tooling), and future Craft apps. The goal is
not "clone the LaTeX editor"; it is to reuse the product shell, command
model, local-first guarantees and quality gates while replacing only the
domain engine.

## What is reusable

| Layer/component | Reuse | Notes |
| --- | --- | --- |
| Workspace shape, lints, xtask gates | lift wholesale | replace names and layer map entries |
| `Session` + JSON command dispatch | reuse pattern | replace document semantics, keep error/result contract |
| CLI command passthrough | mostly lift | rename binary/app and command table |
| MCP stdio server | mostly lift | rename resources/tools and instructions |
| Loopback control channel | mostly lift | keep opt-in, token, loopback, bounded NDJSON |
| Settings/prefs persistence | lift | use `craft-settings`, atomic writes, versioned schema |
| Quit helper | lift | `craft-quit` owns platform close semantics |
| AI provider integration | lift | `craft-ai` + provider presets + request-only token |
| UI chrome pattern | reuse pattern | top bar, file pane, central canvas/editor, preview/status, modals |
| i18n catalog structure | reuse pattern | 16 languages, English fallback, `lang::view` display seam |
| Font/dictionary discovery | reuse pattern | shared `craft-fonts`/dictionary dirs, never vendored app assets |
| Templates/history/autosave | reuse pattern | per-app source allowlist and metadata change |
| TeX parsing/typesetting crates | domain-specific | replace with image/vector/PDF crates |

Anything shared by two or more apps should move to `craft-libs` rather
than being copied. Existing examples: `craft-atomic`, `craft-settings`,
`craft-quit`, `craft-print`, `craft-ai`, spellcheck helpers and the
bidirectional-display dependency seam.

## Workspace layout

A new app should start as a sibling repository or directory, not as a
feature module inside WarraqCraft:

```text
<app>/
  AGENTS.md
  README.md
  ROADMAP.md
  CLAUDE.md
  Cargo.toml
  clippy.toml
  rustfmt.toml
  LICENSE-MIT / LICENSE-APACHE
  NOTICE
  apps/
    <app>/               desktop executable
    <app>-cli/           headless commands + MCP/control client
  crates/
    <domain>parse/       hostile-input lexer/parser/scanner
    <domain>core/        document/model operations
    <domain>render/      rasterization/export/domain output
    <domain>pkg/         optional package/filter/plugin catalog
    engine/              Session + JSON command registry
    mcp/                 opt-in stdio MCP server
    ui-egui/             desktop UI, browser/WASM-ready stubs
  templates/             source-only starter content, if applicable
  scripts/               fetch/package helpers
  docs/                  product, user, automation, architecture docs
  xtask/                 fmt/clippy/test/layers/wasm/assets/package gates
```

Choose domain crate names for the file type, not the app name. Examples:

- PhotoCraft: `pixparse`, `raster`, `imageops`, `photopkg`
- VectorCraft: `svgparse`, `vector`, `pathops`, `vectorpkg`
- PDFCraft: `pdfparse`, `pdfcore`, `pdftools`, `pdfpkg`

## Layering contract

WarraqCraft uses this ordering:

| Layer | Responsibility | Rule |
| --- | --- | --- |
| L0 | hostile input parsing | no filesystem policy beyond bounded reads helpers |
| L1 | domain model/layout | pure operations, deterministic where possible |
| L2 | output/render formats | no UI, no app assumptions |
| L3 | catalogs/plugins | data/completion/registry only |
| L5 | engine | `Session`, filesystem policy, command registry |
| L6 | MCP + UI | adapters only; all behavior calls engine commands |
| L7 | executables | argument parsing and app startup only |

Rules:

- No crate below L6 depends on `egui`, `eframe`, `winit`, `wgpu`, `rfd`
  or `craft-ui`/`craft-quit`.
- The UI never implements domain behavior directly; it calls
  `Session::execute` or a small engine API that mirrors a command.
- `cargo xtask layers` must map the new crates and fail on downward or
  UI dependencies.
- Core crates should compile for `wasm32`; process/network/file work is
  target-gated and replaced by shaped stubs.

## Session and command contract

`Session` is the single mutable application object. Keep it small:

```rust
pub struct Session {
    path: Option<PathBuf>, // or document/project id where appropriate
    src: String,           // text apps; other apps may hold a document model
    dirty: bool,
    // small adapter-owned state only; large domain data lives in crates
}
```

Required behavior:

- `Session::execute(cmd, params) -> Result<Value, EngineError>`.
- Params and results are `serde_json::Value`; errors are actionable.
- Commands use `domain.action` ids (`document.open`, `snapshot.list`).
- MCP tool names use the same ids converted to snake_case
  (`document_open`).
- The CLI `commands` table, `Session::execute`, MCP `TOOL_COMMANDS` and
  control-channel passthrough must contain the same command set.
- Commands that mutate files or discard buffers must say so in docs and
  carry an explicit `confirm` parameter where irreversible.
- No command silently discards a dirty buffer. Save first, fail, or
  require explicit confirmation.
- Paths resolve under the project/app root unless a parameter is an
  explicit user-selected output path.

### Baseline commands

Adapt names to the app's model, but preserve the contract:

| Command | Required behavior |
| --- | --- |
| `document.new` | create untitled dirty buffer/document; refuse to replace a dirty buffer unless `discard: true` |
| `document.open` | load file, validate/bound hostile input; refuse to replace a dirty buffer unless `discard: true` |
| `document.save` | atomic write; optional `path` is save-as; `overwrite: true` is the explicit external-change confirmation |
| `document.compile` / `document.render` | produce the app's output artifact |
| `source.set` / `source.get` | text-source apps only |
| `structure.get` | live outline/objects/layers/pages summary |
| `project.graph` / `project.structure` | multi-file dependency/object graph |
| `template.list/read/new/import` | source-only gallery where useful |
| `project.snapshot`, `snapshot.*` | local bounded history |
| `text.stats` / `document.stats` | document/object metrics |
| `user_tool.*` | fixed-argv user tools, never shell text |
| `ai.providers`, `ai.probe`, `ai.ask` | provider-neutral AI surface |

Domain commands go through the same registry: examples are
`pdf.page_render`, `vector.path_simplify`, `photo.filter_apply`. Do not
give the UI a private fast path.

## Four user/agent surfaces

Every command is reachable four ways:

1. **Desktop UI:** calls `Session::execute` directly.
2. **CLI:** `<app>-cli cmd <id> --params '<json>'` executes on a fresh
   session; `<app>-cli commands` lists the registry.
3. **MCP:** `<app>-cli mcp` runs an opt-in stdio JSON-RPC server. Tool
   schemas are generated from the same command table.
4. **Control channel:** desktop launch with `--control FILE` binds only
   `127.0.0.1`, writes `{"port","token","pid"}`, accepts
   newline-delimited authenticated requests and forwards them to the
   running session.

Control-channel rules that must not change:

- opt-in only; never registered or started by default;
- loopback only;
- fresh owner-only bearer token per launch;
- token never logged;
- request line cap and bounded in-flight queue;
- command errors are JSON results, not process exits;
- the channel is removed on exit.

## Desktop UI shell

The UI crate is a shell over the engine. Keep these seams:

```text
ui-egui/src/
  lib.rs          app state, eframe integration, pane/modal wiring
  lang.rs         language table, formatting/view boundary
  theme.rs        shared color tokens and light/dark palette
  icons.rs        original code-drawn icons (no copied assets)
  fonts.rs        font discovery/fallback; no vendored font policy
  files.rs        project/file tree data
  edits.rs        editor/canvas operations, if text-like
  history.rs      history window, if needed
  ai.rs           provider panel/worker integration
  control.rs      opt-in TCP control channel
  <domain>.rs     preview/canvas/scene rendering adapter
```

Expected chrome:

- top app bar: project/app identity, major actions, settings/language,
  help;
- left pane: file/project tree plus outline/objects;
- center: primary editing canvas/editor;
- right/bottom: preview/properties/log area;
- status bar: cursor/selection, job state, save/dirty marker;
- modal windows only for infrequent workflows; common formatting gets a
  compact toolbar/palette;
- all long work on worker threads with `mpsc` results polled per frame;
- WASM stubs keep UI call sites unconditional but never pretend network,
  native process or filesystem work happened.

## Preferences and recovery

Use `craft-settings` with an explicit versioned prefs struct. Persist
non-secret UI state only:

- theme, zoom/page mode, pane visibility;
- language;
- engine/provider/tool choices;
- last file;
- dirty autosave source/model state and cursor/selection position.

Rules:

- dirty content can be persisted for recovery; clean buffers recover by
  reopening `last_file`;
- writes are atomic and debounced;
- close/quit flushes through `craft-quit`;
- API tokens are session-only unless platform secret storage is
  deliberately added;
- settings migration must tolerate unknown/new fields;
- recovery status is localized, not silently overwritten.

## Local project state

Each app owns a hidden project state dir — WarraqCraft uses
`.warraqcraft/`; a new app should use its own `.<appname>` directory and
a legacy migration path when renaming.

Suggested contents:

```text
.<appname>/
  build/             generated intermediates
  snapshots/         local project history
  preview/           temporary rendered previews
  ai/                future harness task/memory stores
```

Rules:

- never commit generated state;
- bound file count, file size, recursion and total bytes;
- exclude the state dir from snapshot/template scans;
- snapshot ids and template ids use `[A-Za-z0-9_-]{1,64}`;
- restores require `confirm: true`.

## Templates and starter content

The gallery supports shipped templates plus user imports:

- bundled templates live in `templates/` and install beside the app;
- metadata lives in `.meta.toml` (`name`, `name_<lang>`, `desc`,
  `desc_<lang>`, `lang`, `category`, `engine`);
- user imports live in the platform data dir, not the project;
- only allow-listed text sources are copied;
- binaries, fonts, PDFs, build outputs and hidden cache files are not
  imported;
- third-party templates need explicit provenance/license metadata and a
  compatible license; when unclear, make the user import them locally
  instead of bundling.

For non-document apps, "template" can mean document presets, canvas
presets, layer stacks, export presets or toolchains — same text-only and
license rules.

## Internationalization and directionality

WarraqCraft's convention:

- `lang::Lang` enumerates UI languages;
- `lang::ALL` is the complete picker set;
- `Strings` contains every user-facing string;
- English is the fallback;
- `lang::fmt` substitutes placeholders and performs the app's bidi
  display policy;
- `lang::view` is the display boundary for RTL/mixed-direction text;
- localized names/descriptions are metadata fields, not separate source
  files.

A sibling should reuse the same language set and tests. Add new strings
to all catalogs before using them in UI. Keep language separate from
script: Latin and Cyrillic are scripts, not UI languages.

## Assets and fonts

- Icons are code-drawn originals or an allowed icon set (Lucide), never
  screenshots/traces of a competitor.
- Fonts live in `craft-fonts` or a runtime font-discovery path; do not
  vendor shared fonts into the app repo unless this repo owns them and
  their license permits it.
- Every committed asset gets provenance in `NOTICE` or the project's
  attribution manifest.
- No asset whose license is unknown or non-redistributable.
- User-imported assets remain user files; provenance metadata records
  where they came from.

## AI and agent safety

The provider integration is a feature, not a dependency:

- AI can be disabled or absent and the app still edits/renders.
- Provider requests run away from the UI thread.
- Context and prompt sizes are bounded.
- Tokens are request/session data, never persisted or returned.
- Local Ollama setup is explicit and uses fixed commands; remote install
  scripts are not piped into a shell by default.
- Agent-facing commands use the same engine permissions as UI actions.
- A future harness must add memory, tasks, system prompts and command
  approval as described in `AI-HARNESS.md`.

## Quality gates

Every app should expose the same commands:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo xtask ci
cargo xtask layers
cargo xtask wasm
cargo xtask assets      # when the repo has an attribution manifest
```

Minimum invariants:

- no `unwrap`, `expect`, `panic`, `todo`, `unimplemented`,
  `unreachable` or `unsafe` outside tests;
- hostile file/input tests never panic;
- worker failures land in status/result values;
- process calls use fixed argv, bounded timeouts and no shell strings;
- app closes without losing dirty state;
- release package contains only licensed/runtime-required assets.

## Scaffolding checklist

1. Copy the workspace skeleton and rename crates/binaries consistently.
2. Replace the domain parser/model/render crates; keep `engine`, `mcp`,
   `ui-egui` module names and command wiring.
3. Update `xtask layers` with the new crate names and expected layers.
4. Update `xtask wasm` to cover all core crates.
5. Define `Session` and the baseline command set before the UI.
6. Wire CLI `commands`, `Session::execute`, MCP `TOOL_COMMANDS` and the
   MCP `tools/list` schemas together.
7. Add the control channel unchanged except app/file naming.
8. Implement prefs/autosave/dirty recovery before polish.
9. Add localized UI strings across all catalogs from the start.
10. Write hostile-input tests first, then domain fixtures.
11. Add packaging scripts only after `xtask ci` is green.
12. Lift shared behavior into `craft-libs` when the second app needs it.

## First milestone acceptance

A new sibling is successfully scaffolded when:

- `<app>-cli commands` lists the baseline commands;
- `document.new` creates an untitled dirty document and does not write;
- CLI, MCP and control channel execute the same command ids;
- the UI opens, edits, marks dirty, autosaves and recovers;
- all 16 UI languages are present, with English fallback;
- `cargo xtask ci`, `cargo xtask layers` and `cargo xtask wasm` pass;
- the app runs with no network service and no AI provider configured.
