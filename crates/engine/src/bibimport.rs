//! `bib.import` — fold an external bibliography export into a project
//! `.bib` file. CSL-JSON (Zotero's native interchange, also Better
//! BibTeX's export) and RIS (the tagged format every reference manager
//! emits) are detected from content, converted to BibTeX, deduplicated
//! against what the project already holds (DOI/eprint/normalized title,
//! flag-style like `bib.audit`), then appended with generated keys.
//!
//! Two inputs: `path` reads a file (native; absolute allowed — the
//! source is read-only), `data` takes the raw text so CLI/MCP callers
//! and the web build can pipe content in without a filesystem read.
//! The *target* is always a project `.bib`, same rules as `cite.fetch`.
//!
//! Input is untrusted: bounded bytes, bounded entries, and the
//! converters are pure functions over strings — no panics on garbage.

use serde_json::{json, Value};
use texparse::parse_bib;

use crate::{EngineError, Session};

/// Cap on import source size — a Zotero library export is text; beyond
/// a few MiB it's not something we fold blindly.
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// Cap on entries per import run.
const MAX_ENTRIES: usize = 2048;

/// Which serialization the source uses, sniffed from content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// JSON array/object of CSL items (Zotero export).
    CslJson,
    /// RIS tagged records (`TY  -` … `ER  -`).
    Ris,
}

/// Sniff the source format: JSON openers → CSL-JSON, `TY  -` tagged
/// lines → RIS. Everything else is rejected by the caller.
fn sniff(data: &str) -> Option<Format> {
    let t = data.trim_start();
    if t.starts_with('[') || t.starts_with('{') {
        return Some(Format::CslJson);
    }
    // RIS records open with a `TY  -` tag line anywhere near the top.
    for line in t.lines().take(8) {
        if line.starts_with("TY  -") {
            return Some(Format::Ris);
        }
        if !line.trim().is_empty() && !line.contains("  -") {
            break;
        }
    }
    None
}

// ---------- shared helpers ----------

/// BibTeX-safe value: braces kept balanced by dropping stray closers
/// that would end the field early. Internal braces survive.
fn bibval(v: &str) -> String {
    let mut depth = 0i32;
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '{' => {
                depth += 1;
                out.push(c);
            }
            '}' => {
                if depth > 0 {
                    depth -= 1;
                    out.push(c);
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// Emit `@kind{key, fields…}` from `(name, value)` pairs — empty
/// values are dropped.
fn emit(kind: &str, key: &str, fields: &[(&str, String)]) -> String {
    let mut e = format!("@{kind}{{{key},\n");
    for (name, value) in fields {
        if !value.trim().is_empty() {
            e.push_str(&format!("  {name} = {{{}}},\n", bibval(value)));
        }
    }
    e.push_str("}\n");
    e
}

/// First 4-digit run — both CSL `date-parts` and RIS `PY`/`DA` give us
/// at least a year, sometimes a full date.
fn year_of(raw: &str) -> String {
    raw.split(|c: char| !c.is_ascii_digit()).find(|s| s.len() == 4).unwrap_or("").to_string()
}

/// Normalized title used for duplicate detection — same rule as
/// `bib.audit` so a re-import flags the same entries it would report.
fn norm_title(t: &str) -> String {
    let n: String = t.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().to_lowercase();
    n.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------- CSL-JSON ----------

/// CSL item type → BibTeX entry type. `thesis` distinguishes master's
/// from PhD via the genre field when present.
fn csl_kind(v: &Value) -> &'static str {
    let t = v.get("type").and_then(Value::as_str).unwrap_or("");
    match t {
        "article-journal" | "article-magazine" | "article-newspaper" => "article",
        "book" => "book",
        "chapter" => "inbook",
        "paper-conference" => "inproceedings",
        "thesis" => {
            if v.get("genre").and_then(Value::as_str).is_some_and(|g| g.to_ascii_lowercase().contains("master")) {
                "mastersthesis"
            } else {
                "phdthesis"
            }
        }
        "report" => "techreport",
        "manuscript" => "unpublished",
        _ => "misc",
    }
}

/// CSL name list → `Family, Given and …`. `literal` names pass through.
fn csl_names(v: &Value, field: &str) -> String {
    v.get(field)
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|n| {
                    if let Some(lit) = n.get("literal").and_then(Value::as_str) {
                        return Some(lit.to_string());
                    }
                    let fam = n.get("family").and_then(Value::as_str).unwrap_or("");
                    let giv = n.get("given").and_then(Value::as_str).unwrap_or("");
                    if fam.is_empty() && giv.is_empty() {
                        None
                    } else if fam.is_empty() {
                        Some(giv.to_string())
                    } else {
                        Some(format!("{fam}, {giv}").trim_end_matches(", ").to_string())
                    }
                })
                .collect::<Vec<_>>()
                .join(" and ")
        })
        .unwrap_or_default()
}

