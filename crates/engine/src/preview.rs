//! `math.preview`: TeXstudio-style "preview selected math". Two paths:
//!
//! * **Native** — when the fragment stays inside `typeset`'s math
//!   subset (letters, digits, common symbols, scripts, `\frac`,
//!   `\sqrt`) and a system serif TTF is found, the fragment is
//!   typeset in-process and written as a real PDF through `texpdf`:
//!   no external engine, instant, offline-safe. The response carries
//!   `"native": true`.
//! * **External** — anything else wraps in a minimal `article`
//!   preamble and goes through [`compile_file`] exactly as
//!   `document.compile` does.
//!
//! Both paths feed the same content-keyed warm cache, so repeat
//! fragments skip their engine entirely.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{compile_file, compile_file_with, CompileError, CompileReport, EngineError, Session};

/// Largest accepted fragment — previews are small by definition.
const MAX_LATEX: usize = 64 * 1024;
/// `log` in the response: at most the last this-many characters of
/// the report's log tail.
const MAX_LOG_CHARS: usize = 1200;
/// Preview scratch dir under the document's `.warraqcraft/` build dir.
const PREVIEW_DIR: &str = "preview";

/// Minimal `article` preamble around the fragment. Fragments that
/// already carry their own math/environment delimiters (`$`, `\(`,
/// `\[`, `\begin{`) pass through verbatim; anything else is wrapped
/// in `\( … \)`. The preamble occupies the leading lines, so a
/// hostile fragment cannot smuggle `% !TeX` magic comments —
/// `parse_magic` only reads the leading comment block.
fn wrap_fragment(latex: &str) -> String {
    let t = latex.trim();
    let verbatim = t.starts_with('$') || t.starts_with("\\(") || t.starts_with("\\[") || t.starts_with("\\begin{");
    let body = if verbatim { t.to_string() } else { format!("\\( {t} \\)") };
    format!("\\documentclass{{article}}\n\\usepackage{{amsmath,amssymb}}\n\\pagestyle{{empty}}\n\\begin{{document}}\n{body}\n\\end{{document}}\n")
}

/// The open document's directory, or the process cwd for an untitled
/// session (history.rs's `project_root` parent-dir logic with a cwd
/// fallback instead of `NoFile`).
fn doc_dir(session: &Session) -> PathBuf {
    match session.path() {
        Some(p) => p.parent().unwrap_or_else(|| Path::new(".")).to_path_buf(),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

/// The last ≤ `max` characters of `s`, cut on a char boundary.
fn char_tail(s: &str, max: usize) -> String {
    s.chars().skip(s.chars().count().saturating_sub(max)).collect()
}

/// Warm-preview cache cap — each distinct fragment gets its own scratch
/// name; oldest entries beyond this are pruned.
const MAX_WARM: usize = 32;

/// FNV-1a 64-bit — enough for a per-fragment cache key; no hash dep.
fn frag_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100000001b3))
}

/// Prune stale warm entries: keep the newest `MAX_WARM` `*.ok` marks and
/// their scratch/artifacts, delete the rest.
fn prune_warm(dir: &Path) {
    let Ok(read) = std::fs::read_dir(dir) else { return };
    let mut oks: Vec<PathBuf> = read.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "ok")).collect();
    if oks.len() <= MAX_WARM {
        return;
    }
    oks.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    for old in oks.iter().take(oks.len() - MAX_WARM) {
        let stem = old.file_stem().and_then(|s| s.to_str()).unwrap_or("").to_string();
        let _ = std::fs::remove_file(old);
        let _ = std::fs::remove_file(dir.join(format!("{stem}.tex")));
        let _ = std::fs::remove_file(dir.join(crate::BUILD_DIR).join(format!("{stem}.pdf")));
    }
}

