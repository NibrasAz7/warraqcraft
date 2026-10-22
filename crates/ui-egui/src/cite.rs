//! Insert-citation dialog (TeXstudio parity): a modal picker over the
//! project's `.bib` files. `Ctrl+Shift+C` or the top-bar book button
//! opens it; the picked key lands as `\cite{key}` at the caret — or as
//! `key,` when the caret already sits inside a `\cite{…}` argument,
//! merging into a multi-key cite.
//!
//! Scanning is shallow and bounded (`root` plus one subdirectory level,
//! [`MAX_FILES`] files, [`MAX_BYTES`] each) — the project dir is
//! untrusted input. The web build can't read the folder: the dialog
//! still opens and explains itself when the list is empty.

use std::path::{Path, PathBuf};

use crate::{lang, theme};

/// One bibliography row: a `@…{key, …}` entry and the file it came from.
#[derive(Clone)]
pub struct CiteEntry {
    pub key: String,
    pub author: String,
    pub title: String,
    pub year: String,
    pub file: String,
}

/// Scan caps — the project dir is untrusted input.
#[cfg(not(target_arch = "wasm32"))]
const MAX_FILES: usize = 32;
#[cfg(not(target_arch = "wasm32"))]
const MAX_BYTES: u64 = 512 * 1024;

/// Rows rendered at once — the list stays interactive on huge
/// bibliographies; the filter narrows the rest.
const MAX_ROWS: usize = 200;

/// Read a bounded set of `.bib` files into picker rows. Used both by the
/// dialog's root scan and by inline `\cite{` completion over the project
/// bibliography list reported by `project.structure`.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn scan_bib_files(paths: &[PathBuf], entries: &mut Vec<CiteEntry>, issues: &mut Vec<(String, usize)>) {
    use std::io::Read;
    for p in paths.iter().take(MAX_FILES) {
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
        let mut text = String::new();
        let Ok(f) = std::fs::File::open(p) else { continue };
        if f.take(MAX_BYTES).read_to_string(&mut text).is_err() {
            continue;
        }
        let bib = texparse::parse_bib(&text);
        if !bib.issues.is_empty() {
            issues.push((name.clone(), bib.issues.len()));
        }
        for e in &bib.entries {
            entries.push(CiteEntry {
                key: e.key.clone(),
                author: tidy(e.field("author").unwrap_or("")),
                title: tidy(e.field("title").unwrap_or("")),
                year: tidy(e.field("year").unwrap_or("")),
                file: name.clone(),
            });
        }
    }
}

/// The picker's state; the window is visible while `open` is set.
#[derive(Default)]
pub struct CiteDialog {
    open: bool,
    filter: String,
    /// Selected row within the *filtered* list.
    sel: usize,
    entries: Vec<CiteEntry>,
    /// Per-file parse notes: `(file name, issue count)`.
    issues: Vec<(String, usize)>,
    /// Editor caret (char index) captured when the dialog opened — the
    /// filter steals focus, so insertion lands where the user was.
    pub saved_caret: Option<usize>,
    /// Identifier-paste field for `cite.fetch` (DOI/arXiv/PMID).
    fetch_text: String,
    /// A resolve request the app picks up and dispatches on a worker —
    /// the dialog stays open while the network call runs.
    pub pending_fetch: Option<String>,
    /// Set by the app while `cite.fetch` is in flight — the row shows a
    /// busy hint instead of re-firing.
    pub fetch_busy: bool,
    /// Cite keys that have a PDF in the project library — set by the app
    /// when the dialog opens.
    pub attached: std::collections::BTreeSet<String>,
    /// The selected key asked for a PDF attachment — the app picks the
    /// file and runs `library.attach`.
    pub pending_attach: Option<String>,
    /// The selected key asked to open its attached PDF — the app shows
    /// it in the reader pane.
    pub pending_open: Option<String>,
    /// A CSL-JSON/RIS file picked for `bib.import` — the app dispatches
    /// it on the worker so parsing stays off the UI thread.
    pub pending_import: Option<PathBuf>,
    /// Set by the app while `bib.import` runs — the row shows busy.
    pub import_busy: bool,
}

