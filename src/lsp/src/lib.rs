//! Language Server Protocol support for OpenAgent.
//!
//! This crate intentionally keeps the first Rust implementation small and
//! explicit: discover configured/built-in LSP servers, keep reusable stdio
//! JSON-RPC clients per workspace/root/server, and return structured results to
//! tools/CLI/runtime callers.

use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    path::{Component, Path, PathBuf},
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Mutex, OnceLock,
        mpsc::{self, Receiver},
    },
    thread,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
pub const DEFAULT_TIMEOUT_MS: u64 = 8_000;
pub const DEFAULT_DIAGNOSTIC_WAIT_MS: u64 = 500;
pub const DEFAULT_TOUCH_TIMEOUT_MS: u64 = 2_000;
pub const DEFAULT_BROKEN_COOLDOWN_MS: u64 = 60_000;

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
    pub exclude_root_markers: Vec<String>,
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

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LspTouchResult {
    pub server_id: String,
    pub root: String,
    pub file_path: String,
}

#[derive(Clone, Debug, Deserialize)]
struct RawServer {
    command: Option<RawCommand>,
    extensions: Option<Vec<String>>,
    root_markers: Option<Vec<String>>,
    #[serde(alias = "rootMarkers")]
    root_markers_camel: Option<Vec<String>>,
    exclude_root_markers: Option<Vec<String>>,
    #[serde(alias = "excludeRootMarkers")]
    exclude_root_markers_camel: Option<Vec<String>>,
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
    capabilities: Value,
    diagnostics: BTreeMap<String, Vec<Value>>,
    push_diagnostics: BTreeMap<String, Vec<Value>>,
    pull_diagnostics: BTreeMap<String, Vec<Value>>,
    diagnostic_registrations: BTreeMap<String, DiagnosticRegistration>,
    documents: BTreeMap<String, OpenDocument>,
}

#[derive(Clone, Debug)]
struct OpenDocument {
    version: u64,
}

#[derive(Clone, Debug)]
struct DiagnosticRegistration {
    identifier: Option<String>,
    workspace_diagnostics: bool,
}

#[derive(Debug, Default)]
struct DiagnosticRequestResult {
    handled: bool,
    matched: bool,
    by_file: BTreeMap<String, Vec<Value>>,
}

#[derive(Debug, Default)]
struct LspClientPool {
    clients: BTreeMap<String, PooledClient>,
    broken: BTreeMap<String, BrokenClient>,
}

#[derive(Debug)]
struct PooledClient {
    workspace: PathBuf,
    server_id: String,
    root: PathBuf,
    connection: RpcConnection,
}

#[derive(Debug)]
struct BrokenClient {
    workspace: PathBuf,
    server_id: String,
    root: PathBuf,
    reason: String,
    failed_at: Instant,
}

impl Drop for RpcConnection {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl LspServerConfig {
    fn excluding(mut self, markers: &[&str]) -> Self {
        self.exclude_root_markers = markers.iter().map(|item| (*item).to_string()).collect();
        self
    }
}

static LSP_CLIENT_POOL: OnceLock<Mutex<LspClientPool>> = OnceLock::new();

fn client_pool() -> &'static Mutex<LspClientPool> {
    LSP_CLIENT_POOL.get_or_init(|| Mutex::new(LspClientPool::default()))
}

