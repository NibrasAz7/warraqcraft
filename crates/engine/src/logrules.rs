//! Log → Problems enrichment.
//!
//! A data-driven rule table over the diagnostics `parse_log` produces:
//! each rule maps a fixed TeX log signature to a stable id, a
//! plain-language hint, the package whose absence is the usual cause,
//! and the offending command when the message names one. Cascade-prone
//! trailer lines ("Emergency stop", "Fatal error") are flagged as
//! cascades when an earlier diagnostic already explains the failure —
//! one root error shouldn't spam derivative noise. All matching is
//! literal substring: TeX log lines are fixed strings, no regex needed.

use crate::CompileError;

/// One log signature → diagnostic enrichment.
struct Rule {
    /// Stable identifier — drives cascade suppression and tests.
    id: &'static str,
    /// Substrings that must ALL appear in the message.
    has: &'static [&'static str],
    /// Plain-language explanation, possibly with `{x}` placeholders the
    /// enricher fills from the message (`{file}`, `{pkg}`, `{cmd}`).
    hint: &'static str,
    /// A diagnostic carrying this rule is consequence noise when an
    /// earlier diagnostic already exists — flagged `cascade`.
    cascade_on_prior: bool,
}

const RULES: &[Rule] = &[
    Rule {
        id: "missing-file",
        has: &["File `", "' not found"],
        hint: "`{file}` isn't on the TeX search path — a missing .sty/.cls usually means a package to install or load; a missing .tex/image means the path is wrong",
        cascade_on_prior: false,
    },
    Rule {
        id: "undefined-environment",
        has: &["Environment", "undefined"],
        hint: "the environment isn't defined — check the name or load the package that provides it",
        cascade_on_prior: false,
    },
    Rule {
        id: "undefined-command",
        has: &["Undefined control sequence"],
        hint: "`{cmd}` isn't defined — check the spelling, or load the package that provides it",
        cascade_on_prior: false,
    },
    Rule {
        id: "math-outside-math",
        has: &["Missing $ inserted"],
        hint: "math content outside math mode — wrap it in \\(…\\), $…$ or a math environment",
        cascade_on_prior: false,
    },
    Rule {
        id: "missing-begin-document",
        has: &["Missing \\begin{document}"],
        hint: "typeset material appeared before \\begin{document} — or the document body is missing entirely",
        cascade_on_prior: false,
    },
    Rule {
        id: "stray-newline",
        has: &["There's no line here to end"],
        hint: "a `\\\\` line break sits where no line is open — remove it or leave a blank line instead",
        cascade_on_prior: false,
    },
    Rule {
        id: "incomplete-arg",
        has: &["Paragraph ended before"],
        hint: "a command argument ran past a paragraph break — usually an unclosed brace `{`",
        cascade_on_prior: false,
    },
    Rule {
        id: "misplaced-align-tab",
        has: &["Misplaced alignment tab"],
        hint: "`&` is only legal inside tabular/align environments — escape it as `\\&` in text",
        cascade_on_prior: false,
    },
    Rule {
        id: "brace-mismatch",
        has: &["Extra }, or forgotten"],
        hint: "a `}` has no matching `{` — check the braces around this line",
        cascade_on_prior: false,
    },
    Rule {
        id: "option-clash",
        has: &["Option clash for package"],
        hint: "the package was loaded twice with conflicting options — drop the earlier \\usepackage or move its options there",
        cascade_on_prior: false,
    },
    Rule {
        id: "babel-undefined-language",
        has: &["babel Error:", "haven't defined the language"],
        hint: "babel doesn't know `{x}` — check the language name or install its babel definition",
        cascade_on_prior: false,
    },
    Rule {
        id: "font-not-found",
        has: &["Font", "not found"],
        hint: "the font isn't installed — install it system-wide or pick another with fontspec",
        cascade_on_prior: false,
    },
    Rule {
        id: "missing-glyph",
        has: &["Missing character:"],
        hint: "the current font has no glyph for this character — pick a covering font or a Unicode-aware engine",
        cascade_on_prior: false,
    },
    Rule {
        id: "undefined-reference",
        has: &["Reference `", "undefined"],
        hint: "no \\label defines `{x}` — check the key or compile once more",
        cascade_on_prior: false,
    },
    Rule {
        id: "undefined-citation",
        has: &["Citation `", "undefined"],
        hint: "key `{x}` isn't in any project .bib — check the key or add the entry",
        cascade_on_prior: false,
    },
    Rule {
        id: "overfull-hbox",
        has: &["Overfull \\hbox"],
        hint: "a line ran past the margin — rephrase, allow a break, or widen the measure",
        cascade_on_prior: false,
    },
    Rule {
        id: "underfull-box",
        has: &["Underfull \\hbox", "badness"],
        hint: "an under-stretched line — usually harmless; a forced break or short word is often the cause",
        cascade_on_prior: false,
    },
    Rule {
        id: "package-error",
        has: &["Package ", " Error:"],
        hint: "package `{pkg}` reported this — check its options and load order",
        cascade_on_prior: false,
    },
    Rule {
        id: "emergency-stop",
        has: &["Emergency stop"],
        hint: "compilation aborted — this is the consequence of the error above",
        cascade_on_prior: true,
    },
    Rule {
        id: "fatal-error",
        has: &["Fatal error"],
        hint: "fatal engine error — the consequence of the error above",
        cascade_on_prior: true,
    },
];

