//! `project.structure` — merged outline, labels and unresolved files
//! across the `\input`/`\include` dependency graph (M3 awareness).
//!
//! [`texparse::graph::build`] walks the reachable `.tex` files from the
//! effective root (the `% !TeX root` master when declared — matching
//! `compile_file`'s redirect — else the open file). Each file's
//! [`texparse::parse_structure`] result is merged into one answer so the
//! outline and `\ref` completion see the whole project, not just the
//! current buffer. The open file is parsed from the live session buffer
//! so unsaved edits show up immediately; every read is size-capped and a
//! vanished file is skipped, never panicked on.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use texparse::{graph, parse_magic, parse_structure, DepKind};

use crate::{EngineError, Session};

/// Per-file read cap — a hostile `.tex` never stalls the scan.
const MAX_FILE_BYTES: u64 = 1024 * 1024;
/// Most files merged into one answer; the graph caps deeper.
const MAX_PROJECT_FILES: usize = 128;

/// `project.structure` — `{root, files: [{path, name, headings, labels,
/// cite_keys, bibliographies, issues}], labels, cite_keys, missing}`.
/// `labels`/`cite_keys`
/// are order-preserving unions across all files. `missing` lists deps
/// that didn't resolve, minus `.sty`/`.cls` (those live in the TeX
/// distro, not the project — flagging them would be noise).
pub fn run(sess: &Session) -> Result<Value, EngineError> {
    let open = sess.path().ok_or(EngineError::NoFile)?.to_path_buf();
    let dir = open.parent().unwrap_or_else(|| Path::new("."));
    let magic = parse_magic(sess.source());
    let entry = magic.root.as_deref().map(|r| dir.join(r)).filter(|c| c != &open && c.is_file()).unwrap_or_else(|| open.clone());
    let g = graph::build(&entry);
    let open_canon = canon(&open);

    let mut files = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut cite_keys: Vec<String> = Vec::new();
    for f in g.files.iter().take(MAX_PROJECT_FILES) {
        let d = if canon(f) == open_canon {
            parse_structure(sess.source())
        } else {
            let mut buf = Vec::new();
            match File::open(f).map(|h| h.take(MAX_FILE_BYTES).read_to_end(&mut buf)) {
                Ok(Ok(_)) => parse_structure(&String::from_utf8_lossy(&buf)),
                _ => continue, // vanished or unreadable since the walk
            }
        };
        for (l, _) in &d.labels {
            if !labels.contains(l) {
                labels.push(l.clone());
            }
        }
        for (k, _) in &d.cite_keys {
            if !cite_keys.contains(k) {
                cite_keys.push(k.clone());
            }
        }
        files.push(json!({
            "path": f.display().to_string(),
            "name": f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            "headings": d.headings.iter().map(|h| json!({"line": h.pos.line, "level": h.level, "title": h.title})).collect::<Vec<_>>(),
            "labels": d.labels.iter().map(|(l, _)| l).collect::<Vec<_>>(),
            "cite_keys": d.cite_keys.iter().map(|(k, _)| k).collect::<Vec<_>>(),
            "bibliographies": d.bibliographies.iter().map(|(b, _)| b).collect::<Vec<_>>(),
            "issues": d.issues.iter().map(|i| json!({"line": i.pos.line, "col": i.pos.col, "message": i.message})).collect::<Vec<_>>(),
        }));
    }
    Ok(json!({
        "root": entry.display().to_string(),
        "files": files,
        "labels": labels,
        "cite_keys": cite_keys,
        "missing": g
            .missing
            .iter()
            .filter(|(_, d)| !matches!(d.kind, DepKind::UsePackage | DepKind::DocumentClass))
            .map(|(r, d)| json!({"referrer": r.display().to_string(), "kind": format!("{:?}", d.kind), "target": d.target, "line": d.line}))
            .collect::<Vec<_>>(),
    }))
}