/// `math.preview {"latex": "<fragment>"}` →
/// `{"pdf": "<abs path>"|null, "pages": n, "log": "<tail>",
///   "errors": [{"line","message"}], "cached": bool}` — the error list
/// shares `document.compile`'s shape. `BadParam("latex")` on a missing,
/// empty or >64KiB fragment; `Io`/`NoTexEngine` on compile failure.
///
/// Warm path: the scratch name is the fragment's content hash, so a
/// fragment already compiled this session returns its PDF without
/// invoking an engine at all.
pub fn run(session: &Session, params: &Value) -> Result<Value, EngineError> {
    let latex = params.get("latex").and_then(Value::as_str).filter(|s| s.len() <= MAX_LATEX).ok_or(EngineError::BadParam("latex"))?;
    if latex.trim().is_empty() {
        return Err(EngineError::BadParam("latex"));
    }
    let src = wrap_fragment(latex);
    let dir = doc_dir(session).join(crate::BUILD_DIR).join(PREVIEW_DIR);
    std::fs::create_dir_all(&dir)?;
    let h = frag_hash(src.as_bytes());
    let scratch = dir.join(format!("p{h:016x}.tex"));
    let okmark = dir.join(format!("p{h:016x}.ok"));
    let built = dir.join(crate::BUILD_DIR).join(format!("p{h:016x}.pdf"));

    // Same fragment, already compiled → serve the cached PDF.
    if okmark.is_file() && built.is_file() {
        let pages = std::fs::read_to_string(&okmark).ok().and_then(|t| t.trim().parse::<u32>().ok());
        return Ok(json!({"pdf": built.display().to_string(), "pages": pages, "cached": true, "errors": [], "log": ""}));
    }

    // Native path — the subset typesetter handles it without an engine.
    if let Some(bytes) = native_pdf(latex) {
        std::fs::create_dir_all(built.parent().unwrap_or(&dir))?;
        craft_atomic::atomic_write(&built, &bytes)?;
        let _ = craft_atomic::atomic_write(&okmark, b"1");
        prune_warm(&dir);
        return Ok(json!({"pdf": built.display().to_string(), "pages": 1, "cached": false, "native": true, "errors": [], "log": ""}));
    }

    craft_atomic::atomic_write(&scratch, src.as_bytes())?;

    // `compile_file` builds into `<scratch-dir>/.warraqcraft/` — still
    // under the build dir.
    let report = compile_file(&scratch, Some(&src))?;
    if report.pdf.is_some() {
        // Mark the fragment good — failures never poison the cache.
        let _ = craft_atomic::atomic_write(&okmark, report.pages.unwrap_or(1).to_string().as_bytes());
        prune_warm(&dir);
    }
    Ok(json!({
        "pdf": report.pdf.as_ref().map(|p| p.display().to_string()),
        "pages": report.pages,
        "cached": false,
        "native": false,
        "log": char_tail(&report.log_tail, MAX_LOG_CHARS),
        "errors": report.errors.iter().map(|e: &CompileError| json!({"line": e.line, "message": e.message})).collect::<Vec<_>>(),
    }))
}

/// `doc.preview {"latex": "<full source>"}` → same response shape as
/// `math.preview` plus `"pages": n`. When `latex` is omitted the open
/// document's file is read (bounded). The native path typesets the
/// document body in-process when it stays inside `typeset`'s text
/// subset (paragraphs, `\section` headings, styled groups, `$…$`
/// inline math) — anything else falls back to a real compile of the
/// document, so callers always get a PDF or an honest error.
pub fn doc_preview(session: &Session, params: &Value) -> Result<Value, EngineError> {
    const MAX_SRC: usize = 512 * 1024;
    // File-backed documents take the shared pipeline — the same one
    // the UI's preview pane calls — so provenance and cache agree.
    if params.get("latex").is_none() {
        if let Some(p) = session.path() {
            let engine = params.get("engine").and_then(Value::as_str);
            let (report, native, cached) = preview_file(p, engine)?;
            return Ok(json!({
                "pdf": report.pdf.as_ref().map(|p| p.display().to_string()),
                "pages": report.pages,
                "cached": cached,
                "native": native,
                "log": char_tail(&report.log_tail, MAX_LOG_CHARS),
                "errors": report.errors.iter().map(|e: &CompileError| json!({"line": e.line, "message": e.message})).collect::<Vec<_>>(),
            }));
        }
    }
    let src = match params.get("latex").and_then(Value::as_str) {
        Some(s) if s.len() > MAX_SRC => return Err(EngineError::BadParam("latex")),
        Some(s) => s.to_string(),
        None => {
            let p = session.path().ok_or(EngineError::NoFile)?;
            let meta = std::fs::metadata(p)?;
            if meta.len() > MAX_SRC as u64 {
                return Err(EngineError::BadParam("latex"));
            }
            String::from_utf8_lossy(&std::fs::read(p)?).into_owned()
        }
    };
    if src.trim().is_empty() {
        return Err(EngineError::BadParam("latex"));
    }
    let dir = doc_dir(session).join(crate::BUILD_DIR).join(PREVIEW_DIR);
    std::fs::create_dir_all(&dir)?;
    let h = frag_hash(src.as_bytes());
    let okmark = dir.join(format!("d{h:016x}.ok"));
    let built = dir.join(crate::BUILD_DIR).join(format!("d{h:016x}.pdf"));
    if okmark.is_file() && built.is_file() {
        // Mark format: "n<pages> <engine>" native, "e<…>" external.
        let mark = std::fs::read_to_string(&okmark).unwrap_or_default();
        let pages = mark.get(1..).and_then(|s| s.split_whitespace().next()).and_then(|t| t.parse::<u32>().ok());
        return Ok(
            json!({"pdf": built.display().to_string(), "pages": pages, "cached": true, "native": mark.starts_with('n'), "errors": [], "log": ""}),
        );
    }
    if let Some((bytes, pages)) = native_doc_pdf(&src) {
        std::fs::create_dir_all(built.parent().unwrap_or(&dir))?;
        craft_atomic::atomic_write(&built, &bytes)?;
        let _ = craft_atomic::atomic_write(&okmark, format!("n{pages} warraqcraft").as_bytes());
        prune_warm(&dir);
        return Ok(json!({"pdf": built.display().to_string(), "pages": pages, "cached": false, "native": true, "errors": [], "log": ""}));
    }

    // Outside the native subset — compile the document for real.
    let (scratch, owned_src) = match session.path() {
        Some(p) => (p.to_path_buf(), None),
        None => {
            let s = dir.join(format!("d{h:016x}.tex"));
            craft_atomic::atomic_write(&s, src.as_bytes())?;
            (s, Some(src.as_str()))
        }
    };
    let report = compile_file(&scratch, owned_src)?;
    if report.pdf.is_some() {
        let _ = craft_atomic::atomic_write(&okmark, format!("e{} {}", report.pages.unwrap_or(1), report.engine).as_bytes());
        prune_warm(&dir);
    }
    Ok(json!({
        "pdf": report.pdf.as_ref().map(|p| p.display().to_string()),
        "pages": report.pages,
        "cached": false,
        "native": false,
        "log": char_tail(&report.log_tail, MAX_LOG_CHARS),
        "errors": report.errors.iter().map(|e: &CompileError| json!({"line": e.line, "message": e.message})).collect::<Vec<_>>(),
    }))
}

