//! `cite.fetch` — turn a pasted identifier into a `.bib` entry.
//!
//! The parser accepts DOIs in any written form (bare, `doi:`/`DOI `
//! prefixes, doi.org/dx.doi.org URLs, percent-encoded links), arXiv ids
//! old and new (incl. `arXiv:` and abs/pdf links), and PMID/PMCID. Input
//! is untrusted: zero-width characters and NBSPs are stripped, matches
//! are strict-shape only, and every read is bounded.
//!
//! Resolution is keyless and local-first honest: DOIs resolve through
//! doi.org/Crossref content negotiation (they return BibTeX directly),
//! arXiv through the public export API, PMID through NCBI eutils. No API
//! key anywhere. Offline/failed lookups become *pending* entries rather
//! than being dropped — the report says `unresolved` so the UI can offer
//! a retry.

use serde_json::{json, Value};
use texparse::parse_bib;

use crate::{EngineError, Session};

/// Max identifier input length (chars) — pasted text is untrusted.
const MAX_ID_LEN: usize = 512;
/// Max response body we read from any provider.
const MAX_BODY: u64 = 512 * 1024;
/// Provider request ceiling.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// A normalized identifier.
#[derive(Debug, Clone, PartialEq)]
pub enum Ident {
    /// Canonical lowercase DOI, no scheme/prefix.
    Doi(String),
    /// arXiv id without version (`2501.00663` or `hep-th/9901001`).
    Arxiv(String),
    /// Numeric PubMed id.
    Pmid(String),
    /// `PMC…` PubMed Central id.
    Pmcid(String),
}

/// Strip zero-width and non-breaking characters pasted identifiers
/// often carry.
fn scrub(text: &str) -> String {
    text.chars().filter(|c| !matches!(c, '\u{200b}'..='\u{200f}' | '\u{feff}' | '\u{00a0}' | '\u{2060}')).collect()
}

