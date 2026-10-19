//! Template gallery: shipped `<id>.tex` + `<id>.meta.toml` pairs and
//! user-imported `<id>/` text-source folders.
//!
//! Discovery mirrors `find_tectonic()`: the `WARRAQCRAFT_TEMPLATES` env
//! override first (tests, packaging), then `templates/` next to the
//! executable, standard app/resource install locations, and the nearest
//! ancestor directory containing `templates/` — which covers dev
//! checkouts where the binary lives under `target/` and templates sit at
//! the workspace root. User imports live outside the repository under
//! the per-user template dir
//! (`WARRAQCRAFT_USER_TEMPLATES`, `WARRAQCRAFT_CONFIG_DIR/templates`, or
//! the platform data dir) and shadow a bundled template with the same id.
//!
//! Meta files are a restricted TOML subset — `key = "string"` lines only —
//! parsed by hand so the engine needs no `toml` dependency. A malformed
//! meta file is skipped, never fatal; a template without one still lists
//! with `name = id`. Imports copy only allow-listed text sources: fonts,
//! PDFs, binaries, caches and build artifacts never enter the gallery.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

use crate::EngineError;

/// Ids are file-name components; keeping them boring means an `id`
/// can never escape the templates directory (`..`, `/`, `\` rejected).
const ID_MAX: usize = 64;
/// Bound on one directory scan — template dirs are ours but env
/// overrides can point anywhere.
const MAX_TEMPLATES: usize = 512;
/// Templates are starter sources; refuse silly-large files so reads and
/// imports can't allocate unboundedly.
const MAX_TEMPLATE_BYTES: u64 = 1024 * 1024;
/// Total bytes accepted by one `template.import`.
const MAX_IMPORT_TOTAL_BYTES: u64 = 8 * 1024 * 1024;
/// Files in one folder template/import.
const MAX_TEMPLATE_FILES: usize = 512;
/// Imported template trees may nest chapters/styles this deep.
const MAX_IMPORT_DEPTH: u32 = 4;
/// Metadata files are small; cap the whole read before parsing.
const MAX_META_BYTES: u64 = 64 * 1024;
/// Meta values are short display/provenance strings.
const MAX_META_VALUE: usize = 256;
/// Text-only source formats eligible for shipped and imported templates.
const SOURCE_EXTS: [&str; 15] = ["tex", "sty", "cls", "clo", "def", "cfg", "bib", "bst", "txt", "md", "toml", "lco", "ist", "dtx", "ins"];

/// Shared id rule for template and snapshot ids: `[A-Za-z0-9_-]{1,64}`.
pub(crate) fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= ID_MAX && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

struct TemplateDir {
    path: PathBuf,
    user: bool,
}

/// The per-user import destination (and user-gallery source). It need
/// not exist yet — `template.import` creates it.
fn user_templates_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("WARRAQCRAFT_USER_TEMPLATES").filter(|d| !d.is_empty()).map(PathBuf::from) {
        return Some(dir);
    }
    if let Some(dir) = std::env::var_os("WARRAQCRAFT_CONFIG_DIR").filter(|d| !d.is_empty()).map(PathBuf::from) {
        return Some(dir.join("templates"));
    }
    if cfg!(windows) {
        return std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("warraqcraft").join("templates"));
    }
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME").map(|d| PathBuf::from(d).join("Library/Application Support/warraqcraft/templates"));
    }
    std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|d| PathBuf::from(d).join(".local/share")))
        .map(|d| d.join("warraqcraft").join("templates"))
}

/// Bundled template candidates for portable, app-bundle and system
/// install layouts. The ancestor search keeps dev checkouts working.
fn bundled_template_candidates(exe: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = exe.parent() {
        out.push(dir.join("templates"));
        out.push(dir.join("..").join("Resources").join("templates"));
        out.push(dir.join("..").join("share").join("warraqcraft").join("templates"));
    }
    out.extend(exe.ancestors().skip(1).map(|a| a.join("templates")));
    out
}

