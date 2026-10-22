//! `doc.suggest` — propose an edit without applying it, anchored by a
//! TextQuoteSelector-style `{exact, prefix, suffix}` triple instead of a
//! byte range. The anchor is re-resolved at apply time, so a proposal
//! survives unrelated edits (or an external save) that shift offsets;
//! when the quoted text is gone or no longer unique the proposal reports
//! `stale`/`ambiguous` rather than patching the wrong span.
//!
//! Comment-only proposals (`comment` without `replace`) are review
//! annotations — `apply` clears them without touching the buffer.

use serde_json::{json, Value};

use crate::{EngineError, Session};

/// Context captured around `exact` when the caller doesn't supply it —
/// enough to disambiguate repeated phrases without bloating the anchor.
const CTX: usize = 40;

/// One pending proposal.
#[derive(Debug, Clone)]
pub struct Suggestion {
    pub id: String,
    /// The quoted span the proposal is about.
    pub exact: String,
    /// Text expected immediately before `exact` (may be empty).
    pub prefix: String,
    /// Text expected immediately after `exact` (may be empty).
    pub suffix: String,
    /// Replacement text — `None` for a comment-only annotation.
    pub replace: Option<String>,
    /// Free-form reviewer note.
    pub comment: Option<String>,
}

/// Anchor resolution result.
#[derive(Debug, PartialEq)]
enum Resolve {
    /// Exactly one anchor-consistent occurrence.
    Unique(usize, usize),
    /// `exact` occurs more than once and context can't pick one.
    Ambiguous(usize),
    /// `exact` doesn't occur at all.
    Missing,
}

/// All byte offsets where `needle` occurs (bounded scan).
fn occurrences(src: &str, needle: &str) -> Vec<usize> {
    src.match_indices(needle).take(256).map(|(i, _)| i).collect()
}

/// Does `src` around `[start, end)` match the anchor's context?
fn context_ok(src: &str, start: usize, end: usize, prefix: &str, suffix: &str) -> bool {
    let before = src.get(..start).unwrap_or("");
    let after = src.get(end..).unwrap_or("");
    before.ends_with(prefix) && after.starts_with(suffix)
}

/// Resolve an anchor against the live buffer.
fn resolve(src: &str, s: &Suggestion) -> Resolve {
    let occ = occurrences(src, &s.exact);
    if occ.is_empty() {
        return Resolve::Missing;
    }
    let hits: Vec<usize> = occ.iter().copied().filter(|&start| context_ok(src, start, start + s.exact.len(), &s.prefix, &s.suffix)).collect();
    match hits.as_slice() {
        [one] => Resolve::Unique(*one, *one + s.exact.len()),
        [] => Resolve::Missing, // text exists but moved — context fails.
        _ => Resolve::Ambiguous(hits.len()),
    }
}

/// Capture `CTX` chars of context around `range` in `src`, on char
/// boundaries.
fn context_around(src: &str, start: usize, end: usize) -> (String, String) {
    let pre = src.get(..start).unwrap_or("");
    let post = src.get(end..).unwrap_or("");
    let take_tail: usize = pre.chars().rev().take(CTX).map(char::len_utf8).sum();
    let take_head: usize = post.chars().take(CTX).map(char::len_utf8).sum();
    (pre.get(pre.len() - take_tail..).unwrap_or("").to_string(), post.get(..take_head).unwrap_or("").to_string())
}

/// Serialize a proposal + its live resolution status.
fn describe(src: &str, s: &Suggestion) -> Value {
    let (status, range) = match resolve(src, s) {
        Resolve::Unique(a, b) => ("anchored", json!({"start": a, "end": b})),
        Resolve::Ambiguous(n) => ("ambiguous", json!({"hits": n})),
        Resolve::Missing => ("stale", Value::Null),
    };
    json!({
        "id": s.id,
        "status": status,
        "range": range,
        "anchor": {"exact": s.exact, "prefix": s.prefix, "suffix": s.suffix},
        "replace": s.replace,
        "comment": s.comment,
    })
}

/// `doc.suggest` — register a proposal. `find` is required; `replace`
/// and/or `comment` supply the payload. `prefix`/`suffix` are optional —
/// when omitted, live context is captured around the (unique) match.
pub fn suggest(sess: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let find = params.get("find").and_then(Value::as_str).ok_or(EngineError::BadParam("find"))?;
    if find.is_empty() {
        return Err(EngineError::BadParam("find"));
    }
    let replace = params.get("replace").and_then(Value::as_str).map(str::to_string);
    let comment = params.get("comment").and_then(Value::as_str).map(str::to_string);
    if replace.is_none() && comment.is_none() {
        return Err(EngineError::BadParam("replace or comment"));
    }
    let occ = occurrences(sess.source(), find);
    if occ.is_empty() {
        return Ok(json!({"status": "stale", "reason": "find text not present"}));
    }
    let given_prefix = params.get("prefix").and_then(Value::as_str);
    let given_suffix = params.get("suffix").and_then(Value::as_str);
    let (prefix, suffix) = match (given_prefix, given_suffix) {
        (Some(p), Some(s)) => (p.to_string(), s.to_string()),
        _ => {
            // Caller didn't anchor — capture context around the first
            // occurrence; ambiguity is reported, not guessed at.
            let (p, s) = context_around(sess.source(), occ[0], occ[0] + find.len());
            (p, s)
        }
    };
    let id = sess.next_suggestion_id();
    let sug = Suggestion { id: id.clone(), exact: find.to_string(), prefix, suffix, replace, comment };
    let desc = describe(sess.source(), &sug);
    sess.suggestions_mut().push(sug);
    Ok(desc)
}

