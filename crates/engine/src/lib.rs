//! WarraqCraft engine layer (L5): `Session` + command surface.
//!
//! M0 scope: `Session` holds the open document (path, source, dirty flag)
//! and `execute()` dispatches the first command set — `document.open`,
//! `document.save`, `document.compile`, `source.set`, `source.get`,
//! `structure.get`. `document.compile` runs a TeX engine into a
//! `.warraqcraft/` build directory next to the document, parses the log,
//! and reports the PDF path. Engine preference: a bundled or PATH
//! `tectonic` (Track B-lite, zero TeX install) first, then system
//! xelatex/lualatex/pdflatex on PATH (Track C). Shell escape is never
//! enabled — Tectonic has no equivalent flag and the system engines
//! are never passed one. `user_tool.*` commands run TeXstudio-style
//! named external tools (fixed argv, no shell — see [`tools`]).
//! `template.*` commands serve the shipped template gallery (see
//! [`templates`]); `project.snapshot`/`snapshot.*` keep local file
//! history under `.warraqcraft/snapshots/` (see [`history`]);
//! `math.preview` compiles a selected math fragment for inline
//! preview (see [`preview`]) and `text.stats` reports document
//! statistics (see [`stats`]).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use serde_json::{json, Value};
use texparse::{parse_magic, parse_structure, DocStructure};

pub mod ai;
pub mod history;
pub mod preview;
pub mod project;
pub mod repo;
pub mod stats;
pub mod synctex;
pub mod templates;
pub mod tools;

pub use tools::UserTool;

/// Hidden build directory next to the document (`.aux`/`.log`/`.pdf`,
/// `snapshots/`).
pub(crate) const BUILD_DIR: &str = ".warraqcraft";

/// Pre-rename (TexCraft-era) projects kept this state under `.texcraft/`.
/// Adopt it once, if the new dir doesn't exist yet.
const LEGACY_BUILD_DIR: &str = ".texcraft";

/// Rename `root/.texcraft` → `root/.warraqcraft` when the former exists
/// and the latter doesn't. Best-effort: a failed rename just means the
/// old state stays put.
pub(crate) fn migrate_legacy_dir(root: &Path) {
    let new = root.join(BUILD_DIR);
    if !new.exists() {
        let _ = std::fs::rename(root.join(LEGACY_BUILD_DIR), new);
    }
}

/// Packages that require a Unicode TeX engine (XeTeX/LuaTeX).
const UNICODE_PACKAGES: [&str; 4] = ["fontspec", "unicode-math", "polyglossia", "bidi"];

/// Error type for all engine operations.
#[derive(Debug)]
pub enum EngineError {
    /// Filesystem or process error.
    Io(std::io::Error),
    /// No usable TeX engine found on PATH.
    NoTexEngine,
    /// Command needs a file path but the session has none.
    NoFile,
    /// Unknown command id.
    UnknownCommand(String),
    /// A required parameter is missing or of the wrong type.
    BadParam(&'static str),
    /// User-tool lookup failure — the session stays usable.
    Tool(String),
    /// The buffer is dirty where a repository operation needs a clean
    /// working file.
    DirtyBuffer,
    /// The file changed on disk after the buffer was opened or last
    /// saved — overwrite needs an explicit confirmation.
    ExternalChange,
    /// A git command failed or returned an actionable error.
    Git(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::NoTexEngine => write!(f, "no TeX engine found on PATH (install MiKTeX or TeX Live)"),
            Self::NoFile => write!(f, "document has no file path — save it first"),
            Self::UnknownCommand(c) => write!(f, "unknown command '{c}'"),
            Self::BadParam(p) => write!(f, "missing or invalid parameter '{p}'"),
            Self::Tool(m) => write!(f, "user tool: {m}"),
            Self::DirtyBuffer => write!(f, "buffer has unsaved changes — save before changing the repository"),
            Self::ExternalChange => write!(f, "file changed on disk — reload it or save with overwrite confirmation"),
            Self::Git(m) => write!(f, "git: {m}"),
        }
    }
}

impl std::error::Error for EngineError {}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// One TeX compile error from the `.log`.
#[derive(Debug, Clone)]
pub struct CompileError {
    /// Source file, if the log reported or implied one.
    pub file: Option<PathBuf>,
    /// Source line in the `.tex` file, if the log reported one.
    pub line: Option<u32>,
    /// Error message (single line).
    pub message: String,
}

/// Result of one `document.compile` run.
#[derive(Debug)]
pub struct CompileReport {
    /// Engine binary used (e.g. "xelatex").
    pub engine: String,
    /// Path to the produced PDF, if the engine wrote one.
    pub pdf: Option<PathBuf>,
    /// Page count reported in the log.
    pub pages: Option<u32>,
    /// Errors (typically fatal, `!` / `file:line:` log entries).
    pub errors: Vec<CompileError>,
    /// `LaTeX Warning:` entries.
    pub warnings: Vec<CompileError>,
    /// Tail of the build log for diagnostics.
    pub log_tail: String,
}