/// Gallery roots, bundled first so imported ids can shadow them. Setting
/// `WARRAQCRAFT_TEMPLATES` intentionally isolates the gallery (tests and
/// portable packaging): the per-user dir is not mixed in.
fn template_dirs() -> Vec<TemplateDir> {
    if let Some(dir) = std::env::var_os("WARRAQCRAFT_TEMPLATES").map(PathBuf::from).filter(|p| p.is_dir()) {
        return vec![TemplateDir { path: dir, user: false }];
    }
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = bundled_template_candidates(&exe).into_iter().find(|p| p.is_dir()) {
            dirs.push(TemplateDir { path: dir, user: false });
        }
    }
    if let Some(dir) = user_templates_dir().filter(|p| p.is_dir()) {
        dirs.push(TemplateDir { path: dir, user: true });
    }
    dirs
}

fn no_templates_dir() -> EngineError {
    EngineError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "no templates directory found"))
}

/// Extract the closing-quote-delimited string from a restricted TOML
/// basic-string value (`"…"` — `\"` and `\\` escapes honoured).
/// `None` when the value isn't a basic string.
fn toml_string(v: &str) -> Option<String> {
    let v = v.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        match c {
            // Anything after the closing quote (e.g. a `#` comment)
            // is ignored — the restricted format has nothing else.
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => return None,
            },
            _ => {
                if out.len() >= MAX_META_VALUE {
                    return None;
                }
                out.push(c);
            }
        }
    }
    None
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Parsed template metadata. `name`/`desc` are the default (English)
/// strings; `localized` holds per-language overrides from `name_<lang>`
/// / `desc_<lang>` keys, so a template can title itself in any UI
/// language without a new field per language.
#[derive(Default)]
struct Meta {
    name: String,
    desc: String,
    lang: String,
    category: String,
    engine: String,
    main: String,
    license: String,
    source: String,
    localized: BTreeMap<String, (String, String)>,
}

/// `Meta` from a restricted meta file; `None` when the file can't be
/// read, carries no `name`, or has a non-string value. Unknown keys are
/// ignored so newer fields stay compatible.
fn parse_meta(path: &Path) -> Option<Meta> {
    if std::fs::metadata(path).ok()?.len() > MAX_META_BYTES {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let mut meta = Meta::default();
    let mut name = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        match key {
            "name" if name.is_none() => name = Some(toml_string(value.trim())?),
            "desc" => meta.desc = toml_string(value.trim())?,
            "lang" => meta.lang = toml_string(value.trim())?,
            "category" => meta.category = toml_string(value.trim())?,
            "engine" => meta.engine = toml_string(value.trim())?,
            "main" => meta.main = toml_string(value.trim())?,
            "license" => meta.license = toml_string(value.trim())?,
            "source" => meta.source = toml_string(value.trim())?,
            _ => {
                if let Some(l) = key.strip_prefix("name_") {
                    meta.localized.entry(l.to_string()).or_default().0 = toml_string(value.trim())?;
                } else if let Some(l) = key.strip_prefix("desc_") {
                    meta.localized.entry(l.to_string()).or_default().1 = toml_string(value.trim())?;
                }
            }
        }
    }
    meta.name = name?;
    Some(meta)
}

struct Entry {
    id: String,
    name: String,
    desc: String,
    lang: String,
    category: String,
    engine: String,
    main: String,
    license: String,
    source: String,
    localized: BTreeMap<String, (String, String)>,
    /// Main `.tex` for flat entries, template root for folder entries.
    path: PathBuf,
    /// Relative source files for folder entries; one-item for flat.
    files: Vec<String>,
    user: bool,
}