/// The `!`-level lines from `parse_log` get error context; warnings get
/// the same enrichment. Mutates both vectors in place.
pub fn enrich(errors: &mut [CompileError], warnings: &mut [CompileError]) {
    let mut any_prior = false;
    for diag in errors.iter_mut().chain(warnings.iter_mut()) {
        if let Some(rule) = RULES.iter().find(|r| r.has.iter().all(|s| diag.message.contains(s))) {
            diag.rule = Some(rule.id);
            diag.hint = Some(fill(rule.hint, &diag.message));
            diag.package = suggested_package(rule.id, &diag.message);
            diag.command = extract_command(&diag.message);
            diag.cascade = rule.cascade_on_prior && any_prior;
        }
        any_prior = true;
    }
}

/// `{x}`/`{file}`/`{pkg}`/`{cmd}` placeholders → message-derived values.
fn fill(hint: &str, msg: &str) -> String {
    hint.replace("{x}", &extract_quoted(msg).unwrap_or_default())
        .replace("{file}", &extract_quoted(msg).unwrap_or_default())
        .replace("{pkg}", &extract_pkg(msg).unwrap_or_default())
        .replace("{cmd}", &extract_command(msg).unwrap_or_default())
}

/// `…\`name'…` — the text between a backquote and an apostrophe, which is
/// how TeX writes filenames, labels and cite keys into the log.
fn extract_quoted(msg: &str) -> Option<String> {
    let start = msg.find('`')? + 1;
    let rest = msg.get(start..)?;
    let end = rest.find('\'')?;
    rest.get(..end).map(str::to_string).filter(|s| !s.is_empty())
}

/// `Package foo Error:` → `foo`.
fn extract_pkg(msg: &str) -> Option<String> {
    let rest = msg.strip_prefix("! Package ").or_else(|| msg.split("Package ").nth(1))?;
    rest.split([' ', '\'']).next().map(str::to_string).filter(|s| !s.is_empty())
}

/// The last `\word` token in the message — TeX puts the offending
/// command on the diagnostic line.
fn extract_command(msg: &str) -> Option<String> {
    msg.rsplit('\\')
        .next()
        .map(|t| t.chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>())
        .filter(|s| !s.is_empty())
        .map(|s| format!("\\{s}"))
        .filter(|s| msg.contains(s.as_str()))
}

/// For `missing-file` diagnostics ending in `.sty`/`.cls`, the missing
/// file IS the package name — that's the actionable suggestion.
fn suggested_package(rule: &str, msg: &str) -> Option<String> {
    if rule != "missing-file" {
        return None;
    }
    let file = extract_quoted(msg)?;
    [".sty", ".cls"].iter().find_map(|ext| file.strip_suffix(ext).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(msg: &str) -> CompileError {
        CompileError { file: None, line: None, message: msg.to_string(), rule: None, hint: None, package: None, command: None, cascade: false }
    }

    #[test]
    fn undefined_command_gets_hint_and_command() {
        let mut errs = vec![e("! Undefined control sequence.\nl.7 \\badcmd")];
        let mut warns = Vec::new();
        enrich(&mut errs, &mut warns);
        assert_eq!(errs[0].rule, Some("undefined-command"));
        assert!(errs[0].hint.is_some());
        assert!(!errs[0].cascade);
    }

    #[test]
    fn missing_sty_suggests_package() {
        let mut errs = vec![e("! LaTeX Error: File `tikz.sty' not found.")];
        let mut warns = Vec::new();
        enrich(&mut errs, &mut warns);
        assert_eq!(errs[0].package.as_deref(), Some("tikz"));
    }

    #[test]
    fn emergency_stop_is_flagged_cascade_after_a_prior_error() {
        let mut errs = vec![e("! Undefined control sequence.\nl.7 \\badcmd"), e("! Emergency stop.")];
        let mut warns = Vec::new();
        enrich(&mut errs, &mut warns);
        assert!(errs[1].cascade);
        assert_eq!(errs[1].rule, Some("emergency-stop"));
    }

    #[test]
    fn emergency_stop_alone_is_not_cascade() {
        let mut errs = vec![e("! Emergency stop.")];
        let mut warns = Vec::new();
        enrich(&mut errs, &mut warns);
        assert!(!errs[0].cascade);
    }

    #[test]
    fn citation_undefined_extracts_key() {
        let mut errs = Vec::new();
        let mut warns = vec![e("LaTeX Warning: Citation `knuth84' on page 1 undefined on input line 12.")];
        enrich(&mut errs, &mut warns);
        assert_eq!(warns[0].rule, Some("undefined-citation"));
        assert!(warns[0].hint.as_deref().unwrap_or("").contains("knuth84"));
    }
}
