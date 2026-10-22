//! `library.*` — library-lite. Attach a PDF to a cite key and the file
//! is copied into the project's `.warraqcraft/library/<key>.pdf` with an
//! index in `library.json`. Everything lives in the project dir — the
//! library syncs with the repo like any other project file.
//!
//! Keys are validated against cite-key charset; source PDFs are size-
//! capped. The index is rebuilt from disk if absent.

use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

use crate::{EngineError, Session};

/// Attached PDFs are copied into the project — cap the source size.
const MAX_PDF_BYTES: u64 = 128 * 1024 * 1024;

/// `.warraqcraft` store paths under the project dir.
#[cfg(not(target_arch = "wasm32"))]
fn store(dir: &Path) -> (PathBuf, PathBuf) {
    let base = dir.join(".warraqcraft").join("library");
    (base.join("index.json"), base)
}

/// Cite-key charset — same characters `cite.fetch` keys may carry.
fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.chars().count() <= 128 && key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '_' | '-' | '.'))
}

/// Read the index map (key → {pdf}).
#[cfg(not(target_arch = "wasm32"))]
fn read_index(dir: &Path) -> Map<String, Value> {
    let (index, _) = store(dir);
    std::fs::read_to_string(index).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).and_then(|v| v.as_object().cloned()).unwrap_or_default()
}

/// Persist the index atomically.
#[cfg(not(target_arch = "wasm32"))]
fn write_index(dir: &Path, map: &Map<String, Value>) -> Result<(), EngineError> {
    let (index, base) = store(dir);
    std::fs::create_dir_all(&base)?;
    let text = serde_json::to_string_pretty(map).map_err(|e| EngineError::Tool(format!("library index: {e}")))?;
    craft_atomic::atomic_write(&index, text.as_bytes())?;
    Ok(())
}

/// The open file's directory — library state is per project dir.
#[cfg(not(target_arch = "wasm32"))]
fn project_dir(sess: &Session) -> Result<PathBuf, EngineError> {
    sess.path().and_then(|p| p.parent().map(|d| d.to_path_buf())).ok_or(EngineError::NoFile)
}

/// `library.attach {key, pdf}` — copy `pdf` into the project's library
/// and index it under `key`. `pdf` may be project-relative or absolute;
/// it must exist, be a `.pdf`, and be within the size cap.
#[cfg(not(target_arch = "wasm32"))]
pub fn attach(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let key = params.get("key").and_then(Value::as_str).filter(|k| valid_key(k)).ok_or(EngineError::BadParam("key"))?;
    let pdf = params.get("pdf").and_then(Value::as_str).ok_or(EngineError::BadParam("pdf"))?;
    let dir = project_dir(sess)?;
    let src = {
        let p = Path::new(pdf);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            dir.join(p)
        }
    };
    if src.extension().and_then(|e| e.to_str()).is_none_or(|e| !e.eq_ignore_ascii_case("pdf")) {
        return Err(EngineError::BadParam("pdf"));
    }
    let meta = std::fs::metadata(&src).map_err(|_| EngineError::BadParam("pdf"))?;
    if !meta.is_file() || meta.len() > MAX_PDF_BYTES {
        return Err(EngineError::BadParam("pdf"));
    }
    let (_, base) = store(&dir);
    std::fs::create_dir_all(&base)?;
    let dest = base.join(format!("{key}.pdf"));
    let data = std::fs::read(&src)?;
    craft_atomic::atomic_write(&dest, &data)?;

    let mut map = read_index(&dir);
    map.insert(key.to_string(), json!({"pdf": format!("{key}.pdf"), "bytes": meta.len()}));
    write_index(&dir, &map)?;
    Ok(json!({"key": key, "pdf": dest.display().to_string(), "bytes": meta.len()}))
}

/// `library.list` → indexed attachments that still exist on disk.
#[cfg(not(target_arch = "wasm32"))]
pub fn list(sess: &Session) -> Result<Value, EngineError> {
    let dir = project_dir(sess)?;
    let (_, base) = store(&dir);
    let items: Vec<Value> = read_index(&dir)
        .iter()
        .filter_map(|(k, v)| {
            let pdf = base.join(v.get("pdf").and_then(Value::as_str)?);
            pdf.is_file().then(|| json!({"key": k, "pdf": pdf.display().to_string(), "bytes": v.get("bytes").and_then(Value::as_u64).unwrap_or(0)}))
        })
        .collect();
    Ok(json!({"library": items}))
}