fn normalized_rel(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn is_source_file(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| SOURCE_EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
}

fn valid_rel(path: &Path) -> bool {
    !path.as_os_str().is_empty() && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn collect_source_files(dir: &Path, rel: &Path, depth: u32, files: &mut Vec<PathBuf>, total: &mut u64) {
    if depth > MAX_IMPORT_DEPTH || files.len() >= MAX_TEMPLATE_FILES || *total >= MAX_IMPORT_TOTAL_BYTES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if files.len() >= MAX_TEMPLATE_FILES || *total >= MAX_IMPORT_TOTAL_BYTES {
            break;
        }
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_symlink() {
            continue;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            collect_source_files(&path, &rel.join(&name), depth + 1, files, total);
        } else if kind.is_file() && name != "meta.toml" && is_source_file(&path) {
            let len = entry.metadata().map(|m| m.len()).unwrap_or(u64::MAX);
            if len <= MAX_TEMPLATE_BYTES && *total + len <= MAX_IMPORT_TOTAL_BYTES {
                *total += len;
                files.push(rel.join(&name));
            }
        }
    }
}

fn infer_main(files: &[PathBuf], preferred: &str) -> String {
    if !preferred.is_empty() && files.iter().any(|f| normalized_rel(f) == preferred) {
        return preferred.to_string();
    }
    for candidate in ["main.tex", "main_skeleton.tex"] {
        if let Some(path) = files.iter().find(|f| normalized_rel(f) == candidate) {
            return normalized_rel(path);
        }
    }
    files
        .iter()
        .find(|f| f.parent().is_some_and(|p| p.as_os_str().is_empty()) && f.extension().is_some_and(|e| e.eq_ignore_ascii_case("tex")))
        .or_else(|| files.iter().find(|f| f.extension().is_some_and(|e| e.eq_ignore_ascii_case("tex"))))
        .or_else(|| files.first())
        .map(|f| normalized_rel(f))
        .unwrap_or_default()
}

fn entry_from_meta(id: String, meta: Meta, path: PathBuf, files: Vec<String>, user: bool) -> Entry {
    Entry {
        id,
        name: meta.name,
        desc: meta.desc,
        lang: meta.lang,
        category: meta.category,
        engine: meta.engine,
        main: meta.main,
        license: meta.license,
        source: meta.source,
        localized: meta.localized,
        path,
        files,
        user,
    }
}

fn scan_folder(dir: &Path, user: bool) -> Vec<Entry> {
    let Ok(read) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut metas: BTreeMap<String, Meta> = BTreeMap::new();
    let mut flat: BTreeSet<String> = BTreeSet::new();
    let mut folders: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for entry in read.flatten() {
        let path = entry.path();
        let Some(file_name) = entry.file_name().to_str().map(str::to_string) else { continue };
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_file() {
            if let Some(id) = file_name.strip_suffix(".meta.toml") {
                if valid_id(id) && metas.len() < MAX_TEMPLATES {
                    if let Some(meta) = parse_meta(&path) {
                        metas.insert(id.to_string(), meta);
                    }
                }
            } else if let Some(id) = file_name.strip_suffix(".tex") {
                if valid_id(id) && flat.len() < MAX_TEMPLATES {
                    flat.insert(id.to_string());
                }
            }
        } else if kind.is_dir() && valid_id(&file_name) && folders.len() < MAX_TEMPLATES {
            let mut total = 0;
            let mut files = Vec::new();
            collect_source_files(&path, Path::new(""), 0, &mut files, &mut total);
            if !files.is_empty() {
                folders.insert(file_name, files);
            }
        }
    }

    let mut ids: BTreeSet<String> = metas.keys().cloned().collect();
    ids.extend(flat.iter().cloned());
    ids.extend(folders.keys().cloned());
    ids.into_iter()
        .filter_map(|id| {
            let meta = folders
                .get(&id)
                .and_then(|_| parse_meta(&dir.join(&id).join("meta.toml")))
                .or_else(|| metas.remove(&id))
                .unwrap_or_else(|| Meta { name: id.clone(), ..Meta::default() });
            if let Some(files) = folders.remove(&id) {
                let names = files.iter().map(|f| normalized_rel(f)).collect::<Vec<_>>();
                let mut meta = meta;
                meta.main = infer_main(&files, &meta.main);
                Some(entry_from_meta(id.clone(), meta, dir.join(&id), names, user))
            } else if flat.contains(&id) {
                let file = format!("{id}.tex");
                let mut meta = meta;
                meta.main = file.clone();
                Some(entry_from_meta(id.clone(), meta, dir.join(file.clone()), vec![file], user))
            } else {
                None
            }
        })
        .collect()
}

/// Every gallery entry sorted by id; user templates override bundled ids.
fn scan() -> Vec<Entry> {
    let mut by_id = BTreeMap::new();
    for dir in template_dirs() {
        for entry in scan_folder(&dir.path, dir.user) {
            by_id.insert(entry.id.clone(), entry);
        }
    }
    by_id.into_values().collect()
}

fn find_entry(id: &str) -> Result<Entry, EngineError> {
    scan()
        .into_iter()
        .find(|e| e.id == id)
        .ok_or_else(|| EngineError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, format!("no template '{id}'"))))
}