/// Minimal %HH percent-decoding for URL paths (keeps `%` we can't parse).
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if let (Some(h), Some(l)) = (b.get(i + 1), b.get(i + 2)) {
                if let Ok(v) = u8::from_str_radix(&format!("{}{}", *h as char, *l as char), 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A bare DOI body: `10.<4+ digits>/<rest>` — registrar prefix is digits.
fn looks_like_doi(s: &str) -> bool {
    let Some(rest) = s.strip_prefix("10.") else { return false };
    let Some(slash) = rest.find('/') else { return false };
    slash >= 4 && rest[..slash].chars().all(|c| c.is_ascii_digit()) && slash + 1 < rest.len()
}

/// A bare new-style arXiv id `YYMM.NNNNN` or old-style `archive/YYMMNNN`
/// (a trailing `vN` version is stripped first) — kept strict so ordinary
/// words can't match.
fn looks_like_arxiv(s: &str) -> bool {
    let body = arxiv_body(s);
    if let Some((y, rest)) = body.split_once('.') {
        return y.len() == 4 && y.chars().all(|c| c.is_ascii_digit()) && rest.len() >= 4 && rest.chars().all(|c| c.is_ascii_digit());
    }
    if let Some((arch, rest)) = body.split_once('/') {
        return !arch.is_empty()
            && arch.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            && rest.len() >= 6
            && rest.chars().all(|c| c.is_ascii_digit());
    }
    false
}

/// Normalize an arXiv body: drop a trailing `vN` version.
fn arxiv_body(s: &str) -> String {
    match s.rsplit_once('v') {
        Some((base, v)) if !base.is_empty() && v.chars().all(|c| c.is_ascii_digit()) => base.to_string(),
        _ => s.to_string(),
    }
}

/// Parse any paste-able identifier form into a normalized [`Ident`].
/// Returns `None` when nothing matches — the caller reports `invalid`.
pub fn parse_identifier(raw: &str) -> Option<Ident> {
    let s = scrub(raw);
    let s = s.trim();
    if s.is_empty() || s.chars().count() > MAX_ID_LEN {
        return None;
    }
    let low = s.to_ascii_lowercase();

    // URLs first — the path after the host carries the identifier.
    for (host, kind) in [
        ("doi.org/", "doi"),
        ("dx.doi.org/", "doi"),
        ("arxiv.org/abs/", "arxiv"),
        ("arxiv.org/pdf/", "arxiv"),
        ("pubmed.ncbi.nlm.nih.gov/", "pmid"),
        ("ncbi.nlm.nih.gov/pmc/articles/", "pmcid"),
    ] {
        if let Some(pos) = low.find(host) {
            let path = s.get(pos + host.len()..).unwrap_or("");
            let path = path.split(['?', '#', ' ']).next().unwrap_or("").trim_end_matches(['/', '.', ';', ',', ')']);
            let path = path.strip_suffix(".pdf").unwrap_or(path);
            let dec = percent_decode(path);
            return match kind {
                "doi" if looks_like_doi(&dec) => Some(Ident::Doi(dec.to_ascii_lowercase())),
                "arxiv" if looks_like_arxiv(&dec) => Some(Ident::Arxiv(arxiv_body(&dec))),
                "pmid" if path.chars().all(|c| c.is_ascii_digit()) => Some(Ident::Pmid(path.to_string())),
                "pmcid" => {
                    let id = path.trim_start_matches("PMC");
                    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
                        Some(Ident::Pmcid(format!("PMC{id}")))
                    } else {
                        None
                    }
                }
                _ => None,
            };
        }
    }

    // Prefixed forms.
    for (p, kind) in [("doi:", "doi"), ("doi ", "doi"), ("arxiv:", "arxiv"), ("pmid:", "pmid"), ("pmcid:", "pmcid")] {
        if let Some(rest) = low.strip_prefix(p) {
            let rest = s.get(s.len().saturating_sub(rest.len())..).unwrap_or("").trim();
            return match kind {
                "doi" if looks_like_doi(rest) => Some(Ident::Doi(rest.to_ascii_lowercase())),
                "arxiv" if looks_like_arxiv(rest) => Some(Ident::Arxiv(arxiv_body(rest))),
                "pmid" if rest.chars().all(|c| c.is_ascii_digit()) => Some(Ident::Pmid(rest.to_string())),
                "pmcid" => {
                    let id = rest.trim_start_matches("PMC");
                    if !id.is_empty() && id.chars().all(|c| c.is_ascii_digit()) {
                        Some(Ident::Pmcid(format!("PMC{id}")))
                    } else {
                        None
                    }
                }
                _ => None,
            };
        }
    }

    // Bare forms.
    if looks_like_doi(s) {
        return Some(Ident::Doi(low));
    }
    if looks_like_arxiv(s) {
        return Some(Ident::Arxiv(arxiv_body(s)));
    }
    if low.strip_prefix("pmc").is_some_and(|r| r.chars().all(|c| c.is_ascii_digit())) {
        return Some(Ident::Pmcid(s.to_ascii_uppercase().replace("pmc", "PMC")));
    }
    None
}

/// Generate a cite key `surnameYYYYword` from parsed fields; `anon` when
/// the entry has no author.
pub fn make_key(entry_text: &str) -> String {
    let file = parse_bib(entry_text);
    let Some(e) = file.entries.first() else {
        return "ref".to_string();
    };
    let surname = e
        .field("author")
        .and_then(|a| a.split(" and ").next())
        .map(|first| {
            // "Last, First" → Last; "First Last" → Last.
            let name = if first.contains(',') { first.split(',').next().unwrap_or(first) } else { first.split_whitespace().last().unwrap_or(first) };
            name.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "anon".to_string());
    let year =
        e.field("year").and_then(|y| y.chars().filter(|c| c.is_ascii_digit()).collect::<String>().get(..4).map(str::to_string)).unwrap_or_default();
    let word = e
        .field("title")
        .and_then(|t| {
            t.split_whitespace()
                .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
                .find(|w| !w.is_empty() && !matches!(w.as_str(), "a" | "an" | "the" | "on" | "of" | "in"))
        })
        .unwrap_or_default();
    let key = format!("{surname}{year}{word}");
    if key.is_empty() {
        "ref".to_string()
    } else {
        key
    }
}

/// Replace the entry's own key with `key`, keeping everything else.
fn rekey(entry_text: &str, key: &str) -> String {
    let mut out = entry_text.to_string();
    if let Some(open) = out.find('{') {
        if let Some(end) = out.get(open..).and_then(|r| r.find(',').map(|e| open + e)) {
            out.replace_range(open + 1..end, key);
        }
    }
    out
}

/// Do any of the project's `.bib` files already hold this identifier?
/// Returns the existing key so the UI can cite it instead of duplicating.
#[cfg(not(target_arch = "wasm32"))]
fn find_existing(sess: &Session, ident: &Ident) -> Option<String> {
    let dir = read_limited::project_dir(sess)?;
    let wanted = match ident {
        Ident::Doi(d) => d.clone(),
        Ident::Arxiv(a) => a.clone(),
        Ident::Pmid(p) => format!("pmid:{p}"),
        Ident::Pmcid(p) => p.clone(),
    };
    for path in read_limited::bib_files(&dir) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for e in parse_bib(&text).entries {
            for field in ["doi", "eprint", "pmid"] {
                if let Some(v) = e.field(field) {
                    // Normalize the stored value through the same parser —
                    // a `doi = {https://doi.org/…}` value still matches.
                    if parse_identifier(v) == Some(ident.clone()) || v.trim().eq_ignore_ascii_case(&wanted) {
                        return Some(e.key.clone());
                    }
                }
            }
        }
    }
    None
}

/// Which field name an identifier lands in for a pending stub entry.
#[cfg(not(target_arch = "wasm32"))]
fn pending_field(ident: &Ident) -> (&'static str, String) {
    match ident {
        Ident::Doi(d) => ("doi", d.clone()),
        Ident::Arxiv(a) => ("eprint", a.clone()),
        Ident::Pmid(p) => ("note", format!("PMID {p}")),
        Ident::Pmcid(p) => ("note", format!("PMCID {p}")),
    }
}

/// A stub `.bib` entry written when resolution can't happen — offline,
/// provider down, or the id is unregistered but shaped right. Marked so
/// `cite.fetch` on the same text retries instead of duplicating.
#[cfg(not(target_arch = "wasm32"))]
fn pending_entry(ident: &Ident, key: &str) -> String {
    let (field, value) = pending_field(ident);
    if field == "note" {
        return format!("@misc{{{key},\n  note = {{{value} — unresolved; run cite.fetch again when online}},\n  year = {{}},\n}}\n");
    }
    format!("@misc{{{key},\n  {field} = {{{value}}},\n  note = {{unresolved — run cite.fetch again when online}},\n  year = {{}},\n}}\n")
}

/// `cite.fetch` — resolve `{identifier}` to a BibTeX entry, append it to
/// the project `.bib` (`bib` param or the first declared bibliography,
/// else `refs.bib`), and return the key. Duplicates return the existing
/// key; unresolvable identifiers are written as pending stubs.
#[cfg(not(target_arch = "wasm32"))]
pub fn fetch(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let raw = params.get("identifier").and_then(Value::as_str).ok_or(EngineError::BadParam("identifier"))?;
    let dir = read_limited::project_dir(sess).ok_or(EngineError::NoFile)?;
    let Some(ident) = parse_identifier(raw) else {
        return Ok(json!({"status": "invalid", "identifier": raw}));
    };
    if let Some(key) = find_existing(sess, &ident) {
        return Ok(json!({"status": "existing", "key": key}));
    }

    let resolved = resolve(&ident);
    let (entry, provider) = match resolved {
        Ok((body, provider)) => (body, json!(provider)),
        Err(e) => {
            // Pending stub — identifiable, retryable, never dropped.
            let key = pending_key(&ident);
            let entry = pending_entry(&ident, &key);
            let bib = append_entry(sess, &dir, params, &entry)?;
            return Ok(json!({
                "status": "unresolved",
                "key": key,
                "entry": entry,
                "bib": bib.display().to_string(),
                "error": e.to_string(),
            }));
        }
    };

    let key = unique_key(sess, &make_key(&entry));
    let entry = rekey(&entry, &key);
    let bib = append_entry(sess, &dir, params, &entry)?;
    Ok(json!({
        "status": "added",
        "key": key,
        "entry": entry,
        "bib": bib.display().to_string(),
        "provider": provider,
    }))
}

/// `cite.fetch` is network-dependent — unavailable on wasm.
#[cfg(target_arch = "wasm32")]
pub fn fetch(_sess: &Session, _params: &Value) -> Result<Value, EngineError> {
    Err(EngineError::Tool("cite.fetch needs a network resolver — desktop builds only".into()))
}

/// `base`, or `base`+`b`,`c`,… when the key already exists anywhere in
/// the project's `.bib` files — two papers can generate the same
/// author-year-word key.
#[cfg(not(target_arch = "wasm32"))]
fn unique_key(sess: &Session, base: &str) -> String {
    let Some(dir) = read_limited::project_dir(sess) else {
        return base.to_string();
    };
    let mut keys = std::collections::HashSet::new();
    for path in read_limited::bib_files(&dir) {
        if let Ok(text) = std::fs::read_to_string(&path) {
            keys.extend(parse_bib(&text).entries.into_iter().map(|e| e.key));
        }
    }
    if !keys.contains(base) {
        return base.to_string();
    }
    for c in 'b'..='z' {
        let cand = format!("{base}{c}");
        if !keys.contains(&cand) {
            return cand;
        }
    }
    format!("{base}x")
}

/// A stable key for a pending stub — derived from the identifier itself
/// so a retry lands under the same name.
#[cfg(not(target_arch = "wasm32"))]
fn pending_key(ident: &Ident) -> String {
    match ident {
        Ident::Doi(d) => format!("doi{}", d.chars().filter(|c| c.is_alphanumeric()).collect::<String>().chars().take(24).collect::<String>()),
        Ident::Arxiv(a) => format!("arxiv{}", a.chars().filter(|c| c.is_alphanumeric()).collect::<String>()),
        Ident::Pmid(p) => format!("pmid{p}"),
        Ident::Pmcid(p) => format!("pmc{}", p.trim_start_matches("PMC")),
    }
}

/// Append `entry` to the project's bib file: `params["bib"]` names it
/// (resolved inside the project dir; parents/absolute paths refused),
/// else the first declared bibliography, else `refs.bib`.
#[cfg(not(target_arch = "wasm32"))]
fn append_entry(sess: &Session, dir: &std::path::Path, params: &Value, entry: &str) -> Result<std::path::PathBuf, EngineError> {
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
    text.push_str(entry);
    craft_atomic::atomic_write(&target, text.as_bytes())?;
    Ok(target)
}

/// Resolve an identifier through the public, keyless providers.
/// Returns `(bibtex_body, provider_name)`.
#[cfg(not(target_arch = "wasm32"))]
fn resolve(ident: &Ident) -> Result<(String, &'static str), EngineError> {
    match ident {
        Ident::Doi(doi) => {
            // doi.org content negotiation returns BibTeX directly;
            // Crossref's transform endpoint is the same thing upstream.
            let enc = urlencode_path(doi);
            for (url, provider) in [
                (format!("https://doi.org/{enc}"), "doi.org"),
                (format!("https://api.crossref.org/works/{enc}/transform/application/x-bibtex"), "crossref"),
            ] {
                if let Ok(body) = get_text(&url, "application/x-bibtex") {
                    if body.trim_start().starts_with('@') {
                        return Ok((body, provider));
                    }
                }
            }
            Err(EngineError::Tool("doi.org and crossref both failed to resolve the DOI".into()))
        }
        Ident::Arxiv(id) => {
            let body = get_text(&format!("https://export.arxiv.org/api/query?id_list={id}"), "application/atom+xml")
                .map_err(|e| EngineError::Tool(format!("arxiv: {e}")))?;
            let entry = arxiv_to_bibtex(&body, id).ok_or_else(|| EngineError::Tool("arxiv response had no usable entry".into()))?;
            Ok((entry, "arxiv"))
        }
        Ident::Pmid(p) | Ident::Pmcid(p) => {
            let id = p.trim_start_matches("PMC");
            let body =
                get_text(&format!("https://eutils.ncbi.nlm.nih.gov/entrez/eutils/esummary.fcgi?db=pubmed&id={id}&retmode=json"), "application/json")
                    .map_err(|e| EngineError::Tool(format!("eutils: {e}")))?;
            let entry = pmid_to_bibtex(&body, p).ok_or_else(|| EngineError::Tool("eutils response had no usable record".into()))?;
            Ok((entry, "pubmed"))
        }
    }
}

/// One bounded GET — 20 s ceiling, 512 KiB body cap.
#[cfg(not(target_arch = "wasm32"))]
fn get_text(url: &str, accept: &str) -> Result<String, EngineError> {
    let config = ureq::config::Config::builder().timeout_global(Some(TIMEOUT)).build();
    let agent = ureq::Agent::new_with_config(config);
    let mut resp = agent.get(url).header("Accept", accept).call().map_err(|e| EngineError::Tool(format!("get {url}: {e}")))?;
    use std::io::Read;
    let mut buf = Vec::new();
    std::io::Read::take(resp.body_mut().as_reader(), MAX_BODY).read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Path-segment encoding for DOIs — `/` and alphanumerics pass through;
/// everything else percent-encodes.
#[cfg(not(target_arch = "wasm32"))]
fn urlencode_path(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '.' | '_' | '~' | '/' | '(' | ')' => c.to_string(),
            _ => format!("%{:02X}", c as u32),
        })
        .collect()
}