impl CiteDialog {
    /// Raise the dialog and (re)load entries from `root`'s `.bib` files.
    pub fn open(&mut self, root: &Path) {
        self.open = true;
        self.filter.clear();
        self.sel = 0;
        self.entries.clear();
        self.issues.clear();
        self.scan(root);
    }

    /// Whether the dialog is currently up.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Close the dialog — the app calls this after a `cite.fetch`
    /// resolved key is inserted.
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Rescan the `.bib` files keeping filter and selection — used after
    /// `bib.import` writes new entries while the dialog stays open.
    pub fn reload(&mut self, root: &Path) {
        let keep_sel = self.sel;
        self.entries.clear();
        self.issues.clear();
        self.scan(root);
        self.sel = keep_sel;
    }

    /// `.bib` files in `root` and its immediate subdirectories.
    #[cfg(not(target_arch = "wasm32"))]
    fn scan(&mut self, root: &Path) {
        // Candidate dirs: root plus one subdirectory level.
        let mut dirs = vec![root.to_path_buf()];
        if let Ok(read) = std::fs::read_dir(root) {
            let mut subs: Vec<std::path::PathBuf> =
                read.flatten().filter(|e| !e.file_name().to_string_lossy().starts_with('.')).map(|e| e.path()).filter(|p| p.is_dir()).collect();
            subs.sort();
            dirs.extend(subs);
        }
        let mut bibs = Vec::new();
        for d in &dirs {
            let Ok(read) = std::fs::read_dir(d) else { continue };
            for e in read.flatten() {
                if e.file_name().to_string_lossy().starts_with('.') {
                    continue;
                }
                let p = e.path();
                if p.extension().and_then(|x| x.to_str()).is_some_and(|x| x.eq_ignore_ascii_case("bib")) {
                    bibs.push(p);
                }
            }
        }
        bibs.sort();
        bibs.truncate(MAX_FILES);
        scan_bib_files(&bibs, &mut self.entries, &mut self.issues);
    }

    /// The web build can't read the project folder — `open` still raises
    /// the dialog, which shows the empty-state hint.
    #[cfg(target_arch = "wasm32")]
    fn scan(&mut self, _root: &Path) {}

