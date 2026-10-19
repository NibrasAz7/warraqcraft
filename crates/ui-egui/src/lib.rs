//! WarraqCraft Studio — egui front end.
//!
//! LocalLeaf-style three-pane layout: project file tree + document
//! outline on the left, a syntax-highlighted `.tex` editor with gutter
//! and status bar in the center, and a hayro-rendered PDF preview on the
//! right. `document.compile` (crates/engine) shells out to a system TeX
//! engine (never shell-escape) into `.warraqcraft/`; pages are rasterized
//! on a worker thread.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod ai;
mod cite;
/// Opt-in TCP control channel (`warraqcraft --control FILE`, native only).
#[cfg(not(target_arch = "wasm32"))]
pub mod control;
mod edits;
mod files;
mod fold;
mod fonts;
mod highlight;
mod icons;
pub mod lang;
mod mathprev;
mod mirrors;
mod spellcheck;
mod theme;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use egui::text::{CCursor, CCursorRange};
use serde_json::Value;
use texparse::DocStructure;
use warraqcraft_engine::{CompileReport, EngineError, Session};

const DEFAULT_DOC: &str = "\\documentclass{article}\n\\usepackage{amsmath}\n\n\\title{Untitled}\n\\author{}\n\n\\begin{document}\n\\maketitle\n\n\\section{Introduction}\n\n\\end{document}\n";

/// Render scale for preview pages (roughly 110 dpi on screen).
const PREVIEW_SCALE: f32 = 1.6;
/// Safety cap: never render a page bigger than this in either dimension.
const MAX_PREVIEW_PX: f32 = 4096.0;
/// Safety cap: never render more pages than this in one preview.
const MAX_PREVIEW_PAGES: usize = 128;
/// Editor gutter width in points — fold marker + issue dot + number.
const GUTTER_W: f32 = 56.0;
/// How often the project tree is rescanned, seconds.
const TREE_SCAN_SECS: f64 = 1.5;
/// Commands whose `{…}` arg completes project-wide `\label`s (M3) —
/// mirrors `texparse`'s REFS scan set.
const REF_CMDS: &[&str] = &["ref", "eqref", "pageref", "autoref", "cref", "Cref", "vref"];
/// Commands whose `{…}` arg completes bibliography keys — BibTeX and the
/// common biblatex variants.
const CITE_CMDS: &[&str] = &["cite", "parencite", "textcite", "citealp", "citeauthor", "citeyear", "nocite"];
/// Symbol-palette commands, grouped like the palettes in desktop TeX editors.
const SYMBOL_GREEK: &[&str] = &[
    "alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta", "iota", "kappa", "lambda", "mu", "nu", "xi", "pi", "rho", "sigma", "tau",
    "upsilon", "phi", "chi", "psi", "omega", "Gamma", "Delta", "Theta", "Lambda", "Xi", "Pi", "Sigma", "Phi", "Psi", "Omega",
];
const SYMBOL_MATH: &[&str] = &[
    "pm", "mp", "times", "div", "cdot", "ast", "circ", "oplus", "ominus", "otimes", "sqrt{}", "frac{}{}", "sum", "prod", "int", "oint", "partial",
    "nabla", "infty", "aleph", "hbar", "ell", "emptyset",
];
const SYMBOL_RELATIONS: &[&str] = &[
    "leq", "geq", "neq", "approx", "equiv", "propto", "sim", "simeq", "cong", "ll", "gg", "in", "notin", "subset", "supset", "subseteq", "supseteq",
    "cup", "cap", "forall", "exists", "nexists", "vdash", "models",
];
const SYMBOL_ARROWS: &[&str] = &[
    "leftarrow",
    "rightarrow",
    "leftrightarrow",
    "Leftarrow",
    "Rightarrow",
    "Leftrightarrow",
    "mapsto",
    "uparrow",
    "downarrow",
    "updownarrow",
    "longrightarrow",
    "longleftarrow",
    "Longrightarrow",
    "Longleftarrow",
    "hookrightarrow",
    "to",
];

/// A rasterized page plus its PDF size in points (top-left origin) —
/// SyncTeX speaks points, so we need the pt dimensions to map clicks.
struct Page {
    img: egui::ColorImage,
    pt: egui::Vec2,
}

/// A finished compile job: the report plus rasterized page images.
#[cfg(not(target_arch = "wasm32"))]
type CompileJob = Result<(CompileReport, Vec<Page>), String>;

/// One template-gallery card, localized for the active UI language.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default)]
struct TemplateCard {
    id: String,
    name: String,
    desc: String,
    lang: String,
    category: String,
    engine: String,
    origin: String,
    license: String,
    source: String,
    files: Vec<String>,
}

/// Preview zoom mode for the PDF pane.
#[derive(Clone, Copy, PartialEq)]
enum Zoom {
    FitWidth,
    FitPage,
    Pct(f32),
}

impl Zoom {
    /// Stable string form for `WarraqPrefs::zoom`.
    fn pref(&self) -> String {
        match self {
            Zoom::FitWidth => "fit-width".into(),
            Zoom::FitPage => "fit-page".into(),
            Zoom::Pct(p) => format!("{}", p.round() as i32),
        }
    }

    /// Parse `WarraqPrefs::zoom`; unknown values fall back to fit-width.
    fn from_pref(s: &str) -> Zoom {
        match s {
            "fit-page" => Zoom::FitPage,
            "fit-width" => Zoom::FitWidth,
            other => other.parse::<f32>().ok().map(|p| Zoom::Pct(p.clamp(25.0, 400.0))).unwrap_or(Zoom::FitWidth),
        }
    }
}

/// How the preview lays out pages: a continuous scroll, one page at a
/// time, two pages per spread, or book layout (cover alone, then
/// verso/recto pairs).
#[derive(Debug, Clone, Copy, PartialEq)]
enum PageMode {
    Scroll,
    Single,
    TwoUp,
    TwoUpCover,
}

impl PageMode {
    /// All modes, in menu order.
    const ALL: [PageMode; 4] = [PageMode::Scroll, PageMode::Single, PageMode::TwoUp, PageMode::TwoUpCover];

    fn label(self, s: &lang::Strings) -> &'static str {
        match self {
            PageMode::Scroll => s.page_continuous,
            PageMode::Single => s.page_single,
            PageMode::TwoUp => s.page_two_up,
            PageMode::TwoUpCover => s.page_two_up_cover,
        }
    }

    fn pref(self) -> &'static str {
        match self {
            PageMode::Scroll => "scroll",
            PageMode::Single => "single",
            PageMode::TwoUp => "two-up",
            PageMode::TwoUpCover => "two-up-cover",
        }
    }

    fn from_pref(s: &str) -> PageMode {
        match s {
            "single" => PageMode::Single,
            "two-up" => PageMode::TwoUp,
            "two-up-cover" => PageMode::TwoUpCover,
            _ => PageMode::Scroll,
        }
    }
}

/// Persisted preferences (craft-settings `config.toml`): theme, preview
/// zoom, panel visibility, engine choice (by name — the discovered list
/// order isn't stable), and the auto-compile toggle.
#[derive(serde::Serialize, serde::Deserialize, Clone, PartialEq)]
#[serde(default)]
struct WarraqPrefs {
    dark: bool,
    autocompile: bool,
    engine: String,
    zoom: String,
    outline: bool,
    logs: bool,
    /// Spell underlines (dictionary must exist under `dicts/`).
    spellcheck: bool,
    /// Preview page layout (see [`PageMode::pref`]).
    page_mode: String,
    /// BYO AI endpoint + model. The token is deliberately NOT persisted
    /// — it stays in-memory for the session.
    ai_endpoint: String,
    ai_model: String,
    /// Provider preset (`ollama`, `openai`, `openrouter`, `custom`).
    ai_provider: String,
    /// UI language code — see [`lang::Lang::from_code`].
    language: String,
    /// Last document path and dirty-buffer autosave. Only dirty text is
    /// persisted; clean buffers recover by reopening `last_file`.
    last_file: String,
    autosave_source: String,
    autosave_dirty: bool,
    autosave_line: usize,
    autosave_col: usize,
}

impl Default for WarraqPrefs {
    fn default() -> Self {
        Self {
            dark: true,
            autocompile: false,
            engine: String::new(),
            zoom: "fit-width".into(),
            outline: true,
            logs: false,
            spellcheck: true,
            page_mode: "scroll".into(),
            ai_endpoint: String::new(),
            ai_model: String::new(),
            ai_provider: String::new(),
            language: "en".into(),
            last_file: String::new(),
            autosave_source: String::new(),
            autosave_dirty: false,
            autosave_line: 0,
            autosave_col: 0,
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl craft_settings::Settings for WarraqPrefs {
    const APP_ID: &'static str = "warraqcraft";
    const SCHEMA_VERSION: u32 = 2;

    fn migrate(_from: u32, _doc: &mut craft_settings::DocumentMut) -> Result<(), craft_settings::MigrateError> {
        Ok(())
    }
}

/// Native: the real craft-settings store (platform config dir, atomic
/// TOML writes, backup + recovery).
#[cfg(not(target_arch = "wasm32"))]
type PrefsStore = craft_settings::SettingsStore<WarraqPrefs>;

/// Wasm: craft-settings' persistent store doesn't exist there — a
/// same-shaped in-memory stub keeps the call sites unconditional.
#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct PrefsStore(WarraqPrefs);

#[cfg(target_arch = "wasm32")]
impl PrefsStore {
    fn get(&self) -> &WarraqPrefs {
        &self.0
    }

    fn update(&mut self, f: impl FnOnce(&mut WarraqPrefs)) -> bool {
        f(&mut self.0);
        true
    }

    fn flush(&mut self) -> Result<(), ()> {
        Ok(())
    }
}

/// Open the prefs store — platform config dir on native, memory on wasm.
fn open_prefs() -> PrefsStore {
    #[cfg(not(target_arch = "wasm32"))]
    {
        craft_settings::SettingsStore::<WarraqPrefs>::open(craft_settings::Location::Auto).0
    }
    #[cfg(target_arch = "wasm32")]
    {
        PrefsStore::default()
    }
}

/// A SyncTeX highlight on a preview page: which page, the box in PDF
/// points, and the `ctx.input` timestamp it appeared at.
struct Flash {
    page: usize,
    rect_pt: egui::Rect,
    at: f64,
}
const FLASH_SECS: f64 = 1.6;

/// What the "new entry" modal is creating, and where.
struct NewEntry {
    dir: PathBuf,
    name: String,
    is_dir: bool,
}

/// A gutter issue dot for one source line: every message the last
/// compile attributed to it, plus whether any is an error (red) versus
/// all warnings (amber).
struct GutterMark {
    is_error: bool,
    messages: Vec<String>,
}

/// An open completion popup: the candidate list and where the typed
/// prefix begins. `start` is a char index into the source; `env` means
/// we're inside `\begin{…` so candidates come from
/// `texpkg::environments()`.
struct Completion {
    start: usize,
    /// Char index of the caret when the prefix was last recomputed —
    /// the `\word`/`{word` span is `start..end`.
    end: usize,
    env: bool,
    items: Vec<CompletionItem>,
    sel: usize,
}

/// One popup row, pre-rendered: `\cmd`/`env` items come from `texpkg`,
/// `\ref{}` items come from the project's labels. `insert` is the text
/// that replaces the prefix span; `caret_back` is how many chars before
/// its end the caret lands (1 for `\name{}` → inside the braces).
struct CompletionItem {
    name: String,
    /// Signature/mono line next to the name (`[opts]{req}`), may be empty.
    sig: String,
    /// Muted second line — `package — doc` for catalog items, the
    /// defining file for labels.
    meta: String,
    insert: String,
    caret_back: usize,
}

/// One `.tex` file's contribution to the merged project outline.
struct ProjFile {
    path: PathBuf,
    /// Base file name for the row badge (`ch1.tex`).
    name: String,
    /// This file is the one in the editor — its headings come live from
    /// `self.doc`, not this (possibly stale) disk scan.
    is_current: bool,
    headings: Vec<texparse::Heading>,
    /// `\label`s defined here — used to tag `\ref{}` completion items
    /// with their defining file.
    labels: Vec<String>,
    /// `\bibliography` targets declared here — resolved to feed inline
    /// `\cite{}` key completion.
    bibliographies: Vec<String>,
}

/// A finished user-tool run (engine `user_tool.run` result).
#[cfg(not(target_arch = "wasm32"))]
type ToolJob = Result<serde_json::Value, String>;

/// Git status for the collaboration chip/window, decoded from
/// `repo.status` rather than duplicating command parsing in the UI.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Default, Clone)]
struct RepoBranch {
    name: String,
    current: bool,
    upstream: String,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default, Clone)]
struct RepoChange {
    status: String,
    path: String,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default, Clone)]
struct RepoCommit {
    commit: String,
    date: String,
    author: String,
    subject: String,
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Default, Clone)]
struct RepoStatus {
    git: bool,
    in_repo: bool,
    branch: String,
    upstream: String,
    remote: String,
    remote_url: String,
    ahead: u64,
    behind: u64,
    changed: u64,
    conflicts: u64,
    clean: bool,
    conflict_paths: Vec<String>,
    changes: Vec<RepoChange>,
    branches: Vec<RepoBranch>,
    history: Vec<RepoCommit>,
}

/// `(command, result)` — status refreshes and mutating repo actions share
/// one worker channel so Git never blocks a frame.
#[cfg(not(target_arch = "wasm32"))]
type RepoJob = (String, Result<serde_json::Value, String>);

/// The studio application state.
pub struct WarraqApp {
    session: Session,
    doc: DocStructure,
    status: String,
    compiling: bool,
    #[cfg(not(target_arch = "wasm32"))]
    compile_rx: Option<std::sync::mpsc::Receiver<CompileJob>>,
    report: Option<CompileReport>,
    pages: Vec<egui::TextureHandle>,
    /// PDF point size per page, parallel to `pages`.
    page_pts: Vec<egui::Vec2>,
    pdf_path: Option<PathBuf>,
    page_idx: usize,
    zoom: Zoom,
    /// Page layout mode for the preview canvas.
    page_mode: PageMode,
    /// Painted rects of the pages drawn last frame, in page order —
    /// lets ‹ › scroll to a page in continuous mode.
    page_rects: Vec<egui::Rect>,
    /// Page rect the next paint should scroll into view (‹ › jump in
    /// continuous mode).
    pending_page_scroll: Option<egui::Rect>,
    flash: Option<Flash>,

    // Sidebar + editor chrome
    tree: Vec<files::Node>,
    tree_scanned_at: f64,
    expanded: HashSet<PathBuf>,
    main_file: Option<PathBuf>,
    /// Merged project structure (`project.structure`): every `.tex`
    /// reachable from the root, in graph order. Drives the outline and
    /// `\ref{}` completion.
    proj_files: Vec<ProjFile>,
    /// Union of `\label`s across the project (plus the live buffer's).
    proj_labels: Vec<String>,
    /// Bibliography entries from the project's declared `.bib` files —
    /// feeds inline `\cite{}` completion.
    #[cfg(not(target_arch = "wasm32"))]
    proj_bibs: Vec<cite::CiteEntry>,
    /// Unresolved file deps the scan saw (missing `\input`, graphics, bibs).
    proj_missing: usize,
    proj_scanned_at: f64,
    outline_open: bool,
    show_logs: bool,
    cursor_lc: (usize, usize),
    /// 1-based start lines of collapsed fold regions (source coords).
    /// Cleared on any buffer edit or file switch so a stale fold can't
    /// hide the wrong lines; survives compiles.
    folded: BTreeSet<usize>,
    /// Compile error/warning dots for the gutter, keyed by 1-based
    /// source line. Rebuilt on every finished compile — stale marks
    /// survive edits (TeXstudio-style) until the next report.
    marks: BTreeMap<usize, GutterMark>,
    /// Widget id of the editor `TextEdit`, set every frame — keyboard
    /// line commands only fire while it has focus.
    editor_id: Option<egui::Id>,
    /// Pending "jump to byte offset" for the next editor frame (outline
    /// click-through).
    goto: Option<usize>,
    engines: Vec<String>,
    engine_sel: Option<usize>,
    new_entry: Option<NewEntry>,
    /// Insert-citation picker (`Ctrl+Shift+C` / top-bar button).
    cite: cite::CiteDialog,
    /// `% !TeX`/`% !BIB` header directives of the buffer — reparsed
    /// with the structure pass so the status line can show the
    /// engine/root hint.
    magic: texparse::Magic,
    /// Open completion popup, if any.
    complete: Option<Completion>,
    /// Result channel of a running user tool.
    #[cfg(not(target_arch = "wasm32"))]
    tool_rx: Option<std::sync::mpsc::Receiver<ToolJob>>,

    // Auto-compile after idle
    autocompile: bool,
    /// `ctx.input` timestamp of the last buffer edit.
    last_edit: f64,
    /// `last_edit` value at the last compile attempt — a failed compile
    /// does not retrigger until the user edits again.
    compiled_edit: f64,

    // Persisted preferences (M4): memory-only on wasm.
    settings: PrefsStore,

    /// "New from template" gallery: `template.list` cards, the picked
    /// card, search/category state, source preview, and the new file name.
    #[cfg(not(target_arch = "wasm32"))]
    tpl_open: bool,
    #[cfg(not(target_arch = "wasm32"))]
    tpls: Vec<TemplateCard>,
    #[cfg(not(target_arch = "wasm32"))]
    tpl_sel: Option<usize>,
    #[cfg(not(target_arch = "wasm32"))]
    tpl_name: String,
    #[cfg(not(target_arch = "wasm32"))]
    tpl_filter: String,
    #[cfg(not(target_arch = "wasm32"))]
    tpl_category: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    tpl_language: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    tpl_preview: String,
    #[cfg(not(target_arch = "wasm32"))]
    tpl_preview_for: String,
    #[cfg(not(target_arch = "wasm32"))]
    tpl_license: String,

    /// Local-history window: `snapshot.list` rows `(id, ts, label,
    /// files)`, a pending restore id awaiting its second click, and the
    /// label for the next `project.snapshot`.
    #[cfg(not(target_arch = "wasm32"))]
    hist_open: bool,
    #[cfg(not(target_arch = "wasm32"))]
    snaps: Vec<(String, String, String, i64)>,
    #[cfg(not(target_arch = "wasm32"))]
    hist_confirm: Option<String>,
    #[cfg(not(target_arch = "wasm32"))]
    hist_label: String,

    /// Spell underlines (M5): dict-backed on native, inert on wasm.
    spell: spellcheck::SpellCheck,
    /// Bump on every buffer edit — gates `spell.scan` rescans.
    src_stamp: u64,
    /// Active occurrence mirrors (Ctrl+M): primary selection + ranges
    /// that receive the same edit.
    mirrors: Option<mirrors::Mirrors>,
    /// Previous frame's buffer — mirror replication diffs it against
    /// the post-edit text.
    prev_src: String,
    /// AI assist panel state (BYO provider; native does the network).
    ai: ai::AiPanel,
    /// Durable AI harness inspector (tasks, memory, plans, audit).
    harness: ai::HarnessPanel,
    /// Document-statistics window.
    stats_open: bool,
    /// Preferences window.
    settings_open: bool,
    /// Keyboard-shortcuts help window.
    shortcuts_open: bool,
    /// Categorized LaTeX-symbol palette window.
    symbols_open: bool,
    /// Cached worker-compiled previews for the math under the caret.
    mathprev: mathprev::MathPreview,
    /// Print job result channel (submitted on a worker — `submit`
    /// blocks on the spooler).
    #[cfg(not(target_arch = "wasm32"))]
    print_rx: Option<std::sync::mpsc::Receiver<Result<String, String>>>,
    /// Git-native collaboration state and worker channel.
    #[cfg(not(target_arch = "wasm32"))]
    repo_status: Option<RepoStatus>,
    #[cfg(not(target_arch = "wasm32"))]
    repo_rx: Option<std::sync::mpsc::Receiver<RepoJob>>,
    #[cfg(not(target_arch = "wasm32"))]
    repo_checked_at: f64,
    #[cfg(not(target_arch = "wasm32"))]
    repo_open: bool,
    #[cfg(not(target_arch = "wasm32"))]
    repo_commit_msg: String,
    #[cfg(not(target_arch = "wasm32"))]
    repo_stage_all: bool,
    #[cfg(not(target_arch = "wasm32"))]
    repo_remote_name: String,
    #[cfg(not(target_arch = "wasm32"))]
    repo_remote_url: String,
    #[cfg(not(target_arch = "wasm32"))]
    repo_branch_new: String,
    #[cfg(not(target_arch = "wasm32"))]
    repo_pull_rebase: bool,
    #[cfg(not(target_arch = "wasm32"))]
    repo_push_upstream: bool,
    /// Open file changed/deleted on disk after its loaded stamp. The
    /// modal opens on the first transition; the chip remains until
    /// reload/overwrite clears the state.
    external_changed: bool,
    external_modal: bool,
    external_checked_at: f64,
    /// Opt-in `--control FILE` TCP channel, polled in `logic`.
    #[cfg(not(target_arch = "wasm32"))]
    control: Option<control::Control>,
}
const AUTOCOMPILE_IDLE_SECS: f64 = 5.0;
#[cfg(not(target_arch = "wasm32"))]
const REPO_SCAN_SECS: f64 = 10.0;
#[cfg(not(target_arch = "wasm32"))]
const EXTERNAL_SCAN_SECS: f64 = 1.0;

impl WarraqApp {
    /// Create the app; `file` (optional) is opened at startup.
    pub fn new(_cc: &eframe::CreationContext<'_>, file: Option<PathBuf>) -> Self {
        let settings = open_prefs();
        theme::apply(&_cc.egui_ctx, settings.get().dark);
        fonts::install(&_cc.egui_ctx);
        let mut app = WarraqApp {
            session: Session::new(),
            doc: DocStructure::default(),
            status: String::new(),
            compiling: false,
            #[cfg(not(target_arch = "wasm32"))]
            compile_rx: None,
            report: None,
            pages: Vec::new(),
            page_pts: Vec::new(),
            pdf_path: None,
            page_idx: 0,
            zoom: Zoom::FitWidth,
            page_mode: PageMode::Scroll,
            page_rects: Vec::new(),
            pending_page_scroll: None,
            flash: None,
            tree: Vec::new(),
            tree_scanned_at: f64::NEG_INFINITY,
            expanded: HashSet::new(),
            main_file: None,
            proj_files: Vec::new(),
            proj_labels: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            proj_bibs: Vec::new(),
            proj_missing: 0,
            proj_scanned_at: f64::NEG_INFINITY,
            outline_open: true,
            show_logs: false,
            cursor_lc: (1, 1),
            folded: BTreeSet::new(),
            marks: BTreeMap::new(),
            editor_id: None,
            goto: None,
            engines: Vec::new(),
            engine_sel: None,
            new_entry: None,
            cite: cite::CiteDialog::default(),
            magic: texparse::Magic::default(),
            complete: None,
            #[cfg(not(target_arch = "wasm32"))]
            tool_rx: None,
            autocompile: false,
            last_edit: f64::NEG_INFINITY,
            compiled_edit: f64::NEG_INFINITY,
            settings,
            #[cfg(not(target_arch = "wasm32"))]
            tpl_open: false,
            #[cfg(not(target_arch = "wasm32"))]
            tpls: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            tpl_sel: None,
            #[cfg(not(target_arch = "wasm32"))]
            tpl_name: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            tpl_filter: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            tpl_category: None,
            #[cfg(not(target_arch = "wasm32"))]
            tpl_language: None,
            #[cfg(not(target_arch = "wasm32"))]
            tpl_preview: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            tpl_preview_for: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            tpl_license: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            hist_open: false,
            #[cfg(not(target_arch = "wasm32"))]
            snaps: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            hist_confirm: None,
            #[cfg(not(target_arch = "wasm32"))]
            hist_label: String::new(),
            spell: spellcheck::SpellCheck::load(),
            src_stamp: 0,
            mirrors: None,
            prev_src: String::new(),
            ai: ai::AiPanel::new(),
            harness: ai::HarnessPanel::new(),
            stats_open: false,
            settings_open: false,
            shortcuts_open: false,
            symbols_open: false,
            mathprev: mathprev::MathPreview::new(),
            #[cfg(not(target_arch = "wasm32"))]
            print_rx: None,
            #[cfg(not(target_arch = "wasm32"))]
            repo_status: None,
            #[cfg(not(target_arch = "wasm32"))]
            repo_rx: None,
            #[cfg(not(target_arch = "wasm32"))]
            repo_checked_at: f64::NEG_INFINITY,
            #[cfg(not(target_arch = "wasm32"))]
            repo_open: false,
            #[cfg(not(target_arch = "wasm32"))]
            repo_commit_msg: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            repo_stage_all: true,
            #[cfg(not(target_arch = "wasm32"))]
            repo_remote_name: "origin".to_string(),
            #[cfg(not(target_arch = "wasm32"))]
            repo_remote_url: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            repo_branch_new: String::new(),
            #[cfg(not(target_arch = "wasm32"))]
            repo_pull_rebase: false,
            #[cfg(not(target_arch = "wasm32"))]
            repo_push_upstream: false,
            external_changed: false,
            external_modal: false,
            external_checked_at: f64::NEG_INFINITY,
            #[cfg(not(target_arch = "wasm32"))]
            control: None,
        };
        // Restore the non-theme prefs (theme applied above).
        let p = app.settings.get().clone();
        app.autocompile = p.autocompile;
        app.zoom = Zoom::from_pref(&p.zoom);
        app.outline_open = p.outline;
        app.show_logs = p.logs;
        app.page_mode = PageMode::from_pref(&p.page_mode);
        app.spell.set_enabled(p.spellcheck);
        let ai_preset = if !p.ai_provider.is_empty() {
            ai::AiProviderPreset::from_key(&p.ai_provider)
        } else if !p.ai_endpoint.is_empty() {
            ai::AiProviderPreset::from_endpoint(&p.ai_endpoint)
        } else {
            ai::AiProviderPreset::Ollama
        };
        if p.ai_endpoint.is_empty() && p.ai_model.is_empty() {
            app.ai.set_provider(ai_preset);
        } else {
            app.ai.provider = ai_preset;
            if !p.ai_endpoint.is_empty() {
                app.ai.endpoint = p.ai_endpoint.clone();
            }
            if !p.ai_model.is_empty() {
                app.ai.model = p.ai_model.clone();
            }
        }
        app.session = Session::with_source(DEFAULT_DOC.to_string());
        #[cfg(not(target_arch = "wasm32"))]
        {
            app.engines = warraqcraft_engine::detect_engines();
            app.engine_sel = app.engines.iter().position(|e| *e == p.engine);
        }
        match file {
            Some(p) => app.open_path(p),
            None => app.restore_or_default(&p),
        }
        app
    }