/// One CSL-JSON item → BibTeX entry text (key `k` placeholder).
/// `container-title` lands as `journal` for articles, `booktitle` for
/// parts of larger works. Returns `None` when there's nothing citable.
fn csl_to_bibtex(v: &Value) -> Option<String> {
    if !v.is_object() {
        return None;
    }
    let kind = csl_kind(v);
    let title = v.get("title").and_then(Value::as_str).unwrap_or("");
    let container = v.get("container-title").and_then(Value::as_str).unwrap_or("");
    if title.is_empty() && container.is_empty() {
        return None;
    }
    let venue_field = if matches!(kind, "article") { "journal" } else { "booktitle" };
    let year = v
        .get("issued")
        .and_then(|i| {
            i.get("date-parts")
                .and_then(Value::as_array)
                .and_then(|dp| dp.first())
                .and_then(Value::as_array)
                .and_then(|d| d.first())
                .and_then(Value::as_i64)
        })
        .map(|y| y.to_string())
        .or_else(|| v.get("issued").and_then(|i| i.get("raw")).and_then(Value::as_str).map(year_of))
        .unwrap_or_default();
    let fields: Vec<(&str, String)> = vec![
        ("title", title.to_string()),
        ("author", csl_names(v, "author")),
        ("editor", csl_names(v, "editor")),
        ("year", year),
        (venue_field, container.to_string()),
        ("series", v.get("collection-title").and_then(Value::as_str).unwrap_or("").to_string()),
        ("volume", v.get("volume").and_then(Value::as_str).unwrap_or("").to_string()),
        ("number", v.get("issue").and_then(Value::as_str).unwrap_or("").to_string()),
        ("pages", v.get("page").and_then(Value::as_str).unwrap_or("").replace('-', "--")),
        ("publisher", v.get("publisher").and_then(Value::as_str).unwrap_or("").to_string()),
        ("address", v.get("publisher-place").and_then(Value::as_str).unwrap_or("").to_string()),
        ("edition", v.get("edition").and_then(Value::as_str).unwrap_or("").to_string()),
        ("doi", v.get("DOI").and_then(Value::as_str).unwrap_or("").to_string()),
        ("isbn", v.get("ISBN").and_then(Value::as_str).unwrap_or("").to_string()),
        ("issn", v.get("ISSN").and_then(Value::as_str).unwrap_or("").to_string()),
        ("url", v.get("URL").and_then(Value::as_str).unwrap_or("").to_string()),
        ("abstract", v.get("abstract").and_then(Value::as_str).unwrap_or("").to_string()),
    ];
    Some(emit(kind, "k", &fields))
}

/// CSL-JSON source → entry texts. Accepts a top-level array (normal
/// Zotero export) or a single object.
fn csl_entries(data: &str) -> Result<Vec<String>, EngineError> {
    let v: Value = serde_json::from_str(data).map_err(|_| EngineError::BadParam("data"))?;
    let items: Vec<Value> = match v {
        Value::Array(a) => a,
        o => vec![o],
    };
    let mut out = Vec::new();
    for item in items.iter().take(MAX_ENTRIES) {
        if let Some(e) = csl_to_bibtex(item) {
            out.push(e);
        }
    }
    Ok(out)
}

// ---------- RIS ----------

