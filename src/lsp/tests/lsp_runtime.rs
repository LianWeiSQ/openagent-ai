use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use openagent_lsp::{
    LspOperation, LspQuery, command_available, lsp_doctor, lsp_status, query_workspace,
    shutdown_workspace_clients,
};
use serde_json::json;

#[test]
fn status_reports_configured_server_availability() -> Result<(), Box<dyn Error>> {
    let root = temp_dir("openagent-lsp-status")?;
    fs::create_dir_all(root.join(".openagent"))?;
    fs::write(
        root.join(".openagent/lsp.json"),
        serde_json::to_string_pretty(&json!({
            "servers": {
                "fake": {
                    "command": ["sh"],
                    "extensions": [".rs"],
                    "root_markers": ["Cargo.toml"]
                }
            }
        }))?,
    )?;

    let status = lsp_status(&root)?;
    let fake = status
        .iter()
        .find(|server| server.id == "fake")
        .ok_or("missing fake server")?;
    assert_eq!(fake.extensions, vec![".rs"]);
    assert!(fake.available);
    assert_eq!(fake.source, "config");

    let doctor = lsp_doctor(&root)?;
    assert!(doctor.server_count >= 1);
    assert!(doctor.available_count >= 1);

    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn query_uses_stdio_lsp_server_for_symbols_navigation_and_diagnostics() -> Result<(), Box<dyn Error>>
{
    if !command_available("python3") {
        return Ok(());
    }
    let root = temp_dir("openagent-lsp-query")?;
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fake\"\n")?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), "fn main() {\n    broken();\n}\n")?;
    let fake = write_fake_lsp_server(&root)?;
    fs::create_dir_all(root.join(".openagent"))?;
    fs::write(
        root.join(".openagent/lsp.json"),
        serde_json::to_string_pretty(&json!({
            "servers": {
                "fake": {
                    "command": ["python3", fake],
                    "extensions": [".rs"],
                    "root_markers": ["Cargo.toml"]
                }
            }
        }))?,
    )?;

    let symbols = query_workspace(
        &root,
        LspQuery {
            operation: LspOperation::DocumentSymbol,
            file_path: PathBuf::from("src/main.rs"),
            line: None,
            character: None,
            query: None,
            timeout_ms: Some(3_000),
        },
    )?;
    assert_eq!(symbols.server_id, "fake");
    assert_eq!(symbols.result[0]["name"], "main");

    let definition = query_workspace(
        &root,
        LspQuery {
            operation: LspOperation::GoToDefinition,
            file_path: PathBuf::from("src/main.rs"),
            line: Some(2),
            character: Some(5),
            query: None,
            timeout_ms: Some(3_000),
        },
    )?;
    assert_eq!(definition.result[0]["range"]["start"]["line"], 0);

    let diagnostics = query_workspace(
        &root,
        LspQuery {
            operation: LspOperation::Diagnostics,
            file_path: PathBuf::from("src/main.rs"),
            line: None,
            character: None,
            query: None,
            timeout_ms: Some(3_000),
        },
    )?;
    let diagnostic_payload = diagnostics
        .diagnostics
        .values()
        .next()
        .ok_or("missing diagnostics")?;
    assert_eq!(diagnostic_payload[0]["message"], "fake diagnostic");

    let _ = shutdown_workspace_clients(&root);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn query_reuses_pooled_client_and_reports_running_status() -> Result<(), Box<dyn Error>> {
    if !command_available("python3") {
        return Ok(());
    }
    let root = temp_dir("openagent-lsp-pool")?;
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fake\"\n")?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
    let fake = write_fake_lsp_server(&root)?;
    let init_log = root.join("init.log");
    let event_log = root.join("events.log");
    fs::create_dir_all(root.join(".openagent"))?;
    fs::write(
        root.join(".openagent/lsp.json"),
        serde_json::to_string_pretty(&json!({
            "servers": {
                "fake": {
                    "command": ["python3", fake],
                    "extensions": [".rs"],
                    "root_markers": ["Cargo.toml"],
                    "env": {
                        "FAKE_LSP_INIT_LOG": init_log.to_string_lossy(),
                        "FAKE_LSP_EVENT_LOG": event_log.to_string_lossy()
                    }
                }
            }
        }))?,
    )?;

    let first = query_workspace(
        &root,
        LspQuery {
            operation: LspOperation::DocumentSymbol,
            file_path: PathBuf::from("src/main.rs"),
            line: None,
            character: None,
            query: None,
            timeout_ms: Some(3_000),
        },
    )?;
    assert_eq!(first.server_id, "fake");

    let second = query_workspace(
        &root,
        LspQuery {
            operation: LspOperation::GoToDefinition,
            file_path: PathBuf::from("src/main.rs"),
            line: Some(1),
            character: Some(4),
            query: None,
            timeout_ms: Some(3_000),
        },
    )?;
    assert_eq!(second.server_id, "fake");

    let init_count = fs::read_to_string(&init_log)?.lines().count();
    assert_eq!(init_count, 1, "pooled LSP client should initialize once");
    let events = fs::read_to_string(&event_log)?;
    assert!(events.contains("textDocument/didOpen"));
    assert!(events.contains("textDocument/didChange"));
    assert!(lsp_status(&root)?.iter().any(|server| server.id == "fake"
        && server.running
        && Path::new(&server.root) == root.as_path()));

    assert_eq!(shutdown_workspace_clients(&root), 1);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