/// Canonical-or-as-is path for cross-file identity, matching
/// [`texparse::graph`]'s cycle check.
fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    static NEXT_DIR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("warraqcraft-project-test-{}-{tag}-{n}", std::process::id()));
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
    fn merges_headings_and_labels_across_input() {
        let dir = TempDir::new("merge");
        std::fs::write(
            dir.0.join("main.tex"),
            "\\documentclass{article}\n\\begin{document}\n\\input{ch1}\n\\input{ch2}\n\\input{ghost}\n\\end{document}\n",
        )
        .unwrap();
        std::fs::write(dir.0.join("ch1.tex"), "\\section{One}\n\\label{sec:one}\ntext\n").unwrap();
        std::fs::write(dir.0.join("ch2.tex"), "\\section{Two}\n\\label{sec:two}\nsee \\ref{sec:one}\n").unwrap();
        let mut s = Session::new();
        s.execute("document.open", &serde_json::json!({"path": dir.0.join("main.tex")})).unwrap();

        let v = s.execute("project.structure", &serde_json::json!({})).unwrap();
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 3); // main + ch1 + ch2
        let titles: Vec<&str> = files.iter().flat_map(|f| f["headings"].as_array().unwrap().iter().filter_map(|h| h["title"].as_str())).collect();
        assert_eq!(titles, ["One", "Two"]);
        let labels: Vec<&str> = v["labels"].as_array().unwrap().iter().filter_map(|l| l.as_str()).collect();
        assert_eq!(labels, ["sec:one", "sec:two"]);
        // The unresolved `\input{ghost}` surfaces in `missing`.
        let missing = v["missing"].as_array().unwrap();
        assert!(missing.iter().any(|m| m["target"] == "ghost"));
    }

    #[test]
    fn open_file_parses_live_buffer_not_disk() {
        let dir = TempDir::new("live");
        std::fs::write(dir.0.join("main.tex"), "\\documentclass{article}\n\\begin{document}\n\\section{Old}\n\\end{document}\n").unwrap();
        let mut s = Session::new();
        s.execute("document.open", &serde_json::json!({"path": dir.0.join("main.tex")})).unwrap();
        s.execute(
            "source.set",
            &serde_json::json!({"text": "\\documentclass{article}\n\\begin{document}\n\\section{New}\\label{sec:new}\n\\end{document}\n"}),
        )
        .unwrap();

        let v = s.execute("project.structure", &serde_json::json!({})).unwrap();
        let titles: Vec<&str> = v["files"][0]["headings"].as_array().unwrap().iter().filter_map(|h| h["title"].as_str()).collect();
        assert_eq!(titles, ["New"]);
        assert!(v["labels"].as_array().unwrap().iter().any(|l| l == "sec:new"));
    }

    #[test]
    fn magic_root_redirects_the_walk() {
        let dir = TempDir::new("root");
        std::fs::write(dir.0.join("main.tex"), "\\documentclass{article}\n\\begin{document}\n\\input{ch1}\n\\end{document}\n").unwrap();
        std::fs::write(dir.0.join("ch1.tex"), "\\section{Chap}\n").unwrap();
        // Open the *child* with a %!TeX root hint — the scan should still
        // start from main.tex and see both files.
        let mut s = Session::new();
        s.execute("document.open", &serde_json::json!({"path": dir.0.join("ch1.tex")})).unwrap();
        s.execute("source.set", &serde_json::json!({"text": "% !TeX root = main.tex\n\\section{Chap}\n"})).unwrap();

        let v = s.execute("project.structure", &serde_json::json!({})).unwrap();
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        let names: Vec<&str> = files.iter().filter_map(|f| f["name"].as_str()).collect();
        assert_eq!(names, ["main.tex", "ch1.tex"]);
    }

    #[test]
    fn no_open_file_is_an_error_not_a_panic() {
        let s = Session::new();
        assert!(matches!(run(&s), Err(EngineError::NoFile)));
    }
}