impl CompileReport {
    fn to_json(&self) -> Value {
        let errs: Vec<Value> = self
            .errors
            .iter()
            .map(|e| json!({"file": e.file.as_ref().map(|p| p.display().to_string()), "line": e.line, "message": e.message}))
            .collect();
        let warns: Vec<Value> = self
            .warnings
            .iter()
            .map(|w| json!({"file": w.file.as_ref().map(|p| p.display().to_string()), "line": w.line, "message": w.message}))
            .collect();
        json!({
            "engine": self.engine,
            "pdf": self.pdf.as_ref().map(|p| p.display().to_string()),
            "pages": self.pages,
            "errors": errs,
            "warnings": warns,
        })
    }
}

/// Metadata captured when the buffer was loaded/saved. Comparing it with
/// the current file is cheap and catches ordinary external edits and
/// deletes before WarraqCraft overwrites them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp { len: meta.len(), modified: meta.modified().ok() })
}

/// An open WarraqCraft document session.
#[derive(Default)]
pub struct Session {
    path: Option<PathBuf>,
    src: String,
    dirty: bool,
    /// Stamp captured by `open`/`save`; `None` for untitled buffers.
    disk_stamp: Option<FileStamp>,
    user_tools: Vec<UserTool>,
}

impl Session {
    /// Empty session (untitled document).
    pub fn new() -> Self {
        Self::default()
    }

    /// Session prefilled with `src`, not dirty (e.g. an editor template).
    pub fn with_source(src: String) -> Self {
        Self { path: None, src, dirty: false, disk_stamp: None, user_tools: Vec::new() }
    }

    /// Path of the open file, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Current source text.
    pub fn source(&self) -> &str {
        &self.src
    }

    /// Whether the buffer differs from the file on disk.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    /// Replace the session's user-tool list (TeXstudio-style named
    /// external commands; see [`tools`]).
    pub fn set_user_tools(&mut self, tools: Vec<UserTool>) {
        self.user_tools = tools;
    }

    /// The session's configured user tools.
    pub fn user_tools(&self) -> &[UserTool] {
        &self.user_tools
    }

    /// Replace the buffer contents.
    pub fn set_source(&mut self, text: String) {
        self.dirty = self.dirty || text != self.src;
        self.src = text;
    }

    /// Load a `.tex` file into the session.
    pub fn open(&mut self, path: &Path) -> Result<(), EngineError> {
        let bytes = std::fs::read(path)?;
        self.src = String::from_utf8_lossy(&bytes).into_owned();
        self.path = Some(path.to_path_buf());
        self.disk_stamp = file_stamp(path);
        self.dirty = false;
        Ok(())
    }

    /// Whether the open file disappeared or changed after WarraqCraft
    /// loaded/saved it. Untitled buffers cannot conflict with disk state.
    pub fn disk_changed(&self) -> Result<bool, EngineError> {
        let Some(path) = &self.path else { return Ok(false) };
        match std::fs::metadata(path) {
            Ok(meta) => Ok(self.disk_stamp != Some(FileStamp { len: meta.len(), modified: meta.modified().ok() })),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(self.disk_stamp.is_some()),
            Err(e) => Err(EngineError::Io(e)),
        }
    }

    /// Write the buffer back to its file, refusing to overwrite a file
    /// that changed on disk since this buffer was loaded/saved.
    pub fn save(&mut self) -> Result<(), EngineError> {
        self.save_inner(false)
    }

    /// Save even when [`disk_changed`](Self::disk_changed) is true. UI and
    /// agent callers must only use this after explicit user confirmation.
    pub fn save_overwrite(&mut self) -> Result<(), EngineError> {
        self.save_inner(true)
    }

    fn save_inner(&mut self, overwrite: bool) -> Result<(), EngineError> {
        if !overwrite && self.disk_changed()? {
            return Err(EngineError::ExternalChange);
        }
        let Some(path) = self.path.clone() else { return Err(EngineError::NoFile) };
        craft_atomic::atomic_write(&path, self.src.as_bytes())?;
        self.disk_stamp = file_stamp(&path);
        self.dirty = false;
        Ok(())
    }

    /// Write the buffer to a new file and make it the session path. A
    /// new target is a user-selected overwrite; saving the same target
    /// still honours the external-change guard.
    pub fn save_as(&mut self, path: &Path) -> Result<(), EngineError> {
        if self.path.as_deref() == Some(path) {
            return self.save();
        }
        craft_atomic::atomic_write(path, self.src.as_bytes())?;
        self.path = Some(path.to_path_buf());
        self.disk_stamp = file_stamp(path);
        self.dirty = false;
        Ok(())
    }

    /// Live structural scan of the buffer.
    pub fn structure(&self) -> DocStructure {
        parse_structure(&self.src)
    }

