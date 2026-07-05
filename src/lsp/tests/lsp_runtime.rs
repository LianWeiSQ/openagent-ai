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

#[test]
fn diagnostics_merge_dynamic_related_and_workspace_reports() -> Result<(), Box<dyn Error>> {
    if !command_available("python3") {
        return Ok(());
    }
    let root = temp_dir("openagent-lsp-dynamic-diagnostics")?;
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fake\"\n")?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), "fn main() {\n    broken();\n}\n")?;
    fs::write(root.join("src/lib.rs"), "pub fn helper() {}\n")?;
    let fake = write_fake_lsp_server(&root)?;
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
                        "FAKE_LSP_DYNAMIC_DIAGNOSTICS": "1"
                    }
                }
            }
        }))?,
    )?;

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
    let main_path = root.join("src/main.rs").to_string_lossy().to_string();
    let related_path = root.join("src/lib.rs").to_string_lossy().to_string();
    let main = diagnostics
        .diagnostics
        .get(&main_path)
        .ok_or("missing main diagnostics")?;
    let related = diagnostics
        .diagnostics
        .get(&related_path)
        .ok_or("missing related diagnostics")?;
    assert_eq!(
        main.iter()
            .filter(|item| item["message"] == "duplicate diagnostic")
            .count(),
        1,
        "diagnostics should be deduplicated across push/document/workspace sources"
    );
    assert!(
        main.iter()
            .any(|item| item["message"] == "workspace diagnostic")
    );
    assert!(
        related
            .iter()
            .any(|item| item["message"] == "related diagnostic")
    );

    assert_eq!(shutdown_workspace_clients(&root), 1);
    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn startup_failure_marks_server_broken_and_skips_retry() -> Result<(), Box<dyn Error>> {
    if !command_available("python3") {
        return Ok(());
    }
    let root = temp_dir("openagent-lsp-broken")?;
    fs::write(root.join("Cargo.toml"), "[package]\nname = \"fake\"\n")?;
    fs::create_dir_all(root.join("src"))?;
    fs::write(root.join("src/main.rs"), "fn main() {}\n")?;
    let failing = write_failing_lsp_server(&root)?;
    let attempt_log = root.join("attempts.log");
    fs::create_dir_all(root.join(".openagent"))?;
    fs::write(
        root.join(".openagent/lsp.json"),
        serde_json::to_string_pretty(&json!({
            "servers": {
                "fake": {
                    "command": ["python3", failing],
                    "extensions": [".rs"],
                    "root_markers": ["Cargo.toml"],
                    "env": {
                        "FAKE_LSP_ATTEMPT_LOG": attempt_log.to_string_lossy()
                    }
                }
            }
        }))?,
    )?;

    let request = || LspQuery {
        operation: LspOperation::DocumentSymbol,
        file_path: PathBuf::from("src/main.rs"),
        line: None,
        character: None,
        query: None,
        timeout_ms: Some(500),
    };
    let first = query_workspace(&root, request()).expect_err("first startup should fail");
    assert!(
        first.contains("disconnected") || first.contains("failed to write"),
        "{first}"
    );
    let second = query_workspace(&root, request()).expect_err("broken client should be skipped");
    assert!(second.contains("temporarily disabled"), "{second}");
    assert_eq!(fs::read_to_string(&attempt_log)?.lines().count(), 1);

    let status = lsp_status(&root)?;
    let fake = status
        .iter()
        .find(|server| server.id == "fake")
        .ok_or("missing fake server")?;
    assert!(!fake.available);
    assert!(
        fake.reason
            .as_deref()
            .unwrap_or_default()
            .contains("startup failed")
    );

    assert_eq!(shutdown_workspace_clients(&root), 0);
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
from pathlib import Path

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

next_request_id = 1000

def request(method, params):
    global next_request_id
    send({"jsonrpc": "2.0", "id": next_request_id, "method": method, "params": params})
    next_request_id += 1

def dynamic_diagnostics_enabled():
    return os.environ.get("FAKE_LSP_DYNAMIC_DIAGNOSTICS") == "1"

def file_uri(relative):
    return Path.cwd().joinpath(relative).as_uri()

def diagnostic(message, line=1, character=4):
    return {
        "range": {
            "start": {"line": line, "character": character},
            "end": {"line": line, "character": character + 8}
        },
        "severity": 1,
        "source": "fake",
        "message": message
    }

def publish(uri):
    message = "duplicate diagnostic" if dynamic_diagnostics_enabled() else "fake diagnostic"
    send({
        "jsonrpc": "2.0",
        "method": "textDocument/publishDiagnostics",
        "params": {
            "uri": uri,
            "diagnostics": [diagnostic(message)]
        }
    })

while True:
    message = read_message()
    if message is None:
        break
    method = message.get("method")
    id = message.get("id")
    if method is None:
        continue
    params = message.get("params") or {}
    uri = ((params.get("textDocument") or {}).get("uri")) or "file:///fake.rs"
    log_env("FAKE_LSP_EVENT_LOG", method or "")
    if method == "initialize":
        log_env("FAKE_LSP_INIT_LOG", "initialize")
        result(id, {"capabilities": {"textDocumentSync": {"change": 1}, "diagnosticProvider": {}}})
    elif method == "initialized":
        if dynamic_diagnostics_enabled():
            request("client/registerCapability", {
                "registrations": [
                    {
                        "id": "doc-reg",
                        "method": "textDocument/diagnostic",
                        "registerOptions": {"identifier": "doc-source"}
                    },
                    {
                        "id": "workspace-reg",
                        "method": "textDocument/diagnostic",
                        "registerOptions": {
                            "identifier": "workspace-source",
                            "workspaceDiagnostics": True
                        }
                    }
                ]
            })
    elif method == "workspace/didChangeConfiguration":
        pass
    elif method == "exit":
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
        if dynamic_diagnostics_enabled():
            result(id, {
                "kind": "full",
                "items": [diagnostic("duplicate diagnostic")],
                "relatedDocuments": {
                    file_uri("src/lib.rs"): {
                        "kind": "full",
                        "items": [diagnostic("related diagnostic", 0, 0)]
                    }
                }
            })
        else:
            result(id, {"kind": "full", "items": [diagnostic("fake diagnostic")]})
    elif method == "workspace/diagnostic":
        if dynamic_diagnostics_enabled():
            result(id, {
                "items": [
                    {
                        "uri": file_uri("src/main.rs"),
                        "items": [
                            diagnostic("duplicate diagnostic"),
                            diagnostic("workspace diagnostic", 1, 8)
                        ]
                    },
                    {
                        "uri": file_uri("src/lib.rs"),
                        "items": [diagnostic("related diagnostic", 0, 0)]
                    }
                ]
            })
        else:
            result(id, {"items": []})
    elif id is not None:
        result(id, [])
"#,
    )?;
    Ok(path)
}

fn write_failing_lsp_server(root: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let path = root.join("failing_lsp.py");
    fs::write(
        &path,
        r#"
import os

attempt_log = os.environ.get("FAKE_LSP_ATTEMPT_LOG")
if attempt_log:
    with open(attempt_log, "a", encoding="utf-8") as handle:
        handle.write("start\n")
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
