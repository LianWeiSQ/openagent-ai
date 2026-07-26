use std::{
    collections::{BTreeMap, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{Arc, Mutex, OnceLock},
    thread,
};

use serde::Serialize;
use serde_json::{Value, json};

use super::{
    HttpRuntimeConfig, ensure_direct_capability_allowed, new_id, now_ms, resolve_path_in_root,
    session_root, workspace_for_session, workspace_relative_path,
};

const MAX_TERMINAL_INPUT_CHARS: usize = 32_768;
const MAX_TERMINAL_BUFFER_CHARS: usize = 200_000;
const MAX_TERMINAL_CHUNKS_PER_RESPONSE: usize = 500;

#[derive(Clone, Debug, Serialize)]
struct TerminalChunk {
    sequence: u64,
    stream: &'static str,
    text: String,
}

#[derive(Default)]
struct TerminalOutput {
    chunks: VecDeque<TerminalChunk>,
    next_sequence: u64,
    retained_chars: usize,
    truncated_before: u64,
}

impl TerminalOutput {
    fn push(&mut self, stream: &'static str, text: String) {
        if text.is_empty() {
            return;
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.retained_chars = self.retained_chars.saturating_add(text.chars().count());
        self.chunks.push_back(TerminalChunk {
            sequence: self.next_sequence,
            stream,
            text,
        });
        while self.retained_chars > MAX_TERMINAL_BUFFER_CHARS && self.chunks.len() > 1 {
            let Some(removed) = self.chunks.pop_front() else {
                break;
            };
            self.retained_chars = self
                .retained_chars
                .saturating_sub(removed.text.chars().count());
            self.truncated_before = removed.sequence;
        }
    }
}

struct ManagedTerminal {
    id: String,
    runtime_scope: PathBuf,
    session_id: Option<String>,
    workspace: PathBuf,
    cwd: PathBuf,
    shell: String,
    child: Child,
    stdin: Option<ChildStdin>,
    output: Arc<Mutex<TerminalOutput>>,
    started_at_ms: u64,
    interrupted: bool,
    exit_code: Option<i32>,
    finished_at_ms: Option<u64>,
}

static TERMINALS: OnceLock<Mutex<BTreeMap<String, ManagedTerminal>>> = OnceLock::new();

fn terminal_registry() -> &'static Mutex<BTreeMap<String, ManagedTerminal>> {
    TERMINALS.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn list_terminal_sessions_payload(
    config: &HttpRuntimeConfig,
    request_path: &str,
) -> Result<Value, String> {
    ensure_direct_capability_allowed(config, "terminal")?;
    let session_filter = query_value(request_path, "session_id");
    let mut registry = terminal_registry()
        .lock()
        .map_err(|_| "terminal registry is unavailable".to_string())?;
    let mut terminals = Vec::new();
    let runtime_scope = terminal_runtime_scope(config);
    for terminal in registry.values_mut() {
        if terminal.runtime_scope != runtime_scope {
            continue;
        }
        refresh_terminal_status(terminal)?;
        if session_filter
            .as_deref()
            .is_some_and(|session_id| terminal.session_id.as_deref() != Some(session_id))
        {
            continue;
        }
        terminals.push(terminal_summary(terminal));
    }
    Ok(json!({"terminals": terminals}))
}

pub(super) fn start_terminal_session_payload(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, String> {
    ensure_direct_capability_allowed(config, "terminal")?;
    let payload = serde_json::from_str::<Value>(body).unwrap_or_else(|_| json!({}));
    let session_id = payload
        .get("session_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let workspace = workspace_for_session(config, session_id.as_deref())?;
    let requested_cwd = payload
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim();
    let cwd = if requested_cwd.is_empty() {
        resolve_path_in_root(&workspace, ".")?
    } else {
        resolve_path_in_root(&workspace, requested_cwd)?
    };
    if !cwd.is_dir() {
        return Err(format!(
            "terminal cwd is not a directory: {}",
            cwd.display()
        ));
    }

    let (shell, mut command) = terminal_session_command(&payload)?;
    let mut child = command
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to start terminal session: {error}"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "terminal stdin is unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "terminal stdout is unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "terminal stderr is unavailable".to_string())?;
    let output = Arc::new(Mutex::new(TerminalOutput::default()));
    spawn_terminal_reader(stdout, "stdout", Arc::clone(&output));
    spawn_terminal_reader(stderr, "stderr", Arc::clone(&output));

    let id = new_id("terminal");
    let terminal = ManagedTerminal {
        id: id.clone(),
        runtime_scope: terminal_runtime_scope(config),
        session_id,
        workspace,
        cwd,
        shell,
        child,
        stdin: Some(stdin),
        output,
        started_at_ms: now_ms(),
        interrupted: false,
        exit_code: None,
        finished_at_ms: None,
    };
    let summary = terminal_summary(&terminal);
    terminal_registry()
        .lock()
        .map_err(|_| "terminal registry is unavailable".to_string())?
        .insert(id, terminal);
    Ok(summary)
}

pub(super) fn terminal_session_snapshot_payload(
    config: &HttpRuntimeConfig,
    terminal_id: &str,
    request_path: &str,
) -> Result<Value, String> {
    ensure_direct_capability_allowed(config, "terminal")?;
    let after = query_value(request_path, "after")
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default();
    let mut registry = terminal_registry()
        .lock()
        .map_err(|_| "terminal registry is unavailable".to_string())?;
    let terminal = scoped_terminal_mut(&mut registry, config, terminal_id)?;
    refresh_terminal_status(terminal)?;
    let output = terminal
        .output
        .lock()
        .map_err(|_| "terminal output is unavailable".to_string())?;
    let chunks = output
        .chunks
        .iter()
        .filter(|chunk| chunk.sequence > after)
        .take(MAX_TERMINAL_CHUNKS_PER_RESPONSE)
        .cloned()
        .collect::<Vec<_>>();
    let cursor = chunks
        .last()
        .map(|chunk| chunk.sequence)
        .unwrap_or(after.max(output.truncated_before));
    let mut summary = terminal_summary(terminal);
    if let Some(object) = summary.as_object_mut() {
        object.insert("chunks".to_string(), json!(chunks));
        object.insert("cursor".to_string(), json!(cursor));
        object.insert(
            "truncated_before".to_string(),
            json!(output.truncated_before),
        );
    }
    Ok(summary)
}

pub(super) fn terminal_session_input_payload(
    config: &HttpRuntimeConfig,
    terminal_id: &str,
    body: &str,
) -> Result<Value, String> {
    ensure_direct_capability_allowed(config, "terminal")?;
    let payload = serde_json::from_str::<Value>(body).unwrap_or_else(|_| json!({}));
    let input = payload
        .get("input")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if input.chars().count() > MAX_TERMINAL_INPUT_CHARS {
        return Err(format!(
            "terminal input exceeds {MAX_TERMINAL_INPUT_CHARS} characters"
        ));
    }
    let append_newline = payload
        .get("append_newline")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let mut registry = terminal_registry()
        .lock()
        .map_err(|_| "terminal registry is unavailable".to_string())?;
    let terminal = scoped_terminal_mut(&mut registry, config, terminal_id)?;
    refresh_terminal_status(terminal)?;
    if terminal.exit_code.is_some() {
        return Err("terminal session is not running".to_string());
    }
    let stdin = terminal
        .stdin
        .as_mut()
        .ok_or_else(|| "terminal stdin is closed".to_string())?;
    stdin
        .write_all(input.as_bytes())
        .and_then(|_| {
            if append_newline {
                stdin.write_all(b"\n")
            } else {
                Ok(())
            }
        })
        .and_then(|_| stdin.flush())
        .map_err(|error| format!("failed to write terminal input: {error}"))?;
    Ok(terminal_summary(terminal))
}

pub(super) fn interrupt_terminal_session_payload(
    config: &HttpRuntimeConfig,
    terminal_id: &str,
) -> Result<Value, String> {
    ensure_direct_capability_allowed(config, "terminal")?;
    let mut registry = terminal_registry()
        .lock()
        .map_err(|_| "terminal registry is unavailable".to_string())?;
    let terminal = scoped_terminal_mut(&mut registry, config, terminal_id)?;
    refresh_terminal_status(terminal)?;
    if terminal.exit_code.is_none() {
        terminal
            .child
            .kill()
            .map_err(|error| format!("failed to interrupt terminal: {error}"))?;
        terminal.interrupted = true;
        terminal.stdin = None;
        let _ = terminal.child.wait();
        terminal.exit_code = Some(-1);
        terminal.finished_at_ms = Some(now_ms());
    }
    Ok(terminal_summary(terminal))
}

pub(super) fn close_terminal_session_payload(
    config: &HttpRuntimeConfig,
    terminal_id: &str,
) -> Result<Value, String> {
    ensure_direct_capability_allowed(config, "terminal")?;
    let mut registry = terminal_registry()
        .lock()
        .map_err(|_| "terminal registry is unavailable".to_string())?;
    scoped_terminal_mut(&mut registry, config, terminal_id)?;
    let mut terminal = registry
        .remove(terminal_id)
        .ok_or_else(|| "terminal session not found".to_string())?;
    if terminal.exit_code.is_none() {
        let _ = terminal.child.kill();
        let _ = terminal.child.wait();
    }
    Ok(json!({"closed": true, "terminal_id": terminal_id}))
}

fn terminal_runtime_scope(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config)
}

fn scoped_terminal_mut<'a>(
    registry: &'a mut BTreeMap<String, ManagedTerminal>,
    config: &HttpRuntimeConfig,
    terminal_id: &str,
) -> Result<&'a mut ManagedTerminal, String> {
    let runtime_scope = terminal_runtime_scope(config);
    registry
        .get_mut(terminal_id)
        .filter(|terminal| terminal.runtime_scope == runtime_scope)
        .ok_or_else(|| "terminal session not found".to_string())
}

fn terminal_session_command(payload: &Value) -> Result<(String, Command), String> {
    let requested = payload
        .get("shell")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    #[cfg(windows)]
    let shell = requested.unwrap_or("cmd.exe").to_string();
    #[cfg(not(windows))]
    let shell = requested
        .map(str::to_string)
        .or_else(|| {
            std::env::var("SHELL")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| "/bin/sh".to_string());
    if Path::new(&shell).components().count() > 1 && !Path::new(&shell).is_file() {
        return Err(format!("terminal shell does not exist: {shell}"));
    }
    let mut command = Command::new(&shell);
    command.env("TERM", "dumb").env("NO_COLOR", "1");
    Ok((shell, command))
}

fn spawn_terminal_reader<R>(mut reader: R, stream: &'static str, output: Arc<Mutex<TerminalOutput>>)
where
    R: Read + Send + 'static,
{
    thread::Builder::new()
        .name(format!("openagent-terminal-{stream}"))
        .spawn(move || {
            let mut buffer = [0_u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        let text = String::from_utf8_lossy(&buffer[..count]).to_string();
                        if let Ok(mut output) = output.lock() {
                            output.push(stream, text);
                        } else {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .ok();
}

fn refresh_terminal_status(terminal: &mut ManagedTerminal) -> Result<(), String> {
    if terminal.exit_code.is_some() {
        return Ok(());
    }
    if let Some(status) = terminal
        .child
        .try_wait()
        .map_err(|error| format!("failed to poll terminal session: {error}"))?
    {
        terminal.exit_code = Some(status.code().unwrap_or(1));
        terminal.finished_at_ms = Some(now_ms());
        terminal.stdin = None;
    }
    Ok(())
}

fn terminal_summary(terminal: &ManagedTerminal) -> Value {
    let status = if terminal.exit_code.is_none() {
        "running"
    } else if terminal.interrupted {
        "interrupted"
    } else if terminal.exit_code == Some(0) {
        "completed"
    } else {
        "failed"
    };
    json!({
        "terminal_id": terminal.id,
        "session_id": terminal.session_id,
        "workspace": terminal.workspace.to_string_lossy(),
        "cwd": terminal.cwd.to_string_lossy(),
        "cwd_relative": workspace_relative_path(&terminal.workspace, &terminal.cwd),
        "shell": terminal.shell,
        "status": status,
        "running": terminal.exit_code.is_none(),
        "exit_code": terminal.exit_code,
        "started_at_ms": terminal.started_at_ms,
        "finished_at_ms": terminal.finished_at_ms,
    })
}

fn query_value(path: &str, key: &str) -> Option<String> {
    let query = path.split_once('?')?.1;
    query.split('&').find_map(|part| {
        let (candidate, value) = part.split_once('=')?;
        (candidate == key).then(|| percent_decode(value))
    })
}

fn percent_decode(value: &str) -> String {
    let mut bytes = Vec::with_capacity(value.len());
    let raw = value.as_bytes();
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'%'
            && index + 2 < raw.len()
            && let Ok(decoded) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            bytes.push(decoded);
            index += 3;
            continue;
        }
        bytes.push(if raw[index] == b'+' { b' ' } else { raw[index] });
        index += 1;
    }
    String::from_utf8_lossy(&bytes).to_string()
}

#[cfg(test)]
mod tests {
    use std::{fs, thread, time::Duration};

    use serde_json::json;

    use super::*;

    #[test]
    fn persistent_terminal_accepts_incremental_input_and_is_workspace_scoped() {
        let root = std::env::temp_dir().join(format!("openagent-terminal-session-{}", now_ms()));
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join("nested")).expect("workspace");
        let config = HttpRuntimeConfig {
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let terminal =
            start_terminal_session_payload(&config, &json!({"cwd": "nested"}).to_string())
                .expect("start terminal");
        let terminal_id = terminal["terminal_id"].as_str().expect("terminal id");
        #[cfg(windows)]
        let command = "echo first & echo second 1>&2";
        #[cfg(not(windows))]
        let command = "printf first; printf second >&2";
        terminal_session_input_payload(
            &config,
            terminal_id,
            &json!({"input": command}).to_string(),
        )
        .expect("write input");
        thread::sleep(Duration::from_millis(100));
        let snapshot =
            terminal_session_snapshot_payload(&config, terminal_id, "?after=0").expect("snapshot");
        let combined = snapshot["chunks"]
            .as_array()
            .expect("chunks")
            .iter()
            .filter_map(|chunk| chunk["text"].as_str())
            .collect::<String>();
        assert!(combined.contains("first"));
        assert!(combined.contains("second"));
        assert_eq!(snapshot["cwd_relative"], "nested");
        let cursor = snapshot["cursor"].as_u64().expect("cursor");
        let empty =
            terminal_session_snapshot_payload(&config, terminal_id, &format!("?after={cursor}"))
                .expect("incremental snapshot");
        assert_eq!(empty["chunks"].as_array().map(Vec::len), Some(0));
        interrupt_terminal_session_payload(&config, terminal_id).expect("interrupt");
        let closed = close_terminal_session_payload(&config, terminal_id).expect("close");
        assert_eq!(closed["closed"], true);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn persistent_terminal_registry_is_isolated_per_runtime() {
        let root =
            std::env::temp_dir().join(format!("openagent-terminal-scope-{}", new_id("test")));
        let workspace_a = root.join("workspace-a");
        let workspace_b = root.join("workspace-b");
        fs::create_dir_all(&workspace_a).expect("workspace a");
        fs::create_dir_all(&workspace_b).expect("workspace b");
        let config_a = HttpRuntimeConfig {
            workspace: Some(workspace_a.to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions-a").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };
        let config_b = HttpRuntimeConfig {
            workspace: Some(workspace_b.to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions-b").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        };

        let terminal = start_terminal_session_payload(&config_a, "{}").expect("start terminal");
        let terminal_id = terminal["terminal_id"].as_str().expect("terminal id");
        let listed = list_terminal_sessions_payload(&config_b, "/api/terminal/sessions")
            .expect("list terminal sessions");
        assert_eq!(listed["terminals"].as_array().map(Vec::len), Some(0));
        assert!(
            terminal_session_snapshot_payload(&config_b, terminal_id, "?after=0")
                .expect_err("foreign runtime terminal must be hidden")
                .contains("not found")
        );
        close_terminal_session_payload(&config_a, terminal_id).expect("close terminal");
        let _ = fs::remove_dir_all(root);
    }
}
