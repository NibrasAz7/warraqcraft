//! `snippet.list` / `snippet.expand` — TextMate-style snippet sources.
//!
//! Three layers merge, later overriding by trigger: built-ins →
//! `$WARRAQCRAFT_HOME/snippets.json` or `~/.warraqcraft/snippets.json` →
//! the project's `.warraqcraft/snippets.json` (git-shareable). A file is
//! either an array of objects or a `{trigger: body}` map; malformed
//! entries are skipped, never fatal.
//!
//! Bodies carry `$1`…`$9` tabstops, `${1:default}` placeholders, and `$0`
//! for the final caret stop. `expand` renders the insertion text and the
//! first stop's byte offset — the editor takes over from there.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::{EngineError, Session};

/// One snippet.
#[derive(Debug, Clone)]
pub struct Snippet {
    pub trigger: String,
    pub body: String,
    pub description: String,
    /// `math` expands only inside math mode; `text` only outside.
    pub context: String,
    /// `builtin` | `global` | `project`.
    pub source: &'static str,
}

/// Built-in starters — kept deliberately small; user/project files are
/// where real sets live.
const BUILTINS: &[(&str, &str, &str, &str)] = &[
    ("fig", "\\begin{figure}\n    \\centering\n    \\includegraphics[width=${1:0.8}\\textwidth]{${2:image}}\n    \\caption{${3:caption}}\n    \\label{fig:${4:label}}\n\\end{figure}\n$0", "figure + includegraphics", "any"),
    ("tab", "\\begin{table}\n    \\centering\n    \\begin{tabular}{${1:cc}}\n        $0\n    \\end{tabular}\n    \\caption{${2:caption}}\n    \\label{tab:${3:label}}\n\\end{table}", "table + tabular", "any"),
    ("eq", "\\begin{equation}\n    $0\n\\end{equation}", "numbered equation", "any"),
    ("al", "\\begin{align}\n    ${1:lhs} &= ${2:rhs}\n\\end{align}", "align environment", "math"),
    ("itm", "\\begin{itemize}\n    \\item $0\n\\end{itemize}", "itemize list", "any"),
    ("enu", "\\begin{enumerate}\n    \\item $0\n\\end{enumerate}", "enumerate list", "any"),
    ("fra", "\\begin{frame}{${1:title}}\n    $0\n\\end{frame}", "beamer frame", "any"),
    ("frac", "\\frac{${1:a}}{${2:b}}$0", "\\frac{}{}", "math"),
    ("sec", "\\section{${1:title}}\n$0", "section", "any"),
    ("sub", "\\subsection{${1:title}}\n$0", "subsection", "any"),
    ("bf", "\\textbf{${1:text}}$0", "bold", "any"),
    ("it", "\\textit{${1:text}}$0", "italic", "any"),
];

/// Max bytes read from a snippet file — project dirs are untrusted.
const MAX_SNIPPET_FILE: u64 = 256 * 1024;
/// Max snippets per source.
const MAX_SNIPPETS: usize = 512;

/// Global snippets path: `$WARRAQCRAFT_HOME/snippets.json`, else
/// `~/.warraqcraft/snippets.json`.
#[cfg(not(target_arch = "wasm32"))]
fn global_path() -> Option<PathBuf> {
    if let Ok(h) = std::env::var("WARRAQCRAFT_HOME") {
        if !h.is_empty() {
            return Some(Path::new(&h).join("snippets.json"));
        }
    }
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| Path::new(&h).join(".warraqcraft").join("snippets.json"))
}