    /// Restore the last buffer or start with the bundled sample. Dirty text
    /// is persisted separately from the path so an unsaved document survives
    /// a normal close without inventing an on-disk filename.
    fn restore_or_default(&mut self, p: &WarraqPrefs) {
        let last = (!p.last_file.is_empty()).then(|| PathBuf::from(&p.last_file));
        if p.autosave_dirty && !p.autosave_source.is_empty() {
            let mut session = Session::new();
            if let Some(path) = last.as_deref().filter(|path| path.is_file()) {
                let _ = session.open(path);
            }
            session.set_source(p.autosave_source.clone());
            self.session = session;
            self.reparse();
            self.goto = Some(offset_of_line_col(self.session.source(), p.autosave_line, p.autosave_col));
            self.status = self.s().status_recovered.into();
        } else if let Some(path) = last.filter(|path| path.is_file()) {
            self.open_path(path);
            self.goto = Some(offset_of_line_col(self.session.source(), p.autosave_line, p.autosave_col));
        } else {
            self.reparse();
        }
    }

    /// Attach a started control channel (`warraqcraft --control FILE`).
    /// `ctx` lets connection threads wake the UI so a reply doesn't
    /// wait for the next input event.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_control(&mut self, ctx: &egui::Context, c: control::Control) {
        let mut c = c;
        c.wake_on(ctx.clone());
        self.control = Some(c);
    }

    fn reparse(&mut self) {
        self.doc = self.session.structure();
        self.magic = texparse::parse_magic(self.session.source());
    }

    /// Send the last compiled PDF to the system printer on a worker —
    /// `craft_print::submit` blocks in the spooler call.
    #[cfg(not(target_arch = "wasm32"))]
    fn print_pdf(&mut self) {
        if self.print_rx.is_some() {
            self.status = self.s().status_print_busy.into();
            return;
        }
        let Some(pdf) = self.pdf_path.clone() else {
            self.status = self.s().status_no_pdf.into();
            return;
        };
        let title = pdf.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "warraqcraft".into());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let res = std::fs::read(&pdf)
                .map_err(|e| e.to_string())
                .and_then(|bytes| craft_print::submit(&bytes, &craft_print::Job { title, ..Default::default() }).map_err(|e| e.to_string()));
            let _ = tx.send(res);
        });
        self.print_rx = Some(rx);
        self.status = self.s().status_printing.into();
    }

    /// Save a copy of the last compiled PDF where the user chooses.
    #[cfg(not(target_arch = "wasm32"))]
    fn export_pdf(&mut self) {
        let Some(pdf) = self.pdf_path.clone() else {
            self.status = self.s().status_no_pdf.into();
            return;
        };
        let file_name = pdf.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "document.pdf".into());
        let Some(dest) = rfd::FileDialog::new().add_filter("PDF", &["pdf"]).set_file_name(&file_name).save_file() else { return };
        self.status = match std::fs::copy(&pdf, &dest) {
            Ok(_) => lang::fmt(self.s().status_exported, &[("path", &dest.display().to_string())]),
            Err(e) => lang::fmt(self.s().status_failed, &[("context", "export"), ("error", &e.to_string())]),
        };
    }

    /// Drain a finished print job into the status line.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_print(&mut self) {
        use std::sync::mpsc::TryRecvError::*;
        let Some(rx) = &self.print_rx else { return };
        match rx.try_recv() {
            Ok(Ok(job)) => {
                self.status = lang::fmt(self.s().status_printed, &[("job", &job)]);
                self.print_rx = None;
            }
            Ok(Err(e)) => {
                self.status = lang::fmt(self.s().status_print_failed, &[("error", &e)]);
                self.print_rx = None;
            }
            Err(Empty) => {}
            Err(Disconnected) => {
                self.status = self.s().status_print_dead.into();
                self.print_rx = None;
            }
        }
    }

    /// Run a Git command on a worker. The scratch session carries the
    /// same path/source so read-only status also reports the live dirty
    /// flag; mutating commands are pre-guarded by the UI before spawning.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_repo_command(&mut self, command: &str, params: serde_json::Value) {
        if self.repo_rx.is_some() {
            self.status = self.s().repo_busy.into();
            return;
        }
        let branch_mutation = command == "repo.branches" && (params.get("create").is_some() || params.get("switch").is_some());
        let mutating = matches!(command, "repo.pull" | "repo.push" | "repo.commit") || branch_mutation;
        if mutating && self.session.dirty() {
            self.status = self.s().status_disk_dirty.into();
            return;
        }
        if mutating && self.session.disk_changed().unwrap_or(false) {
            self.external_changed = true;
            self.external_modal = true;
            return;
        }
        let path = self.session.path().map(Path::to_path_buf);
        let source = self.session.source().to_string();
        let command = command.to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        self.repo_rx = Some(rx);
        std::thread::spawn(move || {
            let mut session = Session::new();
            if let Some(path) = path {
                let _ = session.open(&path);
                session.set_source(source);
            }
            let result = session.execute(&command, &params).map_err(|e| e.to_string());
            let _ = tx.send((command, result));
        });
    }

    /// Refresh repository status at a slow cadence; git subprocesses are
    /// useful for a chip but too expensive to run every frame.
    #[cfg(not(target_arch = "wasm32"))]
    fn refresh_repo(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if self.repo_rx.is_none() && now - self.repo_checked_at >= REPO_SCAN_SECS {
            self.repo_checked_at = now;
            self.start_repo_command("repo.status", serde_json::json!({}));
        } else if self.repo_rx.is_none() {
            ctx.request_repaint_after_secs((REPO_SCAN_SECS - (now - self.repo_checked_at).max(0.0)) as f32);
        }
    }

    /// Drain the Git worker. Mutating actions get a visible status; a
    /// quiet status refresh only updates the chip/modal data.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_repo(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.repo_rx else { return };
        match rx.try_recv() {
            Ok((command, Ok(v))) => {
                self.repo_rx = None;
                if command == "repo.status" {
                    self.repo_status = Some(repo_status_from_json(&v));
                } else {
                    if command == "repo.remote" {
                        self.repo_remote_url.clear();
                    }
                    self.status = lang::fmt(self.s().status_repo_done, &[("cmd", &command)]);
                    self.repo_checked_at = f64::NEG_INFINITY;
                    self.external_checked_at = f64::NEG_INFINITY;
                    ctx.request_repaint();
                }
            }
            Ok((command, Err(e))) => {
                self.repo_rx = None;
                if command == "repo.status" {
                    self.repo_status = None;
                }
                self.status = lang::fmt(self.s().status_failed, &[("context", &command), ("error", &e)]);
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint(),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.repo_rx = None;
                self.status = self.s().status_tool_dead.into();
            }
        }
    }

    /// Cheap mtime/length check for edits made outside the app. The
    /// indicator persists after the modal is dismissed; reload or an
    /// explicitly confirmed overwrite clears it.
    #[cfg(not(target_arch = "wasm32"))]
    fn check_external_file(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.external_checked_at < EXTERNAL_SCAN_SECS {
            ctx.request_repaint_after_secs((EXTERNAL_SCAN_SECS - (now - self.external_checked_at).max(0.0)) as f32);
            return;
        }
        self.external_checked_at = now;
        ctx.request_repaint_after_secs(EXTERNAL_SCAN_SECS as f32);
        match self.session.disk_changed() {
            Ok(true) => {
                if !self.external_changed {
                    self.external_modal = true;
                }
                self.external_changed = true;
            }
            Ok(false) => {
                self.external_changed = false;
                self.external_modal = false;
            }
            Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", self.s().open_file), ("error", &e.to_string())]),
        }
    }

    /// Reload the file the user explicitly chose to take from disk. This
    /// is the one path that may discard local edits, and only after the
    /// conflict modal's explicit Reload button.
    #[cfg(not(target_arch = "wasm32"))]
    fn reload_current(&mut self) {
        let Some(path) = self.session.path().map(Path::to_path_buf) else { return };
        match self.session.open(&path) {
            Ok(()) => {
                self.external_changed = false;
                self.external_modal = false;
                self.folded.clear();
                self.mirrors = None;
                self.prev_src.clear();
                self.src_stamp = self.src_stamp.wrapping_add(1);
                self.rebuild_marks();
                self.reparse();
                self.goto = Some(offset_of_line_col(self.session.source(), self.cursor_lc.0, self.cursor_lc.1));
                self.status = self.s().status_reloaded.into();
            }
            Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", self.s().reload), ("error", &e.to_string())]),
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn overwrite_current(&mut self) {
        match self.session.save_overwrite() {
            Ok(()) => {
                self.external_changed = false;
                self.external_modal = false;
                self.status = lang::fmt(self.s().status_saved, &[("path", &self.path_display())]);
            }
            Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", self.s().save), ("error", &e.to_string())]),
        }
    }

    fn open_path(&mut self, path: PathBuf) {
        // Switching files never loses work: a dirty buffer with a path is
        // auto-saved first; an untitled dirty buffer refuses to switch.
        if self.session.dirty() && self.session.path() != Some(path.as_path()) {
            if self.session.path().is_some() {
                if let Err(e) = self.session.save() {
                    if matches!(e, EngineError::ExternalChange) {
                        self.external_changed = true;
                        self.external_modal = true;
                    } else {
                        self.status = lang::fmt(self.s().status_failed, &[("context", self.s().save), ("error", &e.to_string())]);
                    }
                    return;
                }
            } else {
                self.status = self.s().status_unsaved_switch.into();
                return;
            }
        }
        match self.session.open(&path) {
            Ok(()) => {
                self.status.clear();
                self.pages.clear();
                self.page_pts.clear();
                self.pdf_path = None;
                self.report = None;
                self.page_idx = 0;
                self.flash = None;
                self.folded.clear();
                self.mirrors = None;
                self.prev_src.clear();
                self.src_stamp = self.src_stamp.wrapping_add(1);
                self.rebuild_marks(); // report is gone — clear the dots
                self.reparse();
                self.proj_scanned_at = f64::NEG_INFINITY;
                self.external_changed = false;
                self.external_modal = false;
                self.external_checked_at = f64::NEG_INFINITY;
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.repo_status = None;
                    self.repo_checked_at = f64::NEG_INFINITY;
                }
                self.load_tools();
            }
            Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", &path.display().to_string()), ("error", &e.to_string())]),
        }
    }

    /// Create an untitled buffer. `document.new` keeps the same command
    /// contract for UI, CLI, control and MCP callers.
    fn new_untitled(&mut self) {
        if self.session.dirty() {
            if self.session.path().is_some() {
                if let Err(e) = self.session.save() {
                    if matches!(e, EngineError::ExternalChange) {
                        self.external_changed = true;
                        self.external_modal = true;
                    } else {
                        self.status = lang::fmt(self.s().status_failed, &[("context", self.s().save), ("error", &e.to_string())]);
                    }
                    return;
                }
            } else {
                self.status = self.s().status_unsaved_new.into();
                return;
            }
        }
        match self.session.execute("document.new", &serde_json::json!({"text": DEFAULT_DOC})) {
            Ok(_) => {
                self.pages.clear();
                self.page_pts.clear();
                self.pdf_path = None;
                self.report = None;
                self.page_idx = 0;
                self.flash = None;
                self.folded.clear();
                self.mirrors = None;
                self.prev_src.clear();
                self.src_stamp = self.src_stamp.wrapping_add(1);
                self.rebuild_marks();
                self.reparse();
                self.proj_scanned_at = f64::NEG_INFINITY;
                self.external_changed = false;
                self.external_modal = false;
                self.external_checked_at = f64::NEG_INFINITY;
                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.repo_status = None;
                    self.repo_checked_at = f64::NEG_INFINITY;
                }
                self.load_tools();
                self.status = self.s().status_untitled.into();
            }
            Err(e) => self.status = e.to_string(),
        }
    }

    /// Insert or wrap LaTeX at the editor caret. Selection ranges come from
    /// egui's persisted TextEdit state; in folded mode `cursor_lc` is the
    /// source-coordinate fallback.
    fn insert_latex(&mut self, ctx: &egui::Context, before: &str, after: &str, caret_back: usize) {
        let mut src = self.session.source().to_string();
        let selection = if self.folded.is_empty() {
            self.editor_id
                .and_then(|id| egui::text_edit::TextEditState::load(ctx, id))
                .and_then(|st| st.cursor.char_range())
                .map(|r| (r.primary.index.0.min(r.secondary.index.0), r.primary.index.0.max(r.secondary.index.0)))
                .filter(|(a, b)| a != b)
        } else {
            None
        };
        let caret_ci = selection
            .map(|(a, _)| a)
            .or_else(|| {
                if self.folded.is_empty() {
                    self.editor_id
                        .and_then(|id| egui::text_edit::TextEditState::load(ctx, id))
                        .and_then(|st| st.cursor.char_range())
                        .map(|r| r.primary.index.0)
                } else {
                    Some(char_index_of_lc(&src, self.cursor_lc.0, self.cursor_lc.1))
                }
            })
            .unwrap_or(0)
            .min(src.chars().count());
        let caret_b = byte_of_char(&src, caret_ci);
        let goto = if let Some((_, end)) = selection {
            let end_b = byte_of_char(&src, end.min(src.chars().count()));
            let selected = src.get(caret_b..end_b).unwrap_or_default().to_string();
            let insert = format!("{before}{selected}{after}");
            src.replace_range(caret_b..end_b, &insert);
            caret_b + insert.len()
        } else {
            let insert = format!("{before}{after}");
            src.insert_str(caret_b, &insert);
            caret_b + insert.len().saturating_sub(caret_back)
        };
        self.mirrors = None;
        self.session.set_source(src);
        self.src_stamp = self.src_stamp.wrapping_add(1);
        self.reparse();
        self.folded.clear();
        self.goto = Some(goto);
        self.last_edit = ctx.input(|i| i.time);
    }

    fn save(&mut self) {
        if self.session.path().is_none() {
            self.save_as();
            return;
        }
        match self.session.save() {
            Ok(()) => {
                self.external_changed = false;
                self.external_modal = false;
                self.status = lang::fmt(self.s().status_saved, &[("path", &self.path_display())]);
            }
            Err(EngineError::ExternalChange) => {
                self.external_changed = true;
                self.external_modal = true;
            }
            Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", self.s().save), ("error", &e.to_string())]),
        }
    }

    fn path_display(&self) -> String {
        self.session.path().map(|p| p.display().to_string()).unwrap_or_else(|| "(untitled)".into())
    }

    /// The project root: the open file's directory, else the cwd.
    fn project_root(&self) -> PathBuf {
        self.session
            .path()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// Current UI language from prefs (unknown codes → English).
    fn lang(&self) -> lang::Lang {
        lang::Lang::from_code(&self.settings.get().language)
    }

    /// The active message catalog.
    fn s(&self) -> &'static lang::Strings {
        lang::strings(self.lang())
    }

    fn set_lang(&mut self, l: lang::Lang) {
        self.settings.update(|p| p.language = l.code().into());
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("LaTeX", &["tex", "sty", "cls"]).pick_file() {
            self.open_path(p);
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn save_as(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("LaTeX", &["tex"]).save_file() {
            match self.session.save_as(&p) {
                Ok(()) => {
                    self.external_changed = false;
                    self.external_modal = false;
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        self.repo_status = None;
                        self.repo_checked_at = f64::NEG_INFINITY;
                    }
                    self.status = lang::fmt(self.s().status_saved, &[("path", &p.display().to_string())]);
                }
                Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", self.s().save), ("error", &e.to_string())]),
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn open_dialog(&mut self) {
        self.status = lang::fmt(self.s().status_unavailable, &[("action", self.s().open_file)]);
    }

    #[cfg(target_arch = "wasm32")]
    fn save_as(&mut self) {
        self.status = lang::fmt(self.s().status_unavailable, &[("action", self.s().save)]);
    }

    /// Load project user tools from `warraqcraft-tools.json` next to the
    /// open document — the same JSON shape `user_tool.set` accepts:
    /// `{"tools": [{"name": "…", "argv": ["…"], "cwd"?: "…", "capture"?: bool}]}`.
    /// Missing/invalid files are silent; a parse error surfaces in the
    /// status line.
    #[cfg(not(target_arch = "wasm32"))]
    fn load_tools(&mut self) {
        let file = self.project_root().join("warraqcraft-tools.json");
        let Ok(text) = std::fs::read_to_string(&file) else { return };
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => match self.session.execute("user_tool.set", &v) {
                Ok(_) => {
                    let n = self.session.user_tools().len();
                    if n > 0 {
                        self.status = lang::fmt(self.s().status_tools_loaded, &[("n", &n.to_string())]);
                    }
                }
                Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", "warraqcraft-tools.json"), ("error", &e.to_string())]),
            },
            Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", "warraqcraft-tools.json"), ("error", &e.to_string())]),
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn load_tools(&mut self) {}

    /// Run a configured user tool on a worker — a scratch `Session`
    /// carries the file path and tool list so `user_tool.run`'s
    /// placeholder substitution and safety rules apply unchanged.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_tool(&mut self, name: String) {
        let tools: Vec<warraqcraft_engine::UserTool> = self.session.user_tools().to_vec();
        let path = self.session.path().map(Path::to_path_buf);
        self.status = lang::fmt(self.s().status_running_tool, &[("name", &name)]);
        let (tx, rx) = std::sync::mpsc::channel();
        self.tool_rx = Some(rx);
        std::thread::spawn(move || {
            let mut s = Session::new();
            if let Some(p) = &path {
                let _ = s.open(p);
            }
            s.set_user_tools(tools);
            let out = s.execute("user_tool.run", &serde_json::json!({"name": name})).map_err(|e| e.to_string());
            let _ = tx.send(out);
        });
    }

    /// Collect a finished user-tool run into the status line — the last
    /// non-empty stdout line when there is one.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_tool(&mut self, _ctx: &egui::Context) {
        let Some(rx) = &self.tool_rx else { return };
        match rx.try_recv() {
            Ok(Ok(v)) => {
                let status = v["status"].as_i64().map(|s| format!("exit {s}")).unwrap_or_else(|| "done".into());
                let timed_out = v["timed_out"].as_bool().unwrap_or(false);
                let name = v["name"].as_str().unwrap_or("tool");
                let tail = v["stdout"].as_str().unwrap_or("").lines().rev().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string());
                self.status = match (timed_out, tail) {
                    (true, _) => lang::fmt(self.s().status_tool_timeout, &[("name", name)]),
                    (false, tail) => {
                        lang::fmt(self.s().status_tool_done, &[("name", name), ("status", &status), ("tail", &tail.unwrap_or_default())])
                    }
                };
                self.tool_rx = None;
            }
            Ok(Err(e)) => {
                self.status = lang::fmt(self.s().status_tool_failed, &[("error", &e)]);
                self.tool_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.status = self.s().status_tool_dead.into();
                self.tool_rx = None;
            }
        }
    }

    /// Completion popup state after each editor frame: a `\word` prefix
    /// completes commands, `\begin{…`/`\end{…` completes environments.
    /// Only when the editor is focused and unfolded.
    fn update_completion(&mut self, out: &egui::text_edit::TextEditOutput, src: &str) {
        let Some(cr) = out.cursor_range else {
            self.complete = None;
            return;
        };
        if !out.response.has_focus() {
            self.complete = None;
            return;
        }
        let ci = cr.primary.index.0;
        // The ≤64 chars before the caret — enough for `\begin{prefix`.
        let back_start = src.char_indices().nth(ci.saturating_sub(64)).map(|(b, _)| b).unwrap_or(0);
        let back = src.get(back_start..byte_of_char(src, ci)).unwrap_or("");
        // `\cite{`-family inside an open arg — complete bibliography keys
        // from the project's declared `.bib` files. A comma starts the next
        // key in a multi-cite list.
        let cite_prefix = CITE_CMDS
            .iter()
            .filter_map(|c| {
                let anchor = format!("\\{c}{{");
                back.rfind(&anchor).map(|p| p + anchor.len())
            })
            .max()
            .and_then(|arg_start| {
                let arg = back.get(arg_start..)?;
                let w = arg.rsplit(',').next()?;
                (w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.' | '/'))).then(|| (ci - w.len(), w.to_string()))
            });
        if let Some((start, w)) = cite_prefix {
            #[cfg(not(target_arch = "wasm32"))]
            let items: Vec<CompletionItem> = self
                .proj_bibs
                .iter()
                .filter(|e| e.key.starts_with(w.as_str()))
                .take(8)
                .map(|e| CompletionItem {
                    name: e.key.clone(),
                    sig: e.year.clone(),
                    meta: format!("{} — {}", e.author, e.file),
                    insert: e.key.clone(),
                    caret_back: 0,
                })
                .collect();
            #[cfg(target_arch = "wasm32")]
            let items: Vec<CompletionItem> = Vec::new();
            if items.is_empty() {
                self.complete = None;
            } else {
                let sel = self.complete.as_ref().map(|c| c.sel.min(items.len() - 1)).unwrap_or(0);
                self.complete = Some(Completion { start, end: ci, env: true, items, sel });
            }
            return;
        }

        // `\ref{`-family inside an open arg — complete project labels.
        // The `}` in a closed arg fails the charset check, so this only
        // fires while the brace is still open.
        let label_prefix = REF_CMDS
            .iter()
            .filter_map(|c| {
                let anchor = format!("\\{c}{{");
                back.rfind(&anchor).map(|p| p + anchor.len())
            })
            .max()
            .and_then(|w_start| {
                let w = back.get(w_start..)?;
                (w.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.' | '/')) && !w.is_empty()).then(|| w.to_string())
            });
        if let Some(w) = label_prefix {
            let label_word = self.s().label_item;
            let items: Vec<_> = self
                .proj_labels
                .iter()
                .filter(|l| l.starts_with(w.as_str()))
                .take(8)
                .map(|l| {
                    // Meta line: the file that defines the label.
                    let file = self.proj_files.iter().find(|f| f.labels.iter().any(|x| x == l)).map(|f| f.name.clone()).unwrap_or_default();
                    CompletionItem {
                        name: l.clone(),
                        sig: String::new(),
                        meta: format!("{label_word} — {file}"),
                        insert: format!("{{{l}}}"),
                        caret_back: 0,
                    }
                })
                .collect();
            if items.is_empty() {
                self.complete = None;
            } else {
                let start = ci - w.len() - 1; // the '{'
                let sel = self.complete.as_ref().map(|c| c.sel.min(items.len() - 1)).unwrap_or(0);
                self.complete = Some(Completion { start, end: ci, env: true, items, sel });
            }
            return;
        }
        // `\begin{` and `\end{` have different lengths — track which hit.
        let env_prefix =
            back.rfind("\\begin{").map(|p| p + "\\begin{".len()).or_else(|| back.rfind("\\end{").map(|p| p + "\\end{".len())).and_then(|w_start| {
                let w = back.get(w_start..)?;
                (w.chars().all(|c| c.is_ascii_alphabetic() || c == '*') && !w.is_empty()).then(|| w.to_string())
            });
        if let Some(w) = env_prefix {
            let items: Vec<_> = texpkg::environments().filter(|c| c.name.starts_with(w.as_str())).take(8).map(|c| catalog_item(c, true)).collect();
            if items.is_empty() {
                self.complete = None;
            } else {
                let start = ci - w.len() - 1; // the '{'
                let sel = self.complete.as_ref().map(|c| c.sel.min(items.len() - 1)).unwrap_or(0);
                self.complete = Some(Completion { start, end: ci, env: true, items, sel });
            }
            return;
        }
        let cmd_prefix = back.rfind('\\').and_then(|p| {
            let w = &back[p + 1..];
            (w.chars().all(|c| c.is_ascii_alphabetic() || c == '@') && w.len() >= 2).then(|| (p, w.to_string()))
        });
        if let Some((_, w)) = cmd_prefix {
            let items: Vec<_> = texpkg::find_prefix(&w).take(8).map(|c| catalog_item(c, false)).collect();
            if items.is_empty() {
                self.complete = None;
            } else {
                let start = ci - w.len() - 1; // the '\'
                let sel = self.complete.as_ref().map(|c| c.sel.min(items.len() - 1)).unwrap_or(0);
                self.complete = Some(Completion { start, end: ci, env: false, items, sel });
            }
            return;
        }
        self.complete = None;
    }

    /// Accept the highlighted completion: replace the prefix span with
    /// `\name`/`{name}` (+ `{}` for required-arg commands, caret inside).
    /// `now` is egui's frame time so the insert counts as a normal edit
    /// for the idle auto-compile timer.
    fn accept_completion(&mut self, now: f64) {
        let Some(c) = self.complete.take() else { return };
        let Some(item) = c.items.get(c.sel) else { return };
        let mut src = self.session.source().to_string();
        let start_b = byte_of_char(&src, c.start);
        let end_b = byte_of_char(&src, c.end);
        src.replace_range(start_b..end_b, &item.insert);
        // Caret: inside the braces for arg-commands, otherwise at end.
        let caret = start_b + item.insert.len() - item.caret_back;
        self.mirrors = None; // completion rewrites the buffer — mirrors die
        self.session.set_source(src);
        self.src_stamp = self.src_stamp.wrapping_add(1);
        self.reparse();
        self.folded.clear();
        self.goto = Some(caret);
        self.last_edit = now;
    }

    /// Float the completion popup under the caret.
    fn draw_completion(&mut self, ctx: &egui::Context, out: &egui::text_edit::TextEditOutput) {
        let Some(c) = &self.complete else { return };
        let Some(cr) = out.cursor_range else { return };
        let caret = out.galley.pos_from_cursor(cr.primary);
        let pos = egui::pos2(out.galley_pos.x + caret.min.x, out.galley_pos.y + caret.max.y + 4.0);
        let mut accept = false;
        egui::Area::new(egui::Id::new("completion")).order(egui::Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(420.0);
                for (i, item) in c.items.iter().enumerate() {
                    let label = if c.env { format!("{}   {}", item.name, item.sig) } else { format!("\\{}   {}", item.name, item.sig) };
                    let resp = ui.selectable_label(i == c.sel, egui::RichText::new(lang::view(&label)).monospace().size(11.5));
                    ui.label(egui::RichText::new(item.meta.clone()).size(9.5).color(theme::pal().muted));
                    if resp.clicked() {
                        accept = true;
                    }
                }
            });
        });
        if accept {
            self.accept_completion(ctx.input(|i| i.time));
        }
    }

    /// Kick off `document.compile` + PDF rasterization on a worker thread.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_compile(&mut self) {
        if self.compiling {
            return;
        }
        if self.session.path().is_none() {
            self.save_as();
        }
        let Some(path) = self.session.path().map(Path::to_path_buf) else {
            self.status = self.s().status_compile_no_save.into();
            return;
        };
        if let Err(e) = self.session.save() {
            if matches!(e, EngineError::ExternalChange) {
                self.external_changed = true;
                self.external_modal = true;
            } else {
                self.status = lang::fmt(self.s().status_save_before_compile, &[("error", &e.to_string())]);
            }
            return;
        }
        self.compiling = true;
        self.compiled_edit = self.last_edit;
        self.status = self.s().status_compiling.into();
        let engine = self.engine_sel.and_then(|i| self.engines.get(i).cloned());
        let (tx, rx) = std::sync::mpsc::channel();
        self.compile_rx = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(compile_and_render(&path, engine.as_deref()));
        });
    }

    #[cfg(target_arch = "wasm32")]
    fn start_compile(&mut self) {
        self.status = lang::fmt(self.s().status_unavailable, &[("action", self.s().recompile)]);
    }

    /// Collect a finished compile: store the report and build textures.
    #[cfg(not(target_arch = "wasm32"))]
    fn poll_compile(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.compile_rx else { return };
        match rx.try_recv() {
            Ok(Ok((report, images))) => {
                let pages = report.pages.unwrap_or(0).to_string();
                let errors = report.errors.len().to_string();
                self.status = match (report.pdf.is_some(), report.errors.len()) {
                    (true, 0) => lang::fmt(self.s().status_compile_ok, &[("engine", &report.engine), ("pages", &pages)]),
                    (true, _) => lang::fmt(self.s().status_compile_issues, &[("engine", &report.engine), ("pages", &pages), ("errors", &errors)]),
                    (false, _) => lang::fmt(self.s().status_compile_failed, &[("engine", &report.engine), ("errors", &errors)]),
                };
                self.page_pts = images.iter().map(|p| p.pt).collect();
                self.pages =
                    images.into_iter().enumerate().map(|(i, p)| ctx.load_texture(format!("page-{i}"), p.img, egui::TextureOptions::LINEAR)).collect();
                self.pdf_path = report.pdf.clone();
                self.page_idx = self.page_idx.min(self.pages.len().saturating_sub(1));
                self.flash = None;
                self.report = Some(report);
                self.rebuild_marks();
                self.compiling = false;
                self.compile_rx = None;
                // Surface failures without making the user open Logs.
                if self.report.as_ref().is_some_and(|r| !r.errors.is_empty()) {
                    self.show_logs = true;
                }
            }
            Ok(Err(e)) => {
                self.status = e;
                self.compiling = false;
                self.compile_rx = None;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => ctx.request_repaint(),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.status = self.s().status_compile_dead.into();
                self.compiling = false;
                self.compile_rx = None;
            }
        }
    }

    /// Rescan the project tree at most every TREE_SCAN_SECS.
    fn refresh_tree(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.tree_scanned_at < TREE_SCAN_SECS {
            return;
        }
        self.tree_scanned_at = now;
        let root = self.project_root();
        let expanded = &self.expanded;
        self.tree = files::scan(&root, &|p| expanded.contains(p));
        // The "main document" marker: first .tex file that declares a
        // documentclass (bounded read), else the open file.
        self.main_file = None;
        for n in &self.tree {
            if n.kind == files::Kind::Tex && file_declares_document(&n.path) {
                self.main_file = Some(n.path.clone());
                break;
            }
        }
    }

    /// Merge the reachable `\input` graph into `proj_*` at most every
    /// TREE_SCAN_SECS via `project.structure` — the outline and `\ref{}`
    /// completion then see the whole document, not just the open buffer.
    fn refresh_project(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.proj_scanned_at < TREE_SCAN_SECS {
            return;
        }
        self.proj_scanned_at = now;
        let Ok(v) = self.session.execute("project.structure", &serde_json::json!({})) else {
            self.proj_files.clear();
            self.proj_missing = 0;
            return;
        };
        let open_canon = self.session.path().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()));
        self.proj_files = v["files"]
            .as_array()
            .map(|fs| {
                fs.iter()
                    .map(|f| {
                        let path = PathBuf::from(f["path"].as_str().unwrap_or_default());
                        ProjFile {
                            is_current: open_canon.as_deref() == Some(std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone()).as_path()),
                            name: f["name"].as_str().unwrap_or_default().to_string(),
                            headings: f["headings"]
                                .as_array()
                                .map(|hs| {
                                    hs.iter()
                                        .map(|h| texparse::Heading {
                                            level: h["level"].as_u64().unwrap_or(0) as u8,
                                            title: h["title"].as_str().unwrap_or_default().to_string(),
                                            pos: texparse::Pos { line: h["line"].as_u64().unwrap_or(1) as usize, col: 1, offset: 0 },
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                            labels: f["labels"]
                                .as_array()
                                .map(|ls| ls.iter().filter_map(|l| l.as_str().map(String::from)).collect())
                                .unwrap_or_default(),
                            bibliographies: f["bibliographies"]
                                .as_array()
                                .map(|bs| bs.iter().filter_map(|b| b.as_str().map(String::from)).collect())
                                .unwrap_or_default(),
                            path,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The open file's labels come live too (the scan may be stale
        // between the 1.5 s refresh ticks).
        if let Some(cur) = self.proj_files.iter_mut().find(|f| f.is_current) {
            cur.labels = self.doc.labels.iter().map(|(l, _)| l.clone()).collect();
        }
        self.proj_labels = v["labels"].as_array().map(|ls| ls.iter().filter_map(|l| l.as_str().map(String::from)).collect()).unwrap_or_default();
        // Live-buffer labels may not be on disk yet — union them in.
        for (l, _) in &self.doc.labels {
            if !self.proj_labels.contains(l) {
                self.proj_labels.push(l.clone());
            }
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let root = std::fs::canonicalize(self.project_root()).unwrap_or_else(|_| self.project_root());
            let mut paths = BTreeSet::new();
            for f in &self.proj_files {
                let dir = f.path.parent().map(Path::to_path_buf).unwrap_or_else(|| self.project_root());
                for bib in &f.bibliographies {
                    let mut path = dir.join(bib);
                    if path.extension().is_none() {
                        path.set_extension("bib");
                    }
                    if let Ok(canon) = std::fs::canonicalize(&path) {
                        if canon.starts_with(&root) {
                            paths.insert(canon);
                        }
                    }
                }
            }
            self.proj_bibs.clear();
            let paths: Vec<PathBuf> = paths.into_iter().collect();
            cite::scan_bib_files(&paths, &mut self.proj_bibs, &mut Vec::new());
        }
        self.proj_missing = v["missing"].as_array().map_or(0, |m| m.len());
    }

    /// Ctrl+click in the editor → SyncTeX forward: flash the matching box
    /// in the PDF preview and scroll to it.
    #[cfg(not(target_arch = "wasm32"))]
    fn forward_sync(&mut self, now: f64) {
        let (Some(pdf), Some(src)) = (self.pdf_path.clone(), self.session.path().map(Path::to_path_buf)) else {
            self.status = self.s().status_no_synctex.into();
            return;
        };
        // synctex records absolute source paths.
        let src = src.canonicalize().unwrap_or(src);
        let (line, col) = self.cursor_lc;
        // synctex columns are byte offsets; our cursor_lc is char-based.
        let text = self.session.source();
        let byte_col = text
            .split('\n')
            .nth(line.saturating_sub(1))
            .map(|l| l.chars().take(col.saturating_sub(1)).map(|c| c.len_utf8()).sum::<usize>() + 1)
            .unwrap_or(col);
        match warraqcraft_engine::synctex::forward(&pdf, &src, line as u32, byte_col as u32) {
            Ok(locs) => match locs.first() {
                Some(loc) => {
                    self.page_idx = (loc.page as usize).saturating_sub(1).min(self.pages.len().saturating_sub(1));
                    let r = egui::Rect::from_min_size(
                        egui::pos2(loc.h as f32, loc.v as f32),
                        egui::vec2(loc.width.max(4.0) as f32, loc.height.max(6.0) as f32),
                    );
                    self.flash = Some(Flash { page: self.page_idx, rect_pt: r, at: now });
                }
                None => self.status = self.s().status_no_synctex.into(),
            },
            Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", "SyncTeX"), ("error", &e.to_string())]),
        }
    }

    /// Click on a PDF page → SyncTeX inverse: move the editor caret to the
    /// source position (opening the file if the hit lands elsewhere).
    #[cfg(not(target_arch = "wasm32"))]
    fn inverse_sync(&mut self, page_rect: egui::Rect, pos: egui::Pos2) {
        let (Some(pdf), Some(pt)) = (self.pdf_path.clone(), self.page_pts.get(self.page_idx).copied()) else { return };
        let frac = (pos - page_rect.min) / page_rect.size();
        let (x, y) = ((frac.x * pt.x) as f64, (frac.y * pt.y) as f64);
        match warraqcraft_engine::synctex::inverse(&pdf, (self.page_idx + 1) as u32, x, y) {
            Ok(Some(loc)) => {
                // synctex reports absolute paths; compare canonicalized.
                let cur = self.session.path().and_then(|p| p.canonicalize().ok());
                if cur.as_deref() != Some(loc.file.as_path()) {
                    self.open_path(loc.file.clone());
                }
                let line = loc.line as usize;
                let col = loc.column.unwrap_or(1) as usize;
                self.goto = Some(offset_of_line_col(self.session.source(), line, col));
                let file = loc.file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| loc.file.display().to_string());
                self.status = lang::fmt(self.s().status_jumped, &[("file", &file), ("line", &line.to_string())]);
            }
            Ok(None) => self.status = self.s().status_no_synctex.into(),
            Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", "SyncTeX"), ("error", &e.to_string())]),
        }
    }

    /// Auto-compile: recompile once the buffer has been idle for
    /// AUTOCOMPILE_IDLE_SECS. A failed compile marks the edit as
    /// "compiled" so we don't hot-loop — the next edit retriggers.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_autocompile(&mut self, ctx: &egui::Context) {
        if !self.autocompile || self.compiling || self.last_edit <= self.compiled_edit || self.session.path().is_none() {
            return;
        }
        let idle = ctx.input(|i| i.time) - self.last_edit;
        if idle >= AUTOCOMPILE_IDLE_SECS {
            self.start_compile();
        } else {
            ctx.request_repaint_after_secs((AUTOCOMPILE_IDLE_SECS - idle) as f32);
        }
    }

    /// Resolve an engine-reported issue path. Relative log paths are under
    /// the project root; an absent path means the open compile document.
    fn issue_path(&self, file: Option<&Path>) -> PathBuf {
        match file {
            Some(p) if p.is_absolute() => p.to_path_buf(),
            Some(p) => self.project_root().join(p),
            None => self.session.path().map(Path::to_path_buf).unwrap_or_default(),
        }
    }

    /// Whether a reported issue belongs to the buffer currently in the
    /// editor — relative paths and symlinks compare through canonicalization.
    fn issue_is_current(&self, file: Option<&Path>) -> bool {
        let target = self.issue_path(file);
        let Some(current) = self.session.path() else { return file.is_none() };
        std::fs::canonicalize(&target).unwrap_or(target) == std::fs::canonicalize(current).unwrap_or_else(|_| current.to_path_buf())
    }

    /// Jump to a compile issue, opening its file when the log attributes it
    /// to a different member of the project.
    fn goto_issue(&mut self, file: Option<&Path>, line: Option<u32>) {
        let path = self.issue_path(file);
        if self.session.path() != Some(path.as_path()) && !path.as_os_str().is_empty() {
            self.open_path(path);
        }
        let Some(line) = line else { return };
        let src = self.session.source().to_string();
        self.goto = Some(offset_of_line_col(&src, line as usize, 1));
    }

    /// Rebuild gutter marks from the compile report. Only issues with a
    /// reported line and attributed to the open file get a dot — positions
    /// are never invented. Marks are kept until the next compile or a file
    /// switch (edits leave them stale, like TeXstudio).
    fn rebuild_marks(&mut self) {
        self.marks.clear();
        let Some(r) = &self.report else { return };
        let marks: Vec<_> = r
            .errors
            .iter()
            .filter(|e| self.issue_is_current(e.file.as_deref()))
            .filter_map(|e| e.line.map(|l| (l as usize, true, e.message.clone())))
            .chain(
                r.warnings
                    .iter()
                    .filter(|w| self.issue_is_current(w.file.as_deref()))
                    .filter_map(|w| w.line.map(|l| (l as usize, false, format!("{}: {}", self.s().warning_label, w.message)))),
            )
            .collect();
        for (line, is_error, message) in marks {
            let mark = self.marks.entry(line).or_insert_with(|| GutterMark { is_error, messages: Vec::new() });
            mark.is_error |= is_error;
            mark.messages.push(message);
        }
    }

    /// `view.fold_all` — collapse every foldable region.
    fn fold_all(&mut self) {
        let line_count = self.session.source().bytes().filter(|b| *b == b'\n').count() + 1;
        for r in fold::regions(&self.doc, line_count) {
            self.folded.insert(r.start);
        }
    }

    /// `view.unfold_all` — expand everything, restoring the live buffer.
    fn unfold_all(&mut self) {
        self.folded.clear();
    }

    /// `view.fold_toggle` — collapse/expand the region starting at
    /// `start` (1-based source line).
    fn toggle_fold(&mut self, start: usize) {
        if !self.folded.remove(&start) {
            self.folded.insert(start);
        }
    }

    /// Editor line commands: Ctrl+D duplicate, Alt+↑/↓ move, Ctrl+/
    /// comment-toggle, Ctrl+Shift+K erase line, Ctrl+Shift+Backspace
    /// erase to end of line. Keys are consumed before the `TextEdit`
    /// sees them; all commands are disabled while a fold is active
    /// (the editor is read-only then anyway).
    fn editor_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.folded.is_empty() {
            return;
        }
        let Some(editor_id) = self.editor_id else { return };
        if !ctx.memory(|m| m.has_focus(editor_id)) {
            return;
        }
        use egui::{Key, Modifiers};
        // Ctrl+M: arm occurrence mirrors on the current selection; with
        // mirrors live it clears them (Esc does too).
        if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::M)) {
            self.toggle_mirrors(ctx, editor_id);
            return;
        }
        if self.mirrors.is_some() && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
            self.mirrors = None;
            return;
        }
        // Most-specific modifiers first (matches_logically ignores
        // extras); distinct keys make the rest order-free.
        enum Op {
            Erase,
            EraseEol,
            Duplicate,
            Move(edits::Move),
            Comment,
        }
        let op = if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::K)) {
            Some(Op::Erase)
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL | Modifiers::SHIFT, Key::Backspace)) {
            Some(Op::EraseEol)
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::D)) {
            Some(Op::Duplicate)
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::ALT, Key::ArrowUp)) {
            Some(Op::Move(edits::Move::Up))
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::ALT, Key::ArrowDown)) {
            Some(Op::Move(edits::Move::Down))
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::CTRL, Key::Slash)) {
            // egui maps the numpad `/` to Key::Slash too — one binding
            // covers both.
            Some(Op::Comment)
        } else {
            None
        };
        let Some(op) = op else { return };

        let mut src = self.session.source().to_string();
        let (line, col) = self.cursor_lc; // 1-based
                                          // (new caret line 0-based, new caret col 1-based)
        let caret = match op {
            Op::Erase => edits::erase_line(&mut src, line.saturating_sub(1)).map(|l| (l, col)),
            Op::EraseEol => {
                edits::erase_to_eol(&mut src, line.saturating_sub(1), col.saturating_sub(1));
                Some((line.saturating_sub(1), col))
            }
            Op::Duplicate => edits::duplicate_line(&mut src, line.saturating_sub(1)).map(|l| (l, col)),
            Op::Move(d) => edits::move_line(&mut src, line.saturating_sub(1), d).map(|l| (l, col)),
            Op::Comment => {
                let (first, last) = self.selection_lines(ctx, editor_id, &src);
                edits::comment_lines(&mut src, first, last);
                Some((line.saturating_sub(1), col))
            }
        };
        if src.as_str() == self.session.source() {
            return; // boundary no-op (e.g. move past the edge)
        }
        self.mirrors = None; // line ops shift ranges — mirrors die
        self.session.set_source(src);
        self.src_stamp = self.src_stamp.wrapping_add(1);
        self.last_edit = ctx.input(|i| i.time);
        self.reparse();
        self.folded.clear(); // any edit invalidates folds
        if let Some((l, c)) = caret {
            self.goto = Some(offset_of_line_col(self.session.source(), l + 1, c));
        }
    }

    /// Ctrl+M: select a word/phrase → every other occurrence becomes a
    /// mirror site; typing at the primary replicates into all of them
    /// (insert/delete replication happens in the editor change path).
    fn toggle_mirrors(&mut self, ctx: &egui::Context, editor_id: egui::Id) {
        if self.mirrors.take().is_some() {
            self.status = self.s().status_mirrors_cleared.into();
            return;
        }
        let src = self.session.source();
        let Some(range) = egui::text_edit::TextEditState::load(ctx, editor_id).and_then(|st| st.cursor.char_range()) else { return };
        let (lo, hi) = (range.primary.index.0.min(range.secondary.index.0), range.primary.index.0.max(range.secondary.index.0));
        if lo == hi {
            self.status = self.s().status_select_text.into();
            return;
        }
        let needle: String = src.chars().skip(lo).take(hi - lo).collect();
        let others: Vec<_> = mirrors::occurrences(src, &needle).into_iter().filter(|r| *r != (lo, hi)).collect();
        if others.is_empty() {
            self.status = self.s().status_no_occurrences.into();
            return;
        }
        self.status = lang::fmt(self.s().status_mirror_sites, &[("n", &(others.len() + 1).to_string())]);
        self.mirrors = Some(mirrors::Mirrors { primary: (lo, hi), others });
    }

    /// 0-based line span of the current selection, or the caret line.
    fn selection_lines(&self, ctx: &egui::Context, editor_id: egui::Id, src: &str) -> (usize, usize) {
        let caret_line = self.cursor_lc.0.saturating_sub(1);
        let Some(st) = egui::text_edit::TextEditState::load(ctx, editor_id) else { return (caret_line, caret_line) };
        let Some(range) = st.cursor.char_range() else { return (caret_line, caret_line) };
        let a = line_of_char_index(src, range.primary.index.0);
        let b = line_of_char_index(src, range.secondary.index.0);
        (a.min(b), a.max(b))
    }

    // ---------- chrome ----------

    /// Top bar: project identity, save state, compile + toggles.
    fn top_bar(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        egui::Panel::top("topbar").frame(egui::Frame::new().fill(theme::pal().top_bg).inner_margin(egui::Margin::symmetric(12, 7))).show(ui, |ui| {
            ui.horizontal(|ui| {
                // Leaf mark + project name + path.
                let (lr, _) = ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::hover());
                icons::leaf(ui.painter(), lr, theme::pal().accent);
                let root = self.project_root();
                let title = self
                    .session
                    .path()
                    .and_then(|p| p.parent().and_then(|d| d.file_name()).map(|d| d.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| s.untitled.into());
                ui.label(egui::RichText::new(lang::view(&title).into_owned()).strong());
                ui.label(egui::RichText::new(format!("~/…/{}", root.display())).small().color(theme::pal().muted));
                let (chip, chip_col) =
                    if self.session.dirty() { (s.unsaved_changes, theme::pal().warn) } else { (s.saved_locally, theme::pal().accent_dim) };
                {
                    let (ir, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    if self.session.dirty() {
                        icons::dot(ui.painter(), ir, chip_col);
                    } else {
                        icons::check(ui.painter(), ir, chip_col);
                    }
                }
                ui.label(egui::RichText::new(lang::view(chip)).small().color(chip_col));
                #[cfg(not(target_arch = "wasm32"))]
                if let Some(repo) = self.repo_status.as_ref().filter(|r| r.in_repo) {
                    let branch = if repo.branch.is_empty() { s.repo.to_string() } else { repo.branch.clone() };
                    if ui
                        .selectable_label(self.repo_open, egui::RichText::new(lang::view(&branch)).small().color(theme::pal().muted))
                        .on_hover_text(lang::view(s.repo_hint).into_owned())
                        .clicked()
                    {
                        self.repo_open = !self.repo_open;
                        self.repo_checked_at = f64::NEG_INFINITY;
                    }
                }
                #[cfg(not(target_arch = "wasm32"))]
                if self.external_changed
                    && ui
                        .selectable_label(false, egui::RichText::new(lang::view(s.external_title)).small().color(theme::pal().warn))
                        .on_hover_text(lang::view(if self.session.dirty() { s.external_dirty } else { s.external_clean }).into_owned())
                        .clicked()
                {
                    self.external_modal = true;
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Engine pick rides on Recompile's chevron.
                    let enabled = !self.compiling;
                    if filled_icon_button(ui, 14.0, icons::rebuild, &lang::view(s.recompile), theme::pal().accent, enabled).clicked() {
                        self.start_compile();
                    }
                    let menu_resp = filled_icon_button(ui, 12.0, icons::chevron_down, "", theme::pal().accent_dim, enabled)
                        .on_hover_text(lang::view(s.compile_options).into_owned());
                    egui::Popup::from_toggle_button_response(&menu_resp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
                        ui.set_min_width(140.0);
                        if ui.selectable_label(self.engine_sel.is_none(), lang::view(s.engine_auto)).clicked() {
                            self.engine_sel = None;
                        }
                        for (i, e) in self.engines.iter().enumerate() {
                            if ui.selectable_label(self.engine_sel == Some(i), e).clicked() {
                                self.engine_sel = Some(i);
                            }
                        }
                        ui.separator();
                        ui.checkbox(&mut self.autocompile, lang::view(s.auto_compile).into_owned());
                        // User tools from warraqcraft-tools.json — fixed-argv
                        // external commands run on a worker (M2 surface).
                        #[cfg(not(target_arch = "wasm32"))]
                        {
                            let tools: Vec<String> = self.session.user_tools().iter().map(|t| t.name.clone()).collect();
                            if !tools.is_empty() {
                                ui.separator();
                                ui.label(egui::RichText::new(lang::view(s.user_tools)).size(10.0).color(theme::pal().muted));
                                let mut run: Option<String> = None;
                                for name in tools {
                                    if ui.selectable_label(false, lang::view(&name)).clicked() {
                                        run = Some(name);
                                    }
                                }
                                if let Some(name) = run {
                                    self.start_tool(name);
                                }
                            }
                        }
                        ui.separator();
                        ui.label(egui::RichText::new(lang::view(s.language)).size(10.0).color(theme::pal().muted));
                        for l in lang::ALL {
                            if ui.selectable_label(self.lang() == *l, lang::view(l.native_name())).clicked() {
                                self.set_lang(*l);
                            }
                        }
                    });
                    if self.compiling {
                        ui.spinner();
                    }
                    ui.separator();
                    if ui.selectable_label(self.show_logs, lang::view(s.logs)).clicked() {
                        self.show_logs = !self.show_logs;
                    }
                    if ui.selectable_label(self.outline_open, lang::view(s.outline)).clicked() {
                        self.outline_open = !self.outline_open;
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.selectable_label(self.hist_open, lang::view(s.history)).clicked() {
                        self.hist_open = !self.hist_open;
                        if self.hist_open {
                            self.load_snaps();
                            self.hist_confirm = None;
                        }
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if icon_button(ui, 14.0, icons::branch, &lang::view(s.repo), true).on_hover_text(lang::view(s.repo_hint).into_owned()).clicked() {
                        self.repo_open = !self.repo_open;
                        self.repo_checked_at = f64::NEG_INFINITY;
                    }
                    if ui.selectable_label(self.stats_open, lang::view(s.stats)).on_hover_text(lang::view(s.stats_hint).into_owned()).clicked() {
                        self.stats_open = !self.stats_open;
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if ui.selectable_label(self.ai.open, lang::view(s.ai)).on_hover_text(lang::view(s.ai_hint).into_owned()).clicked() {
                        self.ai.open = !self.ai.open;
                        if self.ai.open {
                            self.harness.needs_refresh = true;
                        }
                    }
                    if cite::cite_button(ui, s.cite).clicked() {
                        self.open_cite(ui.ctx());
                    }
                    if icon_button(ui, 14.0, icons::settings, &lang::view(s.settings), true).clicked() {
                        self.settings_open = !self.settings_open;
                    }
                    if icon_button(ui, 14.0, icons::question, &lang::view(s.help), true)
                        .on_hover_text(lang::view(s.shortcuts_hint).into_owned())
                        .clicked()
                    {
                        self.shortcuts_open = !self.shortcuts_open;
                    }
                    ui.separator();
                    if theme_toggle(ui, s).clicked() {
                        theme::apply(ui.ctx(), !theme::is_dark());
                    }
                });
            });
        });
    }

    /// Left sidebar: file toolbar, project tree, FILE OUTLINE. Panels do
    /// not mirror in RTL UI — only the text direction does.
    fn sidebar(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let panel = egui::Panel::left("sidebar");
        panel
            .resizable(true)
            .default_size(230.0)
            .size_range(160.0..=420.0)
            .frame(egui::Frame::new().fill(theme::pal().panel_bg).inner_margin(egui::Margin::symmetric(6, 6)))
            .show(ui, |ui| {
                // Icon row: new file / new folder / refresh / open.
                ui.horizontal(|ui| {
                    let root = self.project_root();
                    if icon_button(ui, 14.0, icons::page_new, "", true).on_hover_text(lang::view(s.new_file)).clicked() {
                        self.new_untitled();
                    }
                    if icon_button(ui, 14.0, icons::folder_new, "", true).on_hover_text(lang::view(s.new_folder)).clicked() {
                        self.new_entry = Some(NewEntry { dir: root.clone(), name: String::new(), is_dir: true });
                    }
                    if icon_button(ui, 14.0, icons::rebuild, "", true).on_hover_text(lang::view(s.refresh_tree)).clicked() {
                        self.tree_scanned_at = f64::NEG_INFINITY;
                    }
                    if icon_button(ui, 14.0, icons::folder_open, "", true).on_hover_text(lang::view(s.open_file)).clicked() {
                        self.open_dialog();
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    if icon_button(ui, 14.0, icons::page_plus, "", true).on_hover_text(lang::view(s.new_from_template)).clicked() {
                        self.load_templates();
                        self.tpl_sel = None;
                        self.tpl_name.clear();
                        self.tpl_filter.clear();
                        self.tpl_category = None;
                        self.tpl_language = None;
                        self.tpl_preview.clear();
                        self.tpl_preview_for.clear();
                        self.tpl_open = true;
                    }
                    ui.add_space(4.0);
                });
                ui.separator();

                let tree_h = ui.available_height() * if self.outline_open { 0.55 } else { 1.0 };
                egui::ScrollArea::vertical().id_salt("tree").max_height(tree_h).auto_shrink([false, false]).show(ui, |ui| {
                    let mut open: Option<PathBuf> = None;
                    let mut toggle: Option<PathBuf> = None;
                    for n in &self.tree {
                        let is_cur = self.session.path() == Some(n.path.as_path());
                        let is_main = self.main_file.as_ref() == Some(&n.path);
                        let resp = tree_row(ui, n, is_cur, is_main, self.expanded.contains(&n.path));
                        if resp.clicked() {
                            if n.kind == files::Kind::Dir {
                                toggle = Some(n.path.clone());
                            } else {
                                open = Some(n.path.clone());
                            }
                        }
                        if resp.double_clicked() && n.kind == files::Kind::Tex && n.kind != files::Kind::Dir {
                            open = Some(n.path.clone());
                        }
                    }
                    if self.tree.is_empty() {
                        ui.weak(lang::view(s.empty_folder));
                    }
                    if let Some(p) = toggle {
                        if !self.expanded.remove(&p) {
                            self.expanded.insert(p);
                        }
                        self.tree_scanned_at = f64::NEG_INFINITY;
                    }
                    if let Some(p) = open {
                        self.open_path(p);
                    }
                });

                if self.outline_open {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(lang::view(s.file_outline)).size(10.0).color(theme::pal().faint).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            // view.unfold_all / view.fold_all
                            if ui.small_button(lang::view(s.unfold_all)).on_hover_text("view.unfold_all").clicked() {
                                self.unfold_all();
                            }
                            if ui.small_button(lang::view(s.fold_all)).on_hover_text("view.fold_all").clicked() {
                                self.fold_all();
                            }
                        });
                    });
                    egui::ScrollArea::vertical().id_salt("outline").auto_shrink([false, true]).show(ui, |ui| {
                        if self.proj_missing > 0 {
                            ui.weak(format!("{} {}", self.proj_missing, lang::view(s.missing_files)));
                        }
                        // The merged project view: every reachable .tex in
                        // graph order; the open file contributes its live
                        // (unsaved) headings. Falls back to the buffer's
                        // own headings when the project scan hasn't run.
                        let mut files: Vec<(PathBuf, String, bool, &[texparse::Heading])> = self
                            .proj_files
                            .iter()
                            .map(|pf| {
                                let hs: &[texparse::Heading] = if pf.is_current { &self.doc.headings } else { &pf.headings };
                                (pf.path.clone(), pf.name.clone(), pf.is_current, hs)
                            })
                            .collect();
                        if !files.iter().any(|(_, _, cur, _)| *cur) && !self.doc.headings.is_empty() {
                            let name = self
                                .session
                                .path()
                                .and_then(|p| p.file_name())
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_else(|| "untitled.tex".into());
                            files.push((PathBuf::new(), name, true, &self.doc.headings));
                        }
                        let with_headings = files.iter().filter(|(_, _, _, hs)| !hs.is_empty()).count();
                        if with_headings == 0 {
                            ui.weak(lang::view(s.no_headings));
                        }
                        let show_files = with_headings > 1;
                        // A jump lands either in this buffer (byte offset)
                        // or in another file (path + 1-based line).
                        let mut jump: Option<(Option<PathBuf>, usize)> = None;
                        for (path, fname, is_current, headings) in &files {
                            if headings.is_empty() {
                                continue;
                            }
                            if show_files {
                                ui.label(egui::RichText::new(lang::view(fname)).size(9.5).color(theme::pal().faint).italics());
                            }
                            for h in *headings {
                                let col = outline_color(h.level);
                                let indent = f32::from(h.level) * 10.0;
                                let resp = ui
                                    .horizontal(|ui| {
                                        ui.add_space(indent);
                                        ui.add(
                                            egui::Label::new(egui::RichText::new(lang::view(&h.title)).color(col).size(12.0))
                                                .sense(egui::Sense::click()),
                                        );
                                    })
                                    .response;
                                if resp.hovered() {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if resp.clicked() {
                                    // In-file jumps carry the byte offset;
                                    // cross-file jumps carry the line.
                                    jump = Some(if *is_current { (None, h.pos.offset) } else { (Some(path.clone()), h.pos.line) });
                                }
                            }
                        }
                        match jump {
                            Some((None, off)) => self.goto = Some(off),
                            Some((Some(path), line)) => {
                                self.open_path(path);
                                let src = self.session.source().to_string();
                                self.goto = Some(offset_of_line_col(&src, line, 1));
                            }
                            None => {}
                        }
                    });
                }
            });
    }

    /// Compact LaTeX authoring bar — the same role as the formatting strip
    /// in desktop TeX editors, but kept command/source oriented.
    fn latex_toolbar(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        egui::Frame::new().fill(theme::pal().top_bg).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let ctx = ui.ctx().clone();
                if ui.button("B").on_hover_text(lang::view(s.tb_bold)).clicked() {
                    self.insert_latex(&ctx, "\\textbf{", "}", 1);
                }
                if ui.button("I").on_hover_text(lang::view(s.tb_italic)).clicked() {
                    self.insert_latex(&ctx, "\\textit{", "}", 1);
                }
                if ui.button("U").on_hover_text(lang::view(s.tb_underline)).clicked() {
                    self.insert_latex(&ctx, "\\underline{", "}", 1);
                }
                ui.separator();
                if ui.button("§").on_hover_text(lang::view(s.tb_section)).clicked() {
                    self.insert_latex(&ctx, "\\section{", "}", 1);
                }
                if ui.button("$x$").on_hover_text(lang::view(s.tb_math)).clicked() {
                    self.insert_latex(&ctx, "$", "$", 1);
                }
                if ui.button("Ω").on_hover_text(lang::view(s.tb_symbols)).clicked() {
                    self.symbols_open = !self.symbols_open;
                }
                ui.separator();
                ui.menu_button(lang::view(s.tb_env), |ui| {
                    for env in ["itemize", "enumerate", "description", "equation", "align", "figure", "table", "center"] {
                        if ui.button(env).clicked() {
                            let after = format!("\n\\end{{{env}}}");
                            self.insert_latex(&ctx, &format!("\\begin{{{env}}}\n"), &after, after.len());
                            ui.close();
                        }
                    }
                });
            });
        });
    }

    /// Editor column: tab strip, guttered code editor, status strip.
    fn editor_pane(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::pal().editor_bg)).show(ui, |ui| {
            // Tab strip
            egui::Frame::new().fill(theme::pal().top_bg).inner_margin(egui::Margin::symmetric(8, 0)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let name = self
                        .session
                        .path()
                        .and_then(|p| p.file_name())
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "untitled.tex".into());
                    let tab_frame = egui::Frame::new()
                        .fill(theme::pal().selected)
                        .inner_margin(egui::Margin::symmetric(10, 5))
                        .stroke(egui::Stroke::new(1.0, theme::pal().border));
                    tab_frame.show(ui, |ui| {
                        ui.label(egui::RichText::new(lang::view(&name)).size(12.0));
                        if self.main_file.as_deref() == self.session.path() {
                            ui.label(egui::RichText::new(lang::view(self.s().main_doc)).size(9.5).color(theme::pal().accent));
                        }
                        if self.session.dirty() {
                            ui.label(egui::RichText::new("●").size(9.0).color(theme::pal().warn));
                        }
                    });
                });
            });

            self.latex_toolbar(ui);

            // Editor body
            let mut src = self.session.source().to_string();
            let editor_id = ui.id().with("tex-editor");
            self.editor_id = Some(editor_id);
            let line_count = src.bytes().filter(|b| *b == b'\n').count() + 1;
            let regions = fold::regions(&self.doc, line_count);
            // Defensive: drop fold starts that aren't current regions
            // (the edit path already clears `folded`, so this only
            // prunes leftovers like a reparse after file switch).
            self.folded.retain(|l| regions.iter().any(|r| r.start == *l));
            // Folded mode: the TextEdit shows a derived read-only
            // buffer; `src` stays the true source.
            let view = fold::build_view(&src, &self.folded, &regions);
            let mut folded_text = view.as_ref().map(|v| v.text.clone());
            if let Some(off) = self.goto.take() {
                let mut st = egui::text_edit::TextEditState::load(ui.ctx(), editor_id).unwrap_or_default();
                // `goto` is a source byte offset; in folded mode it must
                // land on the visible row showing that line instead.
                let shown = folded_text.as_deref().unwrap_or(src.as_str());
                let shown_off = match &view {
                    Some(v) => {
                        let line = line_of_byte_offset(&src, off);
                        let row = fold::visible_row(v, &regions, line);
                        offset_of_line_col(&v.text, row + 1, 1)
                    }
                    None => off,
                };
                let idx = shown.get(..shown_off).map(|s| s.chars().count()).unwrap_or_else(|| shown.chars().count());
                st.cursor.set_char_range(Some(CCursorRange::one(CCursor { index: egui::text::CharIndex(idx), prefer_next_row: false })));
                st.store(ui.ctx(), editor_id);
                ui.ctx().memory_mut(|m| m.request_focus(editor_id));
            }
            // Completion popup navigation — consumed before the TextEdit
            // can see the keys (arrows pick, Enter/Tab accept, Esc closes).
            if self.complete.is_some() && view.is_none() {
                let mut accept = false;
                ui.ctx().input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        if let Some(c) = &mut self.complete {
                            c.sel = (c.sel + 1).min(c.items.len().saturating_sub(1));
                        }
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        if let Some(c) = &mut self.complete {
                            c.sel = c.sel.saturating_sub(1);
                        }
                    }
                    accept = i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) || i.consume_key(egui::Modifiers::NONE, egui::Key::Tab) || accept;
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                        self.complete = None;
                    }
                });
                if accept {
                    self.accept_completion(ui.ctx().input(|i| i.time));
                }
            }
            // Mirrors: snapshot the pre-edit buffer so a live edit can be
            // diffed and replicated to the mirrored occurrences.
            if self.mirrors.is_some() {
                self.prev_src = src.clone();
            }
            let mut layouter = |ui: &egui::Ui, s: &dyn egui::TextBuffer, w: f32| highlight::layout(ui, s, w);
            let avail = ui.available_size();
            let buf: &mut String = match folded_text.as_mut() {
                Some(t) => t,
                None => &mut src,
            };
            let mut te = egui::TextEdit::multiline(buf)
                .id(editor_id)
                .layouter(&mut layouter)
                .font(egui::TextStyle::Monospace)
                .desired_width(f32::INFINITY)
                .min_size(avail)
                .lock_focus(true)
                .frame(egui::Frame::new().fill(theme::pal().editor_bg).inner_margin(egui::Margin {
                    left: GUTTER_W as i8,
                    right: 8,
                    top: 4,
                    bottom: 4,
                }));
            if view.is_some() {
                te = te.interactive(false); // read-only while folded
            }
            let out = te.show(ui);
            if out.response.changed() && view.is_none() {
                self.src_stamp = self.src_stamp.wrapping_add(1);
                // Occurrence mirrors: replicate a single-point insert or
                // delete into every mirror; anything more complex (paste,
                // selection replace, undo) ends the mirror session.
                if let Some(m) = self.mirrors.take() {
                    if let Some((nsrc, nm)) = replicate_mirror_edit(&self.prev_src, &src, &m) {
                        src = nsrc;
                        self.mirrors = Some(nm);
                    }
                }
                self.session.set_source(src.clone());
                self.last_edit = ui.ctx().input(|i| i.time);
                self.reparse();
                self.folded.clear(); // any edit invalidates folds
            }
            self.paint_gutter(ui, &out, view.as_ref(), &regions);
            if view.is_none() {
                self.paint_spellcheck(ui, &out, &src);
                self.paint_mirrors(ui, &out);
                self.draw_math_preview(ui, &out, &src);
            }
            let shown = folded_text.as_deref().unwrap_or(src.as_str());
            self.track_cursor(ui.ctx(), editor_id, shown, view.as_ref());
            if view.is_none() {
                self.update_completion(&out, &src);
            }
            self.draw_completion(ui.ctx(), &out);
            // Right-click on a misspelled word → suggestions menu.
            if view.is_none() && self.spell.enabled() && self.spell.is_available() {
                let mut replace: Option<(usize, usize, String)> = None;
                out.response.context_menu(|ui| {
                    let ci =
                        ui.ctx().input(|i| i.pointer.interact_pos()).map(|p| out.galley.cursor_from_pos(p - out.galley_pos).index.0).unwrap_or(0);
                    if let Some(&(s, e)) = self.spell.scan(&src, self.src_stamp).iter().find(|(s, e)| ci >= *s && ci <= *e) {
                        for sug in self.spell.suggest(&src, s, e) {
                            if ui.button(lang::view(&sug)).clicked() {
                                replace = Some((s, e, sug.clone()));
                                ui.close();
                            }
                        }
                        if replace.is_none() && self.spell.suggest(&src, s, e).is_empty() {
                            ui.weak(lang::view(self.s().spell_none));
                        }
                        ui.separator();
                    }
                    let label = if self.spell.enabled() { self.s().spell_on } else { self.s().spell_off };
                    if ui.button(lang::view(label)).clicked() {
                        self.spell.set_enabled(!self.spell.enabled());
                        ui.close();
                    }
                });
                if let Some((s, e, sug)) = replace {
                    let mut buf = self.session.source().to_string();
                    let (bs, be) = (byte_of_char(&buf, s), byte_of_char(&buf, e));
                    buf.replace_range(bs..be, &sug);
                    self.mirrors = None;
                    self.session.set_source(buf);
                    self.src_stamp = self.src_stamp.wrapping_add(1);
                    self.last_edit = ui.ctx().input(|i| i.time);
                    self.reparse();
                }
            }
            #[cfg(not(target_arch = "wasm32"))]
            if out.response.clicked() && ui.ctx().input(|i| i.modifiers.command) {
                let now = ui.ctx().input(|i| i.time);
                self.forward_sync(now);
            }
        });
    }

    /// Wavy red underlines for misspelled ranges — TeXstudio-style
    /// squiggles mapped through the live galley (only when unfolded;
    /// fold-derived view text would mis-map the char indices).
    fn paint_spellcheck(&mut self, ui: &mut egui::Ui, out: &egui::text_edit::TextEditOutput, src: &str) {
        if !self.spell.enabled() || !self.spell.is_available() {
            return;
        }
        let ranges = self.spell.scan(src, self.src_stamp);
        if ranges.is_empty() {
            return;
        }
        let clip = out.response.rect;
        let painter = ui.painter().with_clip_rect(clip);
        let color = theme::pal().error;
        // Char-index → row: glyphs are per-char; a soft-wrap row
        // contributes `glyphs.len()`, a hard break adds the `\n` too.
        let mut row_start = 0usize;
        for placed in &out.galley.rows {
            let row_len = placed.row.glyphs.len();
            let row_end = row_start + row_len;
            for &(s, e) in ranges {
                if e <= row_start || s >= row_end {
                    continue;
                }
                let lo = s.max(row_start) - row_start;
                let hi = e.min(row_end) - row_start;
                let (Some(g0), Some(g1)) = (placed.row.glyphs.get(lo), placed.row.glyphs.get(hi.saturating_sub(1))) else { continue };
                let x0 = out.galley_pos.x + placed.pos.x + g0.pos.x;
                let x1 = out.galley_pos.x + placed.pos.x + g1.pos.x + g1.advance_width;
                let y = out.galley_pos.y + placed.pos.y + placed.row.size.y - 2.0;
                if y < clip.top() || y > clip.bottom() {
                    continue;
                }
                let mut pts = Vec::with_capacity(((x1 - x0) / 2.5) as usize + 2);
                let mut x = x0;
                let mut up = false;
                while x < x1 {
                    pts.push(egui::pos2(x, y - if up { 0.0 } else { 1.6 }));
                    x += 2.5;
                    up = !up;
                }
                pts.push(egui::pos2(x1, y - 0.8));
                painter.add(egui::Shape::line(pts, egui::Stroke::new(1.0, color)));
            }
            row_start = row_end + usize::from(placed.ends_with_newline);
        }
    }

    /// Tinted boxes behind mirror ranges + the primary — the visible
    /// "your edit lands in N places" cue.
    fn paint_mirrors(&self, ui: &mut egui::Ui, out: &egui::text_edit::TextEditOutput) {
        let Some(m) = &self.mirrors else { return };
        let painter = ui.painter().with_clip_rect(out.response.rect);
        let fill = theme::pal().accent.gamma_multiply(0.18);
        let edge = theme::pal().accent;
        for &(s, e) in std::iter::once(&m.primary).chain(m.others.iter()) {
            let mut row_start = 0usize;
            for placed in &out.galley.rows {
                let row_len = placed.row.glyphs.len();
                let row_end = row_start + row_len;
                if e > row_start && s < row_end {
                    let lo = s.max(row_start) - row_start;
                    let hi = e.min(row_end) - row_start;
                    if let (Some(g0), Some(g1)) = (placed.row.glyphs.get(lo), placed.row.glyphs.get(hi.saturating_sub(1))) {
                        let r = egui::Rect::from_min_max(
                            egui::pos2(out.galley_pos.x + placed.pos.x + g0.pos.x, out.galley_pos.y + placed.pos.y),
                            egui::pos2(
                                out.galley_pos.x + placed.pos.x + g1.pos.x + g1.advance_width,
                                out.galley_pos.y + placed.pos.y + placed.row.size.y,
                            ),
                        );
                        painter.rect(r, 1.0, fill, egui::Stroke::new(1.0, edge), egui::StrokeKind::Inside);
                    }
                }
                row_start = row_end + usize::from(placed.ends_with_newline);
            }
        }
    }

    /// Line-number gutter + fold markers + issue dots + active-line
    /// band, painted into the editor's left margin. Rows come from the
    /// live galley so they track scroll; in folded mode `view.src_lines`
    /// maps each visible row back to its source line.
    fn paint_gutter(&mut self, ui: &mut egui::Ui, out: &egui::text_edit::TextEditOutput, view: Option<&fold::View>, regions: &[fold::Region]) {
        let rect = out.response.rect;
        let band = egui::Rect::from_min_max(rect.min, egui::pos2(rect.left() + GUTTER_W, rect.bottom()));
        let painter = ui.painter().with_clip_rect(rect);
        painter.rect_filled(band, 0.0, theme::pal().editor_bg);
        painter.vline(band.right(), band.y_range(), egui::Stroke::new(1.0, theme::pal().border));

        let src_line_of = |i: usize| view.and_then(|v| v.src_lines.get(i).copied()).unwrap_or(i + 1);
        let mono = egui::TextStyle::Monospace.resolve(ui.style());
        let (cur_line, _) = self.cursor_lc;
        for (i, placed) in out.galley.rows.iter().enumerate() {
            let y = out.galley_pos.y + placed.pos.y;
            if y < rect.top() - 4.0 || y > rect.bottom() {
                continue;
            }
            let src_line = src_line_of(i);
            let row_h = placed.row.size.y;

            // Compile issue dot (red error / amber warning), leftmost.
            if let Some(m) = self.marks.get(&src_line) {
                let c = egui::pos2(band.left() + 8.0, y + row_h * 0.5);
                painter.circle_filled(c, 3.0, if m.is_error { theme::pal().error } else { theme::pal().warn });
                let hit = egui::Rect::from_center_size(c, egui::vec2(12.0, row_h));
                ui.interact(hit, ui.id().with(("mark", src_line)), egui::Sense::hover()).on_hover_text(m.messages.join("\n"));
            }

            // Fold triangle on each region start: ▸ collapsed / ▾ open.
            if let Some(r) = regions.iter().find(|r| r.start == src_line) {
                let collapsed = self.folded.contains(&src_line);
                let hit = egui::Rect::from_min_max(egui::pos2(band.left() + 14.0, y), egui::pos2(band.left() + 30.0, y + row_h));
                let resp = ui.interact(hit, ui.id().with(("fold", src_line)), egui::Sense::click());
                let ink = if resp.hovered() { theme::pal().text } else { theme::pal().faint };
                let (cx, cy) = (hit.left() + 6.0, hit.center().y);
                let pts = if collapsed {
                    vec![egui::pos2(cx, cy - 3.5), egui::pos2(cx, cy + 3.5), egui::pos2(cx + 6.0, cy)]
                } else {
                    vec![egui::pos2(cx, cy - 2.5), egui::pos2(cx + 7.0, cy - 2.5), egui::pos2(cx + 3.5, cy + 3.0)]
                };
                painter.add(egui::Shape::convex_polygon(pts, ink, egui::Stroke::NONE));
                let n = r.end - r.start;
                let sf = self.s();
                let resp = resp.on_hover_text(if collapsed {
                    lang::fmt(sf.fold_hidden, &[("n", &n.to_string())])
                } else {
                    lang::fmt(sf.fold_open, &[("n", &n.to_string())])
                });
                if resp.clicked() {
                    self.toggle_fold(src_line);
                }
            }

            let cur = src_line == cur_line;
            let color = if cur { theme::pal().text } else { theme::pal().faint };
            painter.text(egui::pos2(band.right() - 8.0, y), egui::Align2::RIGHT_TOP, format!("{src_line}"), mono.clone(), color);
        }
        // Active line wash (after the gutter so it never hides numbers).
        let cur_row = (0..out.galley.rows.len()).find(|&i| src_line_of(i) == cur_line);
        if let Some(placed) = cur_row.and_then(|i| out.galley.rows.get(i)) {
            let r = egui::Rect::from_min_max(
                egui::pos2(band.right(), out.galley_pos.y + placed.pos.y),
                egui::pos2(rect.right(), out.galley_pos.y + placed.pos.y + placed.row.size.y),
            );
            if r.intersects(rect) {
                painter.rect_filled(r, 0.0, egui::Color32::from_white_alpha(9));
            }
        }
    }

    /// Read the editor's caret into `cursor_lc` for the status bar.
    /// In folded mode the caret's visible row is mapped back to its
    /// source line so Ln/Col and SyncTeX stay in source coordinates.
    fn track_cursor(&mut self, ctx: &egui::Context, editor_id: egui::Id, src: &str, view: Option<&fold::View>) {
        let Some(st) = egui::text_edit::TextEditState::load(ctx, editor_id) else { return };
        let Some(range) = st.cursor.char_range() else { return };
        let char_idx = range.primary.index.0;
        // char index → (line, col), both 1-based
        let mut line = 1usize;
        let mut col = 1usize;
        for (i, c) in src.chars().enumerate() {
            if i >= char_idx {
                break;
            }
            if c == '\n' {
                line += 1;
                col = 1;
            } else {
                col += 1;
            }
        }
        if let Some(v) = view {
            line = v.src_lines.get(line.saturating_sub(1)).copied().unwrap_or(line);
        }
        self.cursor_lc = (line, col);
    }

    /// Editor-column status strip: language, caret, issues.
    fn status_bar(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        egui::Panel::bottom("statusbar").frame(egui::Frame::new().fill(theme::pal().top_bg).inner_margin(egui::Margin::symmetric(10, 3))).show(
            ui,
            |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(s.tex_badge).size(10.5).color(theme::pal().syn_cmd).strong());
                    ui.separator();
                    let n = self.doc.issues.len();
                    let msg = if n == 0 { lang::view(s.issues_none).into_owned() } else { lang::fmt(s.issues_many, &[("n", &n.to_string())]) };
                    ui.label(egui::RichText::new(msg).size(10.5).color(if n == 0 { theme::pal().muted } else { theme::pal().warn }));
                    if !self.folded.is_empty() {
                        ui.separator();
                        ui.label(egui::RichText::new(lang::view(s.folded_warn)).size(10.5).color(theme::pal().warn));
                    }
                    if let Some(h) = self.magic.engine_hint() {
                        ui.separator();
                        ui.label(egui::RichText::new(lang::fmt(s.engine_label, &[("name", h)])).size(10.5).color(theme::pal().syn_cmd))
                            .on_hover_text(lang::view(s.engine_hint).into_owned());
                    }
                    if let Some(r) = &self.magic.root {
                        ui.separator();
                        ui.label(egui::RichText::new(lang::fmt(s.root_label, &[("name", r)])).size(10.5).color(theme::pal().muted))
                            .on_hover_text(lang::view(s.root_hint).into_owned());
                    }
                    ui.separator();
                    ui.weak(lang::view(&self.status));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(lang::fmt(s.line_col, &[("l", &self.cursor_lc.0.to_string()), ("c", &self.cursor_lc.1.to_string())]))
                                .size(10.5)
                                .color(theme::pal().muted),
                        );
                    });
                });
            },
        );
    }

    /// Right pane: page navigation toolbar + page canvas. Panel sides are
    /// fixed in both text directions — only text mirrors in RTL.
    fn preview_pane(&mut self, ui: &mut egui::Ui) {
        let s = self.s();
        let panel = egui::Panel::right("preview");
        panel.resizable(true).default_size(460.0).size_range(240.0..=900.0).frame(egui::Frame::new().fill(theme::pal().canvas_bg)).show(ui, |ui| {
            // Toolbar: ‹ n/m ›  ·  - zoom% +  fit ▾
            egui::Frame::new().fill(theme::pal().top_bg).inner_margin(egui::Margin::symmetric(8, 4)).show(ui, |ui| {
                ui.horizontal(|ui| {
                    let n = self.pages.len();
                    let has = n > 0;
                    if ui.add_enabled(has && self.page_idx > 0, egui::Button::new("‹").min_size(egui::vec2(22.0, 18.0))).clicked() {
                        self.page_step(-1);
                    }
                    ui.label(
                        egui::RichText::new(if has { format!("{} / {}", self.page_idx + 1, n) } else { "– / –".into() })
                            .size(11.0)
                            .color(theme::pal().muted)
                            .monospace(),
                    );
                    if ui.add_enabled(has && self.page_idx + 1 < n, egui::Button::new("›").min_size(egui::vec2(22.0, 18.0))).clicked() {
                        self.page_step(1);
                    }
                    egui::ComboBox::from_id_salt("page-mode").selected_text(lang::view(self.page_mode.label(s))).show_ui(ui, |ui| {
                        for m in PageMode::ALL {
                            ui.selectable_value(&mut self.page_mode, m, lang::view(m.label(s)).into_owned());
                        }
                    });
                    ui.separator();
                    if ui.small_button("−").clicked() {
                        self.zoom = Zoom::Pct((self.zoom_pct(ui) * 0.9).clamp(0.25, 4.0));
                    }
                    let pct = self.zoom_label(ui);
                    ui.label(egui::RichText::new(pct).size(11.0).color(theme::pal().muted).monospace());
                    if ui.small_button("+").clicked() {
                        self.zoom = Zoom::Pct((self.zoom_pct(ui) * 1.1).clamp(0.25, 4.0));
                    }
                    egui::ComboBox::from_id_salt("fit")
                        .selected_text(match self.zoom {
                            Zoom::FitWidth => lang::view(s.zoom_fit_width).into_owned(),
                            Zoom::FitPage => lang::view(s.zoom_fit_page).into_owned(),
                            Zoom::Pct(_) => lang::view(s.zoom_custom).into_owned(),
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.zoom, Zoom::FitWidth, lang::view(s.zoom_fit_width).into_owned());
                            ui.selectable_value(&mut self.zoom, Zoom::FitPage, lang::view(s.zoom_fit_page).into_owned());
                            ui.separator();
                            // Percent presets snap the custom zoom.
                            for &p in ZOOM_PRESETS {
                                ui.selectable_value(&mut self.zoom, Zoom::Pct(p), format!("{:.0}%", p * 100.0));
                            }
                        });
                    #[cfg(not(target_arch = "wasm32"))]
                    if icon_button(ui, 13.0, icons::printer, "", self.pdf_path.is_some() && self.print_rx.is_none())
                        .on_hover_text(lang::view(s.print_hint).into_owned())
                        .clicked()
                    {
                        self.print_pdf();
                    }
                    if icon_button(ui, 13.0, icons::export, "", self.pdf_path.is_some())
                        .on_hover_text(lang::view(s.export_hint).into_owned())
                        .clicked()
                    {
                        self.export_pdf();
                    }
                });
            });
            ui.separator();

            // Canvas: dark backdrop; layout depends on self.page_mode.
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                let area = ui.available_rect_before_wrap();
                ui.painter().rect_filled(area, 0.0, theme::pal().canvas_bg);
                let n = self.pages.len();
                if n == 0 {
                    ui.allocate_rect(area, egui::Sense::hover());
                    ui.painter().text(
                        area.center(),
                        egui::Align2::CENTER_CENTER,
                        lang::view(if self.compiling { s.compiling } else { s.no_pdf }).as_ref(),
                        egui::FontId::proportional(13.0),
                        theme::pal().faint,
                    );
                    return;
                }
                // Ctrl+wheel / pinch zoom (egui's zoom_delta handles
                // the modifier check) — only over the canvas.
                let zd = ui.ctx().input(|i| i.zoom_delta());
                if (zd - 1.0).abs() > f32::EPSILON && ui.ui_contains_pointer() {
                    self.zoom = Zoom::Pct((self.zoom_pct(ui) * zd).clamp(0.25, 4.0));
                }
                const GAP: f32 = 16.0;
                // Zoom baseline: in spread modes a spread is two pages
                // plus a gap wide. Pages may differ in size — the
                // fit math uses the widest/tallest page.
                let mut unit_w = 1.0f32;
                let mut unit_h = 1.0f32;
                for t in &self.pages {
                    let s = t.size_vec2();
                    unit_w = unit_w.max(s.x);
                    unit_h = unit_h.max(s.y);
                }
                if matches!(self.page_mode, PageMode::TwoUp | PageMode::TwoUpCover) {
                    unit_w = unit_w * 2.0 + GAP;
                }
                let zoom = match self.zoom {
                    Zoom::Pct(p) => p,
                    Zoom::FitWidth => (area.width() - 32.0).max(40.0) / unit_w,
                    Zoom::FitPage => ((area.width() - 32.0) / unit_w).min((area.height() - 32.0) / unit_h).max(0.01),
                };
                self.page_rects.clear();
                match self.page_mode {
                    PageMode::Scroll => self.preview_scroll(ui, area, zoom, GAP),
                    _ => {
                        let s = spread_first(self.page_mode, spread_of_page(self.page_mode, self.page_idx));
                        self.preview_spread(ui, area, zoom, GAP, &spread_pages(self.page_mode, s, n));
                    }
                }
            });
        });
    }

    /// Continuous-scroll layout: every page centered in one column.
    /// Paints all pages and records their rects for ‹ › scrolling.
    fn preview_scroll(&mut self, ui: &mut egui::Ui, area: egui::Rect, zoom: f32, gap: f32) {
        let n = self.pages.len();
        let mut total_h = gap;
        let mut max_w = 0.0f32;
        for t in &self.pages {
            let s = t.size_vec2() * zoom;
            total_h += s.y + gap;
            max_w = max_w.max(s.x);
        }
        let needed = egui::vec2((max_w + 32.0).max(area.width()), total_h.max(area.height()));
        let (region, _) = ui.allocate_exact_size(needed, egui::Sense::hover());
        let mut y = region.top() + gap;
        if let Some(r) = self.pending_page_scroll.take() {
            ui.scroll_to_rect(r.expand(gap), Some(egui::Align::Center));
        }
        for i in 0..n {
            let Some(tex) = self.pages.get(i).cloned() else { continue };
            let size = tex.size_vec2() * zoom;
            let page = egui::Rect::from_min_size(egui::pos2(region.center().x - size.x / 2.0, y), size);
            self.page_rects.push(page);
            self.paint_page(ui, i, page, &tex);
            y += size.y + gap;
        }
        // Track which page is nearest the view center for the counter.
        let mid = area.center().y;
        if let Some((i, _)) = self
            .page_rects
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (a.center().y - mid).abs().partial_cmp(&(b.center().y - mid).abs()).unwrap_or(std::cmp::Ordering::Equal))
        {
            self.page_idx = i;
        }
    }

    /// Single-page or spread layout: `idx` are the visible pages (1 or
    /// 2) laid out in a row, centered in the view.
    fn preview_spread(&mut self, ui: &mut egui::Ui, area: egui::Rect, zoom: f32, gap: f32, idx: &[usize]) {
        let mut row_w = gap * idx.len().saturating_sub(1) as f32;
        let mut max_h = 0.0f32;
        for &i in idx {
            if let Some(t) = self.pages.get(i) {
                let s = t.size_vec2() * zoom;
                row_w += s.x;
                max_h = max_h.max(s.y);
            }
        }
        let needed = egui::vec2((row_w + 48.0).max(area.width()), (max_h + 48.0).max(area.height()));
        let (region, _) = ui.allocate_exact_size(needed, egui::Sense::hover());
        let mut x = region.center().x - row_w / 2.0;
        let cy = region.center().y;
        for &i in idx {
            let Some(tex) = self.pages.get(i).cloned() else { continue };
            let size = tex.size_vec2() * zoom;
            let page = egui::Rect::from_min_size(egui::pos2(x, cy - size.y / 2.0), size);
            if self.page_rects.len() <= i {
                self.page_rects.resize(i + 1, egui::Rect::NOTHING);
            }
            if let Some(slot) = self.page_rects.get_mut(i) {
                *slot = page;
            }
            self.paint_page(ui, i, page, &tex);
            x += size.x + gap;
        }
    }

    /// Paint one page at `page`: shadow, image, border, SyncTeX flash
    /// overlay (if this page is flashed) and the inverse-search click.
    fn paint_page(&mut self, ui: &mut egui::Ui, idx: usize, page: egui::Rect, tex: &egui::TextureHandle) {
        let painter = ui.painter();
        painter.rect_filled(page.translate(egui::vec2(2.0, 4.0)), 2.0, egui::Color32::from_black_alpha(120));
        painter.image(tex.id(), page, egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1.0, 1.0)), egui::Color32::WHITE);
        painter.rect_stroke(page, 0.0, egui::Stroke::new(1.0, theme::pal().border), egui::StrokeKind::Outside);

        // SyncTeX flash: highlight the forward-search box, fade out over
        // FLASH_SECS, and scroll it into view once.
        #[cfg(not(target_arch = "wasm32"))]
        if let Some(f) = &self.flash {
            if f.page == idx {
                let age = ui.ctx().input(|i| i.time) - f.at;
                let pt = self.page_pts.get(idx).copied().unwrap_or(tex.size_vec2());
                let scale = egui::vec2(page.width() / pt.x.max(1.0), page.height() / pt.y.max(1.0));
                let r = egui::Rect::from_min_size(
                    page.min + egui::vec2(f.rect_pt.min.x * scale.x, f.rect_pt.min.y * scale.y),
                    egui::vec2(f.rect_pt.width() * scale.x, f.rect_pt.height() * scale.y),
                );
                if age < FLASH_SECS {
                    let a = (1.0 - (age / FLASH_SECS) as f32).clamp(0.0, 1.0);
                    painter.rect_filled(r, 2.0, theme::pal().accent.gamma_multiply(0.25 * a));
                    painter.rect_stroke(r, 2.0, egui::Stroke::new(1.5, theme::pal().accent.gamma_multiply(a)), egui::StrokeKind::Inside);
                    ui.ctx().request_repaint();
                }
                if age < 0.3 {
                    ui.scroll_to_rect(r.expand(90.0), Some(egui::Align::Center));
                }
            }
        }

        // SyncTeX inverse: click on the page jumps the caret.
        #[cfg(not(target_arch = "wasm32"))]
        {
            let hit = ui.interact(page, ui.id().with(("pdf-hit", idx)), egui::Sense::click());
            if hit.clicked() {
                if let Some(pos) = hit.interact_pointer_pos() {
                    self.inverse_sync(page, pos);
                }
            }
        }
    }

    /// ‹ › navigation: a step in pages or spreads, depending on the
    /// layout mode. Continuous mode also marks the target rect for
    /// scroll-into-view on the next paint.
    fn page_step(&mut self, dir: i64) {
        let n = self.pages.len();
        if n == 0 {
            return;
        }
        let spread = spread_of_page(self.page_mode, self.page_idx);
        let s = (spread as i64 + dir).clamp(0, spread_count(self.page_mode, n) as i64 - 1) as usize;
        let next = spread_first(self.page_mode, s).min(n - 1);
        if self.page_mode == PageMode::Scroll {
            self.pending_page_scroll = self.page_rects.get(next).copied();
        }
        self.page_idx = next;
    }

    fn zoom_pct(&self, ui: &egui::Ui) -> f32 {
        match self.zoom {
            Zoom::Pct(p) => p,
            _ => {
                // Resolve current fit into a percentage baseline.
                let Some(tex) = self.pages.get(self.page_idx) else { return 1.0 };
                let w = ui.available_width() - 32.0;
                (w.max(40.0) / tex.size_vec2().x).clamp(0.05, 8.0)
            }
        }
    }

    fn zoom_label(&self, ui: &egui::Ui) -> String {
        format!("{:.0}%", self.zoom_pct(ui) * 100.0)
    }

    /// Bottom logs sheet: errors, warnings, log tail. Declared before the
    /// preview panel so it spans the editor + preview columns.
    fn logs_panel(&mut self, ui: &mut egui::Ui) {
        if !self.show_logs {
            return;
        }
        egui::Panel::bottom("logs")
            .resizable(true)
            .default_size(130.0)
            .size_range(64.0..=400.0)
            .frame(egui::Frame::new().fill(theme::pal().top_bg).inner_margin(egui::Margin::symmetric(10, 4)))
            .show(ui, |ui| {
                let s = self.s();
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(lang::view(s.logs_title)).size(10.0).color(theme::pal().faint).strong());
                    if let Some(r) = &self.report {
                        ui.weak(lang::fmt(
                            s.logs_summary,
                            &[
                                ("errors", &r.errors.len().to_string()),
                                ("warnings", &r.warnings.len().to_string()),
                                ("pages", &r.pages.unwrap_or(0).to_string()),
                                ("engine", &r.engine),
                            ],
                        ));
                    }
                });
                ui.separator();
                egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                    if let Some(r) = &self.report {
                        let issues: Vec<_> = r
                            .errors
                            .iter()
                            .map(|e| (e.file.clone(), e.line, e.message.clone(), true))
                            .chain(r.warnings.iter().map(|w| (w.file.clone(), w.line, format!("{}: {}", s.warning_label, w.message), false)))
                            .collect();
                        let log_tail = r.log_tail.clone();
                        let mut jump = None;
                        for (file, line, message, is_error) in issues {
                            let path = self.issue_path(file.as_deref());
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| s.untitled.into());
                            let msg = match line {
                                Some(l) => format!("{name}:{l}  {message}"),
                                None => format!("{name}  {message}"),
                            };
                            let color = if is_error { theme::pal().error } else { theme::pal().warn };
                            let resp = ui.selectable_label(false, egui::RichText::new(lang::view(&msg)).color(color).size(11.5));
                            if resp.clicked() {
                                jump = Some((file, line));
                            }
                        }
                        if !log_tail.is_empty() {
                            ui.add_space(4.0);
                            ui.label(egui::RichText::new(log_tail).monospace().size(10.5).color(theme::pal().muted));
                        }
                        if let Some((file, line)) = jump {
                            self.goto_issue(file.as_deref(), line);
                        }
                    } else {
                        ui.weak(lang::view(s.logs_empty));
                    }
                });
            });
    }

    /// `Ctrl+Shift+C` / the cite button: snapshot the editor caret, then
    /// open the picker — the dialog's filter steals focus immediately.
    fn open_cite(&mut self, ctx: &egui::Context) {
        // In folded mode the caret's char index is in view coordinates;
        // `cursor_lc` is already mapped back to source coordinates.
        let st_caret = if self.folded.is_empty() {
            self.editor_id
                .and_then(|id| egui::text_edit::TextEditState::load(ctx, id))
                .and_then(|st| st.cursor.char_range())
                .map(|r| r.primary.index.0)
        } else {
            None
        };
        let root = self.project_root();
        self.cite.open(&root);
        self.cite.saved_caret = st_caret.or_else(|| {
            let (l, c) = self.cursor_lc;
            Some(char_index_of_lc(self.session.source(), l, c))
        });
    }

    /// Insert the picked key at the saved caret — `\cite{key}` at a
    /// fresh spot, or `key,` merged into an existing cite's braces.
    /// `now` is egui's frame time so the insert counts as a normal edit
    /// for the idle auto-compile timer.
    fn insert_cite(&mut self, key: &str, now: f64) {
        let mut src = self.session.source().to_string();
        let caret_ci = self.cite.saved_caret.unwrap_or(0).min(src.chars().count());
        let caret_b = byte_of_char(&src, caret_ci);
        let ins = cite::insert_text(&src, caret_b, key);
        src.insert_str(caret_b, &ins);
        self.mirrors = None;
        self.session.set_source(src);
        self.src_stamp = self.src_stamp.wrapping_add(1);
        self.reparse();
        self.folded.clear(); // any edit invalidates folds
        self.goto = Some(caret_b + ins.len());
        self.last_edit = now;
    }

    /// Modal: create a file or folder in the project tree.
    fn new_entry_modal(&mut self, ctx: &egui::Context) {
        let s = self.s();
        let Some(entry) = &mut self.new_entry else { return };
        let title = lang::view(if entry.is_dir { s.new_folder_title } else { s.new_file_title });
        let mut create = false;
        let mut cancel = false;
        egui::Window::new(title).collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0)).show(ctx, |ui| {
            ui.label(lang::fmt(s.in_dir, &[("dir", &entry.dir.display().to_string())]));
            ui.add(
                egui::TextEdit::singleline(&mut entry.name)
                    .hint_text(if entry.is_dir { s.folder_name_hint } else { s.file_name_hint })
                    .desired_width(220.0),
            );
            ui.horizontal(|ui| {
                create = ui.button(lang::view(s.create)).clicked();
                cancel = ui.button(lang::view(s.cancel)).clicked();
            });
        });
        let entry = self.new_entry.take().unwrap_or_else(|| NewEntry { dir: PathBuf::new(), name: String::new(), is_dir: false });
        if cancel || entry.name.trim().is_empty() && create {
            return;
        }
        if create {
            let path = entry.dir.join(entry.name.trim());
            let res = if entry.is_dir {
                std::fs::create_dir(&path).map_err(|e| e.to_string())
            } else {
                craft_atomic::atomic_write(&path, b"").map_err(|e| e.to_string())
            };
            match res {
                Ok(()) => {
                    self.tree_scanned_at = f64::NEG_INFINITY;
                    self.status = lang::fmt(self.s().status_created, &[("path", &path.display().to_string())]);
                }
                Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", &path.display().to_string()), ("error", &e.to_string())]),
            }
        } else {
            self.new_entry = Some(entry);
        }
    }

    /// Whether a gallery card matches the search text and active chips.
    #[cfg(not(target_arch = "wasm32"))]
    fn template_card_matches(card: &TemplateCard, query: &str, category: Option<&str>, language: Option<&str>) -> bool {
        let category = category.unwrap_or_default();
        let card_category = if card.category.is_empty() { "general" } else { card.category.as_str() };
        if !category.is_empty() && card_category != category {
            return false;
        }
        if language.is_some_and(|l| !l.is_empty() && card.lang != l) {
            return false;
        }
        if query.is_empty() {
            return true;
        }
        [card.id.as_str(), card.name.as_str(), card.desc.as_str(), card.lang.as_str(), card.engine.as_str(), card_category]
            .iter()
            .any(|part| part.to_lowercase().contains(query))
    }

    /// Indices of cards visible under the current filter/category.
    #[cfg(not(target_arch = "wasm32"))]
    fn visible_templates(&self) -> Vec<usize> {
        let query = self.tpl_filter.trim().to_lowercase();
        self.tpls
            .iter()
            .enumerate()
            .filter(|(_, card)| Self::template_card_matches(card, &query, self.tpl_category.as_deref(), self.tpl_language.as_deref()))
            .map(|(i, _)| i)
            .collect()
    }

    /// Refresh the template catalog (engine `template.list`).
    #[cfg(not(target_arch = "wasm32"))]
    fn load_templates(&mut self) {
        self.tpls = self
            .session
            .execute("template.list", &serde_json::json!({}))
            .ok()
            .and_then(|v| {
                v["templates"].as_array().map(|a| {
                    let lang = self.lang().code();
                    a.iter()
                        .map(|t| {
                            // Localized name/desc override when the meta
                            // ships one for the active UI language.
                            let loc = &t["localized"][lang];
                            TemplateCard {
                                id: t["id"].as_str().unwrap_or_default().to_string(),
                                name: loc["name"].as_str().filter(|n| !n.is_empty()).or_else(|| t["name"].as_str()).unwrap_or_default().to_string(),
                                desc: loc["desc"].as_str().filter(|d| !d.is_empty()).or_else(|| t["desc"].as_str()).unwrap_or_default().to_string(),
                                lang: t["lang"].as_str().unwrap_or_default().to_string(),
                                category: t["category"].as_str().unwrap_or("general").to_string(),
                                engine: t["engine"].as_str().unwrap_or_default().to_string(),
                                origin: t["origin"].as_str().unwrap_or("bundled").to_string(),
                                license: t["license"].as_str().unwrap_or_default().to_string(),
                                source: t["source"].as_str().unwrap_or_default().to_string(),
                                files: t["files"]
                                    .as_array()
                                    .map(|f| f.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                                    .unwrap_or_default(),
                            }
                        })
                        .collect()
                })
            })
            .unwrap_or_default();
    }

    /// Load the selected template's main source for the preview pane.
    #[cfg(not(target_arch = "wasm32"))]
    fn load_template_preview(&mut self, id: &str) {
        if self.tpl_preview_for == id {
            return;
        }
        self.tpl_preview = self
            .session
            .execute("template.read", &serde_json::json!({"id": id}))
            .ok()
            .and_then(|v| v["content"].as_str().map(str::to_string))
            .unwrap_or_default();
        self.tpl_preview_for = id.to_string();
    }

    /// Copy a user-selected folder into the per-user gallery. The engine
    /// filters this to text sources and records provenance metadata.
    #[cfg(not(target_arch = "wasm32"))]
    fn import_template_folder(&mut self) {
        if let Some(path) = rfd::FileDialog::new().pick_folder() {
            match self.session.execute(
                "template.import",
                &serde_json::json!({"path": path.display().to_string(), "category": "community", "license": self.tpl_license.trim()}),
            ) {
                Ok(v) => {
                    self.status = lang::fmt(self.s().status_imported, &[("id", v["id"].as_str().unwrap_or("template"))]);
                    self.load_templates();
                }
                Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", "template.import"), ("error", &e.to_string())]),
            }
        }
    }

    /// Modal gallery: searchable cards on the left, metadata + source
    /// preview on the right, and a filename row for `template.new`.
    #[cfg(not(target_arch = "wasm32"))]
    fn tpl_modal(&mut self, ctx: &egui::Context) {
        if !self.tpl_open {
            return;
        }
        let s = self.s();
        let mut sel = self.tpl_sel;
        let mut create = false;
        let mut close = false;
        let mut import = false;
        let mut picked = false;
        let visible = self.visible_templates();
        if sel.is_none() || sel.is_some_and(|i| !visible.contains(&i)) {
            sel = visible.first().copied();
            picked = sel.is_some();
        }
        if !ctx.egui_wants_keyboard_input() && !visible.is_empty() {
            let pos = sel.and_then(|i| visible.iter().position(|v| *v == i));
            if ctx.input(|i| i.key_pressed(egui::Key::ArrowDown)) {
                sel = Some(visible[pos.map_or(0, |p| (p + 1) % visible.len())]);
                picked = true;
            } else if ctx.input(|i| i.key_pressed(egui::Key::ArrowUp)) {
                sel = Some(visible[pos.map_or(visible.len() - 1, |p| p.saturating_sub(1))]);
                picked = true;
            } else if ctx.input(|i| i.key_pressed(egui::Key::Enter)) {
                create = sel.is_some();
            }
        }
        let categories =
            self.tpls.iter().map(|t| if t.category.is_empty() { "general".to_string() } else { t.category.clone() }).collect::<BTreeSet<_>>();
        let languages = self.tpls.iter().filter(|t| !t.lang.is_empty()).map(|t| t.lang.clone()).collect::<BTreeSet<_>>();

        egui::Window::new(lang::view(s.tpl_title))
            .collapsible(false)
            .resizable(true)
            .default_size([880.0, 560.0])
            .min_size([760.0, 480.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.tpl_filter).hint_text(lang::view(s.tpl_search_hint)).desired_width(230.0));
                    import = ui.button(lang::view(s.tpl_import)).clicked();
                    ui.add(egui::TextEdit::singleline(&mut self.tpl_license).hint_text(lang::view(s.tpl_license_hint)).desired_width(190.0));
                });
                ui.horizontal_wrapped(|ui| {
                    if ui.selectable_label(self.tpl_category.is_none(), lang::view(s.tpl_all)).clicked() {
                        self.tpl_category = None;
                    }
                    for category in &categories {
                        if ui.selectable_label(self.tpl_category.as_deref() == Some(category.as_str()), category).clicked() {
                            self.tpl_category = Some(category.clone());
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(lang::view(s.language)).size(10.0).color(theme::pal().muted));
                    if ui.selectable_label(self.tpl_language.is_none(), lang::view(s.tpl_all)).clicked() {
                        self.tpl_language = None;
                    }
                    for language in &languages {
                        if ui.selectable_label(self.tpl_language.as_deref() == Some(language.as_str()), language).clicked() {
                            self.tpl_language = Some(language.clone());
                        }
                    }
                });
                ui.separator();
                if self.tpls.is_empty() {
                    ui.label(egui::RichText::new(lang::view(s.tpl_none)).size(11.0).color(theme::pal().muted));
                }
                ui.horizontal(|ui| {
                    ui.set_min_height(330.0);
                    ui.vertical(|ui| {
                        ui.set_width(410.0);
                        egui::ScrollArea::vertical().id_salt("tpl_cards").auto_shrink([false, false]).show(ui, |ui| {
                            egui::Grid::new("tpl_cards_grid").num_columns(2).spacing([7.0, 7.0]).show(ui, |ui| {
                                for (row, i) in visible.iter().copied().enumerate() {
                                    let Some(card) = self.tpls.get(i) else { continue };
                                    let selected = sel == Some(i);
                                    let card_ui = egui::Frame::new()
                                        .fill(if selected { theme::pal().selected } else { theme::pal().raised })
                                        .stroke(egui::Stroke::new(
                                            if selected { 1.5 } else { 1.0 },
                                            if selected { theme::pal().accent } else { theme::pal().border },
                                        ))
                                        .corner_radius(6.0)
                                        .inner_margin(egui::Margin::same(8))
                                        .show(ui, |ui| {
                                            ui.set_width(182.0);
                                            ui.label(
                                                egui::RichText::new(lang::view(if card.name.is_empty() { &card.id } else { &card.name }))
                                                    .size(12.5)
                                                    .strong(),
                                            );
                                            ui.horizontal_wrapped(|ui| {
                                                if !card.lang.is_empty() {
                                                    ui.label(
                                                        egui::RichText::new(format!("[{}]", card.lang)).size(9.0).color(theme::pal().accent_dim),
                                                    );
                                                }
                                                ui.label(
                                                    egui::RichText::new(if card.category.is_empty() { "general" } else { &card.category })
                                                        .size(9.0)
                                                        .color(theme::pal().muted),
                                                );
                                                if !card.engine.is_empty() {
                                                    ui.label(egui::RichText::new(&card.engine).size(9.0).color(theme::pal().muted));
                                                }
                                            });
                                            ui.label(egui::RichText::new(lang::view(&card.desc)).size(9.5).color(theme::pal().muted));
                                            let origin = if card.origin == "user" { s.tpl_imported } else { s.tpl_bundled };
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "{origin} · {}",
                                                    lang::fmt(s.tpl_files, &[("n", &card.files.len().to_string())])
                                                ))
                                                .size(8.5)
                                                .color(theme::pal().faint),
                                            );
                                        });
                                    if card_ui.response.interact(egui::Sense::click()).clicked() {
                                        sel = Some(i);
                                        picked = true;
                                    }
                                    if row % 2 == 1 {
                                        ui.end_row();
                                    }
                                }
                                if visible.len() % 2 == 1 {
                                    ui.end_row();
                                }
                            });
                        });
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_min_width(290.0);
                        ui.label(egui::RichText::new(lang::view(s.tpl_preview)).size(10.0).color(theme::pal().muted));
                        if let Some(card) = sel.and_then(|i| self.tpls.get(i)) {
                            ui.label(egui::RichText::new(lang::view(if card.name.is_empty() { &card.id } else { &card.name })).size(12.0).strong());
                            ui.horizontal_wrapped(|ui| {
                                for badge in [card.lang.as_str(), card.category.as_str(), card.engine.as_str()] {
                                    if !badge.is_empty() {
                                        ui.label(egui::RichText::new(badge).size(9.0).color(theme::pal().accent_dim));
                                    }
                                }
                            });
                            if !card.license.is_empty() || !card.source.is_empty() {
                                ui.label(egui::RichText::new(format!("{} {}", card.license, card.source)).size(8.5).color(theme::pal().muted));
                            }
                            egui::ScrollArea::vertical().id_salt("tpl_source").auto_shrink([false, false]).show(ui, |ui| {
                                ui.label(egui::RichText::new(&self.tpl_preview).size(9.0).monospace());
                            });
                        }
                    });
                });
                ui.separator();
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.tpl_name).hint_text(lang::view(s.file_name_hint)).desired_width(260.0));
                    let can_create = sel.is_some_and(|i| visible.contains(&i)) && !self.tpl_name.trim().is_empty();
                    create |= ui.add_enabled(can_create, egui::Button::new(lang::view(s.create))).clicked();
                    close = ui.button(lang::view(s.cancel)).clicked();
                });
            });

        self.tpl_sel = sel;
        if picked {
            if let Some(card) = sel.and_then(|i| self.tpls.get(i)) {
                self.tpl_name = format!("{}.tex", card.id);
                let id = card.id.clone();
                self.load_template_preview(&id);
            }
        }
        if import {
            self.import_template_folder();
        }
        if create {
            if let Some(card) = self.tpl_sel.and_then(|i| self.tpls.get(i)) {
                if self.tpl_name.trim().is_empty() {
                    self.tpl_name = format!("{}.tex", card.id);
                }
                let path = self.project_root().join(self.tpl_name.trim());
                match self.session.execute("template.new", &serde_json::json!({"id": card.id, "path": path.display().to_string()})) {
                    Ok(_) => {
                        self.tree_scanned_at = f64::NEG_INFINITY;
                        self.open_path(path);
                        self.tpl_open = false;
                    }
                    Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", "template.new"), ("error", &e.to_string())]),
                }
            }
        }
        if close {
            self.tpl_open = false;
        }
    }

    /// Refresh the snapshot list (engine `snapshot.list`).
    #[cfg(not(target_arch = "wasm32"))]
    fn load_snaps(&mut self) {
        self.snaps = self
            .session
            .execute("snapshot.list", &serde_json::json!({}))
            .ok()
            .and_then(|v| {
                v["snapshots"].as_array().map(|a| {
                    a.iter()
                        .map(|s| {
                            (
                                s["id"].as_str().unwrap_or_default().to_string(),
                                s["ts"].as_str().unwrap_or_default().to_string(),
                                s["label"].as_str().unwrap_or_default().to_string(),
                                s["files"].as_i64().unwrap_or(0),
                            )
                        })
                        .collect()
                })
            })
            .unwrap_or_default();
    }

    /// Window: local-history snapshots — take one with a label, or
    /// restore an older tree (two-click confirm; overwrites sources).
    #[cfg(not(target_arch = "wasm32"))]
    fn hist_modal(&mut self, ctx: &egui::Context) {
        if !self.hist_open {
            return;
        }
        let mut snap = false;
        let mut restore: Option<String> = None;
        let mut close = false;
        egui::Window::new(lang::view(self.s().hist_title))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                let s = self.s();
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.hist_label).hint_text(lang::view(s.hist_label_hint)).desired_width(180.0));
                    snap = ui.button(lang::view(s.snapshot_now)).clicked();
                });
                ui.separator();
                if self.snaps.is_empty() {
                    ui.label(egui::RichText::new(lang::view(s.no_snapshots)).size(11.0).color(theme::pal().muted));
                }
                egui::ScrollArea::vertical().max_height(200.0).show(ui, |ui| {
                    for (id, ts, label, files) in &self.snaps {
                        ui.horizontal(|ui| {
                            let confirming = self.hist_confirm.as_deref() == Some(id.as_str());
                            if ui.selectable_label(confirming, lang::view(s.restore)).clicked() {
                                if confirming {
                                    restore = Some(id.clone());
                                } else {
                                    self.hist_confirm = Some(id.clone());
                                }
                            }
                            if confirming {
                                ui.label(egui::RichText::new(lang::view(s.restore_confirm)).size(9.5).color(theme::pal().warn));
                            }
                            let line = if label.is_empty() {
                                format!("{ts}  ({})", lang::fmt(s.engine_files, &[("n", &files.to_string())]))
                            } else {
                                format!("{ts}  {}  ({})", lang::view(label), lang::fmt(s.engine_files, &[("n", &files.to_string())]))
                            };
                            ui.label(egui::RichText::new(line).size(10.5));
                        });
                    }
                });
                ui.separator();
                close = ui.button(lang::view(s.close)).clicked();
            });
        if snap {
            let label = self.hist_label.trim();
            match self.session.execute("project.snapshot", &serde_json::json!({"label": label})) {
                Ok(v) => {
                    self.status = lang::fmt(self.s().status_snapshot, &[("id", v["id"].as_str().unwrap_or("?"))]);
                    self.load_snaps();
                }
                Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", "snapshot"), ("error", &e.to_string())]),
            }
        }
        if let Some(id) = restore {
            match self.session.execute("snapshot.restore", &serde_json::json!({"id": id, "confirm": true})) {
                Ok(_) => {
                    self.status = self.s().status_restored.into();
                    // Reload the open file — restore overwrote it on disk.
                    if let Some(p) = self.session.path().map(Path::to_path_buf) {
                        if let Err(e) = self.session.open(&p) {
                            self.status = lang::fmt(self.s().status_failed, &[("context", self.s().history), ("error", &e.to_string())]);
                        } else {
                            self.reparse();
                        }
                    }
                }
                Err(e) => self.status = lang::fmt(self.s().status_failed, &[("context", "restore"), ("error", &e.to_string())]),
            }
            self.hist_confirm = None;
        }
        if close {
            self.hist_open = false;
        }
    }

    /// Window: document statistics — `texparse::analyze` on the live
    /// buffer (lexer-based; comments and commands don't count as words).
    fn stats_modal(&mut self, ctx: &egui::Context) {
        if !self.stats_open {
            return;
        }
        let s = texparse::analyze(self.session.source());
        let st = self.s();
        egui::Window::new(lang::view(st.stats_title))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut self.stats_open)
            .show(ctx, |ui| {
                egui::Grid::new("stats_grid").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    for (label, v) in [
                        (st.st_words, s.words),
                        (st.st_chars, s.chars),
                        (st.st_chars_ns, s.chars_no_space),
                        (st.st_lines, s.lines),
                        (st.st_commands, s.commands),
                        (st.st_math_inline, s.math_inline),
                        (st.st_math_display, s.math_display),
                        (st.st_envs, s.envs),
                        (st.st_sections, s.sections),
                        (st.st_labels, s.labels),
                        (st.st_cites, s.cites),
                    ] {
                        ui.label(egui::RichText::new(lang::view(label)).size(11.5).color(theme::pal().muted));
                        ui.label(egui::RichText::new(format!("{v}")).size(11.5).monospace());
                        ui.end_row();
                    }
                });
            });
    }

    /// Preferences: the same persisted controls as the toolbar menus,
    /// gathered into one discoverable dialog.
    fn settings_modal(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let s = self.s();
        let mut open = self.settings_open;
        egui::Window::new(lang::view(s.settings_title))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                egui::Grid::new("settings-grid").num_columns(2).spacing(egui::vec2(14.0, 8.0)).show(ui, |ui| {
                    ui.label(lang::view(s.language));
                    let mut language = self.lang();
                    egui::ComboBox::from_id_salt("settings-language").selected_text(lang::view(language.native_name())).show_ui(ui, |ui| {
                        for l in lang::ALL {
                            ui.selectable_value(&mut language, *l, lang::view(l.native_name()));
                        }
                    });
                    if language != self.lang() {
                        self.set_lang(language);
                    }
                    ui.end_row();

                    ui.label(lang::view(s.set_theme));
                    let mut dark = theme::is_dark();
                    egui::ComboBox::from_id_salt("settings-theme")
                        .selected_text(lang::view(if dark { s.theme_dark } else { s.theme_light }))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut dark, true, lang::view(s.theme_dark));
                            ui.selectable_value(&mut dark, false, lang::view(s.theme_light));
                        });
                    if dark != theme::is_dark() {
                        theme::apply(ui.ctx(), dark);
                    }
                    ui.end_row();

                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        ui.label(lang::view(s.set_engine));
                        let selected = self.engine_sel.and_then(|i| self.engines.get(i)).cloned().unwrap_or_else(|| s.engine_auto.into());
                        egui::ComboBox::from_id_salt("settings-engine").selected_text(lang::view(&selected)).show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.engine_sel, None, lang::view(s.engine_auto));
                            for (i, e) in self.engines.iter().enumerate() {
                                ui.selectable_value(&mut self.engine_sel, Some(i), e);
                            }
                        });
                        ui.end_row();
                    }

                    ui.label(lang::view(s.set_page_mode));
                    egui::ComboBox::from_id_salt("settings-page-mode").selected_text(lang::view(self.page_mode.label(s))).show_ui(ui, |ui| {
                        for mode in PageMode::ALL {
                            ui.selectable_value(&mut self.page_mode, mode, lang::view(mode.label(s)));
                        }
                    });
                    ui.end_row();

                    ui.checkbox(&mut self.autocompile, lang::view(s.auto_compile));
                    let mut spell = self.spell.enabled();
                    if ui.checkbox(&mut spell, lang::view(s.set_spellcheck)).changed() {
                        self.spell.set_enabled(spell);
                    }
                    ui.end_row();
                });
            });
        self.settings_open = open;
    }

    /// Categorized symbol palette; a click inserts the LaTeX command at
    /// the caret (or wraps the current selection where that makes sense).
    fn symbols_modal(&mut self, ctx: &egui::Context) {
        if !self.symbols_open {
            return;
        }
        let s = self.s();
        let mut open = self.symbols_open;
        let mut insert: Option<(String, usize)> = None;
        egui::Window::new(lang::view(s.symbols_title))
            .collapsible(false)
            .resizable(true)
            .default_size(egui::vec2(420.0, 300.0))
            .open(&mut open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (label, commands) in [
                        (s.symbols_greek, SYMBOL_GREEK),
                        (s.symbols_math, SYMBOL_MATH),
                        (s.symbols_relations, SYMBOL_RELATIONS),
                        (s.symbols_arrows, SYMBOL_ARROWS),
                    ] {
                        ui.label(egui::RichText::new(lang::view(label)).size(10.0).color(theme::pal().faint));
                        ui.horizontal_wrapped(|ui| {
                            for command in commands {
                                if ui.button(format!("\\{command}")).clicked() {
                                    let caret_back = match *command {
                                        "frac{}{}" => 3,
                                        "sqrt{}" => 1,
                                        _ => 0,
                                    };
                                    insert = Some((format!("\\{command}"), caret_back));
                                }
                            }
                        });
                        ui.add_space(8.0);
                    }
                });
            });
        self.symbols_open = open;
        if let Some((command, caret_back)) = insert {
            self.insert_latex(ctx, &command, "", caret_back);
        }
    }

    /// Window: every key binding the app handles, grouped. Opened by
    /// the `?` toolbar button or F1.
    fn shortcuts_modal(&mut self, ctx: &egui::Context) {
        if !self.shortcuts_open {
            return;
        }
        let s = self.s();
        egui::Window::new(lang::view(s.shortcuts_title))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut self.shortcuts_open)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    for (group, rows) in SHORTCUTS {
                        let group_label = match *group {
                            "file" => s.sc_file,
                            "editing" => s.sc_editing,
                            "navigation" => s.sc_navigation,
                            _ => s.sc_help,
                        };
                        ui.label(egui::RichText::new(lang::view(group_label)).size(10.0).color(theme::pal().faint));
                        egui::Grid::new(("keys", group)).num_columns(2).spacing([24.0, 3.0]).show(ui, |ui| {
                            for (keys, action) in *rows {
                                ui.label(egui::RichText::new(*keys).size(11.5).monospace().color(theme::pal().accent_dim));
                                let action = s.sc_actions.get(*action).copied().unwrap_or_default();
                                ui.label(egui::RichText::new(lang::view(action)).size(11.5));
                                ui.end_row();
                            }
                        });
                        ui.add_space(8.0);
                    }
                });
            });
    }

    /// Window: BYO-provider AI assist — endpoint/model fields, test +
    /// ask buttons, response area. Token stays session-only (never
    /// persisted). Worker replies drain through `AiPanel::poll`.
    #[cfg(not(target_arch = "wasm32"))]
    fn ai_modal(&mut self, ctx: &egui::Context) {
        if !self.ai.open {
            return;
        }
        let mut open = self.ai.open;
        let mut ask = false;
        let mut probe = false;
        let mut setup = false;
        let mut install = false;
        egui::Window::new(lang::view(self.s().ai_title))
            .collapsible(false)
            .resizable(true)
            .default_size([460.0, 390.0])
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                let s = self.s();
                egui::Grid::new("ai_cfg").num_columns(2).spacing([8.0, 4.0]).show(ui, |ui| {
                    ui.label(egui::RichText::new(lang::view(s.ai_provider)).size(11.0).color(theme::pal().muted));
                    let mut provider = self.ai.provider;
                    egui::ComboBox::from_id_salt("ai-provider")
                        .selected_text(match provider {
                            ai::AiProviderPreset::Ollama => "Ollama".to_string(),
                            ai::AiProviderPreset::OpenAi => "OpenAI".to_string(),
                            ai::AiProviderPreset::OpenRouter => "OpenRouter".to_string(),
                            ai::AiProviderPreset::Custom => lang::view(s.ai_custom).into_owned(),
                        })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut provider, ai::AiProviderPreset::Ollama, "Ollama");
                            ui.selectable_value(&mut provider, ai::AiProviderPreset::OpenAi, "OpenAI");
                            ui.selectable_value(&mut provider, ai::AiProviderPreset::OpenRouter, "OpenRouter");
                            ui.selectable_value(&mut provider, ai::AiProviderPreset::Custom, lang::view(s.ai_custom));
                        });
                    if provider != self.ai.provider {
                        self.ai.set_provider(provider);
                    }
                    ui.end_row();
                    ui.label(egui::RichText::new(lang::view(s.ai_endpoint)).size(11.0).color(theme::pal().muted));
                    ui.add(egui::TextEdit::singleline(&mut self.ai.endpoint).hint_text("http://localhost:11434/v1").desired_width(300.0));
                    ui.end_row();
                    ui.label(egui::RichText::new(lang::view(s.ai_model)).size(11.0).color(theme::pal().muted));
                    ui.add(egui::TextEdit::singleline(&mut self.ai.model).hint_text("qwen2.5:0.5b, gpt-4o-mini, …").desired_width(300.0));
                    ui.end_row();
                    if !self.ai.models.is_empty() {
                        ui.label(egui::RichText::new(lang::view(s.ai_models)).size(11.0).color(theme::pal().muted));
                        let models = self.ai.models.clone();
                        egui::ComboBox::from_id_salt("ai-models").selected_text(&self.ai.model).show_ui(ui, |ui| {
                            for model in models {
                                ui.selectable_value(&mut self.ai.model, model.clone(), model);
                            }
                        });
                        ui.end_row();
                    }
                    ui.label(egui::RichText::new(lang::view(s.ai_token)).size(11.0).color(theme::pal().muted));
                    ui.add_enabled(
                        self.ai.provider != ai::AiProviderPreset::Ollama,
                        egui::TextEdit::singleline(&mut self.ai.token).password(true).hint_text(lang::view(s.ai_token_hint)).desired_width(300.0),
                    );
                    ui.end_row();
                });
                ui.horizontal_wrapped(|ui| {
                    probe = ui.add_enabled(!self.ai.busy, egui::Button::new(lang::view(s.ai_test))).clicked();
                    if self.ai.provider == ai::AiProviderPreset::Ollama {
                        setup = ui.add_enabled(!self.ai.busy, egui::Button::new(lang::view(s.ai_setup))).clicked();
                        if self.ai.needs_install {
                            install = ui.add_enabled(!self.ai.busy, egui::Button::new(lang::view(s.ai_install))).clicked();
                        }
                    }
                    if let Some((ok, label)) = &self.ai.status {
                        let text = if self.ai.needs_install { lang::view(s.ai_missing).into_owned() } else { label.clone() };
                        ui.label(egui::RichText::new(text).size(10.5).color(if *ok { theme::pal().accent_dim } else { theme::pal().error }));
                    }
                });
                ui.separator();
                ui.label(egui::RichText::new(lang::view(s.ai_question)).size(10.5).color(theme::pal().muted));
                egui::ScrollArea::vertical().id_salt("ai_prompt").max_height(90.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut self.ai.prompt).desired_width(f32::INFINITY).desired_rows(3));
                });
                ui.horizontal(|ui| {
                    ask = ui.add_enabled(!self.ai.busy && !self.ai.prompt.trim().is_empty(), egui::Button::new(lang::view(s.ai_ask))).clicked();
                    if self.ai.busy {
                        ui.spinner();
                    }
                });
                if !self.ai.response.is_empty() {
                    ui.separator();
                    egui::ScrollArea::vertical().id_salt("ai_resp").show(ui, |ui| {
                        ui.label(egui::RichText::new(&self.ai.response).size(11.0));
                    });
                }
                ui.separator();
                self.harness_section(ui);
            });
        self.ai.open = open;
        if probe {
            self.ai.probe();
        }
        if setup {
            self.ai.setup_ollama(false);
        }
        if install {
            self.ai.setup_ollama(true);
        }
        if ask {
            self.ai.ask(self.session.source());
        }
    }

    /// Harness inspector inside the AI window. It is intentionally thin:
    /// every action calls the engine command surface, so GUI, CLI,
    /// control and MCP share the same durable state and policy.
    #[cfg(not(target_arch = "wasm32"))]
    fn harness_section(&mut self, ui: &mut egui::Ui) {
        if self.harness.needs_refresh {
            self.harness.refresh(&mut self.session);
        }
        enum Action {
            None,
            Refresh,
            CreateTask,
            SelectTask(String),
            ApproveTask(bool),
            CancelTask,
            PreviewPlan,
            RepairPreview,
            RepairApply,
            MemoryScope(&'static str),
            MemorySet,
            MemoryDelete(String),
        }
        let mut action = Action::None;
        let s = self.s();

        egui::CollapsingHeader::new(lang::view(s.ai_tasks)).default_open(true).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.harness.task_title).hint_text(lang::view(s.ai_task_title)).desired_width(160.0));
                if ui.button(lang::view(s.create)).clicked() {
                    action = Action::CreateTask;
                }
                if ui.button(lang::view(s.repo_refresh)).clicked() {
                    action = Action::Refresh;
                }
                if ui.button(lang::view(s.ai_repair)).clicked() {
                    action = Action::RepairPreview;
                }
            });
            egui::ScrollArea::vertical().id_salt("ai_task_prompt").max_height(54.0).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.harness.task_prompt)
                        .hint_text(lang::view(s.ai_task_prompt))
                        .desired_width(f32::INFINITY)
                        .desired_rows(2),
                );
            });
            egui::ScrollArea::vertical().id_salt("ai_tasks").max_height(96.0).show(ui, |ui| {
                if self.harness.tasks.is_empty() {
                    ui.label(egui::RichText::new(lang::view(s.ai_task_none)).size(10.5).color(theme::pal().muted));
                }
                for task in self.harness.tasks.clone() {
                    let id = task.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
                    let title = task.get("title").and_then(Value::as_str).unwrap_or("task").to_string();
                    let state = task.get("state").and_then(Value::as_str).unwrap_or("draft").to_string();
                    let selected = self.harness.selected_task == id;
                    if ui.selectable_label(selected, lang::view(&format!("{title} — {state}"))).clicked() {
                        action = Action::SelectTask(id);
                    }
                }
            });
            if let Some(task) = self.harness.task_detail.clone() {
                let state = task.get("state").and_then(Value::as_str).unwrap_or("draft").to_string();
                ui.horizontal_wrapped(|ui| {
                    ui.label(egui::RichText::new(&state).size(10.5).color(theme::pal().muted));
                    if state == "waiting_approval" {
                        if ui.button(lang::view(s.ai_approve)).clicked() {
                            action = Action::ApproveTask(true);
                        }
                        if ui.button(lang::view(s.ai_reject)).clicked() {
                            action = Action::ApproveTask(false);
                        }
                    }
                    let repair_queued = state == "queued" && task.pointer("/plan/kind").and_then(Value::as_str) == Some("compile_repair");
                    if repair_queued && ui.button(lang::view(s.ai_repair_apply)).clicked() {
                        action = Action::RepairApply;
                    }
                    if !matches!(state.as_str(), "completed" | "failed" | "cancelled") && ui.button(lang::view(s.ai_task_cancel)).clicked() {
                        action = Action::CancelTask;
                    }
                });
                if task.pointer("/plan/kind").and_then(Value::as_str) == Some("compile_repair") {
                    for edit in task.pointer("/plan/edits").and_then(Value::as_array).cloned().unwrap_or_default().iter().take(12) {
                        let op = edit.get("op").and_then(Value::as_str).unwrap_or_default();
                        let line = edit.get("line").and_then(Value::as_u64).unwrap_or(0);
                        let after = edit.get("after").and_then(Value::as_str).unwrap_or("—");
                        ui.label(egui::RichText::new(format!("{op} L{line}: {after}")).size(10.0).monospace());
                    }
                }
            }
        });

        egui::CollapsingHeader::new(lang::view(s.ai_plan)).default_open(false).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("ai_plan_json").max_height(76.0).show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut self.harness.plan_json)
                        .desired_width(f32::INFINITY)
                        .desired_rows(4)
                        .font(egui::TextStyle::Monospace),
                );
            });
            if ui.button(lang::view(s.ai_plan_preview)).clicked() {
                action = Action::PreviewPlan;
            }
            if let Some(result) = &self.harness.plan_result {
                let plan = result.get("plan").cloned().unwrap_or_else(|| result.clone());
                let needs = plan.get("requires_approval").and_then(Value::as_bool).unwrap_or(false);
                ui.label(
                    egui::RichText::new(if needs { lang::view(s.ai_approval_needed) } else { lang::view(s.ai_plan_read_only) })
                        .size(10.5)
                        .color(if needs { theme::pal().warn } else { theme::pal().accent_dim }),
                );
                for command in plan.get("commands").and_then(Value::as_array).cloned().unwrap_or_default() {
                    let name = command.get("command").and_then(Value::as_str).unwrap_or_default();
                    let tier = command.get("tier").and_then(Value::as_str).unwrap_or_default();
                    ui.label(egui::RichText::new(format!("{name} — {tier}")).size(10.0).monospace());
                }
            }
        });

        egui::CollapsingHeader::new(lang::view(s.ai_permissions)).default_open(false).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("ai_permissions").max_height(110.0).show(ui, |ui| {
                for policy in &self.harness.permissions {
                    let command = policy.get("command").and_then(Value::as_str).unwrap_or_default();
                    let tier = policy.get("tier").and_then(Value::as_str).unwrap_or_default();
                    let approval = policy.get("approval_required").and_then(Value::as_bool).unwrap_or(false);
                    let text = if approval {
                        format!("{command} — {tier} ({})", lang::view(s.ai_approval_needed))
                    } else {
                        format!("{command} — {tier}")
                    };
                    ui.label(egui::RichText::new(text).size(10.0).monospace());
                }
            });
        });

        egui::CollapsingHeader::new(lang::view(s.ai_memory)).default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(lang::view(s.ai_memory_scope)).size(10.5).color(theme::pal().muted));
                egui::ComboBox::from_id_salt("ai_memory_scope").selected_text(lang::view(&self.harness.memory_scope)).show_ui(ui, |ui| {
                    for scope in ["session", "project", "task", "user"] {
                        if ui.selectable_label(self.harness.memory_scope == scope, lang::view(scope)).clicked() {
                            action = Action::MemoryScope(scope);
                        }
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.harness.memory_key).hint_text(lang::view(s.ai_memory_key)).desired_width(110.0));
                ui.add(egui::TextEdit::singleline(&mut self.harness.memory_value).hint_text(lang::view(s.ai_memory_value)).desired_width(190.0));
                ui.checkbox(&mut self.harness.memory_pinned, lang::view(s.ai_pinned));
                if ui.button(lang::view(s.ai_memory_set)).clicked() {
                    action = Action::MemorySet;
                }
            });
            egui::ScrollArea::vertical().id_salt("ai_memory").max_height(92.0).show(ui, |ui| {
                for entry in self.harness.memory_entries.clone() {
                    let key = entry.get("key").and_then(Value::as_str).unwrap_or_default().to_string();
                    let pinned = entry.get("pinned").and_then(Value::as_bool).unwrap_or(false);
                    let value = entry.get("value").map(Value::to_string).unwrap_or_default();
                    let value = value.chars().take(72).collect::<String>();
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(format!("{}{key}: {value}", if pinned { "★ " } else { "" })).size(10.0).monospace());
                        if ui.small_button(lang::view(s.ai_delete)).clicked() {
                            action = Action::MemoryDelete(key);
                        }
                    });
                }
            });
        });

        egui::CollapsingHeader::new(lang::view(s.ai_prompts)).default_open(false).show(ui, |ui| {
            for prompt in &self.harness.prompts {
                let id = prompt.get("id").and_then(Value::as_str).unwrap_or_default();
                let version = prompt.get("version").and_then(Value::as_str).unwrap_or_default();
                let source = prompt.get("source").and_then(Value::as_str).unwrap_or_default();
                ui.label(egui::RichText::new(format!("{id} — {version} ({source})")).size(10.0).monospace());
            }
        });

        egui::CollapsingHeader::new(lang::view(s.ai_audit)).default_open(false).show(ui, |ui| {
            egui::ScrollArea::vertical().id_salt("ai_audit").max_height(90.0).show(ui, |ui| {
                for event in &self.harness.audit {
                    let command = event.get("command").and_then(Value::as_str).unwrap_or_default();
                    let tier = event.get("tier").and_then(Value::as_str).unwrap_or_default();
                    let ok = event.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    ui.label(egui::RichText::new(format!("{} {command} — {tier}", if ok { "ok" } else { "err" })).size(10.0).monospace());
                }
            });
        });
        if !self.harness.status.is_empty() {
            ui.label(egui::RichText::new(&self.harness.status).size(10.5).color(theme::pal().error));
        }

        match action {
            Action::None => {}
            Action::Refresh => self.harness.needs_refresh = true,
            Action::CreateTask => self.harness.create_task(&mut self.session),
            Action::SelectTask(id) => self.harness.select_task(&mut self.session, &id),
            Action::ApproveTask(approve) => self.harness.approve_task(&mut self.session, approve),
            Action::CancelTask => self.harness.cancel_task(&mut self.session),
            Action::PreviewPlan => self.harness.preview_plan(&mut self.session),
            Action::RepairPreview => self.harness.repair_preview(&mut self.session),
            Action::RepairApply => self.harness.repair_apply(&mut self.session),
            Action::MemoryScope(scope) => self.harness.set_memory_scope(&mut self.session, scope),
            Action::MemorySet => self.harness.set_memory(&mut self.session),
            Action::MemoryDelete(key) => self.harness.delete_memory(&mut self.session, &key),
        }
    }

    /// Git status/actions window. Commands run through the same engine
    /// command surface as CLI/control/MCP; the window itself only guards
    /// the local dirty/conflict edge cases that cannot be inferred by
    /// the scratch worker session.
    #[cfg(not(target_arch = "wasm32"))]
    fn repo_modal(&mut self, ctx: &egui::Context) {
        if !self.repo_open {
            return;
        }
        let mut open = self.repo_open;
        let status = self.repo_status.clone();
        let mut action: Option<(&'static str, serde_json::Value)> = None;
        let mut commit_msg = String::new();
        egui::Window::new(lang::view(self.s().repo_title))
            .collapsible(false)
            .resizable(true)
            .default_width(560.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                let s = self.s();
                let Some(status) = status else {
                    ui.label(egui::RichText::new(lang::view(s.repo_no_git)).size(11.0).color(theme::pal().muted));
                    return;
                };
                if !status.git {
                    ui.label(egui::RichText::new(lang::view(s.repo_no_git)).size(11.0).color(theme::pal().muted));
                    return;
                }
                if !status.in_repo {
                    ui.label(egui::RichText::new(lang::view(s.repo_no_repo)).size(11.0).color(theme::pal().muted));
                    ui.add(egui::TextEdit::singleline(&mut self.repo_remote_name).hint_text(lang::view(s.repo_remote_name)).desired_width(120.0));
                    ui.add(egui::TextEdit::singleline(&mut self.repo_remote_url).hint_text(lang::view(s.repo_remote_url)).desired_width(390.0));
                    if ui.button(lang::view(s.repo_init)).clicked() {
                        let mut params = serde_json::json!({});
                        if !self.repo_remote_url.trim().is_empty() {
                            let name = if self.repo_remote_name.trim().is_empty() { "origin" } else { self.repo_remote_name.trim() };
                            params["remote"] = self.repo_remote_url.trim().into();
                            params["remote_name"] = name.into();
                        }
                        action = Some(("repo.init", params));
                    }
                    return;
                }

                egui::ScrollArea::vertical().max_height(520.0).show(ui, |ui| {
                    ui.label(egui::RichText::new(lang::fmt(s.repo_branch, &[("name", &status.branch)])).size(11.0));
                    if !status.upstream.is_empty() {
                        ui.label(egui::RichText::new(lang::fmt(s.repo_upstream, &[("name", &status.upstream)])).size(11.0));
                    }
                    if !status.remote_url.is_empty() {
                        ui.label(egui::RichText::new(lang::fmt(s.repo_remote, &[("url", &status.remote_url)])).size(10.5).monospace());
                    }
                    ui.label(egui::RichText::new(lang::fmt(s.repo_changes, &[("n", &status.changed.to_string())])).size(11.0));
                    if status.conflicts > 0 {
                        ui.label(
                            egui::RichText::new(lang::fmt(s.repo_conflicts, &[("n", &status.conflicts.to_string())]))
                                .size(11.0)
                                .color(theme::pal().error),
                        );
                    }
                    if status.ahead > 0 {
                        ui.label(egui::RichText::new(lang::fmt(s.repo_ahead, &[("n", &status.ahead.to_string())])).size(11.0));
                    }
                    if status.behind > 0 {
                        ui.label(
                            egui::RichText::new(lang::fmt(s.repo_behind, &[("n", &status.behind.to_string())])).size(11.0).color(theme::pal().warn),
                        );
                    }
                    if status.clean {
                        ui.label(egui::RichText::new(lang::view(s.repo_clean)).size(10.5).color(theme::pal().accent_dim));
                    }

                    if status.remote_url.is_empty() {
                        ui.add_space(6.0);
                        ui.label(egui::RichText::new(lang::view(s.repo_no_remote)).size(10.5).color(theme::pal().muted));
                    }
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut self.repo_remote_name).hint_text(lang::view(s.repo_remote_name)).desired_width(105.0));
                        ui.add(egui::TextEdit::singleline(&mut self.repo_remote_url).hint_text(lang::view(s.repo_remote_url)).desired_width(300.0));
                        if ui.add_enabled(!self.repo_remote_url.trim().is_empty(), egui::Button::new(lang::view(s.repo_connect))).clicked() {
                            let name = if self.repo_remote_name.trim().is_empty() { "origin" } else { self.repo_remote_name.trim() };
                            action = Some(("repo.remote", serde_json::json!({"name": name, "url": self.repo_remote_url.trim()})));
                        }
                    });

                    ui.add_space(8.0);
                    ui.horizontal_wrapped(|ui| {
                        if ui.button(lang::view(s.repo_refresh)).clicked() {
                            action = Some(("repo.status", serde_json::json!({})));
                        }
                        if ui.add_enabled(status.conflicts == 0, egui::Button::new(lang::view(s.repo_pull))).clicked() {
                            action = Some(("repo.pull", serde_json::json!({"strategy": if self.repo_pull_rebase { "rebase" } else { "ff-only" }})));
                        }
                        if ui.add_enabled(status.conflicts == 0, egui::Button::new(lang::view(s.repo_push))).clicked() {
                            let params = if self.repo_push_upstream && !status.branch.is_empty() {
                                serde_json::json!({"remote": status.remote, "branch": status.branch, "set_upstream": true})
                            } else {
                                serde_json::json!({})
                            };
                            action = Some(("repo.push", params));
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        egui::ComboBox::from_id_salt("repo_pull_strategy")
                            .selected_text(lang::view(if self.repo_pull_rebase { s.repo_pull_rebase } else { s.repo_pull_ff }))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.repo_pull_rebase, false, lang::view(s.repo_pull_ff));
                                ui.selectable_value(&mut self.repo_pull_rebase, true, lang::view(s.repo_pull_rebase));
                            });
                        ui.add_enabled(
                            !status.remote_url.is_empty(),
                            egui::Checkbox::new(&mut self.repo_push_upstream, lang::view(s.repo_set_upstream)),
                        );
                    });

                    egui::CollapsingHeader::new(lang::view(s.repo_branches_title)).default_open(true).show(ui, |ui| {
                        egui::ComboBox::from_id_salt("repo_branch_picker").selected_text(lang::view(&status.branch)).show_ui(ui, |ui| {
                            for branch in &status.branches {
                                let label =
                                    if branch.upstream.is_empty() { branch.name.clone() } else { format!("{} — {}", branch.name, branch.upstream) };
                                if ui.selectable_label(branch.current, lang::view(&label)).clicked() && !branch.current {
                                    action = Some(("repo.branches", serde_json::json!({"switch": branch.name})));
                                }
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.repo_branch_new).hint_text(lang::view(s.repo_new_branch)).desired_width(210.0),
                            );
                            if ui.add_enabled(!self.repo_branch_new.trim().is_empty(), egui::Button::new(lang::view(s.repo_create_branch))).clicked()
                            {
                                action = Some(("repo.branches", serde_json::json!({"create": self.repo_branch_new.trim()})));
                                self.repo_branch_new.clear();
                            }
                        });
                    });

                    egui::CollapsingHeader::new(lang::view(s.repo_changes_title)).default_open(status.conflicts == 0).show(ui, |ui| {
                        egui::ScrollArea::vertical().max_height(96.0).show(ui, |ui| {
                            for change in &status.changes {
                                ui.label(egui::RichText::new(format!("{} {}", change.status, change.path)).size(10.0).monospace());
                            }
                        });
                    });
                    if !status.conflict_paths.is_empty() {
                        egui::CollapsingHeader::new(lang::view(s.repo_conflicts_title)).default_open(true).show(ui, |ui| {
                            for path in &status.conflict_paths {
                                ui.label(egui::RichText::new(path).size(10.0).monospace().color(theme::pal().error));
                            }
                        });
                    }
                    egui::CollapsingHeader::new(lang::view(s.repo_history_title)).default_open(false).show(ui, |ui| {
                        for entry in &status.history {
                            ui.label(
                                egui::RichText::new(format!("{} {} {} {}", entry.commit, entry.date, entry.author, entry.subject))
                                    .size(10.0)
                                    .monospace(),
                            );
                        }
                    });

                    ui.add_space(6.0);
                    ui.checkbox(&mut self.repo_stage_all, lang::view(s.repo_stage_all));
                    let commit_edit =
                        ui.add(egui::TextEdit::singleline(&mut self.repo_commit_msg).hint_text(lang::view(s.repo_commit_hint)).desired_width(430.0));
                    if (ui
                        .add_enabled(status.conflicts == 0 && !self.repo_commit_msg.trim().is_empty(), egui::Button::new(lang::view(s.repo_commit)))
                        .clicked()
                        || (commit_edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.repo_commit_msg.trim().is_empty()))
                        && status.conflicts == 0
                    {
                        commit_msg = self.repo_commit_msg.trim().to_string();
                        action = Some(("repo.commit", serde_json::json!({"message": commit_msg, "stage": self.repo_stage_all})));
                    }
                });
            });
        self.repo_open = open;
        if let Some((command, params)) = action {
            if command != "repo.status" {
                self.repo_open = false;
            }
            self.start_repo_command(command, params);
        }
        if !commit_msg.is_empty() {
            self.repo_commit_msg.clear();
        }
    }

    /// Conflict choice for a file changed on disk. The user may take the
    /// disk copy, write the current buffer over it, or keep editing with
    /// the warning chip still visible — never an automatic overwrite.
    #[cfg(not(target_arch = "wasm32"))]
    fn external_modal(&mut self, ctx: &egui::Context) {
        if !self.external_modal {
            return;
        }
        enum Pick {
            None,
            Reload,
            Overwrite,
            Keep,
        }
        let mut pick = Pick::None;
        let mut open = self.external_modal;
        egui::Window::new(lang::view(self.s().external_title))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .open(&mut open)
            .show(ctx, |ui| {
                let s = self.s();
                let body = if self.session.dirty() { s.external_dirty } else { s.external_clean };
                ui.label(egui::RichText::new(lang::view(body)).size(11.0));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(lang::view(s.reload)).clicked() {
                        pick = Pick::Reload;
                    }
                    if ui.add_enabled(self.session.dirty() || self.external_changed, egui::Button::new(lang::view(s.overwrite))).clicked() {
                        pick = Pick::Overwrite;
                    }
                    if ui.button(lang::view(s.keep_editing)).clicked() {
                        pick = Pick::Keep;
                    }
                });
            });
        self.external_modal = open;
        match pick {
            Pick::Reload => self.reload_current(),
            Pick::Overwrite => self.overwrite_current(),
            Pick::Keep | Pick::None => {}
        }
        if matches!(pick, Pick::Keep) {
            self.external_modal = false;
        }
    }

    /// Floating math preview: when the caret sits inside a math span,
    /// tint the span, compile the fragment on a worker, and paint the
    /// result in an egui::Area anchored bottom-right of the editor.
    fn draw_math_preview(&mut self, ui: &mut egui::Ui, out: &egui::text_edit::TextEditOutput, src: &str) {
        let Some(editor_id) = self.editor_id else { return };
        let ci = egui::text_edit::TextEditState::load(ui.ctx(), editor_id).and_then(|st| st.cursor.char_range()).map(|r| r.primary.index.0);
        let Some(ci) = ci else { return };
        let Some(latex) = mathprev::math_at(src, ci) else { return };
        self.mathprev.request(&latex);
        // Faint tint behind the previewed span — the "this is what's
        // being rendered" cue. `math_span` is byte offsets; galley rows
        // count chars, so convert through the char cursor.
        if let Some((bs, be)) = mathprev::math_span(src, ci) {
            let (cs, ce) =
                (src.get(..bs).map(str::chars).map(Iterator::count).unwrap_or(0), src.get(..be).map(str::chars).map(Iterator::count).unwrap_or(0));
            let painter = ui.painter().with_clip_rect(out.response.rect);
            let fill = theme::pal().accent.gamma_multiply(0.10);
            let mut row_start = 0usize;
            for placed in &out.galley.rows {
                let row_end = row_start + placed.row.glyphs.len();
                if ce > row_start && cs < row_end {
                    let lo = cs.max(row_start) - row_start;
                    let hi = ce.min(row_end) - row_start;
                    if let (Some(g0), Some(g1)) = (placed.row.glyphs.get(lo), placed.row.glyphs.get(hi.saturating_sub(1))) {
                        let r = egui::Rect::from_min_max(
                            egui::pos2(out.galley_pos.x + placed.pos.x + g0.pos.x, out.galley_pos.y + placed.pos.y),
                            egui::pos2(
                                out.galley_pos.x + placed.pos.x + g1.pos.x + g1.advance_width,
                                out.galley_pos.y + placed.pos.y + placed.row.size.y,
                            ),
                        );
                        painter.rect_filled(r, 2.0, fill);
                    }
                }
                row_start = row_end + usize::from(placed.ends_with_newline);
            }
        }
        egui::Area::new(egui::Id::new("math_preview"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::RIGHT_BOTTOM)
            .fixed_pos(ui.max_rect().right_bottom() - egui::vec2(12.0, 12.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(theme::pal().top_bg)
                    .stroke(egui::Stroke::new(1.0, theme::pal().border))
                    .corner_radius(6.0)
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new(format!("${latex}$")).size(10.0).color(theme::pal().muted).monospace());
                        if let Some(tex) = self.mathprev.texture(&latex) {
                            let size = tex.size_vec2();
                            ui.image((tex.id(), size));
                        } else if let Some(err) = self.mathprev.error(&latex) {
                            ui.label(egui::RichText::new(err).size(10.0).color(theme::pal().error));
                        } else {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label(egui::RichText::new(lang::view(self.s().math_rendering)).size(10.0).color(theme::pal().muted));
                            });
                        }
                    });
            });
    }
}

