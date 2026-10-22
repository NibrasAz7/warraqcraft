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

use crate::{compile_file, CompileError, EngineError, Session};

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

/// The web build keeps the external path only — no font filesystem.
#[cfg(target_arch = "wasm32")]
fn native_pdf(_latex: &str) -> Option<Vec<u8>> {
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