    /// Draw the window; returns the picked key (dialog closes on pick,
    /// Esc or the ✕ button).
    pub fn show(&mut self, ctx: &egui::Context, pal: theme::Palette, s: &lang::Strings) -> Option<String> {
        if !self.open {
            return None;
        }
        let mut open = self.open;
        let mut close = false;
        let mut picked: Option<String> = None;
        egui::Window::new(lang::view(s.cite_title))
            .collapsible(false)
            .resizable(true)
            .default_pos(ctx.content_rect().center())
            .pivot(egui::Align2::CENTER_CENTER)
            .open(&mut open)
            .show(ctx, |ui| {
                // Arrows/Enter/Escape are consumed before the filter
                // TextEdit runs — typed text still reaches it. While the
                // fetch field has focus, Enter resolves the identifier
                // instead of picking a list row.
                let fetch_id = egui::Id::new("cite-fetch");
                let fetch_focused = ctx.memory(|m| m.has_focus(fetch_id));
                let mut nav = false;
                let mut want_pick = false;
                let mut want_fetch = false;
                ctx.input_mut(|i| {
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                        self.sel = self.sel.saturating_add(1);
                        nav = true;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                        self.sel = self.sel.saturating_sub(1);
                        nav = true;
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                        if fetch_focused {
                            want_fetch = true;
                        } else {
                            want_pick = true;
                        }
                    }
                    if i.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                        close = true;
                    }
                });
                // wasm never renders the fetch row; the flag stays false.
                #[cfg(target_arch = "wasm32")]
                let _ = want_fetch;

                // Per-file parse notes, kept subtle — entries still list.
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(lang::fmt(s.cite_entries, &[("n", &self.entries.len().to_string())])).size(9.5).color(pal.faint));
                    for (f, n) in self.issues.iter().take(3) {
                        ui.label(egui::RichText::new(format!("{f}: {n} issue(s)")).size(9.5).color(pal.muted));
                    }
                });
                let edit = ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text(lang::view(s.cite_filter_hint)).desired_width(400.0));
                ui.memory_mut(|m| m.request_focus(edit.id));
                if edit.changed() {
                    self.sel = 0;
                }

                // Case-insensitive substring over key + title + author.
                let needle = self.filter.to_lowercase();
                let mut matches: Vec<usize> = Vec::new();
                for (i, e) in self.entries.iter().enumerate() {
                    if needle.is_empty()
                        || e.key.to_lowercase().contains(&needle)
                        || e.title.to_lowercase().contains(&needle)
                        || e.author.to_lowercase().contains(&needle)
                    {
                        matches.push(i);
                    }
                }
                self.sel = self.sel.min(matches.len().saturating_sub(1));
                if want_pick {
                    picked = matches.get(self.sel).and_then(|&i| self.entries.get(i)).map(|e| e.key.clone());
                }

                egui::ScrollArea::vertical().id_salt("cite-list").max_height(300.0).auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_min_width(400.0);
                    if self.entries.is_empty() {
                        let msg = if cfg!(target_arch = "wasm32") { s.cite_no_bib_wasm } else { s.cite_no_bib };
                        ui.label(egui::RichText::new(lang::view(msg)).size(11.0).color(pal.muted));
                        return;
                    }
                    if matches.is_empty() {
                        ui.label(egui::RichText::new(lang::view(s.cite_empty)).size(11.0).color(pal.muted));
                    }
                    for (row, &ei) in matches.iter().take(MAX_ROWS).enumerate() {
                        let Some(e) = self.entries.get(ei) else { continue };
                        let mut detail = e.author.clone();
                        if !e.title.is_empty() {
                            if !detail.is_empty() {
                                detail.push_str(", ");
                            }
                            detail.push_str(&e.title);
                        }
                        if !e.year.is_empty() {
                            if !detail.is_empty() {
                                detail.push(' ');
                            }
                            detail.push_str(&format!("({})", e.year));
                        }
                        let label = if detail.is_empty() { e.key.clone() } else { format!("{} — {detail}", e.key) };
                        let label = if self.attached.contains(&e.key) { format!("📎 {label}") } else { label };
                        let resp = ui.selectable_label(row == self.sel, egui::RichText::new(lang::view(&label)).size(11.5)).on_hover_text(&e.file);
                        if nav && row == self.sel {
                            resp.scroll_to_me(Some(egui::Align::Center));
                        }
                        if resp.clicked() {
                            picked = Some(e.key.clone());
                        }
                    }
                    if matches.len() > MAX_ROWS {
                        ui.label(egui::RichText::new(format!("… {} more — keep typing", matches.len() - MAX_ROWS)).size(10.0).color(pal.faint));
                    }
                });

                // Selected row: attach a PDF to the key, or read the one
                // already attached.
                #[cfg(not(target_arch = "wasm32"))]
                {
                    let sel_key = matches.get(self.sel).and_then(|&i| self.entries.get(i)).map(|e| e.key.clone());
                    if let Some(k) = sel_key {
                        ui.horizontal(|ui| {
                            if ui.small_button(lang::view(s.cite_attach)).clicked() {
                                self.pending_attach = Some(k.clone());
                            }
                            if self.attached.contains(&k) && ui.small_button(lang::view(s.cite_read)).clicked() {
                                self.pending_open = Some(k.clone());
                            }
                        });
                    }
                }

                // Paste an identifier → `cite.fetch` resolves it to a
                // .bib entry via the public providers. Native-only: the
                // web build has no network resolver.
                #[cfg(not(target_arch = "wasm32"))]
                {
                    ui.separator();
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.fetch_text)
                                .id(fetch_id)
                                .hint_text(lang::view(s.cite_fetch_hint))
                                .desired_width(250.0),
                        );
                        let busy = self.fetch_busy;
                        let go = ui
                            .add_enabled(!busy && !self.fetch_text.trim().is_empty(), egui::Button::new(lang::view(s.cite_fetch_go)))
                            .on_hover_text(lang::view(s.cite_fetch_hint));
                        if busy {
                            ui.label(egui::RichText::new(lang::view(s.cite_fetch_busy)).size(10.0).color(pal.muted));
                        }
                        if (go.clicked() || want_fetch) && !busy {
                            let id = self.fetch_text.trim().to_string();
                            if !id.is_empty() {
                                self.pending_fetch = Some(id);
                                self.fetch_text.clear();
                            }
                        }
                        // Reference-manager export → .bib (CSL-JSON/RIS).
                        let imp = ui
                            .add_enabled(!self.import_busy, egui::Button::new(lang::view(s.cite_import)).small())
                            .on_hover_text(lang::view(s.cite_import_hint));
                        if imp.clicked() {
                            if let Some(p) = rfd::FileDialog::new().add_filter("references", &["json", "ris"]).pick_file() {
                                self.pending_import = Some(p);
                            }
                        }
                        if self.import_busy {
                            ui.label(egui::RichText::new(lang::view(s.cite_import_busy)).size(10.0).color(pal.muted));
                        }
                    });
                }
            });
        self.open = open && !close && picked.is_none();
        picked
    }
}