/// Decode the bounded `repo.status` payload into the UI's display shape.
/// Missing fields default to the safest value (not a repo / no counts).
#[cfg(not(target_arch = "wasm32"))]
fn repo_status_from_json(v: &serde_json::Value) -> RepoStatus {
    let n = |key: &str| v.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
    let s = |key: &str| v.get(key).and_then(serde_json::Value::as_str).unwrap_or_default().to_string();
    let rows = |key: &str| v.get(key).and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
    RepoStatus {
        git: v.get("git").and_then(serde_json::Value::as_bool).unwrap_or(false),
        in_repo: v.get("in_repo").and_then(serde_json::Value::as_bool).unwrap_or(false),
        branch: s("branch"),
        upstream: s("upstream"),
        remote: s("remote"),
        remote_url: s("remote_url"),
        ahead: n("ahead"),
        behind: n("behind"),
        changed: n("changed"),
        conflicts: v.get("conflicts").and_then(serde_json::Value::as_array).map_or(0, |a| a.len() as u64),
        clean: v.get("clean").and_then(serde_json::Value::as_bool).unwrap_or(false),
        conflict_paths: rows("conflicts").iter().filter_map(|p| p.as_str().map(str::to_string)).collect(),
        changes: rows("changes")
            .iter()
            .map(|r| RepoChange {
                status: r.get("status").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                path: r.get("path").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
            })
            .collect(),
        branches: rows("branches")
            .iter()
            .map(|b| RepoBranch {
                name: b.get("name").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                current: b.get("current").and_then(serde_json::Value::as_bool).unwrap_or(false),
                upstream: b.get("upstream").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
            })
            .collect(),
        history: rows("history")
            .iter()
            .map(|h| RepoCommit {
                commit: h.get("commit").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                date: h.get("date").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                author: h.get("author").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
                subject: h.get("subject").and_then(serde_json::Value::as_str).unwrap_or_default().to_string(),
            })
            .collect(),
    }
}

