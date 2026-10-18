//! warraqcraft-cli — headless WarraqCraft.
//!
//!   warraqcraft-cli parse <file.tex>     document structure outline
//!   warraqcraft-cli check <file.tex>     structural issues only; exit 1 if any
//!   warraqcraft-cli bib <file.bib>       bibliography entries and issues
//!   warraqcraft-cli compile <file.tex>   system-TeX compile to .warraqcraft/; exit 1 on errors
//!   warraqcraft-cli cmd <command> [--file f.tex] [--params '<json>'] [--check]
//!   warraqcraft-cli commands             list engine command names
//!   warraqcraft-cli mcp                  MCP server over stdin/stdout (opt-in)
//!   warraqcraft-cli ui --control FILE <command> [--params '<json>']
//!                                     drive a running `warraqcraft --control FILE`

use std::path::Path;
use std::process::ExitCode;

use serde_json::Value;
use texparse::{parse_bib, parse_structure};
use warraqcraft_engine::Session;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("parse") => with_file(args.get(1), parse_file),
        Some("check") => with_file(args.get(1), check_file),
        Some("bib") => with_file(args.get(1), bib_file),
        Some("compile") => compile_cmd(args.get(1)),
        // `cmd` and its documented `--cmd` spelling share the same parser:
        // the command name is just the next positional argument.
        Some("cmd") | Some("--cmd") => cmd_cmd(args.get(1..).unwrap_or(&[])),
        Some("commands") => commands_cmd(),
        Some("mcp") => mcp_cmd(),
        Some("ui") => ui_cmd(args.get(1..).unwrap_or(&[])),
        _ => {
            usage();
            if args.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

fn usage() {
    eprintln!(
        "warraqcraft-cli — headless WarraqCraft\n\
         \n\
         usage:\n\
         \x20 warraqcraft-cli parse <file.tex>   document structure outline\n\
         \x20 warraqcraft-cli check <file.tex>   structural issues (exit 1 if any)\n\
         \x20 warraqcraft-cli bib   <file.bib>   bibliography entries\n\
         \x20 warraqcraft-cli compile <file.tex> system-TeX compile (exit 1 on errors)\n\
         \x20 warraqcraft-cli cmd <command> [--file f.tex] [--params '<json>'] [--check]\n\
         \x20                            run an engine command, print JSON result\n\
         \x20                            e.g. warraqcraft-cli cmd source.get --file main.tex\n\
         \x20 warraqcraft-cli commands        list engine command names\n\
         \x20 warraqcraft-cli mcp             MCP server over stdin/stdout (opt-in)\n\
         \x20 warraqcraft-cli ui --control F <command> [--params '<json>']\n\
         \x20                            drive a running `warraqcraft --control F`\n"
    );
}

fn with_file(arg: Option<&String>, f: fn(&str, &str) -> ExitCode) -> ExitCode {
    let Some(path) = arg else {
        usage();
        return ExitCode::FAILURE;
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("warraqcraft-cli: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let src = String::from_utf8_lossy(&bytes);
    f(path, &src)
}

const LEVEL_NAMES: [&str; 7] = ["part", "chapter", "section", "subsection", "subsubsection", "paragraph", "subparagraph"];

fn parse_file(_path: &str, src: &str) -> ExitCode {
    let d = parse_structure(src);
    if let Some((class, _)) = &d.class {
        if d.class_options.is_empty() {
            println!("class: {class}");
        } else {
            println!("class: {class} [{}]", d.class_options);
        }
    }
    if !d.packages.is_empty() {
        println!("packages: {}", d.packages.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>().join(", "));
    }
    if let Some((t, _)) = &d.title {
        println!("title: {t}");
    }
    if let Some((a, _)) = &d.author {
        println!("author: {a}");
    }
    for h in &d.headings {
        let name = LEVEL_NAMES.get(h.level as usize).copied().unwrap_or("?");
        println!("{}:{}: {} {}", h.pos.line, h.pos.col, name, h.title);
    }
    if !d.labels.is_empty() {
        println!("labels: {}", d.labels.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join(", "));
    }
    if !d.cite_keys.is_empty() {
        println!("cites: {}", d.cite_keys.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", "));
    }
    if !d.inputs.is_empty() {
        println!("inputs: {}", d.inputs.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>().join(", "));
    }
    if !d.graphics.is_empty() {
        println!("graphics: {}", d.graphics.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>().join(", "));
    }
    if !d.bibliographies.is_empty() {
        println!("bibliography: {}", d.bibliographies.iter().map(|(f, _)| f.as_str()).collect::<Vec<_>>().join(", "));
    }
    for i in &d.issues {
        println!("issue {}:{}: {}", i.pos.line, i.pos.col, i.message);
    }
    ExitCode::SUCCESS
}

fn check_file(_path: &str, src: &str) -> ExitCode {
    let d = parse_structure(src);
    for i in &d.issues {
        println!("{}:{}: {}", i.pos.line, i.pos.col, i.message);
    }
    if d.issues.is_empty() {
        println!("ok");
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn compile_cmd(arg: Option<&String>) -> ExitCode {
    let Some(path) = arg else {
        usage();
        return ExitCode::FAILURE;
    };
    match warraqcraft_engine::compile_file(Path::new(path), None) {
        Ok(report) => {
            for e in &report.errors {
                match e.line {
                    Some(l) => println!("error {l}: {}", e.message),
                    None => println!("error: {}", e.message),
                }
            }
            for w in &report.warnings {
                match w.line {
                    Some(l) => println!("warning {l}: {}", w.message),
                    None => println!("warning: {}", w.message),
                }
            }
            match &report.pdf {
                Some(p) => println!("{}: wrote {} ({} page(s))", report.engine, p.display(), report.pages.unwrap_or(0)),
                None => println!("{}: no PDF produced", report.engine),
            }
            if report.errors.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => {
            eprintln!("warraqcraft-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

fn bib_file(_path: &str, src: &str) -> ExitCode {
    let f = parse_bib(src);
    for (name, val) in &f.strings {
        println!("@string {name} = \"{val}\"");
    }
    for e in &f.entries {
        if e.key.is_empty() {
            println!("@{} (empty)", e.kind);
            continue;
        }
        let mut fields = e.fields.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>().join(", ");
        if fields.is_empty() {
            fields = "no fields".into();
        }
        println!("{}:{}: @{} {{{}}} — {}", e.pos.line, e.pos.col, e.kind, e.key, fields);
    }
    for i in &f.issues {
        println!("issue {}:{}: {}", i.pos.line, i.pos.col, i.message);
    }
    ExitCode::SUCCESS
}

/// Engine command names — kept in sync with `Session::execute` by hand.
const COMMANDS: &[&str] = &[
    "document.new",
    "document.open",
    "document.save",
    "document.compile",
    "source.set",
    "source.get",
    "structure.get",
    "magic.get",
    "user_tool.list",
    "user_tool.set",
    "user_tool.run",
    "user_tool.clear",
    "template.list",
    "template.read",
    "template.new",
    "template.import",
    "project.graph",
    "project.structure",
    "math.preview",
    "text.stats",
    "project.snapshot",
    "snapshot.list",
    "snapshot.read",
    "snapshot.restore",
];

fn commands_cmd() -> ExitCode {
    for c in COMMANDS {
        println!("{c}");
    }
    ExitCode::SUCCESS
}

fn cmd_cmd(args: &[String]) -> ExitCode {
    let mut name: Option<&str> = None;
    let mut file: Option<&str> = None;
    let mut params = Value::Object(serde_json::Map::new());
    let mut check = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "--file" => {
                i += 1;
                match args.get(i) {
                    Some(v) => file = Some(v.as_str()),
                    None => {
                        eprintln!("warraqcraft-cli: --file needs a path");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--params" => {
                i += 1;
                match args.get(i) {
                    Some(v) => match parse_params(v) {
                        Ok(p) => params = p,
                        Err(e) => {
                            eprintln!("warraqcraft-cli: {e}");
                            return ExitCode::FAILURE;
                        }
                    },
                    None => {
                        eprintln!("warraqcraft-cli: --params needs a JSON object");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--check" => check = true,
            _ if a.starts_with('-') => {
                eprintln!("warraqcraft-cli: unknown option {a}");
                return ExitCode::FAILURE;
            }
            _ => {
                if name.is_some() {
                    eprintln!("warraqcraft-cli: unexpected argument {a}");
                    return ExitCode::FAILURE;
                }
                name = Some(a);
            }
        }
        i += 1;
    }
    let Some(name) = name else {
        usage();
        return ExitCode::FAILURE;
    };
    match run_cmd(file, name, params, check) {
        Ok(pretty) => {
            println!("{pretty}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("warraqcraft-cli: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Run one engine command on a fresh session and return the pretty-printed
/// JSON result. `file` is opened on the session first so file-relative
/// commands work. With `check`, a result reporting a non-empty `"errors"`
/// array or `"ok": false` is an `Err` carrying the result JSON.
fn run_cmd(file: Option<&str>, name: &str, params: Value, check: bool) -> Result<String, String> {
    let mut session = Session::new();
    if let Some(f) = file {
        session.open(Path::new(f)).map_err(|e| format!("cannot open {f}: {e}"))?;
    }
    let result = session.execute(name, &params).map_err(|e| format!("{name}: {e}"))?;
    let pretty = serde_json::to_string_pretty(&result).map_err(|e| format!("{name}: cannot serialize result: {e}"))?;
    if check && check_failed(&result) {
        return Err(format!("{name}: --check failed\n{pretty}"));
    }
    Ok(pretty)
}

fn parse_params(s: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(s).map_err(|e| format!("invalid --params JSON: {e}"))?;
    if v.is_object() {
        Ok(v)
    } else {
        Err("--params must be a JSON object".into())
    }
}

fn check_failed(v: &Value) -> bool {
    v.get("errors").and_then(Value::as_array).is_some_and(|a| !a.is_empty()) || v.get("ok").and_then(Value::as_bool) == Some(false)
}

/// `warraqcraft-cli mcp` — opt-in stdio MCP server (NDJSON JSON-RPC over
/// stdin/stdout; no port is opened). Runs until stdin closes.
fn mcp_cmd() -> ExitCode {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut server = warraqcraft_mcp::McpServer::new();
    match server.serve(stdin.lock(), stdout.lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("warraqcraft-cli mcp: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `warraqcraft-cli ui --control FILE <cmd> [--params '<json>']` — one
/// request against a running app's control channel (`warraqcraft
/// --control FILE`). The control file carries {"port","token","pid"};
/// the channel speaks one newline-delimited JSON request per line.
fn ui_cmd(args: &[String]) -> ExitCode {
    let mut control: Option<&str> = None;
    let mut name: Option<&str> = None;
    let mut params = Value::Object(serde_json::Map::new());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--control" => {
                i += 1;
                match args.get(i) {
                    Some(v) => control = Some(v.as_str()),
                    None => {
                        eprintln!("warraqcraft-cli: --control needs a file path");
                        return ExitCode::FAILURE;
                    }
                }
            }
            "--params" => {
                i += 1;
                match args.get(i) {
                    Some(v) => match parse_params(v) {
                        Ok(p) => params = p,
                        Err(e) => {
                            eprintln!("warraqcraft-cli: {e}");
                            return ExitCode::FAILURE;
                        }
                    },
                    None => {
                        eprintln!("warraqcraft-cli: --params needs a JSON object");
                        return ExitCode::FAILURE;
                    }
                }
            }
            a if a.starts_with('-') => {
                eprintln!("warraqcraft-cli: unknown option {a}");
                return ExitCode::FAILURE;
            }
            a => {
                if name.is_some() {
                    eprintln!("warraqcraft-cli: unexpected argument {a}");
                    return ExitCode::FAILURE;
                }
                name = Some(a);
            }
        }
        i += 1;
    }
    match (control, name) {
        (Some(file), Some(cmd)) => match ui_request(file, cmd, params) {
            Ok(pretty) => {
                println!("{pretty}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("warraqcraft-cli ui: {e}");
                ExitCode::FAILURE
            }
        },
        _ => {
            usage();
            ExitCode::FAILURE
        }
    }
}

/// Send one control-channel request and return the pretty result.
/// Reads `{"port","token"}` from the app's control file.
fn ui_request(control_file: &str, cmd: &str, params: Value) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Write};
    let raw = std::fs::read_to_string(control_file)
        .map_err(|e| format!("cannot read {control_file}: {e} — start the app with `warraqcraft --control {control_file}`"))?;
    let info: Value = serde_json::from_str(&raw).map_err(|e| format!("{control_file}: not a control file: {e}"))?;
    let port = info.get("port").and_then(Value::as_u64).ok_or("control file missing \"port\"")?;
    let token = info.get("token").and_then(Value::as_str).ok_or("control file missing \"token\"")?.to_string();
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port as u16)).map_err(|e| format!("cannot connect to 127.0.0.1:{port}: {e}"))?;
    let req = serde_json::json!({"token": token, "id": 1, "cmd": cmd, "params": params});
    writeln!(stream, "{req}").map_err(|e| format!("send failed: {e}"))?;
    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line).map_err(|e| format!("no reply: {e}"))?;
    let reply: Value = serde_json::from_str(line.trim()).map_err(|e| format!("bad reply: {e}"))?;
    if reply.get("ok").and_then(Value::as_bool) == Some(true) {
        serde_json::to_string_pretty(reply.get("result").unwrap_or(&Value::Null)).map_err(|e| e.to_string())
    } else {
        Err(reply.get("error").and_then(Value::as_str).unwrap_or("request failed").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cmd_source_get_empty_session() {
        let out = run_cmd(None, "source.get", json!({}), false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["text"], json!(""));
    }

    #[test]
    fn cmd_source_set_ok() {
        let out = run_cmd(None, "source.set", json!({"text": "hello"}), false).unwrap();
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["ok"], json!(true));
    }

    #[test]
    fn cmd_unknown_command_errs() {
        assert!(run_cmd(None, "no.such", json!({}), false).is_err());
    }

    #[test]
    fn cmd_missing_param_errs() {
        assert!(run_cmd(None, "source.set", json!({}), false).is_err());
    }

    #[test]
    fn cmd_bad_file_errs() {
        assert!(run_cmd(Some("no/such/file.tex"), "source.get", json!({}), false).is_err());
    }

    #[test]
    fn check_flag_failures() {
        assert!(check_failed(&json!({"errors": ["boom"]})));
        assert!(check_failed(&json!({"ok": false})));
        assert!(!check_failed(&json!({"errors": []})));
        assert!(!check_failed(&json!({"ok": true})));
        assert!(!check_failed(&json!({"text": "hi"})));
    }

    #[test]
    fn params_parsing() {
        assert!(parse_params("{}").is_ok());
        assert!(parse_params("{\"a\": 1}").is_ok());
        assert!(parse_params("[1, 2]").is_err());
        assert!(parse_params("42").is_err());
        assert!(parse_params("{oops").is_err());
    }
}