/// File-based shared pipeline for `doc.preview` and the UI's preview
/// pane: warm cache → native `typeset` → external compile. Returns
/// `(report, native, cached)`; `report.engine` is `"warraqcraft"` on
/// the native path so provenance shows in status lines. The `.ok`
/// cache mark stores `kind pages engine` so hits report faithfully.
pub fn preview_file(path: &Path, engine: Option<&str>) -> Result<(CompileReport, bool, bool), EngineError> {
    const MAX_SRC: usize = 512 * 1024;
    let meta = std::fs::metadata(path)?;
    if meta.len() > MAX_SRC as u64 {
        return Err(EngineError::BadParam("latex"));
    }
    let src = String::from_utf8_lossy(&std::fs::read(path)?).into_owned();
    if src.trim().is_empty() {
        return Err(EngineError::BadParam("latex"));
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
    let dir = parent.join(crate::BUILD_DIR).join(PREVIEW_DIR);
    std::fs::create_dir_all(&dir)?;
    let h = frag_hash(src.as_bytes());
    let okmark = dir.join(format!("d{h:016x}.ok"));
    let built = dir.join(crate::BUILD_DIR).join(format!("d{h:016x}.pdf"));
    if okmark.is_file() && built.is_file() {
        let mark = std::fs::read_to_string(&okmark).unwrap_or_default();
        let native = mark.starts_with('n');
        let mut it = mark.get(1..).unwrap_or_default().split_whitespace();
        let pages = it.next().and_then(|t| t.parse::<u32>().ok());
        let engine_name = it.next().unwrap_or(if native { "warraqcraft" } else { "system" }).to_string();
        return Ok((
            CompileReport { engine: engine_name, pdf: Some(built), pages, errors: Vec::new(), warnings: Vec::new(), log_tail: String::new() },
            native,
            true,
        ));
    }
    if let Some((bytes, pages)) = native_doc_pdf_incr(path, &src) {
        std::fs::create_dir_all(built.parent().unwrap_or(&dir))?;
        craft_atomic::atomic_write(&built, &bytes)?;
        let _ = craft_atomic::atomic_write(&okmark, format!("n{pages} warraqcraft").as_bytes());
        prune_warm(&dir);
        return Ok((
            CompileReport {
                engine: "warraqcraft".to_string(),
                pdf: Some(built),
                pages: Some(pages),
                errors: Vec::new(),
                warnings: Vec::new(),
                log_tail: String::new(),
            },
            true,
            false,
        ));
    }
    let report = compile_file_with(path, Some(&src), engine)?;
    if report.pdf.is_some() {
        let _ = craft_atomic::atomic_write(&okmark, format!("e{} {}", report.pages.unwrap_or(1), report.engine).as_bytes());
        prune_warm(&dir);
    }
    Ok((report, false, false))
}

// ---------- native typeset path ----------

/// Serif TTF candidates for the native preview — first found wins.
/// `WARRAQCRAFT_MATH_FONT` overrides (a `.ttf` path). Italic companions
/// are tried alongside so `Style::Italic` gets a real italic face.
#[cfg(not(target_arch = "wasm32"))]
fn preview_fonts() -> Option<(texpdf::Ttf, Option<texpdf::Ttf>)> {
    let regular = [
        std::env::var("WARRAQCRAFT_MATH_FONT").unwrap_or_default(),
        r"C:\Windows\Fonts\times.ttf".to_string(),
        r"C:\Windows\Fonts\georgia.ttf".to_string(),
        r"C:\Windows\Fonts\arial.ttf".to_string(),
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf".to_string(),
        "/usr/share/fonts/truetype/liberation/LiberationSerif-Regular.ttf".to_string(),
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf".to_string(),
        "/System/Library/Fonts/Supplemental/Times New Roman.ttf".to_string(),
        "/Library/Fonts/Georgia.ttf".to_string(),
    ];
    let italic = [
        r"C:\Windows\Fonts\timesi.ttf",
        r"C:\Windows\Fonts\georgiai.ttf",
        r"C:\Windows\Fonts\ariali.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSerif-Italic.ttf",
        "/usr/share/fonts/truetype/liberation/LiberationSerif-Italic.ttf",
        "/System/Library/Fonts/Supplemental/Times New Roman Italic.ttf",
    ];
    let load = |p: &str| -> Option<texpdf::Ttf> { std::fs::read(p).ok().and_then(|b| texpdf::Ttf::parse(&b).ok()) };
    let reg = regular.iter().filter(|p| !p.is_empty()).find_map(|p| load(p))?;
    let it = italic.iter().find_map(|p| load(p));
    Some((reg, it))
}

/// Strip the inline-math wrappers the preview accepts; `\begin{…}`
/// blocks are never native.
#[cfg(not(target_arch = "wasm32"))]
fn bare_math(latex: &str) -> Option<&str> {
    let t = latex.trim();
    if t.starts_with("\\begin{") {
        return None;
    }
    for (open, close) in [("$", "$"), ("\\(", "\\)"), ("\\[", "\\]")] {
        if t.starts_with(open) && t.ends_with(close) && t.len() > open.len() + close.len() {
            return t.get(open.len()..t.len() - close.len());
        }
    }
    Some(t)
}

/// Typeset the fragment in-process and serialize a one-page PDF —
/// `Some(bytes)` only when the source stays inside the supported
/// subset and every glyph it needs exists in the font. Any miss falls
/// back to the external engine; nothing here can panic on bad input.
#[cfg(not(target_arch = "wasm32"))]
fn native_pdf(latex: &str) -> Option<Vec<u8>> {
    let body = bare_math(latex)?;
    let (reg, ital) = preview_fonts()?;
    let size = 14.0;
    let ital_ref = ital.as_ref().unwrap_or(&reg);
    let measure = |c: char, s: f64, st: typeset::Style| -> f64 {
        match st {
            typeset::Style::Italic => ital_ref.width_pt(c, s),
            typeset::Style::Regular => reg.width_pt(c, s),
        }
    };
    let frag = typeset::math(body, size, &measure)?;
    // Every glyph must exist in its face — .notdef chars mean the
    // fragment isn't faithfully representable; fall back instead.
    for it in &frag.items {
        if let typeset::Item::Text { text, style, .. } = it {
            let f = match style {
                typeset::Style::Italic => ital_ref,
                typeset::Style::Regular => &reg,
            };
            if text.chars().any(|c| !c.is_whitespace() && f.gid(c) == 0) {
                return None;
            }
        }
    }

    // Resolve glyph ids while the faces are still borrowable — the doc
    // takes ownership when fonts are registered.
    let use_italic = ital.is_some();
    let mut uni1 = std::collections::BTreeMap::new();
    let mut uni2 = std::collections::BTreeMap::new();
    let mut runs: Vec<(&str, f64, f64, f64, Vec<u16>)> = Vec::new();
    for it in &frag.items {
        if let typeset::Item::Text { x, y, size, style, text } = it {
            let (face, fname, m) = match style {
                typeset::Style::Italic if use_italic => (ital_ref, "F2", &mut uni2),
                _ => (&reg, "F1", &mut uni1),
            };
            let gids: Vec<u16> = text.chars().map(|c| face.gid(c)).collect();
            for (i, c) in text.chars().enumerate() {
                if let Some(g) = gids.get(i) {
                    m.insert(*g, c);
                }
            }
            runs.push((fname, *size, *x, *y, gids));
        }
    }

    let margin = 10.0;
    let mut doc = texpdf::Doc::new();
    doc.font("F1", reg, uni1);
    if let Some(it) = ital {
        doc.font("F2", it, uni2);
    }
    let mut page = texpdf::Page::new(frag.width + margin * 2.0, frag.height + margin * 2.0);
    for (fname, size, x, y, gids) in runs {
        page.text(fname, size, x + margin, y + margin, gids);
    }
    for it in &frag.items {
        if let typeset::Item::Rule { x, y, w, h } = it {
            page.rule(*x + margin, *y + margin, *w, *h);
        }
    }
    doc.add_page(page);
    doc.save().ok()
}

/// Draw a fragment's items at a baseline — shared by body lines and
/// the stacked footnote column. The `uni` maps collect glyph→unicode
/// pairs for the font's ToUnicode table.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn draw_frag(
    page: &mut texpdf::Page,
    f: &typeset::Fragment,
    base: f64,
    margin: f64,
    reg: &texpdf::Ttf,
    ital: &texpdf::Ttf,
    use_italic: bool,
    uni1: &mut std::collections::BTreeMap<u16, char>,
    uni2: &mut std::collections::BTreeMap<u16, char>,
) {
    for it in &f.items {
        match it {
            typeset::Item::Text { x, y: iy, size: sz, style, text } => {
                let (face, fname, m) = match style {
                    typeset::Style::Italic if use_italic => (ital, "F2", &mut *uni2),
                    _ => (reg, "F1", &mut *uni1),
                };
                let gids: Vec<u16> = text.chars().map(|c| face.gid(c)).collect();
                for (i, c) in text.chars().enumerate() {
                    if let Some(g) = gids.get(i) {
                        m.insert(*g, c);
                    }
                }
                page.text(fname, *sz, margin + x, base + iy, gids);
            }
            typeset::Item::Rule { x, y: ry, w, h } => page.rule(margin + x, base + ry, *w, *h),
        }
    }
}