    /// Save-if-dirty, then compile the document with a system TeX engine.
    pub fn compile(&mut self) -> Result<CompileReport, EngineError> {
        if self.dirty {
            self.save()?;
        }
        let path = self.path.clone().ok_or(EngineError::NoFile)?;
        compile_file(&path, Some(&self.src))
    }

    /// Dispatch a command by id. Params are JSON; results are JSON.
    pub fn execute(&mut self, cmd: &str, params: &Value) -> Result<Value, EngineError> {
        match cmd {
            "document.new" => {
                // Optional starter text; the buffer is dirty and has no path
                // until document.save gives it one. `discard: true` is the
                // caller's explicit acknowledgement that a dirty buffer may
                // be replaced.
                if self.dirty && !params.get("discard").and_then(Value::as_bool).unwrap_or(false) {
                    return Err(EngineError::DirtyBuffer);
                }
                let text = params.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
                self.path = None;
                self.src = text;
                self.dirty = true;
                self.disk_stamp = None;
                self.user_tools.clear();
                Ok(json!({"ok": true, "path": Value::Null}))
            }
            "document.open" => {
                let path = params.get("path").and_then(Value::as_str).ok_or(EngineError::BadParam("path"))?;
                if self.dirty && !params.get("discard").and_then(Value::as_bool).unwrap_or(false) {
                    return Err(EngineError::DirtyBuffer);
                }
                self.open(Path::new(path))?;
                Ok(json!({"ok": true, "path": self.path_string()}))
            }
            "document.save" => {
                let overwrite = params.get("overwrite").and_then(Value::as_bool).unwrap_or(false);
                if let Some(path) = params.get("path").and_then(Value::as_str) {
                    if self.path.as_deref() == Some(Path::new(path)) && overwrite {
                        self.save_overwrite()?;
                    } else {
                        self.save_as(Path::new(path))?;
                    }
                } else if overwrite {
                    self.save_overwrite()?;
                } else {
                    self.save()?;
                }
                Ok(json!({"ok": true, "path": self.path_string()}))
            }
            "document.disk_state" => Ok(json!({
                "path": self.path_string(),
                "dirty": self.dirty,
                "external_changed": self.disk_changed()?,
            })),
            "document.compile" => Ok(self.compile()?.to_json()),
            "source.set" => {
                let text = params.get("text").and_then(Value::as_str).ok_or(EngineError::BadParam("text"))?;
                self.set_source(text.to_string());
                Ok(json!({"ok": true}))
            }
            "source.get" => Ok(json!({"text": self.src})),
            "structure.get" => Ok(structure_json(&self.structure())),
            // The `% !TeX`/`% !BIB` header directives of the buffer —
            // engine/root/encoding/spellcheck hints for the UI.
            "magic.get" => {
                let m = parse_magic(&self.src);
                Ok(json!({
                    "program": m.program,
                    "root": m.root,
                    "encoding": m.encoding,
                    "bibliography": m.bibliography,
                    "spellcheck": m.spellcheck,
                    "jobname": m.jobname,
                    "engine_hint": m.engine_hint(),
                    "extras": m.extras.iter().map(|(k, v)| json!([k, v])).collect::<Vec<_>>(),
                }))
            }
            "user_tool.list" => Ok(json!({"tools": self.user_tools.iter().map(tools::to_json).collect::<Vec<_>>()})),
            "user_tool.set" => {
                let list = params.get("tools").and_then(Value::as_array).ok_or(EngineError::BadParam("tools"))?;
                self.user_tools = list.iter().map(tools::from_json).collect::<Result<Vec<UserTool>, EngineError>>()?;
                Ok(json!({"ok": true}))
            }
            "user_tool.run" => tools::run(self, params),
            "user_tool.clear" => {
                self.user_tools.clear();
                Ok(json!({"ok": true}))
            }
            "template.list" => templates::list(),
            "template.read" => templates::read(params),
            "template.new" => templates::new(params),
            "template.import" => templates::import(params),
            // The reachable file graph from the open document — root
            // candidates, sources, resources, unresolved deps (M4).
            "project.graph" => {
                let Some(p) = &self.path else { return Err(EngineError::NoFile) };
                let g = texparse::graph::build(p);
                Ok(json!({
                    "roots": g.roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "files": g.files.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    "resources": g.resources.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                    // `.sty`/`.cls` "missing" deps are normal — the class
                    // and packages live in the TeX distro, not the project.
                    "missing": g.missing.iter().map(|(r, d)| json!({
                        "referrer": r.display().to_string(),
                        "kind": format!("{:?}", d.kind),
                        "target": d.target,
                        "line": d.line,
                    })).collect::<Vec<_>>(),
                }))
            }
            // Merged outline + labels across the reachable \input graph —
            // the open file is parsed from the live buffer (M3).
            "project.structure" => project::run(self),
            // Git-native collaboration: explicit, fixed-argv operations
            // against the project repository; credentials stay with Git.
            "repo.status" => repo::status(self, params),
            "repo.init" => repo::init(self, params),
            "repo.pull" => repo::pull(self, params),
            "repo.push" => repo::push(self, params),
            "repo.commit" => repo::commit(self, params),
            "repo.branches" => repo::branches(self, params),
            "repo.conflicts" => repo::conflicts(self),
            "repo.history" => repo::history(self, params),
            // Inline math preview: fragment → scratch `.tex` → PDF
            // under `.warraqcraft/preview/` for the UI to rasterize.
            "math.preview" => preview::run(self, params),
            // Document statistics (words, chars, math spans, …).
            "text.stats" => stats::run(self, params),
            "ai.providers" => Ok(ai::providers()),
            "ai.probe" => ai::probe(params),
            "ai.ask" => ai::ask(params),
            "project.snapshot" => history::snapshot(self, params),
            "snapshot.list" => history::list(self),
            "snapshot.read" => history::read(self, params),
            "snapshot.restore" => history::restore(self, params),
            other => Err(EngineError::UnknownCommand(other.to_string())),
        }
    }

