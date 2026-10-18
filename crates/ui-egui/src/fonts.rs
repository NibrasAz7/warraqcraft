//! Extra UI fonts: every `.ttf`/`.otf` found is registered with egui as a
//! per-glyph *fallback* in the Proportional and Monospace families — egui's
//! built-in fonts cover Latin and Cyrillic but not the other scripts this
//! app supports. `scripts/fetch-fonts.ps1` fetches the local extras (Amiri),
//! while `craft-fonts` supplies shared Arabic/CJK coverage in dev checkouts.
//!
//! Discovery: `$WARRAQCRAFT_FONTS`, then `$CRAFT_FONTS_DIR`, then `fonts/`
//! and `craft-fonts/fonts/` next to the exe and in every ancestor dir (dev
//! checkouts: the workspace-root `fonts/` and `../craft-fonts/fonts/`). One
//! subdirectory level is scanned, matching `craft-fonts/fonts/<family>/`.
//! Missing dirs/files are not an error — the app runs, uncovered glyphs just
//! degrade to tofu as before.

use std::path::PathBuf;

/// Register all discovered `.ttf` files as fallback fonts. Call once at
/// app start, before the first frame.
pub fn install(ctx: &egui::Context) {
    let files = font_files();
    if files.is_empty() {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    for path in files {
        let Some(name) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let Ok(bytes) = std::fs::read(&path) else { continue };
        fonts.font_data.insert(name.to_owned(), egui::FontData::from_owned(bytes).into());
        // Push at the END of each family: Latin glyphs still resolve to
        // the bundled fonts first; missing glyphs fall through to ours.
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            list.push(name.to_owned());
        }
        if let Some(list) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            list.push(name.to_owned());
        }
    }
    ctx.set_fonts(fonts);
}

/// Font files in every candidate dir and one subdirectory level below it.
fn font_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in candidate_dirs() {
        collect_font_files(&dir, &mut files);
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for subdir in entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()) {
                collect_font_files(&subdir, &mut files);
            }
        }
    }
    files
}

fn collect_font_files(dir: &std::path::Path, files: &mut Vec<PathBuf>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        files.extend(
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))),
        );
    }
}

fn candidate_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["WARRAQCRAFT_FONTS", "CRAFT_FONTS_DIR"] {
        if let Some(env) = std::env::var_os(var).map(PathBuf::from) {
            dirs.push(env.join("fonts"));
            dirs.push(env);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.extend(dir.ancestors().flat_map(|a| [a.join("fonts"), a.join("craft-fonts/fonts")]));
        }
    }
    dirs
}

#[cfg(test)]
mod tests {
    #[test]
    fn candidate_dirs_never_panics() {
        // With no env vars and a normal exe path this is just a vec of
        // paths — existence is not required.
        let _ = super::candidate_dirs();
    }

    #[test]
    fn collect_accepts_ttf_and_otf() {
        let dir = std::env::temp_dir().join(format!("warraqcraft-fonts-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.ttf"), []).unwrap();
        std::fs::write(dir.join("b.otf"), []).unwrap();
        std::fs::write(dir.join("skip.txt"), []).unwrap();

        let mut files = Vec::new();
        super::collect_font_files(&dir, &mut files);
        assert_eq!(files.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
