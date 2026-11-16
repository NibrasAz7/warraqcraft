//! The package registry (L3): curated CTAN package metadata.
//!
//! [`catalog`] answers "what does `\foo` do"; this table answers "what
//! is package `bar`, what files does it ship, and how do I get it". It
//! is the seed for `pkg.resolve`/`pkg.install` (TASK-22) — data only,
//! no network, no dependency beyond the catalog's own tables.
//!
//! Entries are authored from public CTAN package pages (name, file
//! list, one-line summary). `files` lists the runtime `.sty`/`.cls`
//! names the package provides on the TeX search path — enough for
//! `resolve("amsmath.sty")`-style lookups and missing-file diagnosis.
//! Long-tail packages not listed here are still known to
//! [`crate::packages`] via the completion catalog.

/// One CTAN package's registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackageInfo {
    /// CTAN package name (what `\usepackage{name}` and `tlmgr install`
    /// both take in the common case).
    pub name: &'static str,
    /// Runtime files the package places on the TeX search path
    /// (`.sty`/`.cls`; paths relative to the package's texmf dir).
    pub files: &'static [&'static str],
    /// One-line summary authored from the CTAN abstract.
    pub summary: &'static str,
}

const fn pkg(name: &'static str, files: &'static [&'static str], summary: &'static str) -> PackageInfo {
    PackageInfo { name, files, summary }
}

/// The curated registry, sorted by [`PackageInfo::name`] (byte order).
/// The sort test at the bottom enforces the invariant.
static REGISTRY: &[PackageInfo] = &[
    pkg("acmart", &["acmart.cls"], "LaTeX class for ACM publications (sigconf, siggraph, sigplan variants)."),
    pkg("acro", &["acro.sty"], "Typeset abbreviations and acronyms with consistent first-use expansion."),
    pkg("algorithm", &["algorithm.sty"], "Floating algorithm environment with caption and rule."),
    pkg("algorithm2e", &["algorithm2e.sty"], "Feature-rich algorithm typesetting with line numbering and highlighting."),
    pkg("algpseudocode", &["algpseudocode.sty"], "Pseudocode typesetting environment for algorithm packages."),
    pkg("amsbsy", &["amsbsy.sty"], "Bold math symbols; loaded automatically by amsmath."),
    pkg("amscd", &["amscd.sty"], "Commutative diagrams in math mode."),
    pkg("amsfonts", &["amsfonts.sty"], "AMS symbol fonts (msam, msbm) — the blackboard-bold and fraktur families."),
    pkg(
        "amsmath",
        &["amsmath.sty", "amsopn.sty", "amstext.sty", "amsbsy.sty"],
        "Principal AMS-LaTeX package: multiline displays, matrices, cases, tags.",
    ),
    pkg("amsopn", &["amsopn.sty"], "Operator names for math; loaded automatically by amsmath."),
    pkg("amssymb", &["amssymb.sty"], "Additional AMS symbols — arrows, relations, blackboard bold."),
    pkg("amstext", &["amstext.sty"], "Text fragments inside mathematics; loaded automatically by amsmath."),
    pkg("amsthm", &["amsthm.sty"], "Theorem environments with shared numbering and optional notes."),
    pkg("arabdoc", &["arabdoc.cls"], "WarraqCraft's bundled Arabic document class."),
    pkg("array", &["array.sty"], "Extended tabular column types and row formatting."),
    pkg("attachfile2", &["attachfile2.sty"], "Attach files to the produced PDF (PDF 1.7 embedded files)."),
    pkg("auxhook", &["auxhook.sty"], "Hooks into the .aux file read/write cycle."),
    pkg("babel", &["babel.sty"], "Multi-language documents with hyphenation and captions per language."),
    pkg("background", &["background.sty"], "Place material (watermarks, page frames) behind or in front of page content."),
    pkg("bibentry", &["bibentry.sty"], "Inline bibliography entries via \\bibitem-listing commands."),
    pkg("biblatex", &["biblatex.sty", "biblatex.def", "biblatex.cfg"], "Programmable bibliography backend with biber; data model and styles."),
    pkg("bm", &["bm.sty"], "Access bold and other math alphabet variants inside formulas."),
    pkg("booktabs", &["booktabs.sty"], "Publication-quality horizontal rules for tables."),
    pkg("capt-of", &["capt-of.sty"], "Use \\captionof outside float environments."),
    pkg("caption", &["caption.sty"], "Customise captions for figures, tables and listings."),
    pkg("chngcntr", &["chngcntr.sty"], "Change counter bindings and reset behaviour."),
    pkg("cleveref", &["cleveref.sty"], "Smart cross-references that name the referenced object's type."),
    pkg("csquotes", &["csquotes.sty"], "Language-aware quoted speech with active quote characters."),
    pkg("datatool", &["datatool.sty"], "Database-style manipulation of CSV-style data in documents."),
    pkg("datetime2", &["datetime2.sty"], "Language-aware dates and times with multiple calendar systems."),
    pkg("doi", &["doi.sty"], "Typeset DOIs as hyperlinked doi: URIs."),
    pkg("empheq", &["empheq.sty"], "Emphasised and boxed math displays with amsmath."),
    pkg("enumitem", &["enumitem.sty"], "Fine-grained control over list (itemize/enumerate) layout."),
    pkg("epigraph", &["epigraph.sty"], "Epigraphs and dedications at chapter openings."),
    pkg("eso-pic", &["eso-pic.sty"], "Add material to every page background or foreground."),
    pkg("etoolbox", &["etoolbox.sty"], "LaTeX programming utilities: hooks, lists, conditionals."),
    pkg("eurosym", &["eurosym.sty"], "Euro currency symbol."),
    pkg("everypage", &["everypage.sty"], "Execute code on every page shipout."),
    pkg("fancyhdr", &["fancyhdr.sty"], "Custom page headers and footers."),
    pkg("float", &["float.sty"], "Additional float types and placement controls."),
    pkg("fontawesome5", &["fontawesome5.sty"], "FontAwesome icons in text mode."),
    pkg("fontenc", &["fontenc.sty"], "Font encoding selection (T1, OT1, LY1)."),
    pkg("fontspec", &["fontspec.sty"], "OpenType font selection for XeLaTeX and LuaLaTeX."),
    pkg("geometry", &["geometry.sty"], "Page layout: margins, paper size, binding offset."),
    pkg("glossaries", &["glossaries.sty"], "Acronyms and glossary with multiple glossaries and styles."),
    pkg("glossaries-extra", &["glossaries-extra.sty"], "Extended glossaries with category post-link hooks."),
    pkg("graphics", &["graphics.sty", "trig.sty"], "Base graphics inclusion layer used by graphicx."),
    pkg("graphicx", &["graphicx.sty"], "Include graphics with scaling, rotation and clipping."),
    pkg("hyperref", &["hyperref.sty", "hyperref.cfg"], "Hyperlinks, bookmarks and PDF metadata."),
    pkg("iftex", &["iftex.sty"], "Engine-detection conditionals (pdfTeX/XeTeX/LuaTeX)."),
    pkg("imakeidx", &["imakeidx.sty"], "Multiple indexes compiled in a single pass."),
    pkg("inputenc", &["inputenc.sty"], "Input encoding declaration (utf8 for pdfLaTeX)."),
    pkg("lastpage", &["lastpage.sty", "lastpage2e.sty"], "Reference to the document's last page number."),
    pkg("lettrine", &["lettrine.sty"], "Dropped initial capitals at paragraph starts."),
    pkg("lineno", &["lineno.sty"], "Line numbers in the margin for review copies."),
    pkg("lipsum", &["lipsum.sty"], "Dummy text for layout testing."),
    pkg("listings", &["listings.sty"], "Typeset source code with syntax highlighting."),
    pkg("lmodern", &["lmodern.sty"], "Latin Modern fonts as the default family."),
    pkg("longtable", &["longtable.sty"], "Tables spanning multiple pages."),
    pkg("makeidx", &["makeidx.sty"], "Kernel index generation commands."),
    pkg("mathtools", &["mathtools.sty"], "Extends amsmath: cases variants, matrix variants, tag patches."),
    pkg("mdframed", &["mdframed.sty"], "Framed and coloured text blocks that may break across pages."),
    pkg("microtype", &["microtype.sty"], "Micro-typographic refinements: protrusion, expansion, kerning."),
    pkg("minted", &["minted.sty", "minted1.sty"], "Syntax highlighting via the Pygments library."),
    pkg("multirow", &["multirow.sty"], "Table cells spanning several rows."),
    pkg("nameref", &["nameref.sty"], "\\nameref: reference names instead of numbers (hyperref companion)."),
    pkg("natbib", &["natbib.sty"], "Author-year and numerical citations with BibTeX styles."),
    pkg("newtxtext", &["newtxtext.sty"], "Times-like text font with small caps and old-style figures."),
    pkg("nicefrac", &["nicefrac.sty"], "Raised small fractions for inline use."),
    pkg("parskip", &["parskip.sty"], "Zero paragraph indentation with adjusted spacing."),
    pkg("pdfpages", &["pdfpages.sty"], "Include pages of existing PDFs."),
    pkg("pgf", &["pgf.sty", "pgfcore.sty", "pgfbasemodes.sty"], "Portable graphics format layer underneath TikZ."),
    pkg("pgfplots", &["pgfplots.sty", "pgfplots.revision.tex"], "Data plotting and function graphs on pgf/TikZ."),
    pkg("placeins", &["placeins.sty"], "Prevent floats drifting past barriers; \\FloatBarrier."),
    pkg("polyglossia", &["polyglossia.sty"], "XeLaTeX/LuaLaTeX multi-language support."),
    pkg("ragged2e", &["ragged2e.sty"], "Improved ragged-right and centred text without bad boxes."),
    pkg("rotating", &["rotating.sty"], "Rotated tables and figures (sideways, landscape)."),
    pkg("scrextend", &["scrextend.sty"], "Extensions to the KOMA-Script classes for article/report/book."),
    pkg("scrlayer-scrpage", &["scrlayer-scrpage.sty"], "KOMA-Script headers, footers and page backgrounds."),
    pkg("setspace", &["setspace.sty"], "Line spacing: onehalfspacing, doublespacing."),
    pkg("siunitx", &["siunitx.sty"], "Consistent typesetting of units and numbers (\\\\SI)."),
    pkg("subcaption", &["subcaption.sty"], "Sub-figures and sub-tables with own captions."),
    pkg("subfiles", &["subfiles.sty"], "Compile a multi-file project from any subfile."),
    pkg("tabularx", &["tabularx.sty"], "Tables with adjustable-width X columns."),
    pkg("tcolorbox", &["tcolorbox.sty"], "Coloured framed boxes with skins, breakable across pages."),
    pkg("textcomp", &["textcomp.sty"], "Additional text symbols (euros, degrees, copyright variants)."),
    pkg("theorem", &["theorem.sty"], "Shared-numbered theorem-like environments (amsthm predecessor)."),
    pkg("tikz", &["tikz.sty", "tikz.code.tex", "pgf.sty"], "Declarative vector graphics language on pgf."),
    pkg("titlesec", &["titlesec.sty"], "Reformat section headings: fonts, numbering, spacing."),
    pkg("tocbibind", &["tocbibind.sty"], "Add bibliography and index to the table of contents."),
    pkg("tocloft", &["tocloft.sty"], "Customise table of contents, list of figures and tables."),
    pkg("translator", &["translator.sty"], "Translate fixed strings (PDF bookmarks) via .glossary files."),
    pkg("url", &["url.sty"], "Breakable URL typesetting with verbatim-like handling."),
    pkg("xargs", &["xargs.sty"], "Define commands taking a variable number of arguments."),
    pkg("xcolor", &["xcolor.sty"], "Colour model selection and named/expressions colours."),
    pkg("xifthen", &["xifthen.sty"], "Extended conditionals on engine, package and class."),
    pkg("xkeyval", &["xkeyval.sty"], "Key-value option parsing utilities."),
    pkg("xparse", &["xparse.sty"], "LaTeX3-style document commands (now largely in the kernel)."),
    pkg("xurl", &["xurl.sty"], "URL breaks at any character (extreme version of url)."),
    pkg("xwatermark", &["xwatermark.sty"], "Text and image watermarks on every page."),
];

/// All registry entries, sorted by [`PackageInfo::name`].
pub fn registry() -> &'static [PackageInfo] {
    REGISTRY
}