fn pooled_client_key(workspace: &Path, server_id: &str, root: &Path) -> String {
    format!(
        "{}\n{}\n{}",
        path_to_string(workspace),
        server_id,
        path_to_string(root)
    )
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

impl LspClientPool {
    fn client_for(
        &mut self,
        workspace: &Path,
        selected: &SelectedServer,
        timeout: Duration,
    ) -> Result<&mut PooledClient, String> {
        let key = pooled_client_key(workspace, &selected.server.id, &selected.root);
        if let Some(reason) = self.active_broken_reason(&key) {
            return Err(format!(
                "LSP server '{}' is temporarily disabled after startup failure: {reason}",
                selected.server.id
            ));
        }
        if !self.clients.contains_key(&key) {
            let mut connection = match RpcConnection::start(&selected.server, &selected.root) {
                Ok(connection) => connection,
                Err(error) => {
                    self.mark_broken(&key, workspace, selected, error.clone());
                    return Err(error);
                }
            };
            if let Err(error) = connection.initialize(timeout) {
                self.mark_broken(&key, workspace, selected, error.clone());
                return Err(error);
            }
            self.clients.insert(
                key.clone(),
                PooledClient {
                    workspace: workspace.to_path_buf(),
                    server_id: selected.server.id.clone(),
                    root: selected.root.clone(),
                    connection,
                },
            );
            self.broken.remove(&key);
        }
        self.clients
            .get_mut(&key)
            .ok_or_else(|| format!("failed to cache LSP client '{}'", selected.server.id))
    }

    fn running_root(&self, workspace: &Path, server_id: &str) -> Option<PathBuf> {
        self.clients
            .values()
            .find(|client| client.workspace == workspace && client.server_id == server_id)
            .map(|client| client.root.clone())
    }

    fn remove_key(&mut self, key: &str) -> Option<PooledClient> {
        self.clients.remove(key)
    }

    fn active_broken_reason(&mut self, key: &str) -> Option<String> {
        let expired = self
            .broken
            .get(key)
            .is_some_and(|broken| broken.failed_at.elapsed() >= broken_cooldown());
        if expired {
            self.broken.remove(key);
            return None;
        }
        self.broken.get(key).map(|broken| broken.reason.clone())
    }

    fn mark_broken(
        &mut self,
        key: &str,
        workspace: &Path,
        selected: &SelectedServer,
        reason: String,
    ) {
        self.broken.insert(
            key.to_string(),
            BrokenClient {
                workspace: workspace.to_path_buf(),
                server_id: selected.server.id.clone(),
                root: selected.root.clone(),
                reason,
                failed_at: Instant::now(),
            },
        );
    }

    fn broken_status(&mut self, workspace: &Path, server_id: &str) -> Option<(PathBuf, String)> {
        let keys = self.broken.keys().cloned().collect::<Vec<_>>();
        for key in keys {
            if self
                .broken
                .get(&key)
                .is_some_and(|broken| broken.failed_at.elapsed() >= broken_cooldown())
            {
                self.broken.remove(&key);
            }
        }
        self.broken
            .values()
            .find(|broken| broken.workspace == workspace && broken.server_id == server_id)
            .map(|broken| (broken.root.clone(), broken.reason.clone()))
    }

    fn remove_workspace_broken(&mut self, workspace: &Path) {
        let keys = self
            .broken
            .iter()
            .filter_map(|(key, broken)| (broken.workspace == workspace).then(|| key.clone()))
            .collect::<Vec<_>>();
        for key in keys {
            self.broken.remove(&key);
        }
    }
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
    let mut status = status_from_config(&config, &workspace);
    apply_running_status(&mut status, &workspace)?;
    Ok(status)
}

pub fn lsp_doctor(workspace: impl AsRef<Path>) -> Result<LspDoctorReport, String> {
    let workspace = normalize_path(workspace.as_ref());
    let config = load_workspace_config(&workspace)?;
    let mut servers = status_from_config(&config, &workspace);
    apply_running_status(&mut servers, &workspace)?;
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
    if !command_available_for_root(
        selected
            .server
            .command
            .first()
            .map(String::as_str)
            .unwrap_or(""),
        &selected.root,
    ) {
        return Err(format!(
            "LSP server '{}' is not available: {}",
            selected.server.id,
            selected.server.command.join(" ")
        ));
    }

    let timeout = Duration::from_millis(request.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS));
    let operation = request.operation.clone();
    let client_key = pooled_client_key(&workspace, &selected.server.id, &selected.root);
    let response = {
        let mut pool = client_pool()
            .lock()
            .map_err(|_| "LSP client pool lock is poisoned".to_string())?;
        let client = pool.client_for(&workspace, &selected, timeout)?;
        client.connection.open_document(&file_path, timeout)?;
        let result = run_lsp_operation(
            &mut client.connection,
            &config,
            &workspace,
            &file_path,
            &request,
            timeout,
        )?;
        client.connection.collect_for(Duration::from_millis(
            DEFAULT_DIAGNOSTIC_WAIT_MS.min(timeout.as_millis() as u64),
        ));
        Ok::<_, String>((
            client.server_id.clone(),
            path_to_string(&client.root),
            result,
            client.connection.diagnostics.clone(),
        ))
    };
    let (server_id, root, result, diagnostics) = match response {
        Ok(response) => response,
        Err(error) => {
            let _ = shutdown_pooled_client_by_key(&client_key);
            return Err(error);
        }
    };

    Ok(LspQueryResult {
        operation,
        server_id,
        root,
        file_path: path_to_string(&file_path),
        result,
        diagnostics,
    })
}