/// Parse a snippet file: accepts `[{trigger,body,…}]` or
/// `{trigger: body}` / `{trigger: {body,…}}` maps.
fn parse_file(text: &str, source: &'static str) -> Vec<Snippet> {
    let Ok(v) = serde_json::from_str::<Value>(text) else { return Vec::new() };
    let mut out = Vec::new();
    let mut push = |trigger: &str, body: &str, desc: &str, context: &str| {
        if out.len() < MAX_SNIPPETS && !trigger.is_empty() && !body.is_empty() {
            out.push(Snippet {
                trigger: trigger.to_string(),
                body: body.to_string(),
                description: desc.to_string(),
                context: if matches!(context, "math" | "text") { context } else { "any" }.to_string(),
                source,
            });
        }
    };
    match &v {
        Value::Array(items) => {
            for it in items {
                let t = it.get("trigger").and_then(Value::as_str).unwrap_or("");
                let b = it.get("body").and_then(Value::as_str).unwrap_or("");
                let d = it.get("description").and_then(Value::as_str).unwrap_or("");
                let c = it.get("context").and_then(Value::as_str).unwrap_or("any");
                push(t, b, d, c);
            }
        }
        Value::Object(map) => {
            for (k, val) in map.iter().take(MAX_SNIPPETS) {
                match val {
                    Value::String(b) => push(k, b, "", "any"),
                    Value::Object(_) => {
                        let b = val.get("body").and_then(Value::as_str).unwrap_or("");
                        let d = val.get("description").and_then(Value::as_str).unwrap_or("");
                        let c = val.get("context").and_then(Value::as_str).unwrap_or("any");
                        push(k, b, d, c);
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    out
}

/// Read a snippet file bounded; missing/unreadable → empty.
#[cfg(not(target_arch = "wasm32"))]
fn read_snippets(path: &Path, source: &'static str) -> Vec<Snippet> {
    use std::io::Read;
    let mut buf = String::new();
    let Ok(f) = std::fs::File::open(path) else { return Vec::new() };
    if f.take(MAX_SNIPPET_FILE).read_to_string(&mut buf).is_err() {
        return Vec::new();
    }
    parse_file(&buf, source)
}

/// All snippets merged — later sources override by trigger.
pub fn load(sess: &Session) -> Vec<Snippet> {
    let mut merged: Vec<Snippet> = BUILTINS
        .iter()
        .map(|(t, b, d, c)| Snippet {
            trigger: (*t).to_string(),
            body: (*b).to_string(),
            description: (*d).to_string(),
            context: (*c).to_string(),
            source: "builtin",
        })
        .collect();
    #[cfg(not(target_arch = "wasm32"))]
    {
        for (path, source) in
            [(global_path(), "global"), (sess.path().and_then(|p| p.parent().map(|d| d.join(".warraqcraft").join("snippets.json"))), "project")]
        {
            if let Some(p) = path {
                for snip in read_snippets(&p, source) {
                    if let Some(existing) = merged.iter_mut().find(|s| s.trigger == snip.trigger) {
                        *existing = snip; // override keeps position
                    } else {
                        merged.push(snip);
                    }
                }
            }
        }
    }
    merged
}

/// Render `body`: `${n:default}` → `default`, `$n` → removed; caret lands
/// on `$1`, else `$0`, else end. Returns (text, caret_byte_offset).
pub fn expand(body: &str) -> (String, usize) {
    let mut out = String::with_capacity(body.len());
    let mut caret: Option<usize> = None;
    let bytes = body.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'$' {
            // `${n:default}` — brace form comes first (digit is at i+2).
            if bytes.get(i + 1) == Some(&b'{') {
                if let Some(close) = body.get(i + 2..).and_then(|r| r.find('}')) {
                    let inner = body.get(i + 2..i + 2 + close).unwrap_or("");
                    let (num, def) = match inner.split_once(':') {
                        Some((n, d)) => (n.chars().next().unwrap_or('9'), d),
                        None => (inner.chars().next().unwrap_or('9'), ""),
                    };
                    if num.is_ascii_digit() {
                        // Stop $1 wins outright; $0 fills an empty caret.
                        if num == '1' || (num == '0' && caret.is_none()) {
                            caret = Some(out.len());
                        }
                        out.push_str(def);
                        i += 2 + close + 1;
                        continue;
                    }
                }
            }
            if let Some(d) = bytes.get(i + 1) {
                if d.is_ascii_digit() {
                    // `$n` — `$0` is the final stop but `$1` wins.
                    if *d == b'1' || (*d == b'0' && caret.is_none()) {
                        caret = Some(out.len());
                    }
                    i += 2;
                    continue;
                }
            }
        }
        // Push the char, not the byte — multibyte safe.
        let ch_len = body.get(i..).and_then(|r| r.chars().next()).map(char::len_utf8).unwrap_or(1);
        out.push_str(body.get(i..i + ch_len).unwrap_or(""));
        i += ch_len;
    }
    let caret = caret.unwrap_or(out.len());
    (out, caret)
}

/// `snippet.list` — merged snippets with their source.
pub fn list(sess: &Session) -> Result<Value, EngineError> {
    let items: Vec<Value> = load(sess)
        .iter()
        .map(|s| json!({"trigger": s.trigger, "description": s.description, "context": s.context, "source": s.source, "body": s.body}))
        .collect();
    Ok(json!({"snippets": items}))
}

/// `snippet.expand {trigger}` → `{"body", "caret"}` or `null` when the
/// trigger doesn't exist.
pub fn expand_cmd(sess: &Session, params: &Value) -> Result<Value, EngineError> {
    let trigger = params.get("trigger").and_then(Value::as_str).ok_or(EngineError::BadParam("trigger"))?;
    let all = load(sess);
    match all.iter().find(|s| s.trigger == trigger) {
        Some(s) => {
            let (body, caret) = expand(&s.body);
            Ok(json!({"body": body, "caret": caret, "context": s.context}))
        }
        None => Ok(Value::Null),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_defaults_and_caret() {
        let (body, caret) = expand("\\section{${1:title}}\n$0");
        assert_eq!(body, "\\section{title}\n");
        assert_eq!(caret, "\\section{".len());
    }

    #[test]
    fn dollar_one_wins_over_zero() {
        let (body, caret) = expand("$0$1x");
        assert_eq!(body, "x");
        assert_eq!(caret, 0);
    }

    #[test]
    fn bare_tabstops_drop() {
        let (body, caret) = expand("a$2b$0");
        assert_eq!(body, "ab");
        assert_eq!(caret, 2);
    }

    #[test]
    fn project_file_overrides_builtin() {
        let dir = std::env::temp_dir().join(format!("wc-snip-{}", std::process::id()));
        let _ = std::fs::create_dir_all(dir.join(".warraqcraft"));
        let _ = std::fs::write(dir.join("main.tex"), "\\documentclass{article}");
        let _ = std::fs::write(dir.join(".warraqcraft").join("snippets.json"), r#"[{"trigger":"fig","body":"OVERRIDDEN"}]"#);
        let mut s = Session::new();
        let _ = s.open(&dir.join("main.tex"));
        let fig = load(&s).into_iter().find(|s| s.trigger == "fig").unwrap();
        assert_eq!(fig.body, "OVERRIDDEN");
        assert_eq!(fig.source, "project");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn malformed_files_are_empty_not_fatal() {
        assert!(parse_file("not json", "project").is_empty());
        assert_eq!(parse_file(r#"{"x":"$0"}"#, "project").len(), 1);
        assert!(parse_file("[]", "project").is_empty());
    }
}