/// F12 → capture the window to `warraqcraft-shot.png` in the cwd (this is
/// also the future `ui.screenshot` control-channel implementation).
#[cfg(not(target_arch = "wasm32"))]
fn handle_screenshot(ctx: &egui::Context) {
    use egui::{Event, ViewportCommand};
    if ctx.input(|i| i.key_pressed(egui::Key::F12)) {
        ctx.send_viewport_cmd(ViewportCommand::Screenshot(Default::default()));
    }
    let shots: Vec<egui::ColorImage> = ctx.input(|i| {
        i.events
            .iter()
            .filter_map(|e| match e {
                Event::Screenshot { image, .. } => Some(image.as_ref().clone()),
                _ => None,
            })
            .collect()
    });
    for img in shots {
        let path = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join("warraqcraft-shot.png");
        let [w, h] = img.size;
        if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec()) {
            let _ = buf.save(&path);
            eprintln!("screenshot -> {}", path.display());
        }
    }
}

/// 0-based line of `char_idx` (a `CCursor` char index) in `src`.
fn line_of_char_index(src: &str, char_idx: usize) -> usize {
    src.chars().take(char_idx).filter(|c| *c == '\n').count()
}

/// 1-based line of byte `off` in `src` (clamps a non-boundary offset).
fn line_of_byte_offset(src: &str, off: usize) -> usize {
    let mut end = off.min(src.len());
    while end > 0 && !src.is_char_boundary(end) {
        end -= 1;
    }
    src.get(..end).map(|s| s.bytes().filter(|b| *b == b'\n').count()).unwrap_or(0) + 1
}