/// True when every glyph in the block tree exists in its face —
/// a .notdef anywhere means the doc isn't faithfully representable
/// and the caller should compile externally instead.
#[cfg(not(target_arch = "wasm32"))]
fn blocks_glyphs_ok<'a>(mut bs: impl Iterator<Item = &'a typeset::Block>, reg: &texpdf::Ttf, ital: &texpdf::Ttf) -> bool {
    bs.all(|b| match b {
        typeset::Block::Line(f) => f.items.iter().all(|it| match it {
            typeset::Item::Text { text, style, .. } => {
                let face = if matches!(style, typeset::Style::Italic) { ital } else { reg };
                !text.chars().any(|c| !c.is_whitespace() && face.gid(c) == 0)
            }
            typeset::Item::Rule { .. } => true,
        }),
        typeset::Block::Note(ns) => blocks_glyphs_ok(ns.iter(), reg, ital),
        typeset::Block::Gap(_) => true,
    })
}

/// Stack the accumulated footnotes at the bottom of `page` under a
/// short rule, then clear them. Called before every page turn and at
/// the end of the document.
#[cfg(not(target_arch = "wasm32"))]
#[allow(clippy::too_many_arguments)]
fn flush_notes(
    page: &mut texpdf::Page,
    notes: &mut Vec<typeset::Block>,
    note_h: &mut f64,
    margin: f64,
    text_w: f64,
    leading: f64,
    reg: &texpdf::Ttf,
    ital: &texpdf::Ttf,
    use_italic: bool,
    uni1: &mut std::collections::BTreeMap<u16, char>,
    uni2: &mut std::collections::BTreeMap<u16, char>,
) {
    if notes.is_empty() {
        return;
    }
    page.rule(margin, margin + *note_h + leading * 0.4, text_w * 0.3, 0.6);
    let mut ny = margin + *note_h;
    for nb in notes.iter() {
        match nb {
            typeset::Block::Line(f) => {
                let base = ny - f.height;
                draw_frag(page, f, base, margin, reg, ital, use_italic, uni1, uni2);
                ny = base - f.depth;
            }
            typeset::Block::Gap(g) => ny -= g,
            typeset::Block::Note(_) => {}
        }
    }
    notes.clear();
    *note_h = 0.0;
}