    fn path_string(&self) -> Value {
        self.path.as_ref().map_or(Value::Null, |p| Value::String(p.display().to_string()))
    }
}

fn structure_json(d: &DocStructure) -> Value {
    json!({
        "class": d.class.as_ref().map(|(c, _)| c),
        "class_options": d.class_options,
        "packages": d.packages.iter().map(|(p, _)| p).collect::<Vec<_>>(),
        "title": d.title.as_ref().map(|(t, _)| t),
        "author": d.author.as_ref().map(|(a, _)| a),
        "headings": d.headings.iter().map(|h| json!({"line": h.pos.line, "level": h.level, "title": h.title})).collect::<Vec<_>>(),
        "labels": d.labels.iter().map(|(l, _)| l).collect::<Vec<_>>(),
        "cite_keys": d.cite_keys.iter().map(|(k, _)| k).collect::<Vec<_>>(),
        "bibliographies": d.bibliographies.iter().map(|(b, _)| b).collect::<Vec<_>>(),
        "issues": d.issues.iter().map(|i| json!({"line": i.pos.line, "col": i.pos.col, "message": i.message})).collect::<Vec<_>>(),
    })
}

/// Every TeX engine binary `compile_file` knows how to drive, in
/// preference order (Unicode engines first).
const ENGINE_CANDIDATES: &[&str] = &["xelatex", "lualatex", "pdflatex"];

/// A resolved engine: display name plus the program to invoke
/// (a bare name for PATH tools, a full path for a bundled one).
struct Engine {
    name: String,
    program: PathBuf,
}

fn on_path(name: &str) -> bool {
    Command::new(name).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

/// Locate a usable `tectonic` binary, most-trusted first: the
/// `WARRAQCRAFT_TECTONIC` env override, then bundled next to the app
/// (installed layout), then `bin/` under the cwd or the workspace
/// root (dev layout), finally PATH.
fn find_tectonic() -> Option<PathBuf> {
    let exe_name = if cfg!(windows) { "tectonic.exe" } else { "tectonic" };
    if let Some(p) = std::env::var_os("WARRAQCRAFT_TECTONIC").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe_path) = std::env::current_exe() {
        if let Some(dir) = exe_path.parent() {
            candidates.push(dir.join(exe_name));
            candidates.push(dir.join("bin").join(exe_name));
            // dev layout: <ws>/target/debug/warraqcraft.exe → <ws>/bin/
            if let Some(ws) = dir.ancestors().nth(2) {
                candidates.push(ws.join("bin").join(exe_name));
            }
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("bin").join(exe_name));
    }
    if let Some(p) = candidates.into_iter().find(|p| p.is_file()) {
        return Some(p);
    }
    if on_path("tectonic") {
        return Some(PathBuf::from("tectonic"));
    }
    None
}

/// Engine binaries found — for UI pickers. Bundled/PATH `tectonic`
/// first (the zero-install engine), then system TeX on PATH.
pub fn detect_engines() -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    if find_tectonic().is_some() {
        found.push("tectonic".to_string());
    }
    found.extend(ENGINE_CANDIDATES.iter().filter(|c| on_path(c)).map(|c| c.to_string()));
    found
}

/// Resolve an engine name to a runnable program.
fn resolve_engine(name: &str) -> Option<Engine> {
    if name == "tectonic" {
        return find_tectonic().map(|p| Engine { name: name.to_string(), program: p });
    }
    if on_path(name) {
        return Some(Engine { name: name.to_string(), program: PathBuf::from(name) });
    }
    None
}

/// Pick a TeX engine. Bundled/PATH `tectonic` wins — it is the
/// zero-install engine and is XeTeX-based, so it also covers the
/// `unicode` requirement; otherwise fall back to system TeX on PATH.
fn find_engine(unicode: bool) -> Option<Engine> {
    if let Some(p) = find_tectonic() {
        return Some(Engine { name: "tectonic".to_string(), program: p });
    }
    const UNICODE: [&str; 2] = ["xelatex", "lualatex"];
    let candidates: &[&str] = if unicode { &UNICODE } else { ENGINE_CANDIDATES };
    candidates.iter().find(|c| on_path(c)).map(|c| Engine { name: c.to_string(), program: PathBuf::from(c) })
}