pub fn touch_workspace_file(
    workspace: impl AsRef<Path>,
    file_path: impl AsRef<Path>,
) -> Result<LspTouchResult, String> {
    let workspace = normalize_path(workspace.as_ref());
    let file_path = resolve_path(&workspace, file_path.as_ref());
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
    if !command_available_for_root(
        selected
            .server
            .command
            .first()
            .map(String::as_str)
            .unwrap_or(""),
        &selected.root,
    ) {
        return Err(format!(
            "LSP server '{}' is not available: {}",
            selected.server.id,
            selected.server.command.join(" ")
        ));
    }

    let timeout = Duration::from_millis(DEFAULT_TOUCH_TIMEOUT_MS);
    let client_key = pooled_client_key(&workspace, &selected.server.id, &selected.root);
    let response = {
        let mut pool = client_pool()
            .lock()
            .map_err(|_| "LSP client pool lock is poisoned".to_string())?;
        let client = pool.client_for(&workspace, &selected, timeout)?;
        client.connection.open_document(&file_path, timeout)?;
        client.connection.collect_for(Duration::from_millis(50));
        Ok::<_, String>(LspTouchResult {
            server_id: client.server_id.clone(),
            root: path_to_string(&client.root),
            file_path: path_to_string(&file_path),
        })
    };
    match response {
        Ok(response) => Ok(response),
        Err(error) => {
            let _ = shutdown_pooled_client_by_key(&client_key);
            Err(error)
        }
    }
}

pub fn shutdown_workspace_clients(workspace: impl AsRef<Path>) -> usize {
    let workspace = normalize_path(workspace.as_ref());
    let Ok(mut pool) = client_pool().lock() else {
        return 0;
    };
    let keys = pool
        .clients
        .iter()
        .filter_map(|(key, client)| (client.workspace == workspace).then(|| key.clone()))
        .collect::<Vec<_>>();
    let mut removed = 0usize;
    for key in keys {
        if let Some(mut client) = pool.remove_key(&key) {
            let _ = client.connection.shutdown(Duration::from_millis(500));
            removed += 1;
        }
    }
    pool.remove_workspace_broken(&workspace);
    removed
}

fn run_lsp_operation(
    rpc: &mut RpcConnection,
    config: &LspConfig,
    workspace: &Path,
    file_path: &Path,
    request: &LspQuery,
    timeout: Duration,
) -> Result<Value, String> {
    match request.operation {
        LspOperation::Status => Ok(json!(status_from_config(config, workspace))),
        LspOperation::Diagnostics => {
            rpc.collect_diagnostics(file_path, timeout);
            Ok(json!(rpc.diagnostics))
        }
        LspOperation::GoToDefinition => {
            rpc.position_request("textDocument/definition", file_path, request, timeout)
        }
        LspOperation::FindReferences => rpc.position_request_with_extra(
            "textDocument/references",
            file_path,
            request,
            json!({"context": {"includeDeclaration": true}}),
            timeout,
        ),
        LspOperation::Hover => {
            rpc.position_request("textDocument/hover", file_path, request, timeout)
        }
        LspOperation::DocumentSymbol => rpc.request(
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": file_uri(file_path)}}),
            timeout,
        ),
        LspOperation::WorkspaceSymbol => rpc.request(
            "workspace/symbol",
            json!({"query": request.query.clone().unwrap_or_default()}),
            timeout,
        ),
        LspOperation::GoToImplementation => {
            rpc.position_request("textDocument/implementation", file_path, request, timeout)
        }
        LspOperation::PrepareCallHierarchy => rpc.position_request(
            "textDocument/prepareCallHierarchy",
            file_path,
            request,
            timeout,
        ),
        LspOperation::IncomingCalls => {
            rpc.call_hierarchy("callHierarchy/incomingCalls", file_path, request, timeout)
        }
        LspOperation::OutgoingCalls => {
            rpc.call_hierarchy("callHierarchy/outgoingCalls", file_path, request, timeout)
        }
    }
}

fn apply_running_status(status: &mut [LspStatus], workspace: &Path) -> Result<(), String> {
    let mut pool = client_pool()
        .lock()
        .map_err(|_| "LSP client pool lock is poisoned".to_string())?;
    for server in status {
        if let Some(root) = pool.running_root(workspace, &server.id) {
            server.running = true;
            server.root = path_to_string(&root);
            continue;
        }
        if let Some((root, reason)) = pool.broken_status(workspace, &server.id) {
            server.available = false;
            server.root = path_to_string(&root);
            server.reason = Some(format!("startup failed: {reason}"));
        }
    }
    Ok(())
}

