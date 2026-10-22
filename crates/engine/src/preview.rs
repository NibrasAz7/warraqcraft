//! `math.preview`: TeXstudio-style "preview selected math". A math
//! fragment is wrapped in a minimal `article` preamble, written to a
//! scratch file under `.warraqcraft/preview/` next to the document, and
//! compiled through the same [`compile_file`] pipeline as
//! `document.compile` — engine detection, tectonic preference,
//! nonstop/halt-on-error flags, log parsing; shell escape stays off.
//! The response carries the PDF path and page count; the UI
//! rasterizes and caches it. Read-only for the session: the scratch
//! file and build artifacts are the only filesystem effects.

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
        "log": char_tail(&report.log_tail, MAX_LOG_CHARS),
        "errors": report.errors.iter().map(|e: &CompileError| json!({"line": e.line, "message": e.message})).collect::<Vec<_>>(),
    }))
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
        if crate::detect_engines().is_empty() {
            // No engine: the scratch file is still written, then the
            // compile step errors — never a panic.
            assert!(s.execute("math.preview", &json!({"latex": "x + y"})).is_err());
            assert!(scratch.is_file());
            assert!(std::fs::read_to_string(&scratch).unwrap().contains("\\( x + y \\)"));
            return;
        }
        // An installed engine can still fail here — e.g. tectonic's
        // first-run bundle download on an offline box. Not a defect.
        if let Ok(v) = s.execute("math.preview", &json!({"latex": "x + y"})) {
            assert!(
                v["pdf"]
                    .as_str()
                    .is_some_and(|p| Path::new(p).file_name().is_some_and(|n| n.to_string_lossy().starts_with('p') && p.ends_with(".pdf"))),
                "{v}"
            );
            assert!(v["pages"].as_u64().unwrap_or(0) >= 1, "{v}");
            assert!(v["errors"].as_array().is_some_and(Vec::is_empty), "{v}");
            assert!(v["log"].as_str().is_none_or(|l| l.chars().count() <= MAX_LOG_CHARS));
        }
        assert!(scratch.is_file());
    }
}