/// `<tag>` text inside the first `<entry>` of an arXiv Atom response.
#[cfg(not(target_arch = "wasm32"))]
fn tag_text<'a>(body: &'a str, tag: &str) -> Option<&'a str> {
    let open = format!("<{tag}");
    let start = body.find(&open)?;
    let gt = body.get(start..)?.find('>')? + start + 1;
    let close = format!("</{tag}>");
    let end = body.get(gt..)?.find(&close)? + gt;
    body.get(gt..end)
}

#[cfg(not(target_arch = "wasm32"))]
fn xml_unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&").replace("&quot;", "\"").replace("&apos;", "'")
}

/// Build a `@misc` BibTeX entry from an arXiv Atom feed.
#[cfg(not(target_arch = "wasm32"))]
fn arxiv_to_bibtex(body: &str, id: &str) -> Option<String> {
    let entry = body.split_once("<entry>").map(|(_, r)| r).and_then(|r| r.split("</entry>").next())?;
    let title = tag_text(entry, "title").map(|t| xml_unescape(&t.split_whitespace().collect::<Vec<_>>().join(" ")))?;
    let authors: Vec<String> = entry.match_indices("<name>").filter_map(|(i, _)| tag_text(entry.get(i..)?, "name").map(xml_unescape)).collect();
    let year = tag_text(entry, "published").and_then(|p| p.get(..4)).unwrap_or("");
    let primary = entry
        .split_once("primary_category")
        .and_then(|(_, r)| r.split_once("term=\"").map(|(_, r)| r))
        .and_then(|r| r.split('"').next())
        .unwrap_or("");
    let doi = tag_text(entry, "arxiv:doi").map(str::to_string);
    let mut e = format!(
        "@misc{{k,\n  title = {{{title}}},\n  author = {{{}}},\n  year = {{{year}}},\n  eprint = {{{id}}},\n  archivePrefix = {{arXiv}},\n  primaryClass = {{{primary}}},\n",
        authors.join(" and ")
    );
    if let Some(d) = doi {
        e.push_str(&format!("  doi = {{{d}}},\n"));
    }
    e.push_str("}\n");
    Some(e)
}

