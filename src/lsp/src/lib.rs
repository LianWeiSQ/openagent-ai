//! Language Server Protocol support for OpenAgent.
//!
//! This crate intentionally keeps the first Rust implementation small and
//! explicit: discover configured/built-in LSP servers, run one stdio JSON-RPC
//! session for a query, and return structured results to tools/CLI/runtime
//! callers. Long-lived client pools can be layered on top of this API later.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
pub const DEFAULT_TIMEOUT_MS: u64 = 8_000;
pub const DEFAULT_DIAGNOSTIC_WAIT_MS: u64 = 500;

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct LspConfig {
    pub enabled: bool,
    pub servers: BTreeMap<String, LspServerConfig>,
    pub config_path: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LspServerConfig {
    pub id: String,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub root_markers: Vec<String>,
    pub strict_root: bool,
    pub env: BTreeMap<String, String>,
    pub initialization: Option<Value>,
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LspStatus {
    pub id: String,
    pub name: String,
    pub command: Vec<String>,
    pub extensions: Vec<String>,
    pub root: String,
    pub available: bool,
    pub enabled: bool,
    pub running: bool,
    pub source: String,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LspOperation {
    Status,
    Diagnostics,
    GoToDefinition,
    FindReferences,
    Hover,
    DocumentSymbol,
    WorkspaceSymbol,
    GoToImplementation,
    PrepareCallHierarchy,
    IncomingCalls,
    OutgoingCalls,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LspQuery {
    pub operation: LspOperation,
    pub file_path: PathBuf,
    pub line: Option<u64>,
    pub character: Option<u64>,
    pub query: Option<String>,
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LspQueryResult {
    pub operation: LspOperation,
    pub server_id: String,
    pub root: String,
    pub file_path: String,
    pub result: Value,
    pub diagnostics: BTreeMap<String, Vec<Value>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LspDoctorReport {
    pub enabled: bool,
    pub config_path: Option<String>,
    pub server_count: usize,
    pub available_count: usize,
    pub servers: Vec<LspStatus>,
}

#[derive(Clone, Debug, Deserialize)]
struct RawServer {
    command: Option<RawCommand>,
    extensions: Option<Vec<String>>,
    root_markers: Option<Vec<String>>,
    #[serde(alias = "rootMarkers")]
    root_markers_camel: Option<Vec<String>>,
    strict_root: Option<bool>,
    #[serde(alias = "strictRoot")]
    strict_root_camel: Option<bool>,
    disabled: Option<bool>,
    env: Option<BTreeMap<String, String>>,
    initialization: Option<Value>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
enum RawCommand {
    String(String),
    Array(Vec<String>),
}

#[derive(Debug)]
struct SelectedServer {
    server: LspServerConfig,
    root: PathBuf,
}

#[derive(Debug)]
struct RpcConnection {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: u64,
    root: PathBuf,
    initialization: Option<Value>,
    diagnostics: BTreeMap<String, Vec<Value>>,
}

impl Drop for RpcConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[must_use]
pub fn operation_from_str(raw: &str) -> Option<LspOperation> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "status" => Some(LspOperation::Status),
        "diagnostics" | "diagnostic" => Some(LspOperation::Diagnostics),
        "gotodefinition" | "definition" | "go_to_definition" => Some(LspOperation::GoToDefinition),
        "findreferences" | "references" | "find_references" => Some(LspOperation::FindReferences),
        "hover" => Some(LspOperation::Hover),
        "documentsymbol" | "document_symbol" | "symbols" => Some(LspOperation::DocumentSymbol),
        "workspacesymbol" | "workspace_symbol" | "workspace_symbols" => {
            Some(LspOperation::WorkspaceSymbol)
        }
        "gotoimplementation" | "implementation" | "go_to_implementation" => {
            Some(LspOperation::GoToImplementation)
        }
        "preparecallhierarchy" | "prepare_call_hierarchy" | "call_hierarchy" => {
            Some(LspOperation::PrepareCallHierarchy)
        }
        "incomingcalls" | "incoming_calls" => Some(LspOperation::IncomingCalls),
        "outgoingcalls" | "outgoing_calls" => Some(LspOperation::OutgoingCalls),
        _ => None,
    }
}

#[must_use]
pub fn operation_requires_position(operation: &LspOperation) -> bool {
    matches!(
        operation,
        LspOperation::GoToDefinition
            | LspOperation::FindReferences
            | LspOperation::Hover
            | LspOperation::GoToImplementation
            | LspOperation::PrepareCallHierarchy
            | LspOperation::IncomingCalls
            | LspOperation::OutgoingCalls
    )
}

pub fn load_workspace_config(workspace: impl AsRef<Path>) -> Result<LspConfig, String> {
    let workspace = normalize_path(workspace.as_ref());
    let mut config = LspConfig {
        enabled: true,
        servers: builtin_servers(),
        config_path: None,
    };

    let Some((path, value)) = load_config_value(&workspace)? else {
        return Ok(config);
    };
    config.config_path = Some(path_to_string(&path));

    if let Some(enabled) = value.as_bool() {
        config.enabled = enabled;
        if !enabled {
            config.servers.clear();
        }
        return Ok(config);
    }

    let Some(object) = value.as_object() else {
        return Err(format!(
            "LSP config must be a boolean or object: {}",
            path.display()
        ));
    };
    if let Some(enabled) = object.get("enabled").and_then(Value::as_bool) {
        config.enabled = enabled;
        if !enabled {
            config.servers.clear();
            return Ok(config);
        }
    }
    if let Some(lsp_value) = object.get("lsp") {
        merge_lsp_value(&mut config, lsp_value)?;
    } else if let Some(servers) = object.get("servers") {
        merge_server_map(&mut config.servers, servers)?;
    } else {
        merge_server_map(&mut config.servers, &value)?;
    }
    Ok(config)
}

pub fn lsp_status(workspace: impl AsRef<Path>) -> Result<Vec<LspStatus>, String> {
    let workspace = normalize_path(workspace.as_ref());
    let config = load_workspace_config(&workspace)?;
    Ok(status_from_config(&config, &workspace))
}

pub fn lsp_doctor(workspace: impl AsRef<Path>) -> Result<LspDoctorReport, String> {
    let workspace = normalize_path(workspace.as_ref());
    let config = load_workspace_config(&workspace)?;
    let servers = status_from_config(&config, &workspace);
    let available_count = servers.iter().filter(|server| server.available).count();
    Ok(LspDoctorReport {
        enabled: config.enabled,
        config_path: config.config_path,
        server_count: servers.len(),
        available_count,
        servers,
    })
}

pub fn query_workspace(
    workspace: impl AsRef<Path>,
    request: LspQuery,
) -> Result<LspQueryResult, String> {
    let workspace = normalize_path(workspace.as_ref());
    let file_path = resolve_path(&workspace, &request.file_path);
    if !file_path.exists() {
        return Err(format!("File not found: {}", file_path.display()));
    }
    if file_path.is_dir() {
        return Err(format!("Path is a directory: {}", file_path.display()));
    }
    let config = load_workspace_config(&workspace)?;
    if !config.enabled {
        return Err("LSP is disabled by configuration".to_string());
    }
    let selected = select_server(&config, &workspace, &file_path)?;
    if !command_available(
        selected
            .server
            .command
            .first()
            .map(String::as_str)
            .unwrap_or(""),
    ) {
        return Err(format!(
            "LSP server '{}' is not available: {}",
            selected.server.id,
            selected.server.command.join(" ")
        ));
    }

    let timeout = Duration::from_millis(request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let mut rpc = RpcConnection::start(&selected.server, &selected.root)?;
    rpc.initialize(timeout)?;
    rpc.open_document(&file_path, timeout)?;

    let result = match request.operation {
        LspOperation::Status => json!(status_from_config(&config, &workspace)),
        LspOperation::Diagnostics => {
            rpc.collect_diagnostics(&file_path, timeout);
            json!(rpc.diagnostics)
        }
        LspOperation::GoToDefinition => {
            rpc.position_request("textDocument/definition", &file_path, &request, timeout)?
        }
        LspOperation::FindReferences => rpc.position_request_with_extra(
            "textDocument/references",
            &file_path,
            &request,
            json!({"context": {"includeDeclaration": true}}),
            timeout,
        )?,
        LspOperation::Hover => {
            rpc.position_request("textDocument/hover", &file_path, &request, timeout)?
        }
        LspOperation::DocumentSymbol => rpc.request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": file_uri(&file_path)}}),
            timeout,
        )?,
        LspOperation::WorkspaceSymbol => rpc.request(
            "workspace/symbol",
            json!({"query": request.query.unwrap_or_default()}),
            timeout,
        )?,
        LspOperation::GoToImplementation => {
            rpc.position_request("textDocument/implementation", &file_path, &request, timeout)?
        }
        LspOperation::PrepareCallHierarchy => rpc.position_request(
            "textDocument/prepareCallHierarchy",
            &file_path,
            &request,
            timeout,
        )?,
        LspOperation::IncomingCalls => {
            rpc.call_hierarchy("callHierarchy/incomingCalls", &file_path, &request, timeout)?
        }
        LspOperation::OutgoingCalls => {
            rpc.call_hierarchy("callHierarchy/outgoingCalls", &file_path, &request, timeout)?
        }
    };
    rpc.collect_for(Duration::from_millis(
        DEFAULT_DIAGNOSTIC_WAIT_MS.min(timeout.as_millis() as u64),
    ));
    let diagnostics = rpc.diagnostics.clone();
    let _ = rpc.shutdown(Duration::from_millis(500));

    Ok(LspQueryResult {
        operation: request.operation,
        server_id: selected.server.id,
        root: path_to_string(&selected.root),
        file_path: path_to_string(&file_path),
        result,
        diagnostics,
    })
}

fn merge_lsp_value(config: &mut LspConfig, value: &Value) -> Result<(), String> {
    if let Some(enabled) = value.as_bool() {
        config.enabled = enabled;
        if !enabled {
            config.servers.clear();
        }
        return Ok(());
    }
    let Some(object) = value.as_object() else {
        return Err("lsp config must be a boolean or object".to_string());
    };
    if let Some(enabled) = object.get("enabled").and_then(Value::as_bool) {
        config.enabled = enabled;
        if !enabled {
            config.servers.clear();
            return Ok(());
        }
    }
    if let Some(servers) = object.get("servers") {
        merge_server_map(&mut config.servers, servers)
    } else {
        merge_server_map(&mut config.servers, value)
    }
}

fn merge_server_map(
    servers: &mut BTreeMap<String, LspServerConfig>,
    value: &Value,
) -> Result<(), String> {
    let Some(object) = value.as_object() else {
        return Err("LSP server map must be an object".to_string());
    };
    for (id, item) in object {
        if matches!(id.as_str(), "enabled" | "servers" | "lsp") {
            continue;
        }
        let raw: RawServer = serde_json::from_value(item.clone())
            .map_err(|error| format!("invalid LSP server '{id}': {error}"))?;
        if raw.disabled.unwrap_or(false) {
            servers.remove(id);
            continue;
        }
        let mut server = servers.get(id).cloned().unwrap_or_else(|| LspServerConfig {
            id: id.clone(),
            command: Vec::new(),
            extensions: Vec::new(),
            root_markers: Vec::new(),
            strict_root: false,
            env: BTreeMap::new(),
            initialization: None,
            source: "config".to_string(),
        });
        if let Some(command) = raw.command {
            server.command = match command {
                RawCommand::String(command) => split_command_string(&command),
                RawCommand::Array(command) => command,
            };
        }
        if let Some(extensions) = raw.extensions {
            server.extensions = extensions;
        }
        if let Some(root_markers) = raw.root_markers.or(raw.root_markers_camel) {
            server.root_markers = root_markers;
        }
        if let Some(strict_root) = raw.strict_root.or(raw.strict_root_camel) {
            server.strict_root = strict_root;
        }
        if let Some(env) = raw.env {
            server.env = env;
        }
        if raw.initialization.is_some() {
            server.initialization = raw.initialization;
        }
        server.source = "config".to_string();
        if server.command.is_empty() {
            return Err(format!("LSP server '{id}' is missing command"));
        }
        servers.insert(id.clone(), server);
    }
    Ok(())
}

fn load_config_value(workspace: &Path) -> Result<Option<(PathBuf, Value)>, String> {
    if let Ok(raw) = env::var("OPENAGENT_LSP_CONFIG") {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        if trimmed.starts_with('{') || trimmed == "true" || trimmed == "false" {
            let value = serde_json::from_str(trimmed)
                .map_err(|error| format!("invalid OPENAGENT_LSP_CONFIG JSON: {error}"))?;
            return Ok(Some((PathBuf::from("OPENAGENT_LSP_CONFIG"), value)));
        }
        let path = PathBuf::from(trimmed);
        if path.exists() {
            let value = read_json_file(&path)?;
            return Ok(Some((path, value)));
        }
        return Err(format!("OPENAGENT_LSP_CONFIG path not found: {trimmed}"));
    }

    for relative in [
        ".openagent/lsp.json",
        ".openagent/lsp.jsonc",
        ".openharness/lsp.json",
    ] {
        let path = workspace.join(relative);
        if path.exists() {
            return Ok(Some((path.clone(), read_json_file(&path)?)));
        }
    }
    Ok(None)
}

fn read_json_file(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("failed to parse {}: {error}", path.display()))
}

fn status_from_config(config: &LspConfig, workspace: &Path) -> Vec<LspStatus> {
    if !config.enabled {
        return Vec::new();
    }
    config
        .servers
        .values()
        .map(|server| {
            let program = server.command.first().map(String::as_str).unwrap_or("");
            let available = command_available(program);
            LspStatus {
                id: server.id.clone(),
                name: server.id.clone(),
                command: server.command.clone(),
                extensions: server.extensions.clone(),
                root: path_to_string(workspace),
                available,
                enabled: true,
                running: false,
                source: server.source.clone(),
                reason: (!available).then(|| format!("command not found: {program}")),
            }
        })
        .collect()
}

fn select_server(
    config: &LspConfig,
    workspace: &Path,
    file_path: &Path,
) -> Result<SelectedServer, String> {
    let extension = file_path
        .extension()
        .and_then(OsStr::to_str)
        .map(|ext| format!(".{ext}"))
        .unwrap_or_default();
    for server in config.servers.values() {
        if !server.extensions.is_empty() && !server.extensions.iter().any(|ext| ext == &extension) {
            continue;
        }
        let Some(root) = nearest_root(file_path, workspace, server) else {
            continue;
        };
        return Ok(SelectedServer {
            server: server.clone(),
            root,
        });
    }
    Err(format!(
        "No LSP server configured for {}",
        file_path.display()
    ))
}

fn nearest_root(file_path: &Path, workspace: &Path, server: &LspServerConfig) -> Option<PathBuf> {
    let mut current = file_path.parent().unwrap_or(workspace).to_path_buf();
    loop {
        if server
            .root_markers
            .iter()
            .any(|marker| current.join(marker).exists())
        {
            return Some(current);
        }
        if current == workspace {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    (!server.strict_root).then(|| workspace.to_path_buf())
}

fn builtin_servers() -> BTreeMap<String, LspServerConfig> {
    [
        builtin_server(
            "rust-analyzer",
            &["rust-analyzer"],
            &[".rs"],
            &["Cargo.toml", "rust-project.json"],
            false,
        ),
        builtin_server(
            "typescript",
            &["typescript-language-server", "--stdio"],
            &[".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"],
            &[
                "package-lock.json",
                "bun.lockb",
                "bun.lock",
                "pnpm-lock.yaml",
                "yarn.lock",
                "package.json",
            ],
            false,
        ),
        builtin_server(
            "deno",
            &["deno", "lsp"],
            &[".ts", ".tsx", ".js", ".jsx", ".mjs"],
            &["deno.json", "deno.jsonc"],
            true,
        ),
        builtin_server(
            "pyright",
            &["pyright-langserver", "--stdio"],
            &[".py"],
            &["pyproject.toml", "setup.py", "requirements.txt"],
            false,
        ),
        builtin_server(
            "pylsp",
            &["pylsp"],
            &[".py"],
            &["pyproject.toml", "setup.py", "requirements.txt"],
            false,
        ),
        builtin_server("gopls", &["gopls"], &[".go"], &["go.mod", "go.work"], false),
        builtin_server(
            "clangd",
            &["clangd", "--background-index"],
            &[".c", ".cc", ".cpp", ".cxx", ".h", ".hpp", ".hh"],
            &["compile_commands.json", ".clangd"],
            false,
        ),
    ]
    .into_iter()
    .map(|server| (server.id.clone(), server))
    .collect()
}

fn builtin_server(
    id: &str,
    command: &[&str],
    extensions: &[&str],
    root_markers: &[&str],
    strict_root: bool,
) -> LspServerConfig {
    LspServerConfig {
        id: id.to_string(),
        command: command.iter().map(|item| (*item).to_string()).collect(),
        extensions: extensions.iter().map(|item| (*item).to_string()).collect(),
        root_markers: root_markers
            .iter()
            .map(|item| (*item).to_string())
            .collect(),
        strict_root,
        env: BTreeMap::new(),
        initialization: None,
        source: "builtin".to_string(),
    }
}

fn split_command_string(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect()
}

impl RpcConnection {
    fn start(server: &LspServerConfig, root: &Path) -> Result<Self, String> {
        let program = server
            .command
            .first()
            .ok_or_else(|| format!("LSP server '{}' has no command", server.id))?;
        let mut command = Command::new(program);
        command
            .args(server.command.iter().skip(1))
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for (key, value) in &server.env {
            command.env(key, value);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to start LSP server '{}': {error}", server.id))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| format!("LSP server '{}' did not expose stdin", server.id))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| format!("LSP server '{}' did not expose stdout", server.id))?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(message)) = read_lsp_frame(&mut reader) {
                if tx.send(message).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 1,
            root: root.to_path_buf(),
            initialization: server.initialization.clone(),
            diagnostics: BTreeMap::new(),
        })
    }

    fn initialize(&mut self, timeout: Duration) -> Result<(), String> {
        let process_id = std::process::id();
        let root_uri = file_uri(&self.root);
        let mut params = Map::new();
        params.insert("processId".to_string(), json!(process_id));
        params.insert("rootUri".to_string(), json!(root_uri));
        params.insert(
            "workspaceFolders".to_string(),
            json!([{"name": "workspace", "uri": file_uri(&self.root)}]),
        );
        params.insert(
            "initializationOptions".to_string(),
            self.initialization.clone().unwrap_or_else(|| json!({})),
        );
        params.insert(
            "capabilities".to_string(),
            json!({
                "window": {"workDoneProgress": true},
                "workspace": {
                    "configuration": true,
                    "workspaceFolders": true,
                    "diagnostics": {"refreshSupport": false}
                },
                "textDocument": {
                    "synchronization": {"didOpen": true, "didChange": true},
                    "diagnostic": {"dynamicRegistration": true, "relatedDocumentSupport": true},
                    "publishDiagnostics": {"versionSupport": false},
                    "hover": {"contentFormat": ["markdown", "plaintext"]},
                    "definition": {"linkSupport": true},
                    "references": {},
                    "documentSymbol": {"hierarchicalDocumentSymbolSupport": true},
                    "implementation": {"linkSupport": true},
                    "callHierarchy": {"dynamicRegistration": false}
                }
            }),
        );
        self.request("initialize", Value::Object(params), timeout)?;
        self.notify("initialized", json!({}))?;
        if let Some(initialization) = self.initialization.clone() {
            self.notify(
                "workspace/didChangeConfiguration",
                json!({"settings": initialization}),
            )?;
        }
        Ok(())
    }

    fn open_document(&mut self, file_path: &Path, timeout: Duration) -> Result<(), String> {
        let text = fs::read_to_string(file_path)
            .map_err(|error| format!("failed to read {}: {error}", file_path.display()))?;
        self.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_uri(file_path),
                    "languageId": language_id(file_path),
                    "version": 0,
                    "text": text,
                }
            }),
        )?;
        self.collect_for(Duration::from_millis(100).min(timeout));
        Ok(())
    }

    fn position_request(
        &mut self,
        method: &str,
        file_path: &Path,
        request: &LspQuery,
        timeout: Duration,
    ) -> Result<Value, String> {
        self.position_request_with_extra(method, file_path, request, json!({}), timeout)
    }

    fn position_request_with_extra(
        &mut self,
        method: &str,
        file_path: &Path,
        request: &LspQuery,
        extra: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let (line, character) = zero_based_position(request)?;
        let mut params = Map::new();
        params.insert(
            "textDocument".to_string(),
            json!({"uri": file_uri(file_path)}),
        );
        params.insert(
            "position".to_string(),
            json!({"line": line, "character": character}),
        );
        if let Some(extra) = extra.as_object() {
            for (key, value) in extra {
                params.insert(key.clone(), value.clone());
            }
        }
        self.request(method, Value::Object(params), timeout)
    }

    fn call_hierarchy(
        &mut self,
        method: &str,
        file_path: &Path,
        request: &LspQuery,
        timeout: Duration,
    ) -> Result<Value, String> {
        let prepared = self.position_request(
            "textDocument/prepareCallHierarchy",
            file_path,
            request,
            timeout,
        )?;
        let Some(item) = prepared.as_array().and_then(|items| items.first()).cloned() else {
            return Ok(json!([]));
        };
        self.request(method, json!({"item": item}), timeout)
    }

    fn collect_diagnostics(&mut self, file_path: &Path, timeout: Duration) {
        let result = self.request(
            "textDocument/diagnostic",
            json!({"textDocument": {"uri": file_uri(file_path)}}),
            timeout.min(Duration::from_millis(3_000)),
        );
        if let Ok(report) = result
            && let Some(items) = report.get("items").and_then(Value::as_array)
        {
            self.diagnostics
                .insert(path_to_string(file_path), items.clone());
        }
        self.collect_for(Duration::from_millis(DEFAULT_DIAGNOSTIC_WAIT_MS));
    }

    fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.write(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let started = Instant::now();
        loop {
            let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
                return Err(format!("LSP request timed out: {method}"));
            };
            match self.rx.recv_timeout(remaining) {
                Ok(message) => {
                    if let Some(result) = self.handle_message(message, Some(id))? {
                        return Ok(result);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(format!("LSP request timed out: {method}"));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(format!("LSP server disconnected during request: {method}"));
                }
            }
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.write(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    fn collect_for(&mut self, duration: Duration) {
        let started = Instant::now();
        while let Some(remaining) = duration.checked_sub(started.elapsed()) {
            match self.rx.recv_timeout(remaining) {
                Ok(message) => {
                    let _ = self.handle_message(message, None);
                }
                Err(_) => break,
            }
        }
    }

    fn shutdown(&mut self, timeout: Duration) -> Result<(), String> {
        let _ = self.request("shutdown", json!(null), timeout);
        let _ = self.notify("exit", json!(null));
        let _ = self.child.kill();
        let _ = self.child.wait();
        Ok(())
    }

    fn handle_message(
        &mut self,
        message: Value,
        expected_response_id: Option<u64>,
    ) -> Result<Option<Value>, String> {
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            if method == "textDocument/publishDiagnostics" {
                if let Some(params) = message.get("params") {
                    self.record_publish_diagnostics(params);
                }
                return Ok(None);
            }
            if let Some(id) = message.get("id").cloned() {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                let result = self.default_client_response(method, &params);
                self.write(json!({"jsonrpc": "2.0", "id": id, "result": result}))?;
            }
            return Ok(None);
        }

        if let Some(expected) = expected_response_id
            && message
                .get("id")
                .and_then(Value::as_u64)
                .is_some_and(|id| id == expected)
        {
            if let Some(error) = message.get("error") {
                return Err(format!("LSP server returned error: {error}"));
            }
            return Ok(Some(message.get("result").cloned().unwrap_or(Value::Null)));
        }
        Ok(None)
    }

    fn default_client_response(&self, method: &str, params: &Value) -> Value {
        match method {
            "workspace/workspaceFolders" => {
                json!([{"name": "workspace", "uri": file_uri(&self.root)}])
            }
            "workspace/configuration" => {
                let items = params
                    .get("items")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                let settings = self.initialization.clone().unwrap_or(Value::Null);
                Value::Array(
                    items
                        .iter()
                        .map(|item| {
                            item.get("section")
                                .and_then(Value::as_str)
                                .and_then(|section| nested_setting(&settings, section))
                                .unwrap_or_else(|| settings.clone())
                        })
                        .collect(),
                )
            }
            "window/workDoneProgress/create"
            | "client/registerCapability"
            | "client/unregisterCapability"
            | "workspace/diagnostic/refresh" => Value::Null,
            _ => Value::Null,
        }
    }

    fn record_publish_diagnostics(&mut self, params: &Value) {
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return;
        };
        let Some(path) = file_path_from_uri(uri) else {
            return;
        };
        let diagnostics = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.diagnostics.insert(path, diagnostics);
    }

    fn write(&mut self, message: Value) -> Result<(), String> {
        let body = serde_json::to_vec(&message)
            .map_err(|error| format!("failed to encode LSP message: {error}"))?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n", body.len())
            .map_err(|error| format!("failed to write LSP header: {error}"))?;
        self.stdin
            .write_all(&body)
            .map_err(|error| format!("failed to write LSP body: {error}"))?;
        self.stdin
            .flush()
            .map_err(|error| format!("failed to flush LSP message: {error}"))
    }
}

fn zero_based_position(request: &LspQuery) -> Result<(u64, u64), String> {
    let line = request
        .line
        .ok_or_else(|| "LSP operation requires line".to_string())?;
    let character = request
        .character
        .ok_or_else(|| "LSP operation requires character".to_string())?;
    if line == 0 || character == 0 {
        return Err("LSP line and character are 1-based and must be >= 1".to_string());
    }
    Ok((line - 1, character - 1))
}

fn read_lsp_frame<R: BufRead + Read>(reader: &mut R) -> io::Result<Option<Value>> {
    let mut content_length = None;
    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line)?;
        if bytes == 0 {
            return Ok(None);
        }
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((key, value)) = trimmed.split_once(':')
            && key.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse::<usize>().ok();
        }
    }
    let Some(length) = content_length else {
        return Ok(None);
    };
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body).ok())
}

