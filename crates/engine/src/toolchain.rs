//! Toolchain discovery and probing.
//!
//! GUI apps don't inherit a shell PATH on every platform — an installed
//! MiKTeX/TeX Live/MacTeX/TinyTeX can be invisible to a spawned process.
//! [`find_program`] searches PATH *and* well-known install roots;
//! [`augmented_path`] prepends a resolved binary's directory to the child
//! PATH so helpers (biber, bibtex, kpathsea) resolve next to the engine.
//! [`probe`] reports every known tool as found/broken/missing — we probe
//! and report, never install or mutate the user's toolchain.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

/// External tools WarraqCraft can use, in display order.
pub const KNOWN_TOOLS: &[&str] = &["tectonic", "xelatex", "lualatex", "pdflatex", "bibtex", "biber", "latexmk", "synctex", "git"];

/// One tool's probe result.
#[derive(Debug)]
pub struct ToolProbe {
    pub name: String,
    /// Resolved executable path when found on PATH or a well-known dir.
    pub path: Option<PathBuf>,
    /// First informative line of `--version` (empty → found but silent).
    pub version: Option<String>,
    /// File exists but `--version` failed — broken install worth flagging.
    pub broken: bool,
}

/// `name` with the platform executable suffix.
fn exe(name: &str) -> String {
    if cfg!(windows) && !name.ends_with(".exe") {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

/// Directories that hold TeX-family tools even when PATH doesn't, per OS.
fn well_known_bin_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for var in ["LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)", "PROGRAMW6432"] {
            if let Some(base) = std::env::var_os(var) {
                dirs.push(Path::new(&base).join("MiKTeX").join("miktex").join("bin").join("x64"));
            }
        }
        if let Some(appdata) = std::env::var_os("APPDATA") {
            dirs.push(Path::new(&appdata).join("TinyTeX").join("bin").join("windows"));
            dirs.push(Path::new(&appdata).join("TinyTeX").join("bin").join("win32"));
        }
        // C:\texlive\<year>\bin\<triplet> — scan the root ourselves, no glob dep.
        if let Ok(rd) = std::fs::read_dir(r"C:\texlive") {
            for e in rd.flatten().take(64) {
                let bin = e.path().join("bin");
                if let Ok(rb) = std::fs::read_dir(&bin) {
                    for b in rb.flatten().take(16) {
                        dirs.push(b.path());
                    }
                }
            }
        }
    } else {
        dirs.push(PathBuf::from("/Library/TeX/texbin")); // MacTeX
        dirs.push(PathBuf::from("/opt/texbin"));
        if let Some(home) = std::env::var_os("HOME") {
            for base in ["TinyTeX", ".TinyTeX"] {
                let bin = Path::new(&home).join(base).join("bin");
                if let Ok(rd) = std::fs::read_dir(&bin) {
                    for e in rd.flatten().take(16) {
                        dirs.push(e.path());
                    }
                }
            }
        }
        // /usr/local/texlive/<year>/bin/<triplet>
        if let Ok(rd) = std::fs::read_dir("/usr/local/texlive") {
            for e in rd.flatten().take(64) {
                let bin = e.path().join("bin");
                if let Ok(rb) = std::fs::read_dir(&bin) {
                    for b in rb.flatten().take(16) {
                        dirs.push(b.path());
                    }
                }
            }
        }
    }
    dirs.retain(|d| d.is_dir());
    dirs
}

/// Resolve `name` to an executable path: PATH first (the user's explicit
/// order wins), then the well-known install roots. Returns `None` when
/// the program isn't installed anywhere we know about.
pub fn find_program(name: &str) -> Option<PathBuf> {
    let file = exe(name);
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join(&file);
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    well_known_bin_dirs().into_iter().map(|d| d.join(&file)).find(|p| p.is_file())
}

/// `name` runs on PATH — convenience for `find_program(..).is_some()`.
pub fn on_path(name: &str) -> bool {
    find_program(name).is_some()
}

/// A PATH value with `dir` prepended — children spawned with it resolve
/// helpers sitting next to the engine (biber next to xelatex, etc.).
pub fn augmented_path(dir: &Path) -> OsString {
    let mut out = OsString::from(dir.as_os_str());
    if let Some(existing) = std::env::var_os("PATH") {
        out.push(if cfg!(windows) { ";" } else { ":" });
        out.push(existing);
    }
    out
}

/// Probe every known tool: where it resolves and whether `--version`
/// answers. A tool that exists but can't run is `broken` — surfaced so
/// the UI can say "broken" instead of pretending it's missing.
pub fn probe() -> Vec<ToolProbe> {
    KNOWN_TOOLS
        .iter()
        .map(|&name| {
            let Some(path) = find_program(name) else {
                return ToolProbe { name: name.to_string(), path: None, version: None, broken: false };
            };
            match Command::new(&path).arg("--version").output() {
                Ok(o) if o.status.success() => {
                    let out = String::from_utf8_lossy(&o.stdout);
                    let version = out.lines().find(|l| !l.trim().is_empty()).map(|l| l.trim().to_string());
                    ToolProbe { name: name.to_string(), path: Some(path), version, broken: false }
                }
                _ => ToolProbe { name: name.to_string(), path: Some(path), version: None, broken: true },
            }
        })
        .collect()
}

/// `toolchain.probe` — every known tool as `{name, found, path, version,
/// broken}`. `path` is null when missing; `broken` means the file exists
/// but `--version` failed. `bundled_tectonic` covers the app-shipped
/// binary, which `find_program` can't see (it's not on PATH).
pub fn probe_json(bundled_tectonic: Option<PathBuf>) -> Value {
    let mut probes = probe();
    if let Some(p) = bundled_tectonic {
        for t in probes.iter_mut().filter(|t| t.name == "tectonic" && t.path.is_none()) {
            t.path = Some(p.clone());
        }
    }
    let tools: Vec<Value> = probes
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "found": t.path.is_some() && !t.broken,
                "path": t.path.as_ref().map(|p| p.display().to_string()),
                "version": t.version,
                "broken": t.broken,
            })
        })
        .collect();
    json!({ "tools": tools })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_never_panics_and_covers_known_tools() {
        let probes = probe();
        assert_eq!(probes.len(), KNOWN_TOOLS.len());
        for p in &probes {
            assert!(KNOWN_TOOLS.contains(&p.name.as_str()));
        }
    }

    #[test]
    fn augmented_path_prepends() {
        let dir = Path::new(if cfg!(windows) { r"C:\tex\bin" } else { "/tex/bin" });
        let aug = augmented_path(dir).to_string_lossy().into_owned();
        assert!(aug.starts_with(if cfg!(windows) { r"C:\tex\bin" } else { "/tex/bin" }));
    }
}