fn write_fake_lsp_server(root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let path = root.join("fake_lsp.py");
    fs::write(
        &path,
        r#"
import json
import os
import sys

def log_env(name, line):
    path = os.environ.get(name)
    if not path:
        return
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(line + "\n")

def read_message():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode("utf-8").split(":", 1)
        headers[key.lower()] = value.strip()
    length = int(headers.get("content-length", "0"))
    if length <= 0:
        return None
    return json.loads(sys.stdin.buffer.read(length).decode("utf-8"))

def send(message):
    body = json.dumps(message, separators=(",", ":")).encode("utf-8")
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body))
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()

def result(id, value):
    send({"jsonrpc": "2.0", "id": id, "result": value})

def publish(uri):
    send({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "diagnostics": [{
                "range": {
                    "start": {"line": 1, "character": 4},
                    "end": {"line": 1, "character": 12}
                },
                "severity": 1,
                "source": "fake",
                "message": "fake diagnostic"
            }]
        }
    })

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    id = message.get("id")
    params = message.get("params") or {}
    uri = ((params.get("textDocument") or {}).get("uri")) or "file:///fake.rs"
    log_env("FAKE_LSP_EVENT_LOG", method or "")
    if method == "initialize":
        log_env("FAKE_LSP_INIT_LOG", "initialize")
        result(id, {"capabilities": {"textDocumentSync": {"change": 1}, "diagnosticProvider": {}}})
    elif method in ("initialized", "workspace/didChangeConfiguration", "exit"):
        if method == "exit":
            break
    elif method == "shutdown":
        result(id, None)
    elif method in ("textDocument/didOpen", "textDocument/didChange"):
        publish(uri)
    elif method == "workspace/didChangeWatchedFiles":
        pass
    elif method == "textDocument/documentSymbol":
        result(id, [{
            "name": "main",
            "kind": 12,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 2, "character": 1}},
            "selectionRange": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}}
        }])
    elif method == "textDocument/definition":
        result(id, [{
            "uri": uri,
            "range": {"start": {"line": 0, "character": 3}, "end": {"line": 0, "character": 7}}
        }])
    elif method == "textDocument/diagnostic":
        result(id, {"kind": "full", "items": [{
            "range": {
                "start": {"line": 1, "character": 4},
                "end": {"line": 1, "character": 12}
            },
            "severity": 1,
            "source": "fake",
            "message": "fake diagnostic"
        }]})
    elif id is not None:
        result(id, [])
"#,
    )?;
    Ok(path)
}

fn temp_dir(prefix: &str) -> Result<PathBuf, Box<dyn Error>> {
    let nanos = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos(),
        Err(_) => 0,
    };
    let path = std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path)?;
    Ok(path)
}

#[test]
fn python3_presence_check_does_not_panic() {
    let _ = Command::new("python3").arg("--version").output();
}