/// Compile `path` with a system TeX engine into `<dir>/.warraqcraft/`.
/// `src` (the possibly-unsaved buffer) is only used for engine selection;
/// the file on disk is what gets compiled — save first.
pub fn compile_file(path: &Path, src: Option<&str>) -> Result<CompileReport, EngineError> {
    compile_file_with(path, src, None)
}

/// `compile_file` with an explicit engine override — `Some("lualatex")`
/// pins the binary; `None` auto-picks per the document's packages.
pub fn compile_file_with(path: &Path, src: Option<&str>, engine_override: Option<&str>) -> Result<CompileReport, EngineError> {
    let file_dir = path.parent().unwrap_or_else(|| Path::new("."));

    let owned;
    let src = match src {
        Some(s) => s,
        None => {
            owned = String::from_utf8_lossy(&std::fs::read(path)?).into_owned();
            &owned
        }
    };

    // `% !TeX …` header directives the file declares for editors.
    let magic = parse_magic(src);

    // `% !TeX root = main.tex` — compile the declared master file
    // instead; the path resolves relative to this document's directory.
    let root_path = magic.root.as_deref().map(|r| file_dir.join(r)).filter(|c| c != path && c.is_file());
    let compile_path = root_path.as_deref().unwrap_or(path);
    let parent = compile_path.parent().unwrap_or_else(|| Path::new("."));
    let name = compile_path.file_name().ok_or(EngineError::NoFile)?.to_string_lossy();
    let stem = compile_path.file_stem().ok_or(EngineError::NoFile)?.to_string_lossy();

    // Redirected compiles analyse the root's on-disk text — the buffer
    // belongs to the \input'ed file, not the master.
    let root_src;
    let doc_src = match &root_path {
        Some(_) => {
            root_src = String::from_utf8_lossy(&std::fs::read(compile_path)?).into_owned();
            &root_src
        }
        None => src,
    };

    let doc = parse_structure(doc_src);
    let unicode = doc.packages.iter().any(|(p, _)| UNICODE_PACKAGES.contains(&p.as_str()));
    let engine = match engine_override {
        Some(e) => resolve_engine(e).ok_or(EngineError::NoTexEngine)?,
        // `% !TeX program` wins over auto-detection; an unresolvable hint
        // falls back to the package-driven pick.
        None => magic.engine_hint().and_then(resolve_engine).or_else(|| find_engine(unicode)).ok_or(EngineError::NoTexEngine)?,
    };

    migrate_legacy_dir(parent);
    let outdir = parent.join(BUILD_DIR);
    std::fs::create_dir_all(&outdir)?;

    let output = if engine.name == "tectonic" {
        // Tectonic runs its own latexmk-style loop internally
        // (reruns, bibtex/biber) — one invocation. V1 single-file
        // mode: no -shell-escape equivalent exists, shell escape is
        // banned by design.
        Command::new(&engine.program)
            .arg("--keep-logs")
            .arg("--synctex")
            .arg("--outdir")
            .arg(&outdir)
            .arg(name.as_ref())
            .current_dir(parent)
            .output()?
    } else {
        // -output-directory is resolved relative to the process cwd, so
        // pass the bare name; cwd is set to the document directory below.
        // Never pass -shell-escape / -enable-write18: shell escape is
        // banned.
        let run_engine = || -> Result<std::process::Output, EngineError> {
            Ok(Command::new(&engine.program)
                .arg("-interaction=nonstopmode")
                .arg("-halt-on-error")
                .arg("-file-line-error")
                .arg("-synctex=1")
                .arg(format!("-output-directory={BUILD_DIR}"))
                .arg(name.as_ref())
                .current_dir(parent)
                .output()?)
        };

        let mut output = run_engine()?;

        // Bibliography + rerun passes, latexmk-style: driven by the
        // artifacts the first pass produced and by what the log asks
        // for — not by a fixed sequence or by scanning the source.
        // `.bcf` → biber (biblatex); `\bibdata` in the .aux → bibtex
        // (classic \bibliography). A log that asks for a rerun
        // ("Rerun to get cross-references right", label changes, bib
        // requests) gets one, capped.
        const MAX_RERUNS: u32 = 4;
        if output.status.success() {
            let bcf = outdir.join(format!("{stem}.bcf"));
            let aux = outdir.join(format!("{stem}.aux"));
            let needs_bibtex = std::fs::read_to_string(&aux).map(|s| s.contains("\\bibdata")).unwrap_or(false);
            // `% !BIB program` overrides the tool on the classic bibtex
            // path; a `.bcf` still pins biber (biblatex's format).
            let bib_tool = if bcf.is_file() {
                Some("biber")
            } else if needs_bibtex {
                Some(magic.bibliography.as_deref().filter(|b| !b.is_empty()).unwrap_or("bibtex"))
            } else {
                None
            };
            if let Some(tool) = bib_tool {
                // The aux/bcf live in the out dir; the .bib lives next
                // to the source — BIBINPUTS/BSTINPUTS bridge the gap.
                let abs_parent = std::path::absolute(parent)?;
                let sep = if cfg!(windows) { ";" } else { ":" };
                let _ = Command::new(tool)
                    .arg(stem.as_ref())
                    .current_dir(&outdir)
                    .env("BIBINPUTS", format!("{}{sep}", abs_parent.display()))
                    .env("BSTINPUTS", format!("{}{sep}", abs_parent.display()))
                    .output();
                output = run_engine()?;
            }
            let log_path = outdir.join(format!("{stem}.log"));
            let mut passes = 0u32;
            while output.status.success() && passes < MAX_RERUNS && log_needs_rerun(&log_path) {
                output = run_engine()?;
                passes += 1;
            }
        }
        output
    };

    let log_path = outdir.join(format!("{stem}.log"));
    let log = std::fs::read_to_string(&log_path).unwrap_or_else(|_| {
        // No log file — fall back to the captured process output.
        String::from_utf8_lossy(&output.stdout).into_owned()
    });
    let (errors, warnings, pages) = parse_log(&log, parent);

    let pdf = outdir.join(format!("{stem}.pdf"));
    let pdf = if pdf.is_file() { Some(pdf) } else { None };

    let mut tail: Vec<&str> = log.lines().rev().take(30).collect();
    tail.reverse();
    let log_tail = tail.join("\n");

    Ok(CompileReport { engine: engine.name, pdf, pages, errors, warnings, log_tail })
}