/// Build an `@article` BibTeX entry from an NCBI esummary JSON record.
#[cfg(not(target_arch = "wasm32"))]
fn pmid_to_bibtex(body: &str, id: &str) -> Option<String> {
    let v: Value = serde_json::from_str(body).ok()?;
    let num = id.trim_start_matches("PMC");
    let rec = v.get("result")?.get(num)?;
    let title = rec.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    let authors: Vec<String> = rec
        .get("authors")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.get("name").and_then(Value::as_str).map(str::to_string)).collect())
        .unwrap_or_default();
    let year = rec.get("pubdate").and_then(Value::as_str).and_then(|d| d.get(..4)).unwrap_or("");
    let journal = rec.get("source").and_then(Value::as_str).unwrap_or("");
    let doi = rec
        .get("articleids")
        .and_then(Value::as_array)
        .and_then(|a| a.iter().find(|x| x.get("idtype").and_then(Value::as_str) == Some("doi")))
        .and_then(|x| x.get("value").and_then(Value::as_str));
    let mut e = format!(
        "@article{{k,\n  title = {{{title}}},\n  author = {{{}}},\n  year = {{{year}}},\n  journal = {{{journal}}},\n",
        authors.join(" and ")
    );
    if let Some(d) = doi {
        e.push_str(&format!("  doi = {{{d}}},\n"));
    }
    e.push_str("}\n");
    Some(e)
}