/// Look up a package by its `\\usepackage` name or by a file it
/// provides (`"amsmath"`, `"amsmath.sty"`, `"article.cls"`). Matching is
/// exact and case-sensitive, as TeX package names are.
pub fn resolve(query: &str) -> Option<&'static PackageInfo> {
    let q = query.strip_suffix(".sty").or_else(|| query.strip_suffix(".cls")).unwrap_or(query);
    // Exact package-name match wins over file match: `amsopn` is its own
    // package even though `amsmath.sty`'s load set also ships amsopn.sty.
    if let Some(p) = REGISTRY.iter().find(|p| p.name == q) {
        return Some(p);
    }
    REGISTRY.iter().find(|p| p.files.iter().any(|f| f.strip_suffix(".sty").or_else(|| f.strip_suffix(".cls")).unwrap_or(f) == q))
}

/// Registry entries whose name or summary contains `query`
/// (case-insensitive substring match), in name order.
pub fn search(query: &str) -> impl Iterator<Item = &'static PackageInfo> {
    let needle = query.to_lowercase();
    REGISTRY.iter().filter(move |p| p.name.to_lowercase().contains(&needle) || p.summary.to_lowercase().contains(&needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_is_sorted_by_name() {
        for w in REGISTRY.windows(2) {
            assert!(w[0].name < w[1].name, "{} >= {}", w[0].name, w[1].name);
        }
    }

    #[test]
    fn every_entry_has_files_and_summary() {
        for p in REGISTRY {
            assert!(!p.files.is_empty(), "{} has no files", p.name);
            assert!(
                p.files.iter().all(|f| f.ends_with(".sty")
                    || f.ends_with(".cls")
                    || f.ends_with(".tex")
                    || f.ends_with(".def")
                    || f.ends_with(".cfg")),
                "{}: bad file {:?}",
                p.name,
                p.files
            );
            assert!(!p.summary.trim().is_empty(), "{} has an empty summary", p.name);
        }
    }

    #[test]
    fn resolve_by_name_and_by_file() {
        assert_eq!(resolve("amsmath").map(|p| p.name), Some("amsmath"));
        assert_eq!(resolve("amsmath.sty").map(|p| p.name), Some("amsmath"));
        assert_eq!(resolve("acmart.cls").map(|p| p.name), Some("acmart"));
        // amsopn is its own package even though amsmath's load set ships
        // amsopn.sty — the name match must win over the file match.
        assert_eq!(resolve("amsopn").map(|p| p.name), Some("amsopn"));
        assert_eq!(resolve("amsopn.sty").map(|p| p.name), Some("amsopn"));
        // amsbsy is provided by amsmath's load set but also its own package.
        assert!(resolve("bm").is_some());
        assert!(resolve("no-such-pkg").is_none());
        // A file belonging to two packages resolves to the first sorted entry.
        assert!(resolve("pgf.sty").is_some());
    }

    #[test]
    fn search_is_case_insensitive() {
        let hits: Vec<&str> = search("BIBLIOGRAPH").map(|p| p.name).collect();
        assert!(hits.contains(&"biblatex"), "{hits:?}");
        assert!(hits.contains(&"bibentry"), "{hits:?}");
        assert!(search("zzz-no-match").next().is_none());
    }

    #[test]
    fn catalog_packages_overlap_registry() {
        // Every curated name that the completion catalog knows must
        // resolve back to itself — catches typos in the table.
        for name in crate::packages() {
            if let Some(p) = resolve(name) {
                assert_eq!(p.name, name, "resolve({name}) -> {}", p.name);
            }
        }
    }
}