/// The TeX log asks for another pass — cross-reference rerun requests,
/// changed labels, bib requests (latexmk-style signals).
fn log_needs_rerun(log_path: &Path) -> bool {
    let Ok(log) = std::fs::read_to_string(log_path) else {
        return false;
    };
    log.contains("Rerun to get")
        || log.contains("Label(s) may have changed")
        || log.contains("Please rerun LaTeX")
        || log.contains("Please (re)run Biber")
        || log.contains("Please (re)run BibTeX")
}

/// Parse a TeX log into `(errors, warnings, page_count)`. `base` resolves
/// the relative paths TeX writes into `-file-line-error` records and the
/// parenthesized input-file stack.
fn parse_log(log: &str, base: &Path) -> (Vec<CompileError>, Vec<CompileError>, Option<u32>) {
    let lines: Vec<&str> = log.lines().collect();
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut pages = None;
    let mut file_stack = Vec::new();

    let mut i = 0;
    while i < lines.len() {
        let Some(line) = lines.get(i) else { break };
        i += 1;
        update_log_file_stack(line, base, &mut file_stack);
        let current_file = file_stack.last().cloned();

        // `! message` fatal error; the `l.<n>` context line gives the
        // source line number, while the input stack gives the file.
        if let Some(msg) = line.strip_prefix('!') {
            let mut lineno = None;
            for ctx in lines.iter().skip(i).take(6) {
                if let Some(rest) = ctx.trim().strip_prefix("l.") {
                    lineno = rest.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok();
                    break;
                }
                if ctx.is_empty() {
                    break;
                }
            }
            errors.push(CompileError { file: current_file, line: lineno, message: msg.trim().to_string() });
            continue;
        }

        // `-file-line-error` style: `<file>:<line>: <message>`.
        if let Some((file, line_no, msg)) = file_line_error(line, base) {
            // Skip if a `!` error already covered this file+line.
            if errors.iter().any(|e: &CompileError| e.file.as_deref() == Some(file.as_path()) && e.line == Some(line_no)) {
                continue;
            }
            errors.push(CompileError { file: Some(file), line: Some(line_no), message: msg });
            continue;
        }

        if let Some(msg) = line.strip_prefix("LaTeX Warning:") {
            warnings.push(CompileError { file: current_file, line: input_line_number(msg), message: msg.trim().to_string() });
            continue;
        }

        if line.starts_with("Output written on") {
            // The `(N pages, …)` part may wrap onto a continuation line
            // (TeX logs hard-wrap near column 79) — join a few lines.
            let joined: String = lines.iter().skip(i - 1).take(3).copied().collect::<Vec<_>>().join(" ");
            pages = joined.rsplit('(').next().and_then(|s| s.split_whitespace().next()).and_then(|n| n.parse::<u32>().ok());
        }
    }
    (errors, warnings, pages)
}