/// No screenshots on the web build.
#[cfg(target_arch = "wasm32")]
fn handle_screenshot(_ctx: &egui::Context) {}

/// Byte offset of the `ci`-th char in `src` (clamped to `src.len()`).
fn byte_of_char(src: &str, ci: usize) -> usize {
    src.char_indices().nth(ci).map(|(b, _)| b).unwrap_or(src.len())
}

/// Diff `prev`→`cur` as a single-point insert or delete at the primary
/// caret and replicate it into every mirror — returns the replicated
/// source plus the adjusted ranges, or `None` when the edit isn't a
/// pure point edit (selection-replace, multi-cursor paste, undo…).
/// The caller drops the mirrors in that case.
fn replicate_mirror_edit(prev: &str, cur: &str, m: &mirrors::Mirrors) -> Option<(String, mirrors::Mirrors)> {
    let a: Vec<char> = prev.chars().collect();
    let b: Vec<char> = cur.chars().collect();
    let mut pre = 0usize;
    while pre < a.len().min(b.len()) && a[pre] == b[pre] {
        pre += 1;
    }
    let mut suf = 0usize;
    while suf < a.len().min(b.len()).saturating_sub(pre) && a[a.len() - 1 - suf] == b[b.len() - 1 - suf] {
        suf += 1;
    }
    let deleted = a.len() - pre - suf;
    let inserted: String = b[pre..b.len() - suf].iter().collect();
    if deleted == 0 && !inserted.is_empty() {
        Some(mirrors::insert_replicated(prev, m, pre, &inserted))
    } else if inserted.is_empty() && deleted > 0 {
        Some(mirrors::delete_replicated(prev, m, pre + deleted, deleted))
    } else {
        None
    }
}

