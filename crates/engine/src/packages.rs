//! `pkg.*` — package registry, project manifest, vendor dir and
//! install paths (TASK-31).
//!
//! WarraqCraft's answer to "the package my document needs isn't
//! installed": a per-project manifest (`.warraqcraft/packages.toml`),
//! an optional `vendor/` tree that compiles expose via `TEXMFHOME`,
//! and install commands that prefer the system TeX's own package
//! manager (`tlmgr`/`miktex`) before falling back to vendoring files
//! fetched from CTAN.
//!
//! Layers: this module lives in `engine` (L5) — it reads the project
//! graph through `texparse`, the registry through `texpkg`, and shells
//! out only to probed toolchain binaries. No panics on hostile input:
//! manifests and zip archives are untrusted.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::{EngineError, Session};

/// `.warraqcraft/` under the document's directory — same root the
/// compile pipeline uses for its build dir.
pub fn project_dir(parent: &Path) -> PathBuf {
    parent.join(crate::BUILD_DIR)
}

/// `vendor/` — extracted CTAN `.tds.zip` trees land under
/// `vendor/texmf/` — extracted CTAN `.tds.zip` trees land here as a
/// TDS root so `TEXMFHOME` has one stable tree to search.
pub fn vendor_dir(parent: &Path) -> PathBuf {
    project_dir(parent).join("vendor")
}

fn manifest_path(parent: &Path) -> PathBuf {
    project_dir(parent).join("packages.toml")
}

fn path_sep() -> &'static str {
    if cfg!(windows) {
        ";"
    } else {
        ":"
    }
}

// ---------- manifest ----------

/// One `[packages]` entry: how the package is provided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestEntry {
    /// `\usepackage`/`\documentclass` name.
    pub name: String,
    /// `"ctan"` (installed into the system TeX) or `"vendor"` (files in
    /// `vendor/`).
    pub source: String,
    /// CTAN/package version when known (informational only — TeX has
    /// no runtime version pinning).
    pub version: Option<String>,
    /// Fetch provenance for vendored entries: CTAN zip URL + sha256.
    pub url: Option<String>,
    pub sha256: Option<String>,
}