/// Track TeX's `(file ... )` input nesting on a log line. Only source-like
/// paths are pushed, so package comments and `(3 pages)` never enter the
/// stack; wrapped or malformed lines simply leave the previous file active.
fn update_log_file_stack(line: &str, base: &Path, stack: &mut Vec<PathBuf>) {
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && !matches!(bytes[end], b' ' | b'\t' | b'(' | b')') {
                    end += 1;
                }
                if let Some(raw) = line.get(start..end).map(|s| s.trim_matches('"')) {
                    let path = Path::new(raw);
                    let is_source = path.extension().and_then(|e| e.to_str()).is_some_and(|e| ["tex", "sty", "cls", "bib"].contains(&e));
                    if is_source {
                        stack.push(if path.is_absolute() { path.to_path_buf() } else { base.join(path) });
                    }
                }
                i = end;
            }
            b')' => {
                stack.pop();
            }
            _ => {}
        }
        i += 1;
    }
}

/// Parse a `-file-line-error` line `path:12: message` → (path, 12, message).
/// Skips `C:\...` drive prefixes (colon at index 1).
fn file_line_error(line: &str, base: &Path) -> Option<(PathBuf, u32, String)> {
    let mut search_from = 0;
    // Skip a drive-letter colon like `C:`.
    if line.len() >= 2 && line.as_bytes().get(1) == Some(&b':') && line.as_bytes().first().is_some_and(u8::is_ascii_alphabetic) {
        search_from = 2;
    }
    let hay = line.get(search_from..)?;
    // Find `:<digits>:`.
    let bytes = hay.as_bytes();
    let mut j = 0;
    while j < bytes.len() {
        if bytes[j] == b':' {
            let digits_start = j + 1;
            let mut k = digits_start;
            while k < bytes.len() && bytes[k].is_ascii_digit() {
                k += 1;
            }
            if k > digits_start && k < bytes.len() && bytes[k] == b':' {
                let line_no: u32 = hay.get(digits_start..k)?.parse().ok()?;
                let msg = hay.get(k + 1..)?.trim();
                // A bare path colon (`main.tex:`) without a message is not
                // an error line.
                if !msg.is_empty() {
                    let raw = line.get(..search_from + j)?.trim_start_matches('(').trim_matches('"');
                    let path = Path::new(raw);
                    return Some((if path.is_absolute() { path.to_path_buf() } else { base.join(path) }, line_no, msg.to_string()));
                }
            }
        }
        j += 1;
    }
    None
}