/// Char index of (1-based `line`, 1-based char `col`) in `src` — the
/// inverse of `track_cursor`'s index→Ln/Col walk.
/// Zoom presets in the toolbar combo, ascending — same ladder PDF
/// viewers use.
const ZOOM_PRESETS: &[f32] = &[0.5, 0.75, 1.0, 1.25, 1.5, 2.0, 3.0, 4.0];

/// Key bindings the shortcuts window lists — (group, [(keys, action)]).
/// Keep in sync with `editor_shortcuts`, `ui`'s global keys, and the
/// editor gestures (completion, SyncTeX click).
const SHORTCUTS: &[(&str, &[(&str, usize)])] = &[
    ("file", &[("Ctrl+S", 0), ("print button (toolbar)", 1), ("F12", 2)]),
    (
        "editing",
        &[
            ("Ctrl+Z / Ctrl+Shift+Z", 3),
            ("Ctrl+D", 4),
            ("Alt+↑ / Alt+↓", 5),
            ("Ctrl+/", 6),
            ("Ctrl+Shift+K", 7),
            ("Ctrl+Shift+Backspace", 8),
            ("\\…", 9),
            ("\\begin{ / \\end{", 10),
            ("Ctrl+M (with selection)", 11),
            ("Esc", 12),
        ],
    ),
    ("navigation", &[("Ctrl+click in source", 13), ("click in PDF", 14), ("Ctrl+Shift+C", 15), ("‹ › (toolbar)", 16), ("Ctrl+scroll over PDF", 17)]),
    ("help", &[("F1", 18)]),
];