fn template_file_path(entry: &Entry, rel: &str) -> Option<PathBuf> {
    if entry.files.len() == 1 && entry.path.is_file() {
        return Some(entry.path.clone());
    }
    let rel_path = Path::new(rel);
    valid_rel(rel_path).then(|| entry.path.join(rel_path))
}

fn read_limited(path: &Path) -> Result<String, EngineError> {
    if std::fs::metadata(path).map(|m| m.len()).unwrap_or(u64::MAX) > MAX_TEMPLATE_BYTES {
        return Err(EngineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("template file '{}' too large", path.display()))));
    }
    Ok(String::from_utf8_lossy(&std::fs::read(path)?).into_owned())
}

/// The entry plus its main source text (for `template.read` preview).
fn template_source(id: &str) -> Result<(Entry, String), EngineError> {
    let entry = find_entry(id)?;
    let path = template_file_path(&entry, &entry.main)
        .ok_or_else(|| EngineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("template '{id}' has an invalid main file"))))?;
    let content = read_limited(&path)?;
    Ok((entry, content))
}

fn localized_json(meta: &BTreeMap<String, (String, String)>) -> serde_json::Map<String, Value> {
    meta.iter().map(|(l, (n, d))| (l.clone(), json!({"name": n, "desc": d}))).collect()
}

fn entry_json(e: &Entry) -> Value {
    json!({"id": e.id, "name": e.name, "desc": e.desc, "lang": e.lang,
           "category": e.category, "engine": e.engine, "main": e.main,
           "license": e.license, "source": e.source, "origin": if e.user { "user" } else { "bundled" },
           "files": e.files, "localized": localized_json(&e.localized),
           "path": e.path.display().to_string()})
}

/// `template.list {}` → `{"templates": [{id,name,desc,lang,category,…}]}`.
/// Never fails on content — absent or unreadable dirs list empty.
pub fn list() -> Result<Value, EngineError> {
    let templates = scan().iter().map(entry_json).collect::<Vec<_>>();
    Ok(json!({"templates": templates}))
}

/// `template.read {"id"}` → full metadata plus the main source text.
pub fn read(params: &Value) -> Result<Value, EngineError> {
    let id = params.get("id").and_then(Value::as_str).filter(|s| valid_id(s)).ok_or(EngineError::BadParam("id"))?;
    let (entry, content) = template_source(id)?;
    let mut out = entry_json(&entry);
    out["content"] = Value::String(content);
    Ok(out)
}

/// `template.new {"id","path","force"?}` → `{"created","files"}`.
/// Flat templates write one `.tex`; folder templates copy every source
/// file under `path`'s parent, renaming only the main file to `path`.
/// Refuses to overwrite existing targets unless `"force": true`.
pub fn new(params: &Value) -> Result<Value, EngineError> {
    let id = params.get("id").and_then(Value::as_str).filter(|s| valid_id(s)).ok_or(EngineError::BadParam("id"))?;
    let path = params.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or(EngineError::BadParam("path"))?;
    let dest = Path::new(path);
    let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
    let entry = find_entry(id)?;

    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut writes: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut targets = BTreeSet::new();
    for rel in &entry.files {
        let target = if rel == &entry.main { dest.to_path_buf() } else { parent.join(rel) };
        if !targets.insert(target.clone()) {
            return Err(EngineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, "template destinations collide")));
        }
        let Some(src) = template_file_path(&entry, rel) else {
            return Err(EngineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("template '{id}' has an invalid file"))));
        };
        writes.push((src, target));
    }
    if !force {
        if let Some(existing) = writes.iter().find(|(_, target)| target.exists()) {
            return Err(EngineError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists — pass \"force\": true to overwrite", existing.1.display()),
            )));
        }
    }
    for (src, target) in &writes {
        if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        craft_atomic::atomic_write(target, read_limited(src)?.as_bytes())?;
    }
    Ok(json!({"created": dest.display().to_string(), "files": writes.len()}))
}