/// RIS `TY` tag → BibTeX entry type.
fn ris_kind(t: &str) -> &'static str {
    match t {
        "JOUR" | "JFULL" | "MGZN" | "NEWS" => "article",
        "BOOK" => "book",
        "CHAP" | "SECT" => "inbook",
        "CONF" | "CPAPER" => "inproceedings",
        "THES" => "phdthesis",
        "RPRT" => "techreport",
        "UNPB" => "unpublished",
        _ => "misc",
    }
}

/// Parse the RIS stream into entries: `TAG  - value` lines, records
/// bounded by `TY`/`ER`. Authors collect across repeated `AU`/`A1`
/// lines; `SP`/`EP` combine into `pages`.
fn ris_entries(data: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut kind = "";
    let mut authors: Vec<String> = Vec::new();
    let mut fields: Vec<(&'static str, String)> = Vec::new();
    let (mut sp, mut ep) = (String::new(), String::new());

    fn flush(kind: &str, authors: &[String], fields: &[(&'static str, String)], sp: &str, ep: &str, out: &mut Vec<String>) {
        if kind.is_empty() {
            return;
        }
        let mut f: Vec<(&'static str, String)> = fields.to_vec();
        f.insert(0, ("author", authors.join(" and ")));
        if !sp.is_empty() {
            f.push(("pages", if ep.is_empty() { sp.to_string() } else { format!("{sp}--{ep}") }));
        }
        if f.iter().any(|(n, v)| n != &"author" && !v.is_empty()) {
            out.push(emit(ris_kind(kind), "k", &f));
        }
    }

    for line in data.lines().take(MAX_ENTRIES * 64) {
        let Some(tag) = line.get(..2) else { continue };
        // Record boundaries are recognized even without a value tail.
        match tag {
            "TY" => {
                // A record missing its `ER` still yields what it had.
                flush(kind, &authors, &fields, &sp, &ep, &mut out);
                kind = line.get(6..).unwrap_or("").trim();
                authors.clear();
                fields.clear();
                sp.clear();
                ep.clear();
                continue;
            }
            "ER" => {
                flush(kind, &authors, &fields, &sp, &ep, &mut out);
                kind = "";
                authors.clear();
                fields.clear();
                sp.clear();
                ep.clear();
                if out.len() >= MAX_ENTRIES {
                    break;
                }
                continue;
            }
            _ => {}
        }
        let Some(sep) = line.get(2..6) else { continue };
        if sep != "  - " && sep != "  -" {
            continue;
        }
        let val = line.get(6..).unwrap_or("").trim().to_string();
        match tag {
            "AU" | "A1" => authors.push(val),
            "ED" | "A2" => fields.push(("editor", val)),
            "TI" | "T1" | "CT" => fields.push(("title", val)),
            "T2" | "JO" | "JF" | "JA" | "BT" => fields.push((if ris_kind(kind) == "article" { "journal" } else { "booktitle" }, val)),
            "PY" | "Y1" | "DA" => fields.push(("year", year_of(&val))),
            "VL" => fields.push(("volume", val)),
            "IS" => fields.push(("number", val)),
            "SP" => sp = val,
            "EP" => ep = val,
            "DO" => fields.push(("doi", val)),
            "UR" | "L1" | "L2" => fields.push(("url", val)),
            "SN" => fields.push((if ris_kind(kind) == "book" || ris_kind(kind) == "inbook" { "isbn" } else { "issn" }, val)),
            "PB" => fields.push(("publisher", val)),
            "CY" | "PP" => fields.push(("address", val)),
            "ET" => fields.push(("edition", val)),
            "AB" | "N2" => fields.push(("abstract", val)),
            "T3" | "NV" => {}
            _ => {}
        }
    }
    flush(kind, &authors, &fields, &sp, &ep, &mut out);
    out
}

/// Convert a source buffer to BibTeX entry texts (key `k`).
/// Public for tests; the session-facing wrapper handles files.
pub fn convert(data: &str) -> Result<(Format, Vec<String>), EngineError> {
    let fmt = sniff(data).ok_or(EngineError::BadParam("data"))?;
    let entries = match fmt {
        Format::CslJson => csl_entries(data)?,
        Format::Ris => ris_entries(data),
    };
    Ok((fmt, entries))
}

/// `bib.import` — params `path` (file to read) or `data` (raw text);
/// `bib` picks the target `.bib` (cite.fetch rules). Duplicates by
/// DOI/eprint/normalized title are skipped, keys uniquified.
#[cfg(not(target_arch = "wasm32"))]
pub fn import(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    use crate::citefetch::read_limited;
    let dir = read_limited::project_dir(sess).ok_or(EngineError::NoFile)?;

    let data = if let Some(d) = params.get("data").and_then(Value::as_str) {
        if d.len() as u64 > MAX_BYTES {
            return Err(EngineError::BadParam("data"));
        }
        d.to_string()
    } else if let Some(p) = params.get("path").and_then(Value::as_str) {
        let src = std::path::Path::new(p);
        let src = if src.is_absolute() { src.to_path_buf() } else { dir.join(src) };
        // Bounded read — the file is untrusted.
        use std::io::Read;
        let f = std::fs::File::open(&src)?;
        let mut buf = String::new();
        f.take(MAX_BYTES).read_to_string(&mut buf)?;
        buf
    } else {
        return Err(EngineError::BadParam("path"));
    };

    let (fmt, entries) = convert(&data)?;

    // Existing identifiers + keys across the project's .bib files —
    // the duplicate bar and the uniquifier read the same world.
    let mut idents: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut keys: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in read_limited::bib_files(&dir) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for e in parse_bib(&text).entries {
                keys.insert(e.key.clone());
                for f in ["doi", "eprint"] {
                    if let Some(v) = e.field(f) {
                        idents.insert(format!("{f}:{}", v.trim().to_ascii_lowercase()));
                    }
                }
                if let Some(t) = e.field("title") {
                    let n = norm_title(t);
                    if n.len() >= 12 {
                        idents.insert(format!("title:{n}"));
                    }
                }
            }
        }
    }

    // Target .bib — same rules as cite.fetch.
    let named = params.get("bib").and_then(Value::as_str).unwrap_or("");
    let target = if !named.is_empty() {
        let p = std::path::Path::new(named);
        if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
            return Err(EngineError::BadParam("bib"));
        }
        dir.join(p)
    } else {
        let declared = sess.structure().bibliographies.first().map(|(b, _)| b.clone());
        match declared {
            Some(b) => dir.join(if std::path::Path::new(&b).extension().is_none() { format!("{b}.bib") } else { b }),
            None => dir.join("refs.bib"),
        }
    };

    let mut text = std::fs::read_to_string(&target).unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }

    let mut added: Vec<String> = Vec::new();
    let mut dups = 0usize;
    for raw in entries {
        let parsed = parse_bib(&raw);
        let Some(e) = parsed.entries.first() else { continue };
        // Duplicate if DOI/eprint/normalized title already exists.
        let mut hit = false;
        for f in ["doi", "eprint"] {
            if let Some(v) = e.field(f) {
                if idents.contains(&format!("{f}:{}", v.trim().to_ascii_lowercase())) {
                    hit = true;
                }
            }
        }
        if let Some(t) = e.field("title") {
            let n = norm_title(t);
            if n.len() >= 12 && idents.contains(&format!("title:{n}")) {
                hit = true;
            }
        }
        if hit {
            dups += 1;
            continue;
        }
        // Unique key within project + this run.
        let base = crate::citefetch::make_key(&raw);
        let mut key = base.clone();
        if keys.contains(&key) {
            key = ('b'..='z').map(|c| format!("{base}{c}")).find(|c| !keys.contains(c)).unwrap_or_else(|| format!("{base}x"));
        }
        keys.insert(key.clone());
        // Record its identifiers so a duplicated *file* dedups itself.
        for f in ["doi", "eprint"] {
            if let Some(v) = e.field(f) {
                idents.insert(format!("{f}:{}", v.trim().to_ascii_lowercase()));
            }
        }
        if let Some(t) = e.field("title") {
            let n = norm_title(t);
            if n.len() >= 12 {
                idents.insert(format!("title:{n}"));
            }
        }
        // Rekey: replace the `k` placeholder after the opening brace.
        let mut entry = raw.clone();
        if let Some(open) = entry.find('{') {
            if let Some(end) = entry.get(open..).and_then(|r| r.find(',').map(|e| open + e)) {
                entry.replace_range(open + 1..end, &key);
            }
        }
        text.push_str(&entry);
        added.push(key);
    }
    craft_atomic::atomic_write(&target, text.as_bytes())?;

    Ok(json!({
        "status": "ok",
        "format": match fmt { Format::CslJson => "csl-json", Format::Ris => "ris" },
        "imported": added.len(),
        "duplicates": dups,
        "keys": added,
        "bib": target.display().to_string(),
    }))
}