/// Citation resolution backed by the document's `.bib` files — unsrt
/// numbering (order of first `\cite`) plus formatted References lines.
#[cfg(not(target_arch = "wasm32"))]
struct EngineCites {
    bib: texparse::BibFile,
    order: Vec<String>,
    next: usize,
}

#[cfg(not(target_arch = "wasm32"))]
impl EngineCites {
    /// `Author. Title. Venue, Year.` — plain-text bibliography line.
    fn fmt(&self, key: &str) -> String {
        let Some(e) = self.bib.get(key) else { return key.to_string() };
        let author = e.field("author").or_else(|| e.field("editor")).unwrap_or(key);
        let title = e.field("title").unwrap_or("");
        let venue = ["journal", "booktitle", "publisher", "school", "institution", "howpublished"].iter().find_map(|f| e.field(f)).unwrap_or("");
        let year = e.field("year").unwrap_or("");
        let tail = [venue, year].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
        let mut s = format!("{}. {}.", author.trim_end_matches('.'), title.trim_end_matches('.'));
        if !tail.is_empty() {
            s.push_str(&format!(" {}.", tail));
        }
        s.chars().take(600).collect()
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl typeset::Cites for EngineCites {
    fn num(&mut self, key: &str) -> Option<usize> {
        self.bib.get(key)?;
        if let Some(i) = self.order.iter().position(|k| k == key) {
            return Some(i + 1);
        }
        self.next += 1;
        self.order.push(key.to_string());
        Some(self.next)
    }
    fn entries(&self) -> Vec<String> {
        self.order.iter().enumerate().map(|(i, k)| format!("[{}] {}", i + 1, self.fmt(k))).collect()
    }
}

/// Read every `.bib` named by `\bibliography{a,b}`/`\addbibresource`
/// in `src`, relative to the document's directory. `None` when the
/// doc declares no bibliography (cites then render as literal keys);
/// also `None` when declared but nothing parses — the caller falls
/// back to the external engine, which reports the real problem.
#[cfg(not(target_arch = "wasm32"))]
fn load_cites(path: &Path, src: &str) -> Option<EngineCites> {
    const MAX_BIB: u64 = 4 * 1024 * 1024;
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let parent = if parent.as_os_str().is_empty() { Path::new(".") } else { parent };
    let mut names: Vec<String> = Vec::new();
    for marker in ["\\bibliography{", "\\addbibresource{"] {
        let mut from = 0usize;
        while let Some(rel) = src.get(from..).and_then(|s| s.find(marker)) {
            let i = from + rel + marker.len();
            let Some(end) = src.get(i..).and_then(|s| s.find('}')) else { break };
            for n in src.get(i..i + end.min(256)).unwrap_or_default().split(',') {
                let n = n.trim();
                if !n.is_empty() && names.len() < 16 && !names.iter().any(|x| x == n) {
                    names.push(n.to_string());
                }
            }
            from = i + end + 1;
        }
    }
    if names.is_empty() {
        return None;
    }
    let mut bib = texparse::BibFile::default();
    for n in &names {
        let p = parent.join(if n.ends_with(".bib") { n.to_string() } else { format!("{n}.bib") });
        if std::fs::metadata(&p).map(|m| m.len() > MAX_BIB).unwrap_or(true) {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&p) {
            let f = texparse::parse_bib(&text);
            for e in f.entries {
                if !e.key.is_empty() && bib.get(&e.key).is_none() {
                    bib.entries.push(e);
                }
            }
        }
    }
    if bib.entries.is_empty() {
        return None;
    }
    Some(EngineCites { bib, order: Vec::new(), next: 0 })
}

/// Per-path segment cache for dirty-region typesetting: an edit
/// re-parses the body but reuses the line-broken blocks of every
/// paragraph whose content key survived — only dirty segments pay
/// the breaker before repagination. Bounded to a few dozen files.
#[cfg(not(target_arch = "wasm32"))]
static INCR: std::sync::Mutex<Vec<(PathBuf, Vec<typeset::Segment>)>> = std::sync::Mutex::new(Vec::new());

/// Take the previous segments cached for `path` (empty on first run).
#[cfg(not(target_arch = "wasm32"))]
fn incr_take(path: &Path) -> Vec<typeset::Segment> {
    let Ok(mut g) = INCR.lock() else { return Vec::new() };
    match g.iter().position(|(p, _)| p == path) {
        Some(i) => std::mem::take(&mut g[i].1),
        None => Vec::new(),
    }
}

/// Store the newest segments for `path`, evicting oldest first.
#[cfg(not(target_arch = "wasm32"))]
fn incr_store(path: &Path, segs: Vec<typeset::Segment>) {
    const MAX_PATHS: usize = 32;
    let Ok(mut g) = INCR.lock() else { return };
    g.retain(|(p, _)| p != path);
    while g.len() >= MAX_PATHS {
        g.remove(0);
    }
    g.push((path.to_path_buf(), segs));
}

/// Typeset a document body in-process and serialize a paginated PDF —
/// letter pages, 1-inch margins, ~14pt leading. `Some((bytes, pages))`
/// only when the body stays inside `typeset`'s text subset and every
/// glyph exists in the font; otherwise the caller compiles for real.
#[cfg(not(target_arch = "wasm32"))]
fn native_doc_pdf(src: &str) -> Option<(Vec<u8>, u32)> {
    native_doc_pdf_inner(src, &[], None).map(|(bytes, pages, _)| (bytes, pages))
}

/// The file-backed variant used by `preview_file`: reuses unchanged
/// segments from the previous pass for this path, and wires the doc's
/// `.bib` files for `\cite` numbering + the References block.
#[cfg(not(target_arch = "wasm32"))]
fn native_doc_pdf_incr(path: &Path, src: &str) -> Option<(Vec<u8>, u32)> {
    let prev = incr_take(path);
    let mut cites = load_cites(path, src);
    let (bytes, pages, segs) = native_doc_pdf_inner(src, &prev, cites.as_mut().map(|c| c as &mut dyn typeset::Cites))?;
    incr_store(path, segs);
    Some((bytes, pages))
}

#[cfg(not(target_arch = "wasm32"))]
fn native_doc_pdf_inner(
    src: &str,
    prev: &[typeset::Segment],
    cites: Option<&mut dyn typeset::Cites>,
) -> Option<(Vec<u8>, u32, Vec<typeset::Segment>)> {
    let (reg, ital) = preview_fonts()?;
    let size = 11.0;
    let page_w = 612.0;
    let page_h = 792.0;
    let margin = 72.0;
    let text_w = page_w - margin * 2.0;
    let leading = size * 1.25;
    let ital_ref = ital.as_ref().unwrap_or(&reg);
    let measure = |c: char, s: f64, st: typeset::Style| -> f64 {
        match st {
            typeset::Style::Italic => ital_ref.width_pt(c, s),
            typeset::Style::Regular => reg.width_pt(c, s),
        }
    };
    let segs = typeset::doc_segments(src, size, text_w, &measure, prev, cites)?;
    // Every glyph must exist in its face — .notdef means the doc isn't
    // faithfully representable; fall back instead.
    if !blocks_glyphs_ok(segs.iter().flat_map(|s| s.blocks.iter()), &reg, ital_ref) {
        return None;
    }

    let use_italic = ital.is_some();
    let mut uni1 = std::collections::BTreeMap::new();
    let mut uni2 = std::collections::BTreeMap::new();
    let mut doc = texpdf::Doc::new();
    let mut page = texpdf::Page::new(page_w, page_h);
    // `y` is the current baseline; first line per page tops at margin.
    let mut y = page_h - margin;
    let mut first = true;
    let mut pages = 1u32;
    // Footnotes reserve space from the bottom margin up; notes that
    // don't fit on their page carry to the next, like TeX's footins.
    let mut notes: Vec<typeset::Block> = Vec::new();
    let mut pending: Vec<typeset::Block> = Vec::new();
    let mut note_h = 0.0f64;
    let nlead = leading * 0.85;
    let note_cap = (page_h - margin * 2.0) * 0.3;
    for b in segs.iter().flat_map(|s| s.blocks.iter()) {
        match b {
            typeset::Block::Gap(g) => y -= g,
            typeset::Block::Note(nbs) => {
                for nb in nbs {
                    let cost = match nb {
                        typeset::Block::Gap(g) => *g,
                        _ => nlead,
                    };
                    // Must fit below the lowest content already drawn;
                    // otherwise the note follows TeX and moves on.
                    let fits = cost <= y - margin - leading * 0.5 - note_h && note_h + cost <= note_cap;
                    if fits {
                        note_h += cost;
                        notes.push(nb.clone());
                    } else {
                        pending.push(nb.clone());
                    }
                }
            }
            typeset::Block::Line(f) => {
                let reserve = if note_h > 0.0 { note_h + leading * 0.5 } else { 0.0 };
                let base = if first { y - f.height } else { y - leading };
                if base - f.depth < margin + reserve && !first {
                    flush_notes(&mut page, &mut notes, &mut note_h, margin, text_w, leading, &reg, ital_ref, use_italic, &mut uni1, &mut uni2);
                    doc.add_page(std::mem::replace(&mut page, texpdf::Page::new(page_w, page_h)));
                    y = page_h - margin;
                    pages += 1;
                    first = true;
                    notes = std::mem::take(&mut pending);
                    for nb in &notes {
                        note_h += match nb {
                            typeset::Block::Gap(g) => *g,
                            _ => nlead,
                        };
                    }
                }
                let base = if first { y - f.height } else { y - leading };
                first = false;
                draw_frag(&mut page, f, base, margin, &reg, ital_ref, use_italic, &mut uni1, &mut uni2);
                y = base;
            }
        }
    }
    flush_notes(&mut page, &mut notes, &mut note_h, margin, text_w, leading, &reg, ital_ref, use_italic, &mut uni1, &mut uni2);
    doc.add_page(page);
    doc.font("F1", reg, uni1);
    if let Some(it) = ital {
        doc.font("F2", it, uni2);
    }
    Some((doc.save().ok()?, pages, segs))
}

/// The web build keeps the external path only — no font filesystem.
#[cfg(target_arch = "wasm32")]
fn native_pdf(_latex: &str) -> Option<Vec<u8>> {
    None
}

/// Wasm stubs — no font filesystem or document preview.
#[cfg(target_arch = "wasm32")]
fn native_doc_pdf(_src: &str) -> Option<(Vec<u8>, u32)> {
    None
}

#[cfg(target_arch = "wasm32")]
fn native_doc_pdf_incr(_path: &Path, _src: &str) -> Option<(Vec<u8>, u32)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_DIR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("warraqcraft-preview-test-{}-{tag}-{n}", std::process::id()));
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