/// `doc.suggest.list` — every pending proposal with live status.
pub fn list(sess: &Session) -> Result<Value, EngineError> {
    let src = sess.source();
    let items: Vec<Value> = sess.suggestions().iter().map(|s| describe(src, s)).collect();
    Ok(json!({"suggestions": items}))
}

/// `doc.suggest.apply` — re-resolve the anchor right now and apply.
/// Requires `confirm: true`; a stale/ambiguous anchor is reported, never
/// force-applied.
pub fn apply(sess: &mut Session, params: &Value) -> Result<Value, EngineError> {
    if params.get("confirm").and_then(Value::as_bool) != Some(true) {
        return Err(EngineError::BadParam("confirm"));
    }
    let id = params.get("id").and_then(Value::as_str).ok_or(EngineError::BadParam("id"))?;
    let Some(sug) = sess.suggestions().iter().find(|s| s.id == id).cloned() else {
        return Err(EngineError::BadParam("id"));
    };
    match resolve(sess.source(), &sug) {
        Resolve::Unique(start, end) => {
            let applied = sug.replace.is_some();
            if let Some(new_text) = &sug.replace {
                let src = sess.source();
                let mut buf = String::with_capacity(src.len() + new_text.len());
                buf.push_str(src.get(..start).unwrap_or(""));
                buf.push_str(new_text);
                buf.push_str(src.get(end..).unwrap_or(""));
                sess.set_source(buf);
            }
            sess.suggestions_mut().retain(|s| s.id != id);
            Ok(
                json!({"status": "applied", "id": id, "edited": applied, "range": {"start": start, "end": start + sug.replace.as_deref().map_or(0, str::len)}}),
            )
        }
        Resolve::Ambiguous(n) => Ok(json!({"status": "ambiguous", "id": id, "hits": n})),
        Resolve::Missing => Ok(json!({"status": "stale", "id": id})),
    }
}

/// `doc.suggest.reject` — drop a pending proposal (no buffer change).
pub fn reject(sess: &mut Session, params: &Value) -> Result<Value, EngineError> {
    let id = params.get("id").and_then(Value::as_str).ok_or(EngineError::BadParam("id"))?;
    let had = sess.suggestions().len();
    sess.suggestions_mut().retain(|s| s.id != id);
    Ok(json!({"removed": sess.suggestions().len() < had}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Session;

    fn suggest_params(find: &str, replace: &str) -> Value {
        json!({"find": find, "replace": replace})
    }

    #[test]
    fn propose_then_apply_replaces() {
        let mut s = Session::with_source("one two three".to_string());
        let v = suggest(&mut s, &suggest_params("two", "TWO")).unwrap();
        assert_eq!(v["status"], "anchored");
        let id = v["id"].as_str().unwrap().to_string();
        let v = apply(&mut s, &json!({"id": id, "confirm": true})).unwrap();
        assert_eq!(v["status"], "applied");
        assert_eq!(s.source(), "one TWO three");
    }

    #[test]
    fn anchor_survives_offset_shift() {
        let mut s = Session::with_source("aaa bbb ccc".to_string());
        let v = suggest(&mut s, &suggest_params("ccc", "CCC")).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        // External edit before the anchor — offsets move, quote holds.
        s.set_source("XXXX aaa bbb ccc".to_string());
        let v = apply(&mut s, &json!({"id": id, "confirm": true})).unwrap();
        assert_eq!(v["status"], "applied");
        assert_eq!(s.source(), "XXXX aaa bbb CCC");
    }

    #[test]
    fn stale_when_text_gone() {
        let mut s = Session::with_source("hello world".to_string());
        let v = suggest(&mut s, &suggest_params("world", "W")).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        s.set_source("hello there".to_string());
        let v = apply(&mut s, &json!({"id": id, "confirm": true})).unwrap();
        assert_eq!(v["status"], "stale");
    }

    #[test]
    fn ambiguous_only_without_context() {
        // Auto-captured context disambiguates "x dup y dup z" → anchored.
        let mut s = Session::with_source("x dup y dup z".to_string());
        let v = suggest(&mut s, &suggest_params("dup", "D")).unwrap();
        assert_eq!(v["status"], "anchored");
        // An explicit context-free anchor on repeated text → ambiguous.
        let mut s = Session::with_source("x dup y dup z".to_string());
        let v = suggest(&mut s, &json!({"find": "dup", "replace": "D", "prefix": "", "suffix": ""})).unwrap();
        assert_eq!(v["status"], "ambiguous");
    }

    #[test]
    fn comment_only_applies_without_edit() {
        let mut s = Session::with_source("text".to_string());
        let v = suggest(&mut s, &json!({"find": "text", "comment": "revise"})).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        let v = apply(&mut s, &json!({"id": id, "confirm": true})).unwrap();
        assert_eq!(v["edited"], false);
        assert_eq!(s.source(), "text");
    }

    #[test]
    fn unconfirmed_apply_is_rejected() {
        let mut s = Session::with_source("text".to_string());
        let v = suggest(&mut s, &suggest_params("text", "X")).unwrap();
        let id = v["id"].as_str().unwrap().to_string();
        assert!(apply(&mut s, &json!({"id": id})).is_err());
    }
}