/// Parse `.warraqcraft/packages.toml`. A missing file is an empty
/// manifest; a malformed one is an error — silently ignoring it would
/// hide the user's pinning intent.
pub fn load_manifest(parent: &Path) -> Result<Vec<ManifestEntry>, EngineError> {
    let path = manifest_path(parent);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(EngineError::Io(e)),
    };
    let value: toml::Table = text.parse().map_err(|e| EngineError::Tool(format!("packages.toml: {e}")))?;
    let mut out = Vec::new();
    if let Some(table) = value.get("packages").and_then(toml::Value::as_table) {
        for (name, spec) in table {
            let source = spec.get("source").and_then(toml::Value::as_str).unwrap_or("ctan").to_string();
            out.push(ManifestEntry {
                name: name.clone(),
                source,
                version: spec.get("version").and_then(toml::Value::as_str).map(str::to_string),
                url: spec.get("url").and_then(toml::Value::as_str).map(str::to_string),
                sha256: spec.get("sha256").and_then(toml::Value::as_str).map(str::to_string),
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Write the manifest atomically (via craft-atomic, like other state
/// files). Entries are sorted by name for stable diffs.
pub fn save_manifest(parent: &Path, entries: &[ManifestEntry]) -> Result<(), EngineError> {
    let mut sorted: Vec<&ManifestEntry> = entries.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut text = String::from("# WarraqCraft package manifest — see docs/TASK-31-package-registry.md\n[packages]\n");
    for e in sorted {
        let mut parts = vec![format!("source = \"{}\"", e.source)];
        if let Some(v) = &e.version {
            parts.push(format!("version = \"{v}\""));
        }
        if let Some(u) = &e.url {
            parts.push(format!("url = \"{}\"", u.replace('"', "")));
        }
        if let Some(s) = &e.sha256 {
            parts.push(format!("sha256 = \"{s}\""));
        }
        text.push_str(&format!("{} = {{ {} }}\n", e.name, parts.join(", ")));
    }
    let path = manifest_path(parent);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    craft_atomic::atomic_write(&path, text.as_bytes())?;
    Ok(())
}

/// Insert or replace the entry for `name` in the manifest on disk.
pub fn upsert_manifest(parent: &Path, entry: ManifestEntry) -> Result<(), EngineError> {
    let mut entries = load_manifest(parent)?;
    entries.retain(|e| e.name != entry.name);
    entries.push(entry);
    save_manifest(parent, &entries)
}

// ---------- vendor / TEXMFHOME ----------

/// Env vars pointing TeX engines at `vendor/texmf/`:
///
/// - `TEXINPUTS=<root>/tex//` — recursive `.sty`/`.cls`/`.tex`
///   resolution; honored by MiKTeX *and* web2c/kpathsea engines.
///   The user's own `TEXINPUTS` is preserved after ours.
/// - `TEXMFHOME=<root>` — exposes the whole TDS tree (tex/, fonts/,
///   doc/) to web2c/kpathsea engines (TeX Live, TinyTeX). Set only
///   when the user hasn't one already; MiKTeX ignores it (its file
///   DB only covers registered roots), which is why `TEXINPUTS`
///   carries the loadable-file path there.
///
/// Engines without kpathsea (Tectonic's fixed bundle) ignore both —
/// vendored packages are a system-TeX feature.
///
/// Returns an empty vec when there is no vendor tree.
pub fn tex_env(parent: &Path) -> Vec<(&'static str, std::ffi::OsString)> {
    let root = vendor_dir(parent).join("texmf");
    if !root.is_dir() {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(2);

    let mut ti = root.join("tex").into_os_string();
    ti.push("//"); // kpathsea: `//` = recursive search under this root
    ti.push(path_sep()); // trailing separator appends the default path
    if let Some(existing) = std::env::var_os("TEXINPUTS") {
        ti.push(existing);
    }
    out.push(("TEXINPUTS", ti));

    if std::env::var_os("TEXMFHOME").is_none() {
        out.push(("TEXMFHOME", root.into_os_string()));
    }
    out
}

// ---------- requested packages ----------

/// `\usepackage`/`\RequirePackage`/`\documentclass` names across the
/// project's `\input`/`\include` graph — the "requested" side of
/// `pkg.list`/`pkg.audit`. Mirrors `project.structure`'s walk: the open
/// file is parsed from the live buffer, the rest are capped reads.
pub fn requested(sess: &Session) -> Vec<String> {
    let Some(open) = sess.path() else { return Vec::new() };
    let dir = open.parent().unwrap_or_else(|| Path::new("."));
    let magic = texparse::parse_magic(sess.source());
    let entry = magic.root.as_deref().map(|r| dir.join(r)).filter(|c| c != open && c.is_file()).unwrap_or_else(|| open.to_path_buf());
    let g = texparse::graph::build(&entry);
    let open_canon = canon(open);

    let mut out: Vec<String> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut push = |name: &str| {
        if seen.insert(name.to_string()) {
            out.push(name.to_string());
        }
    };
    for f in g.files.iter().take(128) {
        let d = if canon(f) == open_canon {
            texparse::parse_structure(sess.source())
        } else {
            let mut buf = Vec::new();
            match std::fs::File::open(f).map(|h| h.take(1024 * 1024).read_to_end(&mut buf)) {
                Ok(Ok(_)) => texparse::parse_structure(&String::from_utf8_lossy(&buf)),
                _ => continue,
            }
        };
        if let Some((cls, _)) = &d.class {
            push(cls);
        }
        for (p, _) in &d.packages {
            push(p);
        }
    }
    out
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Does `name` resolve on the TeX search path? Tries `kpsewhich`
/// across the file kinds a package can ship (`.sty`, `.cls`, `.tex`,
/// `.def`, `.cfg` — plain-TeX packages like `plipsum` have no `.sty`).
/// Returns the first resolved path; the caller treats `None` as
/// "not visible to TeX".
#[cfg(not(target_arch = "wasm32"))]
pub fn kpsewhich(parent: &Path, name: &str) -> Option<String> {
    let tool = crate::toolchain::find_program("kpsewhich")?;
    for ext in ["sty", "cls", "tex", "def", "cfg"] {
        let file = format!("{name}.{ext}");
        let out = std::process::Command::new(&tool).arg(&file).current_dir(parent).output().ok()?;
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !s.is_empty() {
                return Some(s);
            }
        }
    }
    None
}

/// Vendored files for `name`: any file under `vendor/texmf/**` whose
/// basename matches `<name>.<ext>` for a loadable TeX file kind.
pub fn vendored_files(parent: &Path, name: &str) -> Vec<PathBuf> {
    let mut hits = Vec::new();
    let root = vendor_dir(parent).join("texmf");
    let mut stack = vec![root.clone()];
    let mut depth = 0usize;
    while let Some(dir) = stack.pop() {
        depth += 1;
        if depth > 10_000 {
            break;
        }
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for ent in rd.flatten().take(4096) {
            let p = ent.path();
            if p.is_dir() {
                stack.push(p);
            } else if let Some(fname) = p.file_name().and_then(|n| n.to_str()) {
                if ["sty", "cls", "tex", "def", "cfg"].iter().any(|e| fname == format!("{name}.{e}")) {
                    hits.push(p);
                }
            }
        }
    }
    hits.sort();
    hits
}

// ---------- commands ----------

fn parent_of(sess: &Session) -> Result<PathBuf, EngineError> {
    let p = sess.path().ok_or(EngineError::NoFile)?;
    // `--file main.tex` leaves an empty/relative parent — absolutize so
    // `current_dir` spawns (kpsewhich) and `.warraqcraft/` paths anchor
    // to the document, not to whatever cwd the caller happened to use.
    let parent = p.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    std::path::absolute(parent).map_err(|e| EngineError::Tool(format!("project dir: {e}")))
}

/// `pkg.list` — requested packages/classes with resolution status.
pub fn list(sess: &Session) -> Result<Value, EngineError> {
    let parent = parent_of(sess)?;
    let manifest = load_manifest(&parent)?;
    let requested = requested(sess);
    let mut names: Vec<String> = requested.clone();
    for m in &manifest {
        if !names.contains(&m.name) {
            names.push(m.name.clone());
        }
    }
    names.sort();
    names.dedup();
    let packages = names
        .iter()
        .map(|n| {
            let vendored = !vendored_files(&parent, n).is_empty();
            #[cfg(not(target_arch = "wasm32"))]
            let installed = kpsewhich(&parent, n).is_some();
            #[cfg(target_arch = "wasm32")]
            let installed = false;
            let man = manifest.iter().find(|m| m.name == *n);
            let reg = texpkg::resolve(n);
            json!({
                "name": n,
                "requested": requested.iter().any(|r| r == n),
                "vendored": vendored,
                "installed": installed || vendored,
                "source": man.map(|m| m.source.clone()),
                "registry": reg.map(|p| json!({"summary": p.summary, "files": p.files})),
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({"packages": packages, "manifest": manifest_path(&parent).display().to_string()}))
}

/// `pkg.audit` — requested packages that resolve nowhere: not in the
/// manifest as vendored, not on the TeX search path.
pub fn audit(sess: &Session) -> Result<Value, EngineError> {
    let parent = parent_of(sess)?;
    let manifest = load_manifest(&parent)?;
    let requested = requested(sess);
    let mut missing = Vec::new();
    for n in &requested {
        let vendored = manifest.iter().any(|m| m.name == *n && m.source == "vendor") || !vendored_files(&parent, n).is_empty();
        #[cfg(not(target_arch = "wasm32"))]
        let installed = kpsewhich(&parent, n).is_some();
        #[cfg(target_arch = "wasm32")]
        let installed = false;
        if !vendored && !installed {
            missing.push(json!({
                "name": n,
                "registry": texpkg::resolve(n).map(|p| p.summary),
            }));
        }
    }
    Ok(json!({"missing": missing}))
}

/// `pkg.resolve` — registry lookup for one name or file.
pub fn resolve_cmd(_sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let name = params.get("name").and_then(Value::as_str).ok_or(EngineError::BadParam("name"))?;
    match texpkg::resolve(name) {
        Some(p) => Ok(json!({
            "name": p.name,
            "files": p.files,
            "summary": p.summary,
        })),
        None => Ok(json!({"name": name, "known": false})),
    }
}

/// `pkg.search` — registry substring search.
pub fn search_cmd(_sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let q = params.get("query").and_then(Value::as_str).unwrap_or("");
    let hits: Vec<Value> = texpkg::search(q).take(50).map(|p| json!({"name": p.name, "summary": p.summary})).collect();
    Ok(json!({"results": hits}))
}

// ---------- install (22d) ----------

/// `pkg.install {name}` — install through the system TeX's own package
/// manager: `tlmgr` (TeX Live/TinyTeX) first, then `miktex packages install`/`mpm` (MiKTeX).
/// Verifies with `kpsewhich` afterwards; records `source = "ctan"` in
/// the manifest on success. When no manager exists (notably a
/// Tectonic-only setup) the honest answer is `pkg.vendor`.
#[cfg(not(target_arch = "wasm32"))]
pub fn install(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let name = params.get("name").and_then(Value::as_str).ok_or(EngineError::BadParam("name"))?;
    let parent = parent_of(sess)?;

    if kpsewhich(&parent, name).is_some() || !vendored_files(&parent, name).is_empty() {
        return Ok(json!({"name": name, "installed": true, "via": "already-present"}));
    }

    let (hit, mut tried) = try_managers(sess, &parent, name)?;
    if let Some(v) = hit {
        return Ok(v);
    }

    // Name-mapping fallback: CTAN's own `miktex`/`texlive` fields cover
    // packages whose manager name differs from the request
    // (e.g. graphicx → graphics). Only consulted after the direct
    // attempt fails — the common case never touches the network here.
    let ctan_name = texpkg::resolve(name).map(|p| p.name).unwrap_or(name);
    if let Ok(bytes) = http_get(&format!("https://www.ctan.org/json/pkg/{ctan_name}")) {
        if let Ok(meta) = serde_json::from_slice::<Value>(&bytes) {
            let mut alts: Vec<String> = Vec::new();
            for key in ["miktex", "texlive"] {
                if let Some(v) = meta.get(key).and_then(Value::as_str) {
                    if v != name && !alts.iter().any(|a| a == v) {
                        alts.push(v.to_string());
                    }
                }
            }
            for alt in alts {
                let (h2, t2) = try_managers(sess, &parent, &alt)?;
                tried.extend(t2);
                if let Some(v) = h2 {
                    return Ok(v);
                }
            }
        }
    }

    if tried.is_empty() {
        return Err(EngineError::Tool(format!(
            "no system package manager found (tlmgr/miktex) — use pkg.vendor for '{name}' or install TeX Live/MiKTeX"
        )));
    }
    Err(EngineError::Tool(format!("package manager install failed for '{name}': {tried:?}")))
}

/// One pass over the probed package managers for `name`. Returns the
/// success payload plus the per-tool attempt log either way.
#[cfg(not(target_arch = "wasm32"))]
fn try_managers(sess: &Session, parent: &Path, name: &str) -> Result<(Option<Value>, Vec<Value>), EngineError> {
    // tlmgr covers TeX Live/TinyTeX; `miktex packages install` is the
    // current MiKTeX CLI; `mpm --install=` is the deprecated legacy
    // spelling kept for older installs.
    let mut tried = Vec::new();
    for tool in ["tlmgr", "miktex", "mpm"] {
        let Some(path) = crate::toolchain::find_program(tool) else { continue };
        let mut argv: Vec<String> = vec![path.display().to_string()];
        match tool {
            "tlmgr" => argv.extend(["install".to_string(), name.to_string()]),
            "miktex" => argv.extend(["packages".to_string(), "install".to_string(), name.to_string()]),
            _ => argv.push(format!("--install={name}")),
        }
        let res = crate::tools::run(sess, &json!({"argv": argv, "timeout_s": 300}))?;
        let ok = res.get("status").and_then(Value::as_i64) == Some(0) && !res.get("timed_out").and_then(Value::as_bool).unwrap_or(false);
        tried.push(json!({"tool": tool, "ok": ok}));
        // MiKTeX refreshes its file-name DB asynchronously after install —
        // poll briefly instead of declaring failure on the first miss.
        let mut verified = false;
        if ok {
            for _ in 0..20 {
                if kpsewhich(parent, name).is_some() {
                    verified = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        }
        if verified {
            upsert_manifest(parent, ManifestEntry { name: name.to_string(), source: "ctan".to_string(), version: None, url: None, sha256: None })?;
            return Ok((Some(json!({"name": name, "installed": true, "via": tool, "tried": tried})), tried));
        }
    }
    Ok((None, tried))
}

#[cfg(target_arch = "wasm32")]
pub fn install(_sess: &Session, _params: &Value) -> Result<Value, EngineError> {
    Err(EngineError::Tool("pkg.install is not supported on wasm".into()))
}

// ---------- vendor fetch (22e) ----------

#[cfg(not(target_arch = "wasm32"))]
const HTTP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// Largest CTAN zip accepted — TDS packages are small; this only stops
/// a hostile/mirror-error body from filling the disk.
#[cfg(not(target_arch = "wasm32"))]
const MAX_ZIP_BYTES: u64 = 64 * 1024 * 1024;
/// Largest total extracted payload kept — zip-bomb guard.
#[cfg(not(target_arch = "wasm32"))]
const MAX_EXTRACT_BYTES: u64 = 256 * 1024 * 1024;

#[cfg(not(target_arch = "wasm32"))]
fn http_get(url: &str) -> Result<Vec<u8>, EngineError> {
    let config = ureq::config::Config::builder().timeout_global(Some(HTTP_TIMEOUT)).build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent.get(url).call().map_err(|e| EngineError::Tool(format!("get {url}: {e}")))?;
    let mut buf = Vec::new();
    resp.body_mut().as_reader().take(MAX_ZIP_BYTES).read_to_end(&mut buf)?;
    Ok(buf)
}

/// `pkg.vendor {name}` — fetch the package's CTAN `.tds.zip` and
/// extract the whole TDS tree into `vendor/texmf/` (shared texmf root,
/// `TEXMFHOME`-exposed at compile — tex/, fonts/, all of it). Records
/// URL + sha256 in the
/// manifest. Requires the CTAN `install` field — packages without one
/// (e.g. biblatex, enumitem) need `pkg.install` on a system TeX.
#[cfg(not(target_arch = "wasm32"))]
pub fn vendor(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let name = params.get("name").and_then(Value::as_str).ok_or(EngineError::BadParam("name"))?;
    let parent = parent_of(sess)?;
    let ctan_name = texpkg::resolve(name).map(|p| p.name).unwrap_or(name);

    let meta_bytes = http_get(&format!("https://www.ctan.org/json/pkg/{ctan_name}"))?;
    let meta: Value = serde_json::from_slice(&meta_bytes).map_err(|e| EngineError::Tool(format!("ctan json: {e}")))?;
    let install_path = meta
        .get("install")
        .and_then(Value::as_str)
        .ok_or_else(|| EngineError::Tool(format!("CTAN package '{ctan_name}' has no install zip — use pkg.install on a system TeX")))?;
    if !install_path.ends_with(".zip") {
        return Err(EngineError::Tool(format!("unexpected CTAN install path '{install_path}'")));
    }
    // CTAN serves install archives under /install/, not the
    // tex-archive root — the JSON `install` field is relative to it.
    let url = format!("https://mirrors.ctan.org/install{install_path}");
    let bytes = http_get(&url)?;
    if bytes.len() as u64 > MAX_ZIP_BYTES {
        return Err(EngineError::Tool("CTAN zip exceeds size cap".into()));
    }
    use sha2::Digest;
    let sha256 = hex_lower(&sha2::Sha256::digest(&bytes));

    // Extract the whole TDS tree into the shared vendor/texmf root —
    // tex/, fonts/, everything. `TEXMFHOME` resolves all of it, so
    // packages shipping fonts work too. `enclosed_name` blocks zip-slip.
    let dest = vendor_dir(&parent).join("texmf");
    let (count, written) = extract_tds(&bytes, &dest, ctan_name)?;

    upsert_manifest(
        &parent,
        ManifestEntry {
            name: ctan_name.to_string(),
            source: "vendor".to_string(),
            version: meta.pointer("/version/number").and_then(Value::as_str).map(str::to_string),
            url: Some(url.clone()),
            sha256: Some(sha256.clone()),
        },
    )?;
    Ok(json!({
        "name": ctan_name,
        "vendored": true,
        "files": count,
        "bytes": written,
        "sha256": sha256,
        "url": url,
    }))
}

#[cfg(target_arch = "wasm32")]
pub fn vendor(_sess: &Session, _params: &Value) -> Result<Value, EngineError> {
    Err(EngineError::Tool("pkg.vendor is not supported on wasm".into()))
}

/// TDS top-level categories a vendored package may contribute.
/// Anything else in the archive (README, scripts at root, ...) is skipped.
#[cfg(not(target_arch = "wasm32"))]
const TDS_ROOTS: &[&str] = &[
    "tex",
    "fonts",
    "doc",
    "source",
    "scripts",
    "bibtex",
    "makeindex",
    "metapost",
    "metafont",
    "tex4ht",
    "omega",
    "pdftex",
    "xelatex",
    "luatex",
    "etex",
    "hitex",
    "ptex",
    "uptex",
    "euptex",
    "platex",
    "uplatex",
    "generic",
    "tpm",
];

/// Unpack a CTAN `.tds.zip` into `dest` (a TDS root, i.e.
/// `vendor/texmf`). Every TDS-rooted entry is kept — tex/,
/// fonts/, doc/ — since `TEXMFHOME` resolves the whole tree. Entries
/// outside that root, directories, and zip-slip paths are skipped.
/// Returns `(file_count, bytes_written)`; errors on an empty result or
/// when the size/count caps are exceeded.
#[cfg(not(target_arch = "wasm32"))]
fn extract_tds(bytes: &[u8], dest: &Path, ctan_name: &str) -> Result<(u64, u64), EngineError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| EngineError::Tool(format!("zip: {e}")))?;
    let mut written = 0u64;
    let mut count = 0u64;
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| EngineError::Tool(format!("zip entry {i}: {e}")))?;
        if file.is_dir() {
            continue;
        }
        let Some(rel) = file.enclosed_name() else { continue };
        // Two layouts exist in the wild: legacy zips root at
        // `texmf-dist/`, current `/install/` archives are already
        // TDS-rooted (`tex/`, `fonts/`, ...). Normalize, then keep only
        // standard TDS top-level categories.
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let stripped = rel_str.strip_prefix("texmf-dist/").unwrap_or(rel_str.as_str());
        let top = stripped.split('/').next().unwrap_or("");
        if !TDS_ROOTS.contains(&top) {
            continue;
        }
        let out = dest.join(stripped);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut w = std::fs::File::create(&out)?;
        let n = std::io::copy(&mut file, &mut w)?;
        written += n;
        count += 1;
        if written > MAX_EXTRACT_BYTES || count > 100_000 {
            return Err(EngineError::Tool("CTAN zip extraction exceeds caps".into()));
        }
    }
    if count == 0 {
        return Err(EngineError::Tool(format!("'{ctan_name}' zip had no TDS files")));
    }
    Ok((count, written))
}

#[cfg(not(target_arch = "wasm32"))]
fn hex_lower(digest: &[u8]) -> String {
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trip() {
        let dir = std::env::temp_dir().join(format!("wc-pkg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let entries = vec![
            ManifestEntry { name: "amsmath".into(), source: "ctan".into(), version: Some("2.17".into()), url: None, sha256: None },
            ManifestEntry {
                name: "mypkg".into(),
                source: "vendor".into(),
                version: None,
                url: Some("https://x/y.zip".into()),
                sha256: Some("ab".into()),
            },
        ];
        save_manifest(&dir, &entries).unwrap();
        let back = load_manifest(&dir).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "amsmath");
        assert_eq!(back[1].sha256.as_deref(), Some("ab"));
        assert!(manifest_path(&dir).ends_with("packages.toml"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn manifest_missing_is_empty_and_bad_is_error() {
        let dir = std::env::temp_dir().join(format!("wc-pkg-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(load_manifest(&dir).unwrap().is_empty());
        std::fs::create_dir_all(project_dir(&dir)).unwrap();
        std::fs::write(manifest_path(&dir), "not = [valid").unwrap();
        assert!(load_manifest(&dir).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tex_env_empty_without_vendor() {
        let dir = std::env::temp_dir().join(format!("wc-pkg-ti-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert!(tex_env(&dir).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extract_tds_keeps_whole_tree_and_blocks_slip() {
        use std::io::Write;
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opt = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            for (name, body) in [
                ("texmf-dist/tex/latex/foo/foo.sty", b"sty".as_slice()),
                ("texmf-dist/fonts/type1/foo/foo.pfb", b"pfb".as_slice()),
                ("texmf-dist/doc/foo/foo.pdf", b"pdf".as_slice()),
                ("texmf-dist/../../evil.sty", b"evil".as_slice()),
                ("other-root/file.sty", b"skip".as_slice()),
                ("tex/latex/bar/bar.sty", b"new".as_slice()),
                ("README.txt", b"root".as_slice()),
            ] {
                w.start_file(name, opt).unwrap();
                w.write_all(body).unwrap();
            }
            w.finish().unwrap();
        }
        let dest = std::env::temp_dir().join(format!("wc-pkg-tds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dest);
        let (count, written) = extract_tds(buf.get_ref(), &dest, "foo").unwrap();
        assert_eq!(count, 4);
        assert_eq!(written, 12);
        assert!(dest.join("tex/latex/foo/foo.sty").is_file());
        assert!(dest.join("fonts/type1/foo/foo.pfb").is_file());
        assert!(dest.join("doc/foo/foo.pdf").is_file());
        assert!(dest.join("tex/latex/bar/bar.sty").is_file());
        assert!(!dest.join("README.txt").exists());
        assert!(!dest.join("evil.sty").exists());
        let _ = std::fs::remove_dir_all(&dest);
    }

    #[test]
    fn extract_tds_empty_is_error() {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opt = zip::write::SimpleFileOptions::default();
            w.start_file("README.txt", opt).unwrap();
            w.finish().unwrap();
        }
        let dest = std::env::temp_dir().join(format!("wc-pkg-tds0-{}", std::process::id()));
        assert!(extract_tds(buf.get_ref(), &dest, "foo").is_err());
    }
}