/// `library.get {key}` → the attached PDF path, or null.
#[cfg(not(target_arch = "wasm32"))]
pub fn get(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let key = params.get("key").and_then(Value::as_str).filter(|k| valid_key(k)).ok_or(EngineError::BadParam("key"))?;
    let dir = project_dir(sess)?;
    let (_, base) = store(&dir);
    let pdf = read_index(&dir).get(key).and_then(|v| v.get("pdf")).and_then(Value::as_str).map(|name| base.join(name)).filter(|p| p.is_file());
    Ok(match pdf {
        Some(p) => json!({"key": key, "pdf": p.display().to_string()}),
        None => Value::Null,
    })
}

/// `library.detach {key}` — remove the index entry and the stored file.
#[cfg(not(target_arch = "wasm32"))]
pub fn detach(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let key = params.get("key").and_then(Value::as_str).filter(|k| valid_key(k)).ok_or(EngineError::BadParam("key"))?;
    let dir = project_dir(sess)?;
    let mut map = read_index(&dir);
    let removed = map.remove(key).is_some();
    if removed {
        let (_, base) = store(&dir);
        let _ = std::fs::remove_file(base.join(format!("{key}.pdf")));
        write_index(&dir, &map)?;
    }
    Ok(json!({"removed": removed}))
}

// ---- wasm: no project filesystem — commands degrade honestly. ----

#[cfg(target_arch = "wasm32")]
pub fn attach(_s: &Session, _p: &Value) -> Result<Value, EngineError> {
    Err(EngineError::Tool("library.* needs the project filesystem — desktop builds only".into()))
}
#[cfg(target_arch = "wasm32")]
pub fn list(_s: &Session) -> Result<Value, EngineError> {
    Ok(json!({"library": []}))
}
#[cfg(target_arch = "wasm32")]
pub fn get(_s: &Session, _p: &Value) -> Result<Value, EngineError> {
    Ok(Value::Null)
}
#[cfg(target_arch = "wasm32")]
pub fn detach(_s: &Session, _p: &Value) -> Result<Value, EngineError> {
    Err(EngineError::Tool("library.* needs the project filesystem — desktop builds only".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("wc-lib-{}-{tag}", std::process::id()));
            let _ = std::fs::create_dir_all(&p);
            Self(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn fixture(d: &Dir) -> Session {
        let p = d.0.join("main.tex");
        std::fs::write(&p, "\\documentclass{article}\\begin{document}hi\\end{document}").unwrap();
        let mut s = Session::new();
        s.open(&p).unwrap();
        s
    }

    #[test]
    fn attach_list_get_detach_round_trip() {
        let d = Dir::new("rt");
        let s = fixture(&d);
        let src = d.0.join("paper.pdf");
        std::fs::write(&src, b"%PDF-1.4 fake").unwrap();

        let v = attach(&s, &json!({"key": "knuth84", "pdf": "paper.pdf"})).unwrap();
        assert!(v["pdf"].as_str().unwrap().ends_with("knuth84.pdf"));

        let l = list(&s).unwrap();
        assert_eq!(l["library"].as_array().unwrap().len(), 1);

        let g = get(&s, &json!({"key": "knuth84"})).unwrap();
        assert!(g["pdf"].as_str().is_some());
        assert!(get(&s, &json!({"key": "missing"})).unwrap().is_null());

        let v = detach(&s, &json!({"key": "knuth84"})).unwrap();
        assert_eq!(v["removed"], true);
        assert!(get(&s, &json!({"key": "knuth84"})).unwrap().is_null());
    }

    #[test]
    fn bad_keys_and_non_pdfs_rejected() {
        let d = Dir::new("bad");
        let s = fixture(&d);
        assert!(attach(&s, &json!({"key": "a b", "pdf": "x.pdf"})).is_err());
        assert!(attach(&s, &json!({"key": "", "pdf": "x.pdf"})).is_err());
        let not_pdf = d.0.join("x.txt");
        std::fs::write(&not_pdf, b"hi").unwrap();
        assert!(attach(&s, &json!({"key": "k", "pdf": "x.txt"})).is_err());
        assert!(attach(&s, &json!({"key": "k", "pdf": "nope.pdf"})).is_err());
    }
}