/// `bib.audit` — flag, never merge. Scans the project's `.bib` files and
#[cfg(not(target_arch = "wasm32"))]
/// reports three things: entries that share an identifier (DOI/eprint/
/// normalized title → probable duplicates to review), pending stubs left
/// by unresolved `cite.fetch` calls, and per-file parse issue counts.
/// Everything is a report — no entry is ever rewritten or removed.
pub fn audit(sess: &Session) -> Result<Value, EngineError> {
    let Some(dir) = read_limited::project_dir(sess) else {
        return Err(EngineError::NoFile);
    };
    let mut dups: Vec<Value> = Vec::new();
    let mut pending: Vec<Value> = Vec::new();
    let mut issues: Vec<Value> = Vec::new();
    let mut entries = 0usize;
    // identifier value → (field, keys) — a shared DOI/eprint/title across
    // two keys is a probable duplicate.
    let mut seen: Vec<(String, String, String)> = Vec::new(); // (ident, key, field)

    for path in read_limited::bib_files(&dir) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        let file = parse_bib(&text);
        let fname = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if !file.issues.is_empty() {
            issues.push(json!({"file": fname, "count": file.issues.len()}));
        }
        for e in &file.entries {
            entries += 1;
            // A cite.fetch pending stub: `note` carries the marker.
            if e.field("note").is_some_and(|n| n.to_ascii_lowercase().contains("unresolved")) {
                pending.push(json!({"key": e.key, "file": fname}));
            }
            for f in ["doi", "eprint"] {
                if let Some(v) = e.field(f) {
                    seen.push((v.trim().to_ascii_lowercase(), e.key.clone(), f.to_string()));
                }
            }
            if let Some(t) = e.field("title") {
                let norm: String = t.chars().filter(|c| c.is_alphanumeric() || c.is_whitespace()).collect::<String>().to_lowercase();
                let norm = norm.split_whitespace().collect::<Vec<_>>().join(" ");
                if norm.len() >= 12 {
                    seen.push((norm, e.key.clone(), "title".to_string()));
                }
            }
        }
    }
    // Group identical identifiers across *different* keys — flag them.
    for i in 0..seen.len() {
        for j in i + 1..seen.len() {
            if seen[i].0 == seen[j].0 && seen[i].1 != seen[j].1 && seen[i].2 == seen[j].2 {
                dups.push(json!({"field": seen[i].2, "value": seen[i].0, "keys": [seen[i].1, seen[j].1]}));
            }
        }
    }
    Ok(json!({
        "entries": entries,
        "duplicates": dups,
        "pending": pending,
        "issues": issues,
    }))
}