fn nested_setting(settings: &Value, section: &str) -> Option<Value> {
    let mut current = settings;
    for key in section.split('.') {
        current = current.get(key)?;
    }
    Some(current.clone())
}

fn language_id(file_path: &Path) -> String {
    match file_path.extension().and_then(OsStr::to_str).unwrap_or("") {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "swift" => "swift",
        _ => "plaintext",
    }
    .to_string()
}

#[must_use]
pub fn command_available(command: &str) -> bool {
    if command.trim().is_empty() {
        return false;
    }
    let path = Path::new(command);
    if path.components().count() > 1 || path.is_absolute() {
        return path.exists();
    }
    env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).any(|path| path.join(command).exists()))
        .unwrap_or(false)
}

fn resolve_path(workspace: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        normalize_path(path)
    } else {
        normalize_path(&workspace.join(path))
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Normal(part) => normalized.push(part),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
        }
    }
    normalized
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

fn file_uri(path: &Path) -> String {
    let text = path_to_string(&normalize_path(path));
    format!("file://{}", text.replace(' ', "%20"))
}

fn file_path_from_uri(uri: &str) -> Option<String> {
    let raw = uri.strip_prefix("file://")?.replace("%20", " ");
    Some(raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_operation_aliases() {
        assert_eq!(
            operation_from_str("definition"),
            Some(LspOperation::GoToDefinition)
        );
        assert_eq!(
            operation_from_str("workspace_symbol"),
            Some(LspOperation::WorkspaceSymbol)
        );
        assert!(operation_from_str("nope").is_none());
    }

    #[test]
    fn default_config_contains_builtin_servers() -> Result<(), String> {
        let temp = unique_temp_dir("openagent-lsp-config")?;
        let config = load_workspace_config(&temp)?;
        assert!(config.enabled);
        assert!(config.servers.contains_key("rust-analyzer"));
        assert!(config.servers.contains_key("typescript"));
        let _ = fs::remove_dir_all(temp);
        Ok(())
    }

    fn unique_temp_dir(prefix: &str) -> Result<PathBuf, String> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let path = env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()));
        fs::create_dir_all(&path)
            .map_err(|error| format!("failed to create {}: {error}", path.display()))?;
        Ok(path)
    }
}