/// The web build can't read arbitrary paths — `data`-only imports still
/// can't write a project `.bib`, so the command is native for now.
#[cfg(target_arch = "wasm32")]
pub fn import(_sess: &Session, _params: &Value) -> Result<Value, EngineError> {
    Err(EngineError::Tool("bib.import writes to project files — desktop builds only".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_formats() {
        assert_eq!(sniff("[{\"type\":\"book\"}]"), Some(Format::CslJson));
        assert_eq!(sniff("TY  - JOUR\nTI  - X\nER  -"), Some(Format::Ris));
        assert_eq!(sniff("hello"), None);
    }

    #[test]
    fn csl_maps_fields_and_names() {
        let data = r#"[{
            "type":"article-journal","title":"On X","DOI":"10.1/a",
            "author":[{"family":"Doe","given":"Jane"},{"family":"Roe","given":"Richard"}],
            "issued":{"date-parts":[[2020,5]]},"container-title":"J. Testing",
            "volume":"3","issue":"2","page":"10-20","publisher":"Acme"
        }]"#;
        let (_, entries) = convert(data).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert!(e.starts_with("@article{"));
        assert!(e.contains("author = {Doe, Jane and Roe, Richard}"));
        assert!(e.contains("journal = {J. Testing}"));
        assert!(e.contains("doi = {10.1/a}"));
        assert!(e.contains("pages = {10--20}"));
    }

    #[test]
    fn csl_types_and_thesis_genre() {
        assert_eq!(csl_kind(&json!({"type":"thesis","genre":"PhD"})), "phdthesis");
        assert_eq!(csl_kind(&json!({"type":"thesis","genre":"Master's thesis"})), "mastersthesis");
        assert_eq!(csl_kind(&json!({"type":"paper-conference"})), "inproceedings");
    }

    #[test]
    fn ris_records_split_and_map() {
        let data = "TY  - JOUR\nAU  - Doe, Jane\nAU  - Roe, Richard\nTI  - On X\nJO  - J Test\nPY  - 2020\nVL  - 3\nSP  - 10\nEP  - 20\nDO  - 10.1/a\nER  -\nTY  - BOOK\nTI  - Y\nSN  - 978-1\nER  -";
        let (_, entries) = convert(data).unwrap();
        assert_eq!(entries.len(), 2);
        assert!(entries[0].starts_with("@article{"));
        assert!(entries[0].contains("author = {Doe, Jane and Roe, Richard}"));
        assert!(entries[0].contains("pages = {10--20}"));
        assert!(entries[1].starts_with("@book{"));
        assert!(entries[1].contains("isbn = {978-1}"));
    }

    #[test]
    fn hostile_input_never_panics() {
        for junk in ["", "[", "{", "TY  -", "[{}]", "[[[]]]", "TY  - \nER  -", "%random"] {
            let _ = convert(junk);
        }
        // Unbalanced braces in a value can't break the emitted entry.
        let e = emit("misc", "k", &[("title", "a}b{c".to_string())]);
        assert_eq!(parse_bib(&e).entries.len(), 1);
    }

    #[test]
    fn over_long_source_capped() {
        let mut data = String::from("TY  - JOUR\n");
        for _ in 0..MAX_ENTRIES + 4 {
            data.push_str("TI  - T\nER  -\nTY  - JOUR\n");
        }
        data.push_str("ER  -");
        let (_, entries) = convert(&data).unwrap();
        assert!(entries.len() <= MAX_ENTRIES);
    }
}