fn shutdown_pooled_client_by_key(key: &str) -> bool {
    let Ok(mut pool) = client_pool().lock() else {
        return false;
    };
    let Some(mut client) = pool.remove_key(key) else {
        return false;
    };
    let _ = client.connection.shutdown(Duration::from_millis(500));
    true
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
            exclude_root_markers: Vec::new(),
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
        if let Some(exclude_root_markers) =
            raw.exclude_root_markers.or(raw.exclude_root_markers_camel)
        {
            server.exclude_root_markers = exclude_root_markers;
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
            let available = command_available_for_root(program, workspace);
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
    let selectors = file_selectors(file_path);
    let mut selected: Option<(u32, usize, String, SelectedServer)> = None;
    for server in config.servers.values() {
        if !server.extensions.is_empty()
            && !server
                .extensions
                .iter()
                .any(|ext| selectors.iter().any(|selector| selector == ext))
        {
            continue;
        }
        let Some(root) = nearest_root(file_path, workspace, server) else {
            continue;
        };
        let priority = server_priority(server);
        let depth = root.components().count();
        let candidate = SelectedServer {
            server: server.clone(),
            root,
        };
        let replace = selected
            .as_ref()
            .is_none_or(|(best_priority, best_depth, best_id, _)| {
                priority < *best_priority
                    || (priority == *best_priority
                        && (depth > *best_depth || (depth == *best_depth && server.id < *best_id)))
            });
        if replace {
            selected = Some((priority, depth, server.id.clone(), candidate));
        }
    }
    selected
        .map(|(_, _, _, selected)| selected)
        .ok_or_else(|| format!("No LSP server configured for {}", file_path.display()))
}

fn file_selectors(file_path: &Path) -> Vec<String> {
    let mut selectors = Vec::new();
    if let Some(ext) = file_path.extension().and_then(OsStr::to_str) {
        selectors.push(format!(".{ext}"));
    }
    if let Some(name) = file_path.file_name().and_then(OsStr::to_str) {
        selectors.push(name.to_string());
    }
    selectors
}

fn server_priority(server: &LspServerConfig) -> u32 {
    if server.source == "config" {
        return 0;
    }
    match server.id.as_str() {
        "deno" => 0,
        "rust-analyzer" | "typescript" | "vue" | "pyright" | "gopls" | "clangd" | "ruby-lsp"
        | "elixir-ls" | "zls" | "yaml-ls" | "lua-ls" | "prisma" | "dart" | "ocaml-lsp" | "bash"
        | "terraform" | "dockerfile" => 10,
        "pylsp" | "ty" => 20,
        "biome" | "oxlint" => 50,
        _ => 100,
    }
}

fn nearest_root(file_path: &Path, workspace: &Path, server: &LspServerConfig) -> Option<PathBuf> {
    if !server.exclude_root_markers.is_empty()
        && nearest_marker(file_path, workspace, &server.exclude_root_markers).is_some()
    {
        return None;
    }
    nearest_marker(file_path, workspace, &server.root_markers)
        .or_else(|| (!server.strict_root).then(|| workspace.to_path_buf()))
}

fn nearest_marker(file_path: &Path, workspace: &Path, markers: &[String]) -> Option<PathBuf> {
    if markers.is_empty() {
        return None;
    }
    let mut current = file_path.parent().unwrap_or(workspace).to_path_buf();
    loop {
        if markers.iter().any(|marker| current.join(marker).exists()) {
            return Some(current);
        }
        if current == workspace {
            break;
        }
        if !current.pop() {
            break;
        }
    }
    None
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
        )
        .excluding(&["deno.json", "deno.jsonc"]),
        builtin_server(
            "vue",
            &["vue-language-server", "--stdio"],
            &[".vue"],
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
            "biome",
            &["biome", "lsp-proxy", "--stdio"],
            &[
                ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".json", ".jsonc",
                ".vue", ".astro", ".svelte", ".css", ".graphql", ".gql", ".html",
            ],
            &[
                "biome.json",
                "biome.jsonc",
                "package-lock.json",
                "bun.lockb",
                "bun.lock",
                "pnpm-lock.yaml",
                "yarn.lock",
            ],
            false,
        ),
        builtin_server(
            "oxlint",
            &["oxlint", "--lsp"],
            &[
                ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".vue", ".astro",
                ".svelte",
            ],
            &[
                ".oxlintrc.json",
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
        builtin_server(
            "ty",
            &["ty", "server"],
            &[".py", ".pyi"],
            &[
                "pyproject.toml",
                "ty.toml",
                "setup.py",
                "setup.cfg",
                "requirements.txt",
                "Pipfile",
                "pyrightconfig.json",
            ],
            false,
        ),
        builtin_server("gopls", &["gopls"], &[".go"], &["go.mod", "go.work"], false),
        builtin_server(
            "ruby-lsp",
            &["ruby-lsp"],
            &[".rb"],
            &["Gemfile", ".ruby-version"],
            false,
        ),
        builtin_server(
            "clangd",
            &["clangd", "--background-index"],
            &[
                ".c", ".cc", ".cpp", ".cxx", ".c++", ".h", ".hpp", ".hh", ".hxx", ".h++",
            ],
            &["compile_commands.json", "compile_flags.txt", ".clangd"],
            false,
        ),
        builtin_server(
            "elixir-ls",
            &["elixir-ls"],
            &[".ex", ".exs"],
            &["mix.exs", "mix.lock"],
            false,
        ),
        builtin_server("zls", &["zls"], &[".zig", ".zon"], &["build.zig"], false),
        builtin_server(
            "yaml-ls",
            &["yaml-language-server", "--stdio"],
            &[".yaml", ".yml"],
            &[
                "package-lock.json",
                "bun.lockb",
                "bun.lock",
                "pnpm-lock.yaml",
                "yarn.lock",
            ],
            false,
        ),
        builtin_server(
            "lua-ls",
            &["lua-language-server"],
            &[".lua"],
            &[
                ".luarc.json",
                ".luarc.jsonc",
                ".luacheckrc",
                ".stylua.toml",
                "stylua.toml",
                "selene.toml",
                "selene.yml",
            ],
            false,
        ),
        builtin_server(
            "prisma",
            &["prisma", "language-server"],
            &[".prisma"],
            &["schema.prisma", "prisma/schema.prisma", "prisma"],
            false,
        )
        .excluding(&["package.json"]),
        builtin_server(
            "dart",
            &["dart", "language-server", "--lsp"],
            &[".dart"],
            &["pubspec.yaml", "analysis_options.yaml"],
            false,
        ),
        builtin_server(
            "ocaml-lsp",
            &["ocamllsp"],
            &[".ml", ".mli"],
            &["dune-project", "dune-workspace", ".merlin", "opam"],
            false,
        ),
        builtin_server(
            "bash",
            &["bash-language-server", "start"],
            &[".sh", ".bash", ".zsh", ".ksh"],
            &[],
            false,
        ),
        builtin_server(
            "terraform",
            &["terraform-ls", "serve"],
            &[".tf", ".tfvars"],
            &[".terraform.lock.hcl", "terraform.tfstate"],
            false,
        ),
        builtin_server(
            "dockerfile",
            &["docker-langserver", "--stdio"],
            &[".dockerfile", "Dockerfile"],
            &[],
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
        exclude_root_markers: Vec::new(),
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
        let program_path =
            resolve_command_for_root(program, root).unwrap_or_else(|| PathBuf::from(program));
        let mut command = Command::new(program_path);
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
            capabilities: Value::Null,
            diagnostics: BTreeMap::new(),
            push_diagnostics: BTreeMap::new(),
            pull_diagnostics: BTreeMap::new(),
            diagnostic_registrations: BTreeMap::new(),
            documents: BTreeMap::new(),
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
        let initialized = self.request("initialize", Value::Object(params), timeout)?;
        self.capabilities = initialized
            .get("capabilities")
            .cloned()
            .unwrap_or(Value::Null);
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
        let path_key = path_to_string(file_path);
        if let Some(document) = self.documents.get(&path_key).cloned() {
            let next_version = document.version.saturating_add(1);
            self.notify(
                "workspace/didChangeWatchedFiles",
                json!({
                    "changes": [{
                        "uri": file_uri(file_path),
                        "type": 2
                    }]
                }),
            )?;
            self.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": {
                        "uri": file_uri(file_path),
                        "version": next_version,
                    },
                    "contentChanges": [{
                        "text": text,
                    }]
                }),
            )?;
            self.documents.insert(
                path_key,
                OpenDocument {
                    version: next_version,
                },
            );
            self.collect_for(Duration::from_millis(100).min(timeout));
            return Ok(());
        }
        self.notify(
            "workspace/didChangeWatchedFiles",
            json!({
                "changes": [{
                    "uri": file_uri(file_path),
                    "type": 1
                }]
            }),
        )?;
        self.diagnostics.remove(&path_key);
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
        self.documents.insert(path_key, OpenDocument { version: 0 });
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
        let timeout = timeout.min(Duration::from_millis(3_000));
        let mut results = Vec::new();
        for identifier in self.document_diagnostic_identifiers() {
            if let Some(result) =
                self.request_document_diagnostic_report(file_path, identifier.as_deref(), timeout)
            {
                results.push(result);
            }
        }
        for identifier in self.workspace_diagnostic_identifiers() {
            if let Some(result) =
                self.request_workspace_diagnostic_report(file_path, identifier.as_deref(), timeout)
            {
                results.push(result);
            }
        }
        self.merge_pull_diagnostic_results(file_path, results);
        self.collect_for(Duration::from_millis(DEFAULT_DIAGNOSTIC_WAIT_MS));
    }

    fn document_diagnostic_identifiers(&self) -> Vec<Option<String>> {
        let document_registrations = self
            .diagnostic_registrations
            .values()
            .filter(|registration| !registration.workspace_diagnostics)
            .collect::<Vec<_>>();
        let mut identifiers = Vec::new();
        if self.has_static_pull_diagnostics()
            || document_registrations.is_empty()
            || document_registrations
                .iter()
                .any(|registration| registration.identifier.is_some())
        {
            identifiers.push(None);
        }
        for registration in document_registrations {
            push_unique_identifier(&mut identifiers, registration.identifier.clone());
        }
        if identifiers.is_empty() {
            identifiers.push(None);
        }
        identifiers
    }

    fn workspace_diagnostic_identifiers(&self) -> Vec<Option<String>> {
        let workspace_registrations = self
            .diagnostic_registrations
            .values()
            .filter(|registration| registration.workspace_diagnostics)
            .collect::<Vec<_>>();
        if workspace_registrations.is_empty() {
            return Vec::new();
        }
        let mut identifiers = Vec::new();
        for registration in workspace_registrations {
            push_unique_identifier(&mut identifiers, registration.identifier.clone());
        }
        if identifiers.is_empty() {
            identifiers.push(None);
        }
        identifiers
    }

    fn has_static_pull_diagnostics(&self) -> bool {
        !self
            .capabilities
            .get("diagnosticProvider")
            .is_none_or(Value::is_null)
    }

    fn request_document_diagnostic_report(
        &mut self,
        file_path: &Path,
        identifier: Option<&str>,
        timeout: Duration,
    ) -> Option<DiagnosticRequestResult> {
        let mut params = Map::new();
        if let Some(identifier) = identifier {
            params.insert("identifier".to_string(), json!(identifier));
        }
        params.insert(
            "textDocument".to_string(),
            json!({"uri": file_uri(file_path)}),
        );
        let report = self
            .request("textDocument/diagnostic", Value::Object(params), timeout)
            .ok()?;
        let mut result = DiagnosticRequestResult::default();
        let file_key = path_to_string(file_path);
        if let Some(items) = report.get("items").and_then(Value::as_array) {
            result.handled = true;
            result.matched = true;
            result
                .by_file
                .entry(file_key.clone())
                .or_default()
                .extend(items.clone());
        }
        if let Some(related) = report.get("relatedDocuments").and_then(Value::as_object) {
            for (uri, document_report) in related {
                let Some(path) = self.path_key_from_uri(uri) else {
                    continue;
                };
                let Some(items) = document_report.get("items").and_then(Value::as_array) else {
                    continue;
                };
                result.handled = true;
                result.matched = result.matched || path == file_key;
                result
                    .by_file
                    .entry(path)
                    .or_default()
                    .extend(items.clone());
            }
        }
        result.handled.then_some(result)
    }

    fn request_workspace_diagnostic_report(
        &mut self,
        file_path: &Path,
        identifier: Option<&str>,
        timeout: Duration,
    ) -> Option<DiagnosticRequestResult> {
        let mut params = Map::new();
        if let Some(identifier) = identifier {
            params.insert("identifier".to_string(), json!(identifier));
        }
        params.insert("previousResultIds".to_string(), json!([]));
        let report = self
            .request("workspace/diagnostic", Value::Object(params), timeout)
            .ok()?;
        let file_key = path_to_string(file_path);
        let mut result = DiagnosticRequestResult {
            handled: true,
            ..DiagnosticRequestResult::default()
        };
        for item in report
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(uri) = item.get("uri").and_then(Value::as_str) else {
                continue;
            };
            let Some(path) = self.path_key_from_uri(uri) else {
                continue;
            };
            let Some(items) = item.get("items").and_then(Value::as_array) else {
                continue;
            };
            result.matched = result.matched || path == file_key;
            result
                .by_file
                .entry(path)
                .or_default()
                .extend(items.clone());
        }
        Some(result)
    }

    fn merge_pull_diagnostic_results(
        &mut self,
        file_path: &Path,
        results: Vec<DiagnosticRequestResult>,
    ) {
        if results.is_empty() || !results.iter().any(|result| result.handled) {
            return;
        }
        let file_key = path_to_string(file_path);
        let matched = results.iter().any(|result| result.matched);
        let mut merged: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for result in results {
            for (path, items) in result.by_file {
                merged.entry(path).or_default().extend(items);
            }
        }
        if matched {
            merged.entry(file_key).or_default();
        }
        for (path, items) in merged {
            self.update_pull_diagnostics(path, items);
        }
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
                if method == "client/registerCapability" {
                    self.record_capability_registrations(&params);
                } else if method == "client/unregisterCapability" {
                    self.remove_capability_registrations(&params);
                }
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
        let Some(path) = self.path_key_from_uri(uri) else {
            return;
        };
        let diagnostics = params
            .get("diagnostics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        self.update_push_diagnostics(path, diagnostics);
    }

    fn update_push_diagnostics(&mut self, path: String, diagnostics: Vec<Value>) {
        self.push_diagnostics.insert(path.clone(), diagnostics);
        self.refresh_merged_diagnostics(&path);
    }

    fn update_pull_diagnostics(&mut self, path: String, diagnostics: Vec<Value>) {
        self.pull_diagnostics
            .insert(path.clone(), dedupe_diagnostics(diagnostics));
        self.refresh_merged_diagnostics(&path);
    }

    fn refresh_merged_diagnostics(&mut self, path: &str) {
        let mut merged = Vec::new();
        if let Some(items) = self.push_diagnostics.get(path) {
            merged.extend(items.clone());
        }
        if let Some(items) = self.pull_diagnostics.get(path) {
            merged.extend(items.clone());
        }
        self.diagnostics
            .insert(path.to_string(), dedupe_diagnostics(merged));
    }

    fn record_capability_registrations(&mut self, params: &Value) {
        let Some(registrations) = params.get("registrations").and_then(Value::as_array) else {
            return;
        };
        for registration in registrations {
            if registration.get("method").and_then(Value::as_str) != Some("textDocument/diagnostic")
            {
                continue;
            }
            let Some(id) = registration.get("id").and_then(Value::as_str) else {
                continue;
            };
            let options = registration
                .get("registerOptions")
                .and_then(Value::as_object);
            self.diagnostic_registrations.insert(
                id.to_string(),
                DiagnosticRegistration {
                    identifier: options
                        .and_then(|options| options.get("identifier"))
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    workspace_diagnostics: options
                        .and_then(|options| options.get("workspaceDiagnostics"))
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                },
            );
        }
    }

    fn remove_capability_registrations(&mut self, params: &Value) {
        let registrations = params
            .get("unregisterations")
            .or_else(|| params.get("unregistrations"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for registration in registrations {
            if registration.get("method").and_then(Value::as_str) != Some("textDocument/diagnostic")
            {
                continue;
            }
            if let Some(id) = registration.get("id").and_then(Value::as_str) {
                self.diagnostic_registrations.remove(id);
            }
        }
    }

    fn path_key_from_uri(&self, uri: &str) -> Option<String> {
        let raw_path = file_path_from_uri(uri)?;
        let normalized = normalize_path(Path::new(&raw_path));
        let root = normalize_path(&self.root);
        if normalized.starts_with(&root) {
            return Some(path_to_string(&normalized));
        }
        let root_canonical = fs::canonicalize(&root)
            .ok()
            .map(|path| normalize_path(&path));
        let path_canonical = fs::canonicalize(&normalized)
            .ok()
            .map(|path| normalize_path(&path));
        if let (Some(root_canonical), Some(path_canonical)) = (root_canonical, path_canonical)
            && let Ok(relative) = path_canonical.strip_prefix(&root_canonical)
        {
            return Some(path_to_string(&normalize_path(&root.join(relative))));
        }
        Some(path_to_string(&normalized))
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

fn push_unique_identifier(target: &mut Vec<Option<String>>, identifier: Option<String>) {
    if !target.iter().any(|existing| existing == &identifier) {
        target.push(identifier);
    }
}

fn dedupe_diagnostics(items: Vec<Value>) -> Vec<Value> {
    let mut seen = BTreeMap::new();
    let mut result = Vec::new();
    for item in items {
        let key = serde_json::to_string(&json!({
            "code": item.get("code").cloned().unwrap_or(Value::Null),
            "severity": item.get("severity").cloned().unwrap_or(Value::Null),
            "message": item.get("message").cloned().unwrap_or(Value::Null),
            "source": item.get("source").cloned().unwrap_or(Value::Null),
            "range": item.get("range").cloned().unwrap_or(Value::Null),
        }))
        .unwrap_or_else(|_| item.to_string());
        if seen.insert(key, ()).is_none() {
            result.push(item);
        }
    }
    result
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

fn command_available_for_root(command: &str, root: &Path) -> bool {
    resolve_command_for_root(command, root).is_some()
}

fn resolve_command_for_root(command: &str, root: &Path) -> Option<PathBuf> {
    if command.trim().is_empty() {
        return None;
    }
    let path = Path::new(command);
    if path.components().count() > 1 || path.is_absolute() {
        return path.exists().then(|| path.to_path_buf());
    }
    local_bin_command(command, root).or_else(|| path_command(command))
}

fn local_bin_command(command: &str, root: &Path) -> Option<PathBuf> {
    let mut current = normalize_path(root);
    loop {
        let bin = current.join("node_modules").join(".bin");
        for candidate in command_name_candidates(command) {
            let path = bin.join(candidate);
            if path.exists() {
                return Some(path);
            }
        }
        if !current.pop() {
            break;
        }
    }
    None
}

fn path_command(command: &str) -> Option<PathBuf> {
    env::var_os("PATH").and_then(|paths| {
        env::split_paths(&paths)
            .flat_map(|path| {
                command_name_candidates(command)
                    .into_iter()
                    .map(move |name| path.join(name))
            })
            .find(|path| path.exists())
    })
}

fn command_name_candidates(command: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    candidates.push(command.to_string());
    #[cfg(windows)]
    {
        if !command.ends_with(".cmd") {
            candidates.push(format!("{command}.cmd"));
        }
        if !command.ends_with(".exe") {
            candidates.push(format!("{command}.exe"));
        }
    }
    candidates
}

fn broken_cooldown() -> Duration {
    Duration::from_millis(
        env::var("OPENAGENT_LSP_BROKEN_COOLDOWN_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_BROKEN_COOLDOWN_MS),
    )
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
        assert!(config.servers.contains_key("vue"));
        assert!(config.servers.contains_key("biome"));
        assert!(config.servers.contains_key("yaml-ls"));
        assert_eq!(
            config.servers["typescript"].exclude_root_markers,
            vec!["deno.json", "deno.jsonc"]
        );
        let _ = fs::remove_dir_all(temp);
        Ok(())
    }

    #[test]
    fn config_parses_exclude_root_markers() -> Result<(), String> {
        let mut config = LspConfig {
            enabled: true,
            servers: BTreeMap::new(),
            config_path: None,
        };
        merge_lsp_value(
            &mut config,
            &json!({
                "servers": {
                    "fake": {
                        "command": ["fake-lsp"],
                        "extensions": [".ts"],
                        "rootMarkers": ["package.json"],
                        "excludeRootMarkers": ["deno.json"]
                    }
                }
            }),
        )?;
        let fake = config.servers.get("fake").ok_or("missing fake server")?;
        assert_eq!(fake.root_markers, vec!["package.json"]);
        assert_eq!(fake.exclude_root_markers, vec!["deno.json"]);
        Ok(())
    }

    #[test]
    fn deno_root_marker_excludes_typescript_server() -> Result<(), String> {
        let temp = unique_temp_dir("openagent-lsp-deno-root")?;
        fs::write(temp.join("package.json"), "{}").map_err(|error| error.to_string())?;
        fs::write(temp.join("deno.json"), "{}").map_err(|error| error.to_string())?;
        fs::create_dir_all(temp.join("src")).map_err(|error| error.to_string())?;
        fs::write(temp.join("src/main.ts"), "export const value = 1;\n")
            .map_err(|error| error.to_string())?;

        let config = load_workspace_config(&temp)?;
        let selected = select_server(&config, &temp, &temp.join("src/main.ts"))?;
        assert_eq!(selected.server.id, "deno");
        assert_eq!(selected.root, temp);
        let _ = fs::remove_dir_all(selected.root);
        Ok(())
    }

    #[test]
    fn status_detects_workspace_local_node_bin() -> Result<(), String> {
        let temp = unique_temp_dir("openagent-lsp-local-bin")?;
        fs::create_dir_all(temp.join("node_modules/.bin")).map_err(|error| error.to_string())?;
        fs::write(temp.join("node_modules/.bin/fake-lsp"), "#!/bin/sh\n")
            .map_err(|error| error.to_string())?;
        fs::create_dir_all(temp.join(".openagent")).map_err(|error| error.to_string())?;
        fs::write(
            temp.join(".openagent/lsp.json"),
            serde_json::to_string_pretty(&json!({
                "servers": {
                    "fake": {
                        "command": ["fake-lsp"],
                        "extensions": [".txt"]
                    }
                }
            }))
            .map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let status = lsp_status(&temp)?;
        let fake = status
            .iter()
            .find(|server| server.id == "fake")
            .ok_or("missing fake status")?;
        assert!(fake.available);
        let _ = fs::remove_dir_all(temp);
        Ok(())
    }

    #[test]
    fn dockerfile_builtin_matches_file_name_without_extension() -> Result<(), String> {
        let temp = unique_temp_dir("openagent-lsp-dockerfile")?;
        fs::write(temp.join("Dockerfile"), "FROM scratch\n").map_err(|error| error.to_string())?;
        let config = load_workspace_config(&temp)?;
        let selected = select_server(&config, &temp, &temp.join("Dockerfile"))?;
        assert_eq!(selected.server.id, "dockerfile");
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