/// `... on input line 12.` → `12`.
fn input_line_number(msg: &str) -> Option<u32> {
    let idx = msg.rfind("on input line ")?;
    msg.get(idx + "on input line ".len()..)?.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_LOG: &str = "This is XeTeX, Version 3.141592653\n\
         minimal.tex:7: Undefined control sequence\n\
         l.7 \\badcommand\n\
         ! LaTeX Error: \\begin{document} ended by \\end{itemize}.\n\
         l.9 \\end{itemize}\n\
         LaTeX Warning: Reference `foo' on page 1 undefined on input line 12.\n\
         Output written on minimal.pdf (3 pages, 12345 bytes).\n";

    #[test]
    fn log_parses_errors_warnings_pages() {
        let (errors, warnings, pages) = parse_log(SAMPLE_LOG, Path::new(""));
        assert_eq!(pages, Some(3));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings.first().map(|w| w.line), Some(Some(12)));
        assert!(errors.iter().any(|e| e.line == Some(7)));
        assert!(errors.iter().any(|e| e.line == Some(9) && e.message.contains("\\begin{document}")));
    }

    #[test]
    fn file_line_error_skips_drive_colon() {
        let (file, line, msg) = file_line_error("C:/tmp/main.tex:4: Missing $ inserted", Path::new("")).unwrap();
        assert_eq!(file, Path::new("C:/tmp/main.tex"));
        assert_eq!((line, msg.as_str()), (4, "Missing $ inserted"));
        assert_eq!(file_line_error("This is XeTeX, Version 3.14", Path::new("")), None);
    }

    #[test]
    fn log_tracks_input_files_for_errors_and_warnings() {
        let base = Path::new("project");
        let log =
            "(./main.tex (./intro.tex\n! Undefined control sequence.\nl.4 \\bad\n)\nLaTeX Warning: Reference `x' undefined on input line 7.\n)\n";
        let (errors, warnings, _) = parse_log(log, base);
        assert_eq!(errors.first().and_then(|e| e.file.as_deref()), Some(base.join("intro.tex").as_path()));
        assert_eq!(warnings.first().and_then(|w| w.file.as_deref()), Some(base.join("main.tex").as_path()));
    }

    #[test]
    fn execute_rejects_unknown_and_bad_params() {
        let mut s = Session::new();
        assert!(matches!(s.execute("nope", &json!({})), Err(EngineError::UnknownCommand(_))));
        assert!(matches!(s.execute("document.open", &json!({})), Err(EngineError::BadParam("path"))));
        assert!(matches!(s.execute("source.set", &json!({"text": 42})), Err(EngineError::BadParam("text"))));
        assert!(matches!(s.execute("document.save", &json!({})), Err(EngineError::NoFile)));
    }

    #[test]
    fn document_new_makes_an_untitled_dirty_buffer() {
        let mut s = Session::new();
        s.execute("document.new", &json!({"text": "\\documentclass{article}\n"})).unwrap();
        assert_eq!(s.path(), None);
        assert!(s.dirty());
        assert_eq!(s.source(), "\\documentclass{article}\n");
    }

    #[test]
    fn dirty_buffer_blocks_open_and_new_until_discard_is_explicit() {
        let mut s = Session::new();
        s.execute("document.new", &json!({"text": "unsaved\n"})).unwrap();
        assert!(matches!(s.execute("document.new", &json!({})), Err(EngineError::DirtyBuffer)));
        assert!(matches!(s.execute("document.open", &json!({"path": "other.tex"})), Err(EngineError::DirtyBuffer)));
        assert_eq!(s.source(), "unsaved\n");
        s.execute("document.new", &json!({"text": "replacement\n", "discard": true})).unwrap();
        assert_eq!(s.source(), "replacement\n");
    }

    static NEXT_DIR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("warraqcraft-engine-test-{}-{tag}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn external_disk_change_blocks_save_until_confirmed() {
        let dir = TempDir::new("external");
        let path = dir.0.join("main.tex");
        std::fs::write(&path, "one\n").unwrap();
        let mut s = Session::new();
        s.execute("document.open", &json!({"path": path})).unwrap();
        std::fs::write(&path, "one changed externally\n").unwrap();

        let state = s.execute("document.disk_state", &json!({})).unwrap();
        assert_eq!(state["external_changed"], true);
        assert!(matches!(s.execute("document.save", &json!({})), Err(EngineError::ExternalChange)));
        s.execute("document.save", &json!({"overwrite": true})).unwrap();
        let state = s.execute("document.disk_state", &json!({})).unwrap();
        assert_eq!(state["external_changed"], false);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "one\n");
    }

    #[test]
    fn repo_status_and_dirty_guard_when_git_is_available() {
        if Command::new("git").arg("--version").output().map(|o| !o.status.success()).unwrap_or(true) {
            return;
        }
        let dir = TempDir::new("repo");
        let path = dir.0.join("main.tex");
        std::fs::write(&path, "doc\n").unwrap();
        let mut s = Session::new();
        s.execute("document.open", &json!({"path": path})).unwrap();
        s.execute("repo.init", &json!({})).unwrap();

        let st = s.execute("repo.status", &json!({})).unwrap();
        assert_eq!(st["git"], true);
        assert_eq!(st["in_repo"], true);
        assert_eq!(st["untracked"], 1);
        s.execute("source.set", &json!({"text": "dirty\n"})).unwrap();
        assert!(matches!(s.execute("repo.pull", &json!({})), Err(EngineError::DirtyBuffer)));
    }

    #[test]
    fn execute_source_and_structure() {
        let mut s = Session::new();
        s.execute("source.set", &json!({"text": "\\documentclass{article}\n\\begin{document}\n\\section{Hi}\n\\end{document}\n"})).unwrap();
        let st = s.execute("structure.get", &json!({})).unwrap();
        assert_eq!(st["class"], "article");
        assert_eq!(st["headings"][0]["title"], "Hi");
        assert!(s.dirty());
    }

    #[test]
    fn magic_get_reads_header_directives() {
        let mut s = Session::new();
        s.execute(
            "source.set",
            &json!({"text": "% !TeX program = xelatex\n% !TeX root = main.tex\n% !BIB program = biber\n\\documentclass{article}\n"}),
        )
        .unwrap();
        let m = s.execute("magic.get", &json!({})).unwrap();
        assert_eq!(m["program"], "xelatex");
        assert_eq!(m["root"], "main.tex");
        assert_eq!(m["bibliography"], "biber");
        assert_eq!(m["engine_hint"], "xelatex");
    }

    #[test]
    fn hostile_params_never_panic() {
        let mut s = Session::new();
        for cmd in [
            "document.new",
            "document.open",
            "document.save",
            "document.disk_state",
            "document.compile",
            "source.set",
            "source.get",
            "structure.get",
            "magic.get",
            "user_tool.list",
            "user_tool.set",
            "user_tool.run",
            "user_tool.clear",
            "template.list",
            "template.read",
            "template.new",
            "template.import",
            "math.preview",
            "text.stats",
            "project.graph",
            "project.structure",
            "repo.status",
            "repo.init",
            "repo.pull",
            "repo.push",
            "repo.commit",
            "repo.branches",
            "repo.conflicts",
            "repo.history",
            "project.snapshot",
            "snapshot.list",
            "snapshot.read",
            "snapshot.restore",
            "garbage",
        ] {
            for p in [json!(null), json!({"path": 1, "text": null}), json!("x"), json!([1, 2])] {
                let _ = s.execute(cmd, &p);
            }
        }
    }
}