    fn fixture(dir: &Path) -> Session {
        let main = dir.join("main.tex");
        std::fs::write(&main, "\\documentclass{article}\n\\begin{document}x\\end{document}\n").unwrap();
        let mut s = Session::new();
        s.execute("document.open", &json!({"path": main})).unwrap();
        s
    }

    #[test]
    fn bad_latex_is_bad_param_never_panic() {
        let mut s = Session::new();
        for p in [json!({}), json!({"latex": 42}), json!({"latex": ["$x$"]}), json!({"latex": ""}), json!({"latex": "   \n "})] {
            assert!(matches!(s.execute("math.preview", &p), Err(EngineError::BadParam("latex"))), "{p}");
        }
        let giant = json!({"latex": "x".repeat(MAX_LATEX + 1)});
        assert!(matches!(s.execute("math.preview", &giant), Err(EngineError::BadParam("latex"))));
        let max_ok_but_no_engine_or_fine = json!({"latex": "x".repeat(MAX_LATEX)});
        assert!(!matches!(s.execute("math.preview", &max_ok_but_no_engine_or_fine), Err(EngineError::BadParam(_))));
    }

    #[test]
    fn fragment_wrapping() {
        let plain = wrap_fragment(" x + y ");
        assert!(plain.starts_with("\\documentclass{article}"));
        assert!(plain.contains("\\( x + y \\)"));
        assert!(plain.ends_with("\\end{document}\n"));
        for v in ["$x$", "\\(x\\)", "\\[x\\]", "\\begin{equation}x\\end{equation}", "  $x$  "] {
            let wrapped = wrap_fragment(v);
            assert!(wrapped.contains(v.trim()), "{v}");
            assert!(!wrapped.contains("\\( \\"), "{v}");
        }
    }