/// How many spreads `n` pages make in `mode`. Single/continuous treat
/// each page as its own spread; TwoUp pairs [0,1],[2,3],…; TwoUpCover
/// keeps page 0 solo then pairs [1,2],[3,4],….
fn spread_count(mode: PageMode, n: usize) -> usize {
    match mode {
        PageMode::Scroll | PageMode::Single => n,
        PageMode::TwoUp => n.div_ceil(2),
        PageMode::TwoUpCover => 1 + n / 2,
    }
}

/// Which spread `page` belongs to in `mode`.
fn spread_of_page(mode: PageMode, page: usize) -> usize {
    match mode {
        PageMode::Scroll | PageMode::Single => page,
        PageMode::TwoUp => page / 2,
        PageMode::TwoUpCover => {
            if page == 0 {
                0
            } else {
                page.div_ceil(2)
            }
        }
    }
}

/// First page of spread `s` in `mode`.
fn spread_first(mode: PageMode, s: usize) -> usize {
    match mode {
        PageMode::Scroll | PageMode::Single => s,
        PageMode::TwoUp => s * 2,
        PageMode::TwoUpCover => {
            if s == 0 {
                0
            } else {
                2 * s - 1
            }
        }
    }
}

/// Pages in the spread starting at `first`, clamped to `n` — `[first]`
/// or `[first, first+1]` (empty if `first` is out of range). Single/
/// continuous spreads and the cover page are always solo.
fn spread_pages(mode: PageMode, first: usize, n: usize) -> Vec<usize> {
    if first >= n {
        return Vec::new();
    }
    let solo = matches!(mode, PageMode::Scroll | PageMode::Single) || (mode == PageMode::TwoUpCover && first == 0);
    if !solo && first + 1 < n {
        vec![first, first + 1]
    } else {
        vec![first]
    }
}