/// The top-bar "cite" button: a tiny open-book glyph plus the label.
/// `label` is painted as-is; egui's shaper handles RTL/scripts itself.
pub fn cite_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(44.0, 20.0), egui::Sense::click());
    let resp = resp.on_hover_text("insert citation — Ctrl+Shift+C");
    let p = ui.painter();
    let ink = if resp.hovered() { theme::pal().text } else { theme::pal().muted };
    let cy = rect.center().y;
    let bx = rect.left() + 3.0;
    let stroke = egui::Stroke::new(1.0, ink);
    // Open book: two angled pages dipping toward a centre spine.
    let cx = bx + 6.0;
    let (top, bot) = (cy - 4.5, cy + 4.0);
    let outer_top = top + 1.5;
    let (lx, rx) = (bx, bx + 12.0);
    p.line_segment([egui::pos2(lx, outer_top), egui::pos2(cx, top)], stroke);
    p.line_segment([egui::pos2(lx, outer_top), egui::pos2(lx, bot - 1.5)], stroke);
    p.line_segment([egui::pos2(lx, bot - 1.5), egui::pos2(cx, bot)], stroke);
    p.line_segment([egui::pos2(cx, top), egui::pos2(rx, outer_top)], stroke);
    p.line_segment([egui::pos2(rx, outer_top), egui::pos2(rx, bot - 1.5)], stroke);
    p.line_segment([egui::pos2(rx, bot - 1.5), egui::pos2(cx, bot)], stroke);
    p.line_segment([egui::pos2(cx, top), egui::pos2(cx, bot)], stroke);
    p.text(egui::pos2(bx + 16.0, cy), egui::Align2::LEFT_CENTER, label, egui::FontId::proportional(11.5), ink);
    resp
}

/// What to insert at byte offset `caret` for `key`: `\cite{key}` at a
/// fresh spot, or `key,` when `caret` already sits inside a `\…cite{…}`
/// argument — merging into a multi-key cite, comma seeding the next
/// key slot.
pub fn insert_text(src: &str, caret: usize, key: &str) -> String {
    if inside_cite_args(src, caret) {
        format!("{key},")
    } else {
        format!("\\cite{{{key}}}")
    }
}

/// Like [`insert_text`] but with a page locator: `\cite[p.~N]{key}` —
/// "quote page N of the attached PDF".
pub fn insert_text_paged(src: &str, caret: usize, key: &str, page: usize) -> String {
    if inside_cite_args(src, caret) {
        format!("{key},")
    } else {
        format!("\\cite[p.~{page}]{{{key}}}")
    }
}