    #[test]
    fn scratch_written_and_compile_resilient() {
        let dir = TempDir::new("compile");
        let mut s = fixture(&dir.0);
        // Hash the same fragment the Ok-branch below compiles — scratch
        // names are content-keyed now.
        let h = frag_hash(wrap_fragment("x + y").as_bytes());
        let scratch = dir.0.join(crate::BUILD_DIR).join(PREVIEW_DIR).join(format!("p{h:016x}.tex"));
        let pdf = dir.0.join(crate::BUILD_DIR).join(PREVIEW_DIR).join(crate::BUILD_DIR).join(format!("p{h:016x}.pdf"));
        let v = s.execute("math.preview", &json!({"latex": "x + y"}));
        match v {
            Ok(v) => {
                if v["native"].as_bool() == Some(true) {
                    // In-process typeset — a real PDF, no engine spawn.
                    assert!(pdf.is_file(), "native pdf should be written: {v}");
                    assert_eq!(v["pages"].as_u64(), Some(1), "{v}");
                    return;
                }
                assert!(
                    v["pdf"]
                        .as_str()
                        .is_some_and(|p| Path::new(p).file_name().is_some_and(|n| n.to_string_lossy().starts_with('p') && p.ends_with(".pdf"))),
                    "{v}"
                );
                assert!(v["pages"].as_u64().unwrap_or(0) >= 1, "{v}");
                assert!(v["errors"].as_array().is_some_and(Vec::is_empty), "{v}");
                assert!(v["log"].as_str().is_none_or(|l| l.chars().count() <= MAX_LOG_CHARS));
                assert!(scratch.is_file());
            }
            Err(_) => {
                // No usable path at all: the scratch is still written
                // before the external compile errors — never a panic.
                assert!(scratch.is_file());
                assert!(std::fs::read_to_string(&scratch).unwrap().contains("\\( x + y \\)"));
            }
        }
    }
}