fn sanitize_id(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        let c = if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' };
        if !(c == '_' && out.ends_with('_')) {
            out.push(c);
        }
    }
    let out = out.trim_matches('_').chars().take(ID_MAX).collect::<String>();
    if out.is_empty() {
        "template".into()
    } else {
        out
    }
}

fn meta_line(out: &mut String, key: &str, value: &str) {
    let value = value.chars().take(MAX_META_VALUE).collect::<String>();
    if !value.is_empty() {
        out.push_str(&format!("{key} = \"{}\"\n", toml_escape(&value)));
    }
}

/// `template.import {"path","id"?,"name"?,"desc"?,"lang"?,"category"?,
/// "engine"?,"license"?,"main"?,"force"?}` copies a user-selected text
/// source file/folder into the per-user gallery. Binary assets, PDFs,
/// fonts, logs, build outputs, symlinks and hidden dirs are skipped.
pub fn import(params: &Value) -> Result<Value, EngineError> {
    let source = params.get("path").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or(EngineError::BadParam("path"))?;
    let source = Path::new(source);
    if !source.exists() {
        return Err(EngineError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("template source '{}' does not exist", source.display()),
        )));
    }
    let raw_id = params
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| source.file_stem().or_else(|| source.file_name()).and_then(|n| n.to_str()).map(str::to_string))
        .ok_or(EngineError::BadParam("id"))?;
    let id = params.get("id").and_then(Value::as_str).map_or_else(|| sanitize_id(&raw_id), |id| id.to_string());
    if !valid_id(&id) {
        return Err(EngineError::BadParam("id"));
    }

    let mut files = Vec::new();
    if source.is_file() {
        if !is_source_file(source) {
            return Err(EngineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidInput, "template source must be a text LaTeX source file")));
        }
        if source.metadata().map(|m| m.len()).unwrap_or(u64::MAX) > MAX_TEMPLATE_BYTES {
            return Err(EngineError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, "template source too large")));
        }
        files.push(source.file_name().map(PathBuf::from).unwrap_or_else(|| PathBuf::from("main.tex")));
    } else {
        let mut total = 0;
        collect_source_files(source, Path::new(""), 0, &mut files, &mut total);
    }
    if files.is_empty() {
        return Err(EngineError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, "no text template sources found")));
    }

    let user_dir = user_templates_dir().ok_or_else(no_templates_dir)?;
    let dest_dir = user_dir.join(&id);
    let force = params.get("force").and_then(Value::as_bool).unwrap_or(false);
    if dest_dir.exists() {
        if !force {
            return Err(EngineError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("template '{id}' already exists — pass \"force\": true to replace its files"),
            )));
        }
        if dest_dir.is_dir() {
            std::fs::remove_dir_all(&dest_dir)?;
        } else {
            std::fs::remove_file(&dest_dir)?;
        }
    }
    std::fs::create_dir_all(&dest_dir)?;
    for rel in &files {
        if !valid_rel(rel) {
            continue;
        }
        let src = if source.is_file() { source.to_path_buf() } else { source.join(rel) };
        let dest = dest_dir.join(rel);
        if let Some(parent) = dest.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        craft_atomic::atomic_write(&dest, read_limited(&src)?.as_bytes())?;
    }

    let get = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
    let name = {
        let v = get("name");
        if v.is_empty() {
            id.clone()
        } else {
            v
        }
    };
    let main = infer_main(&files, &get("main"));
    let mut meta = String::new();
    meta_line(&mut meta, "name", &name);
    meta_line(&mut meta, "desc", &get("desc"));
    meta_line(&mut meta, "lang", &get("lang"));
    meta_line(&mut meta, "category", &get("category"));
    meta_line(&mut meta, "engine", &get("engine"));
    meta_line(&mut meta, "main", &main);
    meta_line(&mut meta, "license", &get("license"));
    meta_line(&mut meta, "source", &source.display().to_string());
    craft_atomic::atomic_write(&dest_dir.join("meta.toml"), meta.as_bytes())?;
    Ok(json!({"id": id, "dir": dest_dir.display().to_string(), "files": files.len()}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Serialise tests that flip template env vars — env is process-wide.
    static ENV_LOCK: Mutex<()> = Mutex::new(());
    static NEXT_DIR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let n = NEXT_DIR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("warraqcraft-templates-test-{}-{tag}-{n}", std::process::id()));
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
    fn bundled_template_candidates_cover_installed_layouts() {
        let mac = bundled_template_candidates(Path::new("/Applications/WarraqCraft.app/Contents/MacOS/warraqcraft"));
        assert!(mac.iter().any(|p| p.ends_with(Path::new("Resources").join("templates"))));
        let linux = bundled_template_candidates(Path::new("/usr/bin/warraqcraft"));
        assert!(linux.iter().any(|p| p.ends_with(Path::new("share").join("warraqcraft").join("templates"))));
    }

    #[test]
    fn list_read_and_new_over_tempdir() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new("gallery");
        std::fs::write(dir.0.join("alpha.tex"), "\\documentclass{article}\n").unwrap();
        std::fs::write(dir.0.join("alpha.meta.toml"), "name = \"Alpha doc\"\ndesc = \"A starter.\"\ncategory = \"starter\"\nengine = \"pdflatex\"\n")
            .unwrap();
        std::fs::write(dir.0.join("plain.tex"), "\\documentclass{report}\n").unwrap();
        std::fs::write(dir.0.join("bad.tex"), "x\n").unwrap();
        std::fs::write(dir.0.join("bad.meta.toml"), "name = not-a-string\n").unwrap();
        std::env::set_var("WARRAQCRAFT_TEMPLATES", &dir.0);

        let mut s = crate::Session::new();
        let out = s.execute("template.list", &json!({})).unwrap();
        let list = out["templates"].as_array().unwrap();
        assert_eq!(list.len(), 3);
        let alpha = list.iter().find(|t| t["id"] == "alpha").unwrap();
        assert_eq!(alpha["name"], "Alpha doc");
        assert_eq!(alpha["desc"], "A starter.");
        assert_eq!(alpha["category"], "starter");
        assert_eq!(alpha["engine"], "pdflatex");
        let plain = list.iter().find(|t| t["id"] == "plain").unwrap();
        assert_eq!(plain["name"], "plain");
        assert_eq!(plain["desc"], "");
        assert_eq!(list.iter().find(|t| t["id"] == "bad").unwrap()["name"], "bad");

        let read = s.execute("template.read", &json!({"id": "alpha"})).unwrap();
        assert_eq!(read["name"], "Alpha doc");
        assert!(read["content"].as_str().unwrap().contains("\\documentclass"));

        let dest = dir.0.join("sub").join("doc.tex");
        let out = s.execute("template.new", &json!({"id": "alpha", "path": dest})).unwrap();
        assert!(out["created"].as_str().unwrap().ends_with("doc.tex"));
        assert!(dest.is_file());
        assert!(s.execute("template.new", &json!({"id": "alpha", "path": dest})).is_err());
        s.execute("template.new", &json!({"id": "alpha", "path": dest, "force": true})).unwrap();

        std::env::remove_var("WARRAQCRAFT_TEMPLATES");
    }

    #[test]
    fn folder_templates_copy_the_whole_text_source_set() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new("folders");
        let tpl = dir.0.join("book");
        std::fs::create_dir_all(tpl.join("chapters")).unwrap();
        std::fs::write(tpl.join("main.tex"), "\\input{chapters/one}\n").unwrap();
        std::fs::write(tpl.join("chapters").join("one.tex"), "One\n").unwrap();
        std::fs::write(tpl.join("refs.bib"), "@book{x,title={X}}\n").unwrap();
        std::fs::write(tpl.join("out.pdf"), b"not text").unwrap();
        std::fs::write(tpl.join("meta.toml"), "name = \"Book\"\nlang = \"en\"\nmain = \"main.tex\"\n").unwrap();
        std::env::set_var("WARRAQCRAFT_TEMPLATES", &dir.0);

        let mut s = crate::Session::new();
        let list = s.execute("template.list", &json!({})).unwrap();
        let book = &list["templates"].as_array().unwrap()[0];
        assert_eq!(book["id"], "book");
        assert_eq!(book["files"].as_array().unwrap().len(), 3);
        assert_eq!(book["main"], "main.tex");

        let dest = dir.0.join("out").join("novel.tex");
        let out = s.execute("template.new", &json!({"id": "book", "path": dest})).unwrap();
        assert_eq!(out["files"], 3);
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "\\input{chapters/one}\n");
        assert_eq!(std::fs::read_to_string(dir.0.join("out/chapters/one.tex")).unwrap(), "One\n");
        assert!(dir.0.join("out/refs.bib").is_file());
        assert!(!dir.0.join("out/out.pdf").exists());
        std::env::remove_var("WARRAQCRAFT_TEMPLATES");
    }

    #[test]
    fn import_copies_text_sources_and_records_metadata() {
        let _guard = ENV_LOCK.lock().unwrap();
        let src = TempDir::new("import-src");
        let user = TempDir::new("import-user");
        std::fs::create_dir_all(src.0.join("chapters")).unwrap();
        std::fs::write(src.0.join("main.tex"), "\\input{chapters/one}\n").unwrap();
        std::fs::write(src.0.join("chapters/one.tex"), "One\n").unwrap();
        std::fs::write(src.0.join("main.pdf"), b"pdf").unwrap();
        std::fs::write(src.0.join("font.ttf"), b"font").unwrap();
        std::env::set_var("WARRAQCRAFT_USER_TEMPLATES", &user.0);

        let mut s = crate::Session::new();
        let out = s.execute(
            "template.import",
            &json!({"path": src.0, "id": "community_book", "name": "Community Book", "lang": "ar", "category": "community", "license": "CC-BY-NC-4.0"}),
        ).unwrap();
        assert_eq!(out["id"], "community_book");
        assert_eq!(out["files"], 2);
        assert!(user.0.join("community_book/main.tex").is_file());
        assert!(user.0.join("community_book/chapters/one.tex").is_file());
        assert!(!user.0.join("community_book/main.pdf").exists());
        let meta = std::fs::read_to_string(user.0.join("community_book/meta.toml")).unwrap();
        assert!(meta.contains("CC-BY-NC-4.0"));

        // Replacing an import removes files that are no longer present;
        // otherwise stale sources silently become part of the template.
        std::fs::write(user.0.join("community_book/stale.tex"), "stale\n").unwrap();
        s.execute("template.import", &json!({"path": src.0, "id": "community_book", "name": "Community Book", "force": true})).unwrap();
        assert!(!user.0.join("community_book/stale.tex").exists());

        std::env::set_var("WARRAQCRAFT_TEMPLATES", &user.0);
        let read = s.execute("template.read", &json!({"id": "community_book"})).unwrap();
        assert!(read["content"].as_str().unwrap().contains("\\input"));
        std::env::remove_var("WARRAQCRAFT_TEMPLATES");
        std::env::remove_var("WARRAQCRAFT_USER_TEMPLATES");
    }

    #[test]
    fn hostile_ids_rejected() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = TempDir::new("ids");
        std::fs::write(dir.0.join("ok.tex"), "x\n").unwrap();
        std::env::set_var("WARRAQCRAFT_TEMPLATES", &dir.0);
        let mut s = crate::Session::new();
        for bad in ["../evil", "a/b", "a\\b", "", &"x".repeat(65)] {
            assert!(matches!(s.execute("template.read", &json!({"id": bad})), Err(EngineError::BadParam("id"))), "id {bad:?}");
            assert!(
                matches!(s.execute("template.new", &json!({"id": bad, "path": dir.0.join("o.tex")})), Err(EngineError::BadParam("id"))),
                "id {bad:?}"
            );
        }
        assert!(matches!(s.execute("template.read", &json!({"id": "ghost"})), Err(EngineError::Io(_))));
        assert!(matches!(s.execute("template.new", &json!({"id": "ok"})), Err(EngineError::BadParam("path"))));
        assert!(matches!(s.execute("template.import", &json!({"path": dir.0, "id": "../evil"})), Err(EngineError::BadParam("id"))));
        std::env::remove_var("WARRAQCRAFT_TEMPLATES");
    }
}