/// Whether `caret` (a byte offset, clamped to a boundary) sits inside
/// the braces of a `\cite`-family argument: the nearest preceding brace
/// is a `{` whose command name starts or ends with `cite` (`\cite`,
/// `\citet`, `\parencite`, …). A `}` first means a group closed between
/// the brace and the caret — we're outside.
fn inside_cite_args(src: &str, caret: usize) -> bool {
    let mut end = caret.min(src.len());
    while end > 0 && !src.is_char_boundary(end) {
        end -= 1;
    }
    let Some(back) = src.get(..end) else { return false };
    // Bound the look-back — a cite's braces never span far.
    let start = back.char_indices().nth(back.chars().count().saturating_sub(256)).map(|(b, _)| b).unwrap_or(0);
    let Some(tail) = back.get(start..) else { return false };
    let bytes = tail.as_bytes();
    for i in (0..bytes.len()).rev() {
        match bytes[i] {
            b'}' => return false,
            b'{' => {
                let Some(before) = tail.get(..i) else { return false };
                let name: String = before.chars().rev().take_while(|c| c.is_ascii_alphabetic()).collect::<Vec<_>>().into_iter().rev().collect();
                let Some(prefix) = before.get(..before.len() - name.len()) else { return false };
                if !prefix.ends_with('\\') {
                    return false;
                }
                let lc = name.to_ascii_lowercase();
                return lc.starts_with("cite") || lc.ends_with("cite");
            }
            _ => {}
        }
    }
    false
}

/// Collapse whitespace runs — `.bib` values can span lines.
#[cfg(not(target_arch = "wasm32"))]
fn tidy(v: &str) -> String {
    v.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at_end(src: &str, key: &str) -> String {
        insert_text(src, src.len(), key)
    }

    // ---------- insert_text ----------

    #[test]
    fn fresh_spot_inserts_cite_command() {
        assert_eq!(at_end("see ", "knuth84"), "\\cite{knuth84}");
        assert_eq!(insert_text("", 0, "k"), "\\cite{k}");
    }

    #[test]
    fn inside_cite_merges_with_comma() {
        assert_eq!(at_end("\\cite{", "b"), "b,");
        assert_eq!(at_end("\\cite{one,", "b"), "b,");
        assert_eq!(at_end("\\cite{one", "b"), "b,");
        assert_eq!(at_end("\\citet{a", "b"), "b,");
        assert_eq!(at_end("\\parencite{a", "b"), "b,");
        assert_eq!(at_end("\\Cite{a", "b"), "b,");
    }

    #[test]
    fn outside_cite_stays_fresh() {
        // Closed braces between the '{' and the caret → outside.
        assert_eq!(at_end("\\cite{a} and ", "b"), "\\cite{b}");
        assert_eq!(at_end("\\cite{a}", "b"), "\\cite{b}");
        // A brace that isn't a cite command.
        assert_eq!(at_end("\\section{t", "b"), "\\cite{b}");
        assert_eq!(at_end("text {", "b"), "\\cite{b}");
        // Nested brace inside a cite: the inner '{' isn't a command.
        assert_eq!(at_end("\\cite{a{b}c", "d"), "\\cite{d}");
        // Escaped brace isn't a group open.
        assert_eq!(at_end("\\{", "b"), "\\cite{b}");
    }

    #[test]
    fn caret_mid_argument_and_boundaries() {
        // Caret between keys inside the braces.
        assert_eq!(insert_text("\\cite{a,b}", 7, "c"), "c,");
        // Non-boundary caret clamps instead of panicking.
        assert_eq!(insert_text("α", 1, "k"), "\\cite{k}");
        // Unicode before the caret doesn't confuse the look-back.
        assert_eq!(insert_text("αβ \\cite{x", "αβ \\cite{x".len(), "k"), "k,");
    }
}