/// `bib.audit` needs the project filesystem — empty report on wasm.
#[cfg(target_arch = "wasm32")]
pub fn audit(_sess: &Session) -> Result<Value, EngineError> {
    Ok(json!({"entries": 0, "duplicates": [], "pending": [], "issues": []}))
}

/// Helper namespace — collecting a project's `.bib` files without
/// pulling a dir walker into this module.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod read_limited {
    use std::path::{Path, PathBuf};

    /// The project dir for a session — the open file's parent, or the
    /// process working dir when the file was opened with a bare
    /// relative name (`parent()` of `main.tex` is `""`, which no
    /// `read_dir` accepts).
    pub fn project_dir(sess: &crate::Session) -> Option<PathBuf> {
        let p = sess.path()?;
        match p.parent() {
            Some(d) if !d.as_os_str().is_empty() => Some(d.to_path_buf()),
            Some(_) => Some(PathBuf::from(".")),
            None => None,
        }
    }

    /// `.bib` files directly inside `dir` — bounded, top-level only.
    pub fn bib_files(dir: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .map(|rd| rd.flatten().take(256).map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "bib") && p.is_file()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doi_forms() {
        assert_eq!(parse_identifier("10.1038/nature12373"), Some(Ident::Doi("10.1038/nature12373".into())));
        assert_eq!(parse_identifier("doi:10.1145/3292500.3330670"), Some(Ident::Doi("10.1145/3292500.3330670".into())));
        assert_eq!(parse_identifier("DOI 10.1000/xyz"), Some(Ident::Doi("10.1000/xyz".into())));
        assert_eq!(parse_identifier("https://doi.org/10.1109/5.771073"), Some(Ident::Doi("10.1109/5.771073".into())));
        assert_eq!(parse_identifier("https://dx.doi.org/10.1007/s00221-011-2879-3?x=1"), Some(Ident::Doi("10.1007/s00221-011-2879-3".into())));
    }

    #[test]
    fn arxiv_forms() {
        assert_eq!(parse_identifier("2501.00663"), Some(Ident::Arxiv("2501.00663".into())));
        assert_eq!(parse_identifier("2501.00663v2"), Some(Ident::Arxiv("2501.00663".into())));
        assert_eq!(parse_identifier("arXiv:hep-th/9901001"), Some(Ident::Arxiv("hep-th/9901001".into())));
        assert_eq!(parse_identifier("https://arxiv.org/abs/2405.12345"), Some(Ident::Arxiv("2405.12345".into())));
        assert_eq!(parse_identifier("https://arxiv.org/pdf/2405.12345v3.pdf"), Some(Ident::Arxiv("2405.12345".into())));
    }

    #[test]
    fn pmid_forms() {
        assert_eq!(parse_identifier("pmid:36702931"), Some(Ident::Pmid("36702931".into())));
        assert_eq!(parse_identifier("https://pubmed.ncbi.nlm.nih.gov/36702931/"), Some(Ident::Pmid("36702931".into())));
        assert_eq!(parse_identifier("PMC1234567"), Some(Ident::Pmcid("PMC1234567".into())));
    }

    #[test]
    fn junk_is_invalid() {
        assert_eq!(parse_identifier("hello world"), None);
        assert_eq!(parse_identifier(""), None);
        assert_eq!(parse_identifier("10."), None);
        assert_eq!(parse_identifier("not a doi 10.12/34x is not"), None);
        // Zero-width junk must not rescue a bad identifier.
        assert_eq!(parse_identifier("\u{200b}\u{200b}"), None);
    }

    #[test]
    fn rekey_swaps_key_only() {
        let e = "@article{oldkey,\n  title = {T},\n}\n";
        assert!(rekey(e, "knuth84").starts_with("@article{knuth84,"));
        assert!(rekey(e, "knuth84").contains("title = {T}"));
    }

    #[test]
    fn make_key_shape() {
        let e = "@article{x,\n  author = {Knuth, Donald},\n  year = {1984},\n  title = {The TeXbook},\n}";
        assert_eq!(make_key(e), "knuth1984texbook");
    }
}
