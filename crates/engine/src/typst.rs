//! `document.compile` for `.typ` files — shells out to the `typst`
//! CLI (MIT/Apache-2.0), the same external-engine pattern as system
//! TeX. One invocation produces the PDF; there is no rerun/bib loop.
//!
//! Typst packages (`#import "@preview/…"`) auto-download into a cache
//! dir — we pin `--package-cache-path` inside the project so packages
//! stay local and committable (`.warraqcraft/vendor/typst-cache`).

use std::path::{Path, PathBuf};

use crate::{CompileError, CompileReport, EngineError, BUILD_DIR};

/// Locate the typst binary: `WARRAQCRAFT_TYPST` override first (same
/// convention as `WARRAQCRAFT_TECTONIC`), then PATH / well-known dirs.
fn find_typst() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("WARRAQCRAFT_TYPST").map(PathBuf::from).filter(|p| p.is_file()) {
        return Some(p);
    }
    crate::toolchain::find_program("typst")
}

/// Compile `path` (a `.typ` file) to PDF via the typst CLI.
///
/// `path.parent()` of a bare filename is `""`, not `None` — normalize
/// to the current dir, same as the TeX path.
pub fn compile_file(path: &Path) -> Result<CompileReport, EngineError> {
    let prog =
        find_typst().ok_or_else(|| EngineError::Tool("no typst binary found — install Typst (https://typst.app) or set WARRAQCRAFT_TYPST".into()))?;
    let name = path.file_name().ok_or(EngineError::NoFile)?.to_string_lossy().into_owned();
    let stem = path.file_stem().ok_or(EngineError::NoFile)?.to_string_lossy().into_owned();
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let parent_abs = std::path::absolute(parent)?;

    let outdir = parent_abs.join(BUILD_DIR);
    std::fs::create_dir_all(&outdir)?;
    let pdf = outdir.join(format!("{stem}.pdf"));

    // Project-local package cache — typst creates the dir itself;
    // `@preview` imports then live under version control with the
    // project instead of a per-user cache. `vendor/typst-packages/`
    // (hand-vendored local packages) is passed through when present.
    let cache = parent_abs.join(BUILD_DIR).join("vendor").join("typst-cache");
    std::fs::create_dir_all(&cache)?;

    let pkgdir = parent_abs.join(BUILD_DIR).join("vendor").join("typst-packages");
    let mut cmd = std::process::Command::new(&prog);
    cmd.arg("compile").arg("--root").arg(&parent_abs).arg("--diagnostic-format").arg("short").arg("--package-cache-path").arg(&cache);
    if pkgdir.is_dir() {
        cmd.arg("--package-path").arg(&pkgdir);
    }
    cmd.arg(&name).arg(&pdf).current_dir(&parent_abs).stdin(std::process::Stdio::null());

    let out = cmd.output()?;
    // Diagnostics go to stderr in typst; keep stdout too — either may
    // carry them depending on version.
    let mut text = String::from_utf8_lossy(&out.stderr).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stdout));

    let (errors, warnings) = parse_diagnostics(&text);
    let log_tail = tail(&text, 8 * 1024);

    if !out.status.success() {
        let mut errors = errors;
        if errors.is_empty() {
            errors.push(CompileError::raw(None, None, "typst compile failed".into()));
        }
        return Ok(CompileReport { engine: "typst".into(), pdf: None, pages: None, errors, warnings, log_tail });
    }
    if !pdf.is_file() {
        return Ok(CompileReport {
            engine: "typst".into(),
            pdf: None,
            pages: None,
            errors: vec![CompileError::raw(None, None, "typst reported success but wrote no PDF".into())],
            warnings,
            log_tail,
        });
    }
    Ok(CompileReport { engine: "typst".into(), pdf: Some(pdf), pages: None, errors, warnings, log_tail })
}

/// Parse `--diagnostic-format short` lines:
/// `\\?\<abs>\path\file.typ:LINE:COL: error: message`
/// (also `warning:`). Windows paths contain `:` so severity is located
/// first, then line/col are peeled off the tail. Anything unrecognized
/// is skipped — a format change degrades to fewer diagnostics, never
/// a panic (hostile-input rule).
fn parse_diagnostics(text: &str) -> (Vec<CompileError>, Vec<CompileError>) {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    for line in text.lines().take(4096) {
        let line = line.trim();
        let (head, sev, msg) = if let Some(i) = line.find(": error: ") {
            (&line[..i], "error", &line[i + ": error: ".len()..])
        } else if let Some(i) = line.find(": warning: ") {
            (&line[..i], "warning", &line[i + ": warning: ".len()..])
        } else {
            continue;
        };
        // head = `<path>:<line>:<col>` — peel col then line off the end.
        let Some((head, col)) = head.rsplit_once(':') else { continue };
        let Ok(_col) = col.trim().parse::<u32>() else { continue };
        let Some((path, line_no)) = head.rsplit_once(':') else { continue };
        let Ok(line_no) = line_no.trim().parse::<u32>() else { continue };
        // `\\?\` verbatim prefix is a Windows-ism — strip for display.
        let path = path.trim_start_matches("\\\\?\\");
        let d = CompileError::raw(Some(PathBuf::from(path)), Some(line_no), msg.trim().to_string());
        if sev == "error" {
            errors.push(d);
        } else {
            warnings.push(d);
        }
    }
    (errors, warnings)
}

fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let start = text.len() - max;
    // Don't slice inside a UTF-8 char — walk forward to a boundary.
    let mut start = start;
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_short_diagnostics() {
        let text =
            "\\\\?\\C:\\proj\\main.typ:12:5: error: unknown variable: foo\n\\\\?\\C:\\proj\\main.typ:3:1: warning: unused import\nnot a diag line\n";
        let (errors, warnings) = parse_diagnostics(text);
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].line, Some(12));
        assert_eq!(errors[0].message, "unknown variable: foo");
        assert_eq!(errors[0].file.as_deref(), Some(Path::new("C:\\proj\\main.typ")));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].line, Some(3));
    }

    #[test]
    fn ignores_garbage_lines() {
        let (e, w) = parse_diagnostics("error: no location\nfoo.typ:notanum:2: error: x\n:: error: y\n");
        assert!(e.is_empty() && w.is_empty());
    }

    #[test]
    fn tail_respects_char_boundary() {
        let s = format!("{}é", "x".repeat(9000));
        let t = tail(&s, 8192);
        assert!(t.len() <= 8192 + 4);
    }
}