/// A `texpkg` catalog entry → popup item: signature text, `package —
/// doc` meta line, and the precomputed insertion (`\name`/`{name}`, with
/// `{}` added for required-arg commands and the caret parked inside).
fn catalog_item(c: &'static texpkg::CommandSpec, env: bool) -> CompletionItem {
    let sig = c
        .args
        .iter()
        .map(|a| match a.kind {
            texpkg::ArgKind::Opt | texpkg::ArgKind::Key => format!("[{}]", a.name),
            _ => format!("{{{}}}", a.name),
        })
        .collect::<String>();
    let req = !env && c.args.iter().any(|a| matches!(a.kind, texpkg::ArgKind::Req));
    CompletionItem {
        name: c.name.to_string(),
        sig,
        meta: format!("{} — {}", c.package, c.doc),
        insert: if env { format!("{{{}}}", c.name) } else { format!("\\{}{}", c.name, if req { "{}" } else { "" }) },
        caret_back: usize::from(req),
    }
}

fn char_index_of_lc(src: &str, line: usize, col: usize) -> usize {
    let mut idx = 0usize;
    for (i, l) in src.split('\n').enumerate() {
        if i + 1 == line {
            return idx + col.saturating_sub(1).min(l.chars().count());
        }
        idx += l.chars().count() + 1;
    }
    src.chars().count()
}

/// Byte offset of (1-based `line`, 1-based byte `col`) in `src`.
/// Input-derived values: clamps instead of panicking.
fn offset_of_line_col(src: &str, line: usize, col: usize) -> usize {
    let mut off = 0usize;
    for (i, l) in src.split('\n').enumerate() {
        if i + 1 == line {
            return (off + col.saturating_sub(1)).min(off + l.len());
        }
        off = off.saturating_add(l.len()).saturating_add(1);
    }
    src.len()
}

/// A toolbar button whose glyph is painted, not font-rendered — the
/// bundled fonts lack most symbol ranges, so Unicode icons show as
/// squares. `paint` draws inside a `side`×`side` rect left of `label`.
fn icon_button(ui: &mut egui::Ui, side: f32, paint: fn(&egui::Painter, egui::Rect, egui::Color32), label: &str, enabled: bool) -> egui::Response {
    let text_w = if label.is_empty() {
        0.0
    } else {
        ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(11.5), theme::pal().text).size().x
    };
    let w = side + if label.is_empty() { 10.0 } else { text_w + 14.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, side + 8.0), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let ink = if !enabled {
        theme::pal().faint
    } else if resp.hovered() {
        theme::pal().text
    } else {
        theme::pal().muted
    };
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, theme::pal().hovered);
    }
    paint(ui.painter(), egui::Rect::from_center_size(rect.left_center() + egui::vec2(side / 2.0 + 4.0, 0.0), egui::vec2(side, side)), ink);
    if !label.is_empty() {
        ui.painter().text(
            egui::pos2(rect.left() + side + 9.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.5),
            ink,
        );
    }
    resp
}

/// Icon-only button on a filled pill — the Recompile/dropdown style.
fn filled_icon_button(
    ui: &mut egui::Ui,
    side: f32,
    paint: fn(&egui::Painter, egui::Rect, egui::Color32),
    label: &str,
    fill: egui::Color32,
    enabled: bool,
) -> egui::Response {
    let text_w = if label.is_empty() {
        0.0
    } else {
        ui.painter().layout_no_wrap(label.to_string(), egui::FontId::proportional(11.5), egui::Color32::WHITE).size().x
    };
    let w = side + if label.is_empty() { 12.0 } else { text_w + 16.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 22.0), if enabled { egui::Sense::click() } else { egui::Sense::hover() });
    let bg = if !enabled {
        fill.gamma_multiply(0.45)
    } else if resp.hovered() {
        fill
    } else {
        fill.gamma_multiply(0.85)
    };
    ui.painter().rect_filled(rect, 4.0, bg);
    let ink = egui::Color32::WHITE;
    paint(ui.painter(), egui::Rect::from_center_size(rect.left_center() + egui::vec2(side / 2.0 + 6.0, 0.0), egui::vec2(side, side)), ink);
    if !label.is_empty() {
        ui.painter().text(
            egui::pos2(rect.left() + side + 10.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.5),
            ink,
        );
    }
    resp
}

/// Painted light/dark toggle — a sun in light mode, a crescent in dark
/// (drawn geometry, so no icon font is needed).
fn theme_toggle(ui: &mut egui::Ui, s: &lang::Strings) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(22.0, 20.0), egui::Sense::click());
    let resp = resp.on_hover_text(lang::view(if theme::is_dark() { s.theme_to_light } else { s.theme_to_dark }).into_owned());
    let p = ui.painter();
    let c = rect.center();
    let ink = if resp.hovered() { theme::pal().text } else { theme::pal().muted };
    if theme::is_dark() {
        // Crescent: filled disc with the panel colour bitten out.
        p.circle_filled(c, 6.0, ink);
        p.circle_filled(c + egui::vec2(3.0, -2.0), 5.0, theme::pal().top_bg);
    } else {
        // Sun: stroked disc + 8 rays.
        p.circle_stroke(c, 4.5, egui::Stroke::new(1.4, ink));
        for i in 0..8 {
            let a = i as f32 * std::f32::consts::FRAC_PI_4;
            let dir = egui::vec2(a.cos(), a.sin());
            p.line_segment([c + dir * 6.2, c + dir * 8.6], egui::Stroke::new(1.4, ink));
        }
    }
    resp
}

/// The `pos`/`title` heading row colour, by level.
fn outline_color(level: u8) -> egui::Color32 {
    match level {
        0 | 1 => theme::pal().text,
        2 => theme::pal().accent,
        3 => theme::pal().warn,
        _ => theme::pal().muted,
    }
}

/// One file-tree row: chevron (dirs), kind icon, name, main-doc marker.
/// Returns the row response.
fn tree_row(ui: &mut egui::Ui, n: &files::Node, current: bool, is_main: bool, open: bool) -> egui::Response {
    let h = 20.0;
    let indent = n.depth as f32 * 14.0;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click());
    if current || resp.hovered() {
        ui.painter().rect_filled(rect, 0.0, if current { theme::pal().selected } else { theme::pal().hovered });
    }
    if current {
        ui.painter().rect_filled(egui::Rect::from_min_size(rect.min, egui::vec2(2.5, h)), 0.0, theme::pal().accent);
    }
    let mut x = rect.left() + 6.0 + indent;
    let painter = ui.painter();
    if n.kind == files::Kind::Dir {
        // chevron
        let cy = rect.center().y;
        let pts = if open {
            vec![egui::pos2(x, cy - 2.0), egui::pos2(x + 7.0, cy - 2.0), egui::pos2(x + 3.5, cy + 3.0)]
        } else {
            vec![egui::pos2(x, cy - 3.5), egui::pos2(x, cy + 3.5), egui::pos2(x + 6.0, cy)]
        };
        painter.add(egui::Shape::convex_polygon(pts, theme::pal().muted, egui::Stroke::NONE));
        x += 12.0;
        // folder glyph: tab + body
        let (fx, fy) = (x, cy - 4.5);
        painter.rect_filled(egui::Rect::from_min_size(egui::pos2(fx, fy), egui::vec2(5.0, 3.0)), 1.0, theme::pal().warn);
        painter.rect_filled(egui::Rect::from_min_size(egui::pos2(fx, fy + 2.0), egui::vec2(11.0, 8.0)), 1.0, theme::pal().warn);
        x += 15.0;
    } else {
        x += 4.0;
        // document glyph: page with folded corner
        let (fx, fy) = (x, rect.center().y - 5.0);
        let col = match n.kind {
            files::Kind::Tex => theme::pal().syn_math,
            files::Kind::Bib => theme::pal().warn,
            files::Kind::Style => theme::pal().syn_cmd,
            files::Kind::Image => theme::pal().syn_brace,
            _ => theme::pal().muted,
        };
        let body = egui::Rect::from_min_size(egui::pos2(fx, fy), egui::vec2(9.0, 11.0));
        painter.rect_filled(body, 1.0, col);
        painter.rect_filled(egui::Rect::from_min_size(egui::pos2(fx + 6.0, fy), egui::vec2(3.0, 3.0)), 0.0, theme::pal().panel_bg);
        x += 13.0;
    }
    painter.text(
        egui::pos2(x, rect.center().y),
        egui::Align2::LEFT_CENTER,
        lang::view(&n.name),
        egui::FontId::proportional(11.5),
        if current { theme::pal().text } else { theme::pal().muted },
    );
    if is_main {
        painter.text(
            egui::pos2(rect.right() - 8.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "★",
            egui::FontId::proportional(10.0),
            theme::pal().accent,
        );
    }
    resp.on_hover_text(lang::view(&n.path.display().to_string()).into_owned())
}

/// Cheap main-document probe: does this `.tex` contain `\documentclass`?
/// Reads at most 64 KiB — enough for any sane preamble.
fn file_declares_document(path: &Path) -> bool {
    use std::io::Read;
    let mut buf = String::new();
    let Ok(f) = std::fs::File::open(path) else { return false };
    let mut take = f.take(65_536);
    if take.read_to_string(&mut buf).is_err() {
        return false;
    }
    buf.contains("\\documentclass")
}

/// Run a system-TeX compile of `path`, then rasterize every produced PDF
/// page into an `egui::ColorImage` with `hayro`.
#[cfg(not(target_arch = "wasm32"))]
fn compile_and_render(path: &Path, engine: Option<&str>) -> CompileJob {
    let report = warraqcraft_engine::compile_file_with(path, None, engine).map_err(|e| e.to_string())?;
    let Some(pdf_path) = &report.pdf else { return Ok((report, Vec::new())) };

    let data = std::fs::read(pdf_path).map_err(|e| format!("cannot read {}: {e}", pdf_path.display()))?;
    let pdf = hayro::hayro_syntax::Pdf::new(data).map_err(|e| format!("cannot parse PDF: {e:?}"))?;

    let cache = hayro::RenderCache::new();
    let interp = hayro::hayro_interpret::InterpreterSettings::default();
    let settings = hayro::RenderSettings::default();

    let mut images = Vec::new();
    for page in pdf.pages().iter().take(MAX_PREVIEW_PAGES) {
        let (w, h) = page.render_dimensions();
        let scale = PREVIEW_SCALE.min(MAX_PREVIEW_PX / w.max(h).max(1.0));
        let pixmap_settings = hayro::PixmapSettings { x_scale: scale, y_scale: scale, bg_color: hayro::vello_cpu::color::palette::css::WHITE };
        let pixmap = hayro::render(page, &cache, &interp, &settings, &pixmap_settings);
        images.push(Page {
            img: egui::ColorImage::from_rgba_premultiplied([usize::from(pixmap.width()), usize::from(pixmap.height())], pixmap.data_as_u8_slice()),
            pt: egui::vec2(w, h),
        });
    }
    Ok((report, images))
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// End-to-end: compile examples/minimal.tex via system TeX and
    /// rasterize the PDF. Skips silently when no TeX engine is installed.
    #[test]
    fn compile_and_render_minimal() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/minimal.tex");
        match compile_and_render(&path, None) {
            Ok((report, pages)) => {
                assert!(report.errors.is_empty(), "compile errors: {:?}", report.errors);
                assert!(!pages.is_empty(), "no pages rendered");
                assert!(pages.iter().all(|p| p.img.size[0] > 0 && p.img.size[1] > 0 && p.pt.x > 0.0 && p.pt.y > 0.0));
            }
            Err(e) => {
                assert!(e.contains("no TeX engine"), "unexpected failure: {e}");
            }
        }
    }

    /// Spread mapping invariants: every page is covered by exactly one
    /// spread, spreads tile the document in order, and round-tripping
    /// `spread_of_page` → `spread_first` stays inside the spread.
    #[test]
    fn spreads_cover_document() {
        for mode in [PageMode::TwoUp, PageMode::TwoUpCover] {
            for n in 0..=9usize {
                let count = spread_count(mode, n);
                let mut covered = Vec::new();
                for s in 0..count {
                    let first = spread_first(mode, s);
                    assert!(first < n || n == 0, "{mode:?} spread {s} of {n} starts past the end");
                    for p in spread_pages(mode, first, n) {
                        assert_eq!(spread_of_page(mode, p), s, "{mode:?} page {p} not in spread {s} (n={n})");
                        covered.push(p);
                    }
                }
                assert_eq!(covered, (0..n).collect::<Vec<_>>(), "{mode:?} spreads do not tile {n} pages");
            }
        }
    }

    /// TwoUpCover is book layout: page 0 alone, then verso/recto pairs.
    #[test]
    fn cover_spreads_are_book_layout() {
        let m = PageMode::TwoUpCover;
        assert_eq!(spread_count(m, 5), 3);
        assert_eq!(spread_pages(m, spread_first(m, 0), 5), vec![0]);
        assert_eq!(spread_pages(m, spread_first(m, 1), 5), vec![1, 2]);
        assert_eq!(spread_pages(m, spread_first(m, 2), 5), vec![3, 4]);
        assert_eq!(spread_of_page(m, 4), 2);
        // Even page count leaves a solo last page, still reachable.
        assert_eq!(spread_pages(m, spread_first(m, 2), 4), vec![3]);
    }

    /// TwoUp is plain pairs; Single/continuous are identity spreads.
    #[test]
    fn two_up_and_single_spreads() {
        assert_eq!(spread_count(PageMode::TwoUp, 5), 3);
        assert_eq!(spread_pages(PageMode::TwoUp, spread_first(PageMode::TwoUp, 2), 5), vec![4]);
        assert_eq!(spread_of_page(PageMode::TwoUp, 3), 1);
        for p in 0..4 {
            assert_eq!(spread_of_page(PageMode::Single, p), p);
            assert_eq!(spread_first(PageMode::Scroll, p), p);
        }
        assert_eq!(spread_count(PageMode::Scroll, 0), 0);
    }

    /// The mode round-trips through the persisted prefs string.
    #[test]
    fn page_mode_pref_roundtrip() {
        for m in PageMode::ALL {
            assert_eq!(PageMode::from_pref(m.pref()), m);
        }
        assert_eq!(PageMode::from_pref("nonsense"), PageMode::Scroll);
    }

    /// Gallery search covers localized metadata and respects the chip.
    #[test]
    fn template_filter_matches_metadata() {
        let card = TemplateCard {
            id: "arabdoc".into(),
            name: "Arabic document".into(),
            desc: "RTL starter".into(),
            lang: "ar".into(),
            category: "academic".into(),
            engine: "xelatex".into(),
            ..TemplateCard::default()
        };
        assert!(WarraqApp::template_card_matches(&card, "rtl", None, None));
        assert!(WarraqApp::template_card_matches(&card, "xelatex", Some("academic"), Some("ar")));
        assert!(!WarraqApp::template_card_matches(&card, "rtl", Some("legal"), None));
        assert!(!WarraqApp::template_card_matches(&card, "rtl", None, Some("fa")));
        assert!(!WarraqApp::template_card_matches(&card, "missing", None, None));
    }

    /// The status chip consumes the engine JSON shape, not raw Git text.
    #[test]
    fn repo_status_decoder_uses_engine_fields() {
        let st = repo_status_from_json(&serde_json::json!({
            "git": true,
            "in_repo": true,
            "branch": "main",
            "upstream": "origin/main",
            "remote": "origin",
            "remote_url": "https://***@example/repo.git",
            "ahead": 2,
            "behind": 1,
            "changed": 3,
            "conflicts": ["a.tex"],
            "changes": [{"status": "M ", "path": "a.tex"}],
            "branches": [{"name": "main", "current": true, "upstream": "origin/main"}],
            "history": [{"commit": "abc123", "date": "2026-01-01", "author": "A", "subject": "start"}],
            "clean": false
        }));
        assert_eq!((st.branch.as_str(), st.ahead, st.behind, st.changed, st.conflicts), ("main", 2, 1, 3, 1));
        assert_eq!((st.remote.as_str(), st.conflict_paths.len(), st.changes.len(), st.branches.len(), st.history.len()), ("origin", 1, 1, 1, 1));
        assert!(!st.clean);
        assert!(!repo_status_from_json(&serde_json::json!({})).in_repo);
    }
}

impl eframe::App for WarraqApp {
    /// Non-painting work — runs every frame, also while the window is
    /// minimized or hidden (a finished compile still drains there).
    /// `craft_quit::on_close_request` stays last: it quits through the
    /// platform's own path where closing the window directly is unsafe
    /// (macOS).
    #[cfg_attr(not(target_arch = "wasm32"), allow(clippy::disallowed_methods))] // Instant::now for prefs debounce
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        #[cfg(not(target_arch = "wasm32"))]
        {
            if let Some(c) = &self.control {
                c.poll(&mut self.session);
            }
            self.poll_compile(ctx);
            self.poll_tool(ctx);
            self.poll_print();
            self.refresh_repo(ctx);
            self.poll_repo(ctx);
            self.check_external_file(ctx);
            self.tick_autocompile(ctx);
            // Write prefs when the debounce has passed — wasm's store is
            // memory-only and Instant isn't readable there anyway.
            self.settings.tick(std::time::Instant::now());
        }
        self.refresh_tree(ctx);
        self.refresh_project(ctx);
        self.mathprev.poll(ctx);
        self.ai.poll();
        // Mirror UI state into prefs each frame — `update` no-ops when
        // nothing changed; `tick` writes the file after its debounce.
        let engine = self.engine_sel.and_then(|i| self.engines.get(i).cloned()).unwrap_or_default();
        let (dark, auto, zoom, outline, logs, spell, pm) =
            (theme::is_dark(), self.autocompile, self.zoom.pref(), self.outline_open, self.show_logs, self.spell.enabled(), self.page_mode.pref());
        let (ai_ep, ai_model, ai_provider) = (self.ai.endpoint.clone(), self.ai.model.clone(), self.ai.provider.key());
        let last_file = self.session.path().map(|p| p.display().to_string()).unwrap_or_default();
        let autosave_source = if self.session.dirty() { self.session.source().to_string() } else { String::new() };
        let (autosave_line, autosave_col) = self.cursor_lc;
        let autosave_dirty = self.session.dirty() && !autosave_source.is_empty();
        self.settings.update(|p| {
            p.dark = dark;
            p.autocompile = auto;
            p.engine = engine.clone();
            p.zoom = zoom.clone();
            p.outline = outline;
            p.logs = logs;
            p.spellcheck = spell;
            p.page_mode = pm.into();
            p.ai_endpoint = ai_ep.clone();
            p.ai_model = ai_model.clone();
            p.ai_provider = ai_provider.into();
            p.last_file = last_file.clone();
            p.autosave_source = autosave_source.clone();
            p.autosave_dirty = autosave_dirty;
            p.autosave_line = autosave_line;
            p.autosave_col = autosave_col;
        });
        craft_quit::on_close_request(ctx);
    }

    fn on_exit(&mut self) {
        let _ = self.settings.flush();
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if ctx.input(|i| i.key_pressed(egui::Key::S) && i.modifiers.ctrl) {
            self.save();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::C) && i.modifiers.ctrl && i.modifiers.shift) && !self.cite.is_open() {
            self.open_cite(&ctx);
        }
        if ctx.input(|i| i.key_pressed(egui::Key::F1)) {
            self.shortcuts_open = !self.shortcuts_open;
        }
        self.editor_shortcuts(&ctx);
        handle_screenshot(&ctx);

        self.top_bar(ui);
        self.sidebar(ui);
        self.logs_panel(ui);
        self.preview_pane(ui);
        self.status_bar(ui);
        self.editor_pane(ui);
        self.new_entry_modal(&ctx);
        self.stats_modal(&ctx);
        self.settings_modal(&ctx);
        self.symbols_modal(&ctx);
        self.shortcuts_modal(&ctx);
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.tpl_modal(&ctx);
            self.hist_modal(&ctx);
            self.ai_modal(&ctx);
            self.repo_modal(&ctx);
            self.external_modal(&ctx);
        }
        if let Some(key) = self.cite.show(&ctx, theme::pal(), self.s()) {
            let now = ctx.input(|i| i.time);
            self.insert_cite(&key, now);
        }
    }
}
