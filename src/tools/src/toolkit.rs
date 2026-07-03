//! Workspace runtime, tool registry, and built-in tools for the Rust rewrite.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    process::Command,
    time::{Duration, SystemTime},
};

use openagent_core::{
    PermissionManager, SkillDiscoveryReport, SkillRegistry, pattern_for, render_skill_document,
};
use openagent_protocol::{
    PermissionAction, PermissionRuleset, ToolConcurrency, ToolExecutionSchema, ToolExecutionScope,
    ToolResult, ToolSchema,
};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub const CRATE_NAME: &str = env!("CARGO_PKG_NAME");
pub const TASK_TOOL_ID: &str = "task";
pub const WEB_FETCH_TOOL_ID: &str = "web_fetch";

const DEFAULT_READ_LIMIT: usize = 2000;
const MAX_LINE_LENGTH: usize = 2000;
const MAX_READ_BYTES: usize = 50 * 1024;
const DEFAULT_TOOL_MAX_LINES: usize = 2000;
const DEFAULT_TOOL_MAX_BYTES: usize = 50 * 1024;
const GLOB_LIMIT: usize = 100;
const GREP_LIMIT: usize = 100;
const LS_LIMIT: usize = 100;
const CODE_SEARCH_MAX_HITS: usize = 200;
const CODE_SEARCH_MAX_PREVIEW_HITS: usize = 20;
const CODE_SEARCH_MAX_LINE_CHARS: usize = 240;
const WEB_FETCH_DEFAULT_TIMEOUT_MS: u64 = 10_000;
const WEB_FETCH_MAX_BYTES: usize = 128 * 1024;
const BENCHMARK_MODE_ENV: &str = "OPENAGENT_BENCHMARK_MODE";

const DEFAULT_LS_IGNORE: &[&str] = &[
    "node_modules/",
    "__pycache__/",
    ".git/",
    "dist/",
    "build/",
    "target/",
    "vendor/",
    ".idea/",
    ".vscode/",
    ".venv/",
    "venv/",
    "env/",
    "coverage/",
];

const BINARY_EXTENSIONS: &[&str] = &[
    ".zip", ".tar", ".gz", ".exe", ".dll", ".so", ".class", ".jar", ".war", ".7z", ".doc", ".docx",
    ".xls", ".xlsx", ".ppt", ".pptx", ".odt", ".ods", ".odp", ".bin", ".dat", ".obj", ".o", ".a",
    ".lib", ".wasm", ".pyc", ".pyo", ".pdf", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".ico",
];

type ToolResultValue<T> = Result<T, String>;

#[must_use]
pub const fn crate_name() -> &'static str {
    CRATE_NAME
}

#[must_use]
pub fn protocol_crate_name() -> &'static str {
    openagent_protocol::crate_name()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub id: String,
    pub description: String,
    pub parameter_schema: Value,
    pub dangerous: bool,
    pub group: String,
    pub execution_scope: ToolExecutionScope,
    pub execution_schema: ToolExecutionSchema,
}

impl ToolDefinition {
    #[must_use]
    pub fn tool_schema(&self) -> ToolSchema {
        ToolSchema {
            name: self.id.clone(),
            description: self.description.clone(),
            schema: Some(self.parameter_schema.clone()),
            group: self.group.clone(),
            dangerous: self.dangerous,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, ToolDefinition>,
}

impl ToolRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn scoped(&mut self, namespace: impl Into<String>) -> ScopedToolRegistry<'_> {
        ScopedToolRegistry {
            registry: self,
            namespace: namespace.into(),
        }
    }

    pub fn register(&mut self, tool: ToolDefinition) {
        self.tools.insert(tool.id.clone(), tool);
    }

    #[must_use]
    pub fn get(&self, tool_id: &str) -> Option<&ToolDefinition> {
        self.tools.get(tool_id)
    }

    #[must_use]
    pub fn all(&self) -> Vec<&ToolDefinition> {
        self.tools.values().collect()
    }

    pub fn clear(&mut self) {
        self.tools.clear();
    }

    #[must_use]
    pub fn tool_schemas(&self, execution_mode: &str) -> Vec<ToolSchema> {
        self.tools
            .values()
            .filter(|tool| tool_available(tool, execution_mode))
            .map(ToolDefinition::tool_schema)
            .collect()
    }
}

pub struct ScopedToolRegistry<'a> {
    registry: &'a mut ToolRegistry,
    namespace: String,
}

impl ScopedToolRegistry<'_> {
    pub fn register(&mut self, mut tool: ToolDefinition) {
        tool.id = qualify_tool_id(&self.namespace, &tool.id);
        self.registry.register(tool);
    }
}

#[must_use]
pub fn qualify_tool_id(namespace: &str, tool_id: &str) -> String {
    if tool_id == "default" {
        namespace.to_string()
    } else {
        format!("{namespace}_{tool_id}")
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct TaskSubagentDescriptor {
    pub id: String,
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TaskSubagentRoute {
    pub subagent_id: String,
    pub score: u64,
    pub matched_terms: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TaskPermissionRule {
    pub pattern: String,
    pub action: PermissionAction,
}

#[must_use]
pub fn task_subagent_permission_action(
    rules: &[TaskPermissionRule],
    subagent_id: &str,
) -> Option<PermissionAction> {
    let mut matched = None;
    for rule in rules {
        if task_permission_pattern_matches(&rule.pattern, subagent_id) {
            matched = Some(rule.action.clone());
        }
    }
    matched
}

#[must_use]
pub fn task_subagent_is_visible(rules: &[TaskPermissionRule], subagent_id: &str) -> bool {
    !matches!(
        task_subagent_permission_action(rules, subagent_id),
        Some(PermissionAction::Deny)
    )
}

fn task_permission_pattern_matches(pattern: &str, value: &str) -> bool {
    let pattern = pattern.trim();
    if pattern == "*" || pattern == value {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return value.ends_with(suffix);
    }
    if let Some((prefix, suffix)) = pattern.split_once('*') {
        return value.starts_with(prefix) && value.ends_with(suffix);
    }
    false
}

#[must_use]
pub fn task_tool_description(subagents: &[TaskSubagentDescriptor]) -> String {
    let mut description = [
        "Launch a subagent in an independent context window for complex, multi-step work.",
        "Use this when the task is best handled by a specialized agent whose intermediate tool calls should not enter the parent context.",
        "Pass a compact description, the full prompt for the subagent, and subagent_type.",
        "Do not use this for simple one-shot file reads or searches that local tools can handle directly.",
        "The parent agent must summarize the returned result for the user when needed.",
    ]
    .join("\n");
    if subagents.is_empty() {
        description.push_str("\n\nAvailable subagents: none.");
    } else {
        description.push_str("\n\nAvailable subagents:");
        for agent in subagents {
            let summary = if agent.description.trim().is_empty() {
                "No description provided."
            } else {
                agent.description.trim()
            };
            description.push_str(&format!("\n- {}: {}", agent.id, summary));
        }
    }
    description
}

#[must_use]
pub fn task_tool_definition(subagents: &[TaskSubagentDescriptor]) -> ToolDefinition {
    ToolDefinition {
        id: TASK_TOOL_ID.to_string(),
        description: task_tool_description(subagents),
        parameter_schema: json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "description": {
                    "type": "string",
                    "description": "A short title for the delegated task."
                },
                "prompt": {
                    "type": "string",
                    "description": "The complete instructions for the subagent."
                },
                "subagent_type": {
                    "type": "string",
                    "description": "The subagent id to run."
                },
                "task_id": {
                    "type": "string",
                    "description": "Optional existing subagent session id to resume."
                },
                "command": {
                    "type": "string",
                    "description": "Optional command label for the subagent task."
                },
                "background": {
                    "type": "boolean",
                    "description": "Reserved for background execution; current runtime executes foreground tasks."
                },
                "isolate_workspace": {
                    "type": "boolean",
                    "description": "Run the subagent in an isolated copy of the parent workspace."
                }
            },
            "required": ["description", "prompt", "subagent_type"]
        }),
        dangerous: true,
        group: "agent".to_string(),
        execution_scope: ToolExecutionScope::Agnostic,
        execution_schema: exclusive_schema(
            "agent",
            false,
            true,
            false,
            false,
            false,
            Some("task:{subagent_type}"),
        ),
    }
}

pub fn register_task_tool(registry: &mut ToolRegistry, subagents: &[TaskSubagentDescriptor]) {
    registry.register(task_tool_definition(subagents));
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkspaceIsolation {
    pub enabled: bool,
    pub method: String,
    pub source_workspace: String,
    pub workspace: String,
}

pub fn prepare_isolated_workspace(
    source_workspace: impl AsRef<Path>,
    isolation_root: impl AsRef<Path>,
    task_id: &str,
) -> ToolResultValue<WorkspaceIsolation> {
    let source = root_path(source_workspace.as_ref());
    if !source.is_dir() {
        return Err(format!(
            "workspace isolation requires a directory source: {}",
            source.display()
        ));
    }
    let root_raw = isolation_root
        .as_ref()
        .join(sanitize_isolation_segment(task_id));
    if root_raw.exists() {
        fs::remove_dir_all(&root_raw).map_err(io_error)?;
    }
    fs::create_dir_all(&root_raw).map_err(io_error)?;
    let root = root_path(&root_raw);
    copy_workspace_dir(&source, &root, &root)?;
    Ok(WorkspaceIsolation {
        enabled: true,
        method: "directory_copy".to_string(),
        source_workspace: path_to_string(&source),
        workspace: path_to_string(&root),
    })
}

#[must_use]
pub fn select_task_subagent_for_prompt(
    subagents: &[TaskSubagentDescriptor],
    prompt: &str,
) -> Option<TaskSubagentRoute> {
    let prompt_terms = route_terms(prompt);
    if prompt_terms.is_empty() {
        return None;
    }
    let normalized_prompt = route_normalized_text(prompt);
    let mut scored = subagents
        .iter()
        .filter_map(|agent| {
            let route_text = format!("{} {} {}", agent.id, agent.name, agent.description);
            let route_terms = route_terms(&route_text);
            let mut matched_terms = prompt_terms
                .intersection(&route_terms)
                .cloned()
                .collect::<Vec<_>>();
            matched_terms.sort();
            let mut score = matched_terms.len() as u64 * 2;
            if route_contains_phrase(&normalized_prompt, &agent.id)
                || route_contains_phrase(&normalized_prompt, &agent.name)
            {
                score = score.saturating_add(6);
            }
            if score == 0 {
                return None;
            }
            Some(TaskSubagentRoute {
                subagent_id: agent.id.clone(),
                score,
                matched_terms,
            })
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then_with(|| left.subagent_id.cmp(&right.subagent_id))
    });
    let best = scored.first()?;
    let second_score = scored.get(1).map(|route| route.score).unwrap_or_default();
    if best.score >= 4 && best.score >= second_score.saturating_add(2) {
        Some(best.clone())
    } else {
        None
    }
}

fn route_terms(text: &str) -> BTreeSet<String> {
    const STOP_WORDS: &[&str] = &[
        "the", "and", "for", "with", "this", "that", "from", "into", "about", "your", "agent",
        "subagent", "task", "please", "need", "needs", "using", "use", "can", "you", "are",
    ];
    route_normalized_text(text)
        .split_whitespace()
        .map(route_term_alias)
        .filter(|term| term.len() >= 3 && !STOP_WORDS.contains(&term.as_str()))
        .collect()
}

fn route_normalized_text(text: &str) -> String {
    text.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect()
}

fn route_term_alias(term: &str) -> String {
    match term {
        "doc" | "docs" | "documentation" => "documentation".to_string(),
        "dependencies" => "dependency".to_string(),
        "researching" | "researched" => "research".to_string(),
        "debugging" | "debugged" => "debug".to_string(),
        "reviews" | "reviewing" => "review".to_string(),
        value => value.to_string(),
    }
}

fn route_contains_phrase(normalized_text: &str, phrase: &str) -> bool {
    let phrase = route_normalized_text(phrase);
    let phrase = phrase.split_whitespace().collect::<Vec<_>>().join(" ");
    if phrase.is_empty() {
        return false;
    }
    format!(" {normalized_text} ").contains(&format!(" {phrase} "))
}

fn sanitize_isolation_segment(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "task".to_string()
    } else {
        sanitized
    }
}

fn copy_workspace_dir(
    source: &Path,
    destination: &Path,
    isolation_target: &Path,
) -> ToolResultValue<()> {
    for entry in fs::read_dir(source).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let source_path = entry.path();
        let name = entry.file_name();
        if should_skip_isolation_entry(&name) || isolation_target.starts_with(&source_path) {
            continue;
        }
        let destination_path = destination.join(&name);
        let metadata = entry.file_type().map_err(io_error)?;
        if metadata.is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            fs::create_dir_all(&destination_path).map_err(io_error)?;
            copy_workspace_dir(&source_path, &destination_path, isolation_target)?;
        } else if metadata.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent).map_err(io_error)?;
            }
            fs::copy(&source_path, &destination_path).map_err(io_error)?;
        }
    }
    Ok(())
}

fn should_skip_isolation_entry(name: &OsStr) -> bool {
    matches!(
        name.to_str().unwrap_or_default(),
        ".git" | "target" | "node_modules" | ".venv" | "venv" | "__pycache__"
    )
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct ToolOutput {
    pub title: String,
    pub output: String,
    pub metadata: BTreeMap<String, Value>,
    pub truncated: bool,
    pub error: Option<String>,
}

impl ToolOutput {
    #[must_use]
    pub fn new(title: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            output: output.into(),
            metadata: BTreeMap::new(),
            truncated: false,
            error: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
    pub priority: String,
    pub id: String,
}

impl TodoItem {
    #[must_use]
    pub fn new(
        content: impl Into<String>,
        status: impl Into<String>,
        priority: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self {
            content: content.into(),
            status: status.into(),
            priority: priority.into(),
            id: id.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct ToolContext {
    pub session_id: String,
    pub session_root: PathBuf,
    pub call_id: String,
    pub execution_mode: String,
    pub workspace_root: Option<String>,
    pub execution_metadata: BTreeMap<String, Value>,
    pub agent_options: BTreeMap<String, Value>,
    pub require_read_before_write: bool,
    pub permission_manager: Option<PermissionManager>,
    pub dangerously_skip_permissions: bool,
    pub read_files: BTreeSet<PathBuf>,
    pub memory: BTreeMap<String, Value>,
    pub todos: Vec<TodoItem>,
    pub question_answers: Option<Vec<Vec<String>>>,
}

impl ToolContext {
    #[must_use]
    pub fn new(session_root: impl Into<PathBuf>) -> Self {
        let root = root_path(&session_root.into());
        Self {
            session_id: String::new(),
            session_root: root.clone(),
            call_id: String::new(),
            execution_mode: "local".to_string(),
            workspace_root: Some(root.to_string_lossy().to_string()),
            execution_metadata: BTreeMap::new(),
            agent_options: BTreeMap::new(),
            require_read_before_write: true,
            permission_manager: None,
            dangerously_skip_permissions: false,
            read_files: BTreeSet::new(),
            memory: BTreeMap::new(),
            todos: Vec::new(),
            question_answers: None,
        }
    }

    #[must_use]
    pub fn with_session_id(mut self, session_id: impl Into<String>) -> Self {
        self.session_id = session_id.into();
        self
    }

    #[must_use]
    pub fn with_call_id(mut self, call_id: impl Into<String>) -> Self {
        self.call_id = call_id.into();
        self
    }

    #[must_use]
    pub fn with_read_before_write(mut self, enabled: bool) -> Self {
        self.require_read_before_write = enabled;
        self
    }

    #[must_use]
    pub fn with_permission_manager(mut self, manager: PermissionManager) -> Self {
        self.permission_manager = Some(manager);
        self
    }

    #[must_use]
    pub fn with_permission_ruleset(mut self, ruleset: PermissionRuleset) -> Self {
        let mut manager = PermissionManager::new();
        manager.set_ruleset(ruleset);
        self.permission_manager = Some(manager);
        self
    }

    #[must_use]
    pub fn with_dangerously_skip_permissions(mut self, enabled: bool) -> Self {
        self.dangerously_skip_permissions = enabled;
        self
    }

    pub fn set_question_answers(&mut self, answers: Vec<Vec<String>>) {
        self.question_answers = Some(answers);
    }

    pub fn remember_read(&mut self, target: &Path) {
        self.read_files.insert(normalize_path(target));
    }

    #[must_use]
    pub fn has_read_file(&self, target: &Path) -> bool {
        self.read_files.contains(&normalize_path(target))
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CommandResult {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
    pub cwd: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WorkspaceEntry {
    pub path: String,
    pub is_dir: bool,
    pub mtime: f64,
}

#[derive(Clone, Debug)]
pub struct LocalWorkspaceRuntime {
    pub root: PathBuf,
    pub workspace_root: String,
}

impl LocalWorkspaceRuntime {
    #[must_use]
    pub fn new(root: impl Into<PathBuf>) -> Self {
        let root = root_path(&root.into());
        Self {
            workspace_root: root.to_string_lossy().to_string(),
            root,
        }
    }

    #[must_use]
    pub fn execution_metadata(&self) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("execution_mode".to_string(), json!("local")),
            ("workspace_root".to_string(), json!(self.workspace_root)),
        ])
    }

    #[must_use]
    pub fn display_path(&self, path: impl AsRef<Path>) -> String {
        let target = normalize_path(path.as_ref());
        target
            .strip_prefix(&self.root)
            .map(path_to_string)
            .unwrap_or_else(|_| path_to_string(&target))
    }

    pub fn resolve_path(
        &self,
        path: Option<&str>,
        default_to_root: bool,
    ) -> ToolResultValue<PathBuf> {
        match path {
            Some(raw) => resolve_optional_path(&self.root, Some(raw)),
            None if default_to_root => resolve_optional_path(&self.root, None),
            None => Err("path is required".to_string()),
        }
    }

    pub fn resolve_file_path(&self, path: &str) -> ToolResultValue<PathBuf> {
        resolve_path_in_root(&self.root, path)
    }

    pub fn run_command(
        &self,
        command: &str,
        cwd: Option<&str>,
        _timeout_ms: u64,
    ) -> ToolResultValue<CommandResult> {
        let resolved_cwd = self.resolve_path(cwd, true)?;
        let output = shell_command(command)
            .current_dir(&resolved_cwd)
            .output()
            .map_err(|error| format!("failed to run command: {error}"))?;
        Ok(CommandResult {
            returncode: output.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
            cwd: path_to_string(&resolved_cwd),
        })
    }

    pub fn read_text(&self, path: &str) -> ToolResultValue<String> {
        fs::read_to_string(self.resolve_file_path(path)?).map_err(io_error)
    }

    pub fn write_text(&self, path: &str, content: &str) -> ToolResultValue<()> {
        let target = self.resolve_file_path(path)?;
        write_text(&target, content)
    }

    pub fn edit_text(
        &self,
        path: &str,
        old_string: &str,
        new_string: &str,
        replace_all: bool,
    ) -> ToolResultValue<String> {
        let target = self.resolve_file_path(path)?;
        let text = fs::read_to_string(&target).map_err(io_error)?;
        let new_text = replace_text(&text, old_string, new_string, replace_all)?;
        write_text(&target, &new_text)?;
        Ok(new_text)
    }

    pub fn glob(&self, base_path: Option<&str>, pattern: &str) -> ToolResultValue<Vec<PathBuf>> {
        let base = self.resolve_path(base_path, true)?;
        let mut matches = glob_paths(&self.root, &base, pattern)?;
        matches.sort_by(|left, right| {
            path_mtime(right)
                .total_cmp(&path_mtime(left))
                .then_with(|| path_to_string(left).cmp(&path_to_string(right)))
        });
        matches.truncate(GLOB_LIMIT);
        Ok(matches)
    }

    pub fn grep(
        &self,
        base_path: Option<&str>,
        pattern: &str,
        include_glob: Option<&str>,
    ) -> ToolResultValue<Vec<GrepMatch>> {
        let base = self.resolve_path(base_path, true)?;
        grep_paths(&self.root, &base, pattern, include_glob)
    }

    pub fn ls(
        &self,
        base_path: Option<&str>,
        ignore: &[String],
    ) -> ToolResultValue<Vec<WorkspaceEntry>> {
        let base = self.resolve_path(base_path, true)?;
        let mut entries = Vec::new();
        for path in walk_paths(&base)? {
            if path == base {
                continue;
            }
            let relative = path
                .strip_prefix(&base)
                .map(path_to_string)
                .unwrap_or_else(|_| path_to_string(&path));
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .map(str::to_string)
                .unwrap_or_default();
            if should_ignore(&relative, &name, ignore) {
                continue;
            }
            entries.push(WorkspaceEntry {
                path: path_to_string(&path),
                is_dir: path.is_dir(),
                mtime: path_mtime(&path),
            });
            if entries.len() >= LS_LIMIT {
                break;
            }
        }
        Ok(entries)
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TruncatedResult {
    pub content: String,
    pub truncated: bool,
    pub original_lines: usize,
    pub original_bytes: usize,
    pub output_path: Option<String>,
}

#[must_use]
pub fn truncate_output(
    content: &str,
    max_lines: Option<usize>,
    max_bytes: Option<usize>,
) -> TruncatedResult {
    let resolved_max_lines = max_lines.unwrap_or(DEFAULT_TOOL_MAX_LINES);
    let resolved_max_bytes = max_bytes.unwrap_or(DEFAULT_TOOL_MAX_BYTES);
    let original_lines = if content.is_empty() {
        0
    } else {
        content.matches('\n').count() + 1
    };
    let original_bytes = content.len();

    if original_lines <= resolved_max_lines && original_bytes <= resolved_max_bytes {
        return TruncatedResult {
            content: content.to_string(),
            truncated: false,
            original_lines,
            original_bytes,
            output_path: None,
        };
    }

    let mut rendered = content.to_string();
    let lines = content.split('\n').collect::<Vec<_>>();
    if lines.len() > resolved_max_lines {
        rendered = lines[..resolved_max_lines].join("\n");
        rendered.push_str(&format!(
            "\n\n... output truncated (original {original_lines} lines, showing first {resolved_max_lines} lines)"
        ));
    }

    if rendered.len() > resolved_max_bytes {
        let mut end = resolved_max_bytes.min(rendered.len());
        while end > 0 && !rendered.is_char_boundary(end) {
            end -= 1;
        }
        rendered = rendered[..end].to_string();
        rendered.push_str(&format!(
            "\n\n... output truncated (original {original_bytes} bytes)"
        ));
    }

    TruncatedResult {
        content: rendered,
        truncated: true,
        original_lines,
        original_bytes,
        output_path: None,
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ReadFormat {
    pub output: String,
    pub preview: String,
    pub truncated: bool,
}

#[must_use]
pub fn format_read_output_from_text(text: &str, offset: usize, limit: usize) -> ReadFormat {
    let lines = if text.is_empty() {
        Vec::new()
    } else {
        text.lines().collect::<Vec<_>>()
    };
    let total_lines = lines.len();
    let start = offset;
    let max_lines = limit;
    let mut raw = Vec::new();
    let mut preview_lines = Vec::new();
    let mut bytes_used = 0usize;
    let mut truncated_by_bytes = false;

    for line in lines.iter().skip(start).take(max_lines) {
        let mut rendered = (*line).to_string();
        if rendered.len() > MAX_LINE_LENGTH {
            rendered.truncate(MAX_LINE_LENGTH);
            rendered.push_str("...");
        }
        let encoded_size = rendered.len() + usize::from(!raw.is_empty());
        if bytes_used + encoded_size > MAX_READ_BYTES {
            truncated_by_bytes = true;
            break;
        }
        bytes_used += encoded_size;
        if preview_lines.len() < 20 {
            preview_lines.push(rendered.clone());
        }
        raw.push(rendered);
    }

    let numbered = raw
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{:05}| {line}", index + start + 1))
        .collect::<Vec<_>>();
    let last_read_line = start + raw.len();
    let has_more_lines = total_lines > last_read_line;
    let truncated = truncated_by_bytes || has_more_lines;

    let mut output_lines = vec!["<file>".to_string()];
    output_lines.extend(numbered);
    output_lines.push(String::new());
    if truncated_by_bytes {
        output_lines.push(format!(
            "(Output truncated at {MAX_READ_BYTES} bytes. Use 'offset' parameter to read beyond line {last_read_line})"
        ));
    } else if has_more_lines {
        output_lines.push(format!(
            "(File has more lines. Use 'offset' parameter to read beyond line {last_read_line})"
        ));
    } else {
        output_lines.push(format!("(End of file - total {total_lines} lines)"));
    }
    output_lines.push("</file>".to_string());

    ReadFormat {
        output: output_lines.join("\n"),
        preview: preview_lines.join("\n"),
        truncated,
    }
}

#[derive(Clone, Debug)]
pub struct Toolkit {
    pub registry: ToolRegistry,
    pub max_output_lines: usize,
    pub max_output_bytes: usize,
}

impl Default for Toolkit {
    fn default() -> Self {
        Self {
            registry: ToolRegistry::new(),
            max_output_lines: DEFAULT_TOOL_MAX_LINES,
            max_output_bytes: DEFAULT_TOOL_MAX_BYTES,
        }
    }
}

impl Toolkit {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_builtins() -> Self {
        let mut toolkit = Self::new();
        toolkit.load_builtin();
        toolkit
    }

    pub fn load_builtin(&mut self) {
        register_builtin_tools(&mut self.registry);
    }

    #[must_use]
    pub fn get_all_tools(&self, execution_mode: &str) -> Vec<ToolSchema> {
        self.registry.tool_schemas(execution_mode)
    }

    #[must_use]
    pub fn execute(
        &self,
        name: &str,
        input: Value,
        call_id: &str,
        ctx: &mut ToolContext,
    ) -> ToolResult {
        let Some(tool) = self.registry.get(name) else {
            return ToolResult {
                call_id: call_id.to_string(),
                output: String::new(),
                error: Some(format!("Tool not found: {name}")),
                metadata: BTreeMap::from([("tool".to_string(), json!(name))]),
            };
        };
        if !tool_available(tool, &ctx.execution_mode) {
            return ToolResult {
                call_id: call_id.to_string(),
                output: String::new(),
                error: Some(format!(
                    "Tool \"{name}\" is not available in {} mode.",
                    ctx.execution_mode
                )),
                metadata: BTreeMap::from([
                    ("tool".to_string(), json!(tool.id)),
                    ("execution_mode".to_string(), json!(ctx.execution_mode)),
                    (
                        "error_kind".to_string(),
                        json!("execution_scope_unavailable"),
                    ),
                ]),
            };
        }
        if let Some(result) = permission_gate(tool, &input, call_id, ctx) {
            return result;
        }

        ctx.call_id = call_id.to_string();
        match execute_builtin(name, input, ctx) {
            Ok(output) => self.finish_tool_result(tool, call_id, output, ctx),
            Err(error) => ToolResult {
                call_id: call_id.to_string(),
                output: String::new(),
                error: Some(error),
                metadata: BTreeMap::from([("tool".to_string(), json!(tool.id))]),
            },
        }
    }

    #[must_use]
    pub fn permission_result_for_tool(
        &self,
        name: &str,
        input: &Value,
        call_id: &str,
        ctx: &ToolContext,
    ) -> Option<ToolResult> {
        let Some(tool) = self.registry.get(name) else {
            return Some(ToolResult {
                call_id: call_id.to_string(),
                output: String::new(),
                error: Some(format!("Tool not found: {name}")),
                metadata: BTreeMap::from([("tool".to_string(), json!(name))]),
            });
        };
        if !tool_available(tool, &ctx.execution_mode) {
            return Some(ToolResult {
                call_id: call_id.to_string(),
                output: String::new(),
                error: Some(format!(
                    "Tool \"{name}\" is not available in {} mode.",
                    ctx.execution_mode
                )),
                metadata: BTreeMap::from([
                    ("tool".to_string(), json!(tool.id)),
                    ("execution_mode".to_string(), json!(ctx.execution_mode)),
                    (
                        "error_kind".to_string(),
                        json!("execution_scope_unavailable"),
                    ),
                ]),
            });
        }
        permission_gate(tool, input, call_id, ctx)
    }

    fn finish_tool_result(
        &self,
        tool: &ToolDefinition,
        call_id: &str,
        output: ToolOutput,
        ctx: &ToolContext,
    ) -> ToolResult {
        let truncated_output = truncate_output(
            &output.output,
            Some(self.max_output_lines),
            Some(self.max_output_bytes),
        );
        let output_truncated = truncated_output.truncated;
        let mut metadata = output.metadata;
        metadata
            .entry("tool".to_string())
            .or_insert_with(|| json!(tool.id));
        metadata
            .entry("title".to_string())
            .or_insert_with(|| json!(output.title));
        metadata.insert(
            "truncated".to_string(),
            json!(output.truncated || output_truncated),
        );
        metadata.insert("output_truncated".to_string(), json!(output_truncated));
        metadata.insert(
            "original_lines".to_string(),
            json!(truncated_output.original_lines),
        );
        metadata.insert(
            "original_bytes".to_string(),
            json!(truncated_output.original_bytes),
        );

        let mut output_text = truncated_output.content;
        if output_truncated {
            match write_truncated_output(&ctx.session_root, call_id, &output.output) {
                Ok(path) => {
                    metadata.insert("output_path".to_string(), json!(path_to_string(&path)));
                }
                Err(error) => {
                    output_text.push_str(&format!("\n\nFailed to save full output: {error}"));
                }
            }
        }

        ToolResult {
            call_id: call_id.to_string(),
            output: output_text,
            error: output.error,
            metadata,
        }
    }
}

fn permission_gate(
    tool: &ToolDefinition,
    input: &Value,
    call_id: &str,
    ctx: &ToolContext,
) -> Option<ToolResult> {
    let manager = ctx.permission_manager.as_ref()?;
    let tool_call = json!({"name": tool.id, "input": input});
    match manager.decide(&tool_call) {
        PermissionAction::Allow => None,
        PermissionAction::Ask if ctx.dangerously_skip_permissions => None,
        PermissionAction::Ask => Some(permission_tool_result(
            tool,
            input,
            call_id,
            PermissionFailure {
                action: PermissionAction::Ask,
                message: "Permission requires user confirmation",
                error_kind: "permission_required",
                requires_approval: true,
                dangerously_skip_permissions: ctx.dangerously_skip_permissions,
            },
        )),
        PermissionAction::Deny => Some(permission_tool_result(
            tool,
            input,
            call_id,
            PermissionFailure {
                action: PermissionAction::Deny,
                message: "Permission denied",
                error_kind: "permission_denied",
                requires_approval: false,
                dangerously_skip_permissions: ctx.dangerously_skip_permissions,
            },
        )),
    }
}

struct PermissionFailure {
    action: PermissionAction,
    message: &'static str,
    error_kind: &'static str,
    requires_approval: bool,
    dangerously_skip_permissions: bool,
}

fn permission_tool_result(
    tool: &ToolDefinition,
    input: &Value,
    call_id: &str,
    failure: PermissionFailure,
) -> ToolResult {
    ToolResult {
        call_id: call_id.to_string(),
        output: String::new(),
        error: Some(format!("{}: {}", failure.message, tool.id)),
        metadata: BTreeMap::from([
            ("tool".to_string(), json!(tool.id)),
            (
                "permission_action".to_string(),
                json!(permission_action_name(&failure.action)),
            ),
            ("permission_pattern".to_string(), json!(pattern_for(input))),
            (
                "permission_required".to_string(),
                json!(failure.requires_approval),
            ),
            (
                "requires_approval".to_string(),
                json!(failure.requires_approval),
            ),
            (
                "dangerously_skip_permissions".to_string(),
                json!(failure.dangerously_skip_permissions),
            ),
            ("dangerous".to_string(), json!(tool.dangerous)),
            ("group".to_string(), json!(tool.group)),
            ("call_id".to_string(), json!(call_id)),
            ("input".to_string(), input.clone()),
            ("error_kind".to_string(), json!(failure.error_kind)),
        ]),
    }
}

fn permission_action_name(action: &PermissionAction) -> &'static str {
    match action {
        PermissionAction::Allow => "allow",
        PermissionAction::Deny => "deny",
        PermissionAction::Ask => "ask",
    }
}

pub fn register_builtin_tools(registry: &mut ToolRegistry) {
    let workspace_read = readonly_schema("workspace-read", false, false, None);
    let file_read = readonly_schema("workspace-read", false, true, None);
    let file_write = exclusive_schema(
        "workspace-write",
        true,
        true,
        false,
        false,
        false,
        Some("file:{file_path}"),
    );

    registry.register(ToolDefinition {
        id: "read".to_string(),
        description: "Read a UTF-8 workspace file.".to_string(),
        parameter_schema: schema(&["file_path"], &["file_path", "offset", "limit"]),
        dangerous: false,
        group: "file".to_string(),
        execution_scope: ToolExecutionScope::Workspace,
        execution_schema: file_read,
    });
    registry.register(ToolDefinition {
        id: "write".to_string(),
        description: "Write a UTF-8 workspace file.".to_string(),
        parameter_schema: schema(&["file_path", "content"], &["file_path", "content"]),
        dangerous: true,
        group: "file".to_string(),
        execution_scope: ToolExecutionScope::Workspace,
        execution_schema: file_write.clone(),
    });
    registry.register(ToolDefinition {
        id: "edit".to_string(),
        description: "Edit a UTF-8 workspace file.".to_string(),
        parameter_schema: schema(
            &["file_path", "old_string", "new_string"],
            &["file_path", "old_string", "new_string", "replace_all"],
        ),
        dangerous: true,
        group: "file".to_string(),
        execution_scope: ToolExecutionScope::Workspace,
        execution_schema: file_write,
    });
    registry.register(ToolDefinition {
        id: "glob".to_string(),
        description: "Find workspace paths by glob pattern.".to_string(),
        parameter_schema: schema(&["pattern"], &["pattern", "path"]),
        dangerous: false,
        group: "file".to_string(),
        execution_scope: ToolExecutionScope::Workspace,
        execution_schema: workspace_read.clone(),
    });
    registry.register(ToolDefinition {
        id: "grep".to_string(),
        description: "Search workspace files with a regular expression.".to_string(),
        parameter_schema: schema(&["pattern"], &["pattern", "path", "glob", "include"]),
        dangerous: false,
        group: "file".to_string(),
        execution_scope: ToolExecutionScope::Workspace,
        execution_schema: workspace_read.clone(),
    });
    registry.register(ToolDefinition {
        id: "ls".to_string(),
        description: "List a workspace directory tree.".to_string(),
        parameter_schema: schema(&[], &["path", "ignore"]),
        dangerous: false,
        group: "file".to_string(),
        execution_scope: ToolExecutionScope::Workspace,
        execution_schema: workspace_read.clone(),
    });
    registry.register(ToolDefinition {
        id: "bash".to_string(),
        description: "Run a shell command in the workspace.".to_string(),
        parameter_schema: schema(
            &["command"],
            &["command", "timeout", "workdir", "description"],
        ),
        dangerous: true,
        group: "shell".to_string(),
        execution_scope: ToolExecutionScope::Workspace,
        execution_schema: exclusive_schema("shell", true, true, true, true, false, None),
    });
    registry.register(ToolDefinition {
        id: "skill".to_string(),
        description: "Load or list Markdown skills discovered from the workspace.".to_string(),
        parameter_schema: schema(
            &[],
            &[
                "name",
                "query",
                "limit",
                "include_content",
                "include_diagnostics",
            ],
        ),
        dangerous: false,
        group: "skill".to_string(),
        execution_scope: ToolExecutionScope::Agnostic,
        execution_schema: readonly_schema("skill", false, false, None),
    });
    registry.register(ToolDefinition {
        id: WEB_FETCH_TOOL_ID.to_string(),
        description:
            "Fetch an HTTP(S) URL for read-only external documentation and dependency research."
                .to_string(),
        parameter_schema: schema(&["url"], &["url", "timeout", "max_bytes"]),
        dangerous: false,
        group: "network".to_string(),
        execution_scope: ToolExecutionScope::HostOnly,
        execution_schema: readonly_schema("network-read", true, false, Some(4)),
    });
    registry.register(ToolDefinition {
        id: "code_search".to_string(),
        description: "Search files for a literal substring.".to_string(),
        parameter_schema: schema(&["query"], &["query", "glob", "path"]),
        dangerous: false,
        group: "search".to_string(),
        execution_scope: ToolExecutionScope::HostOnly,
        execution_schema: workspace_read,
    });
    registry.register(ToolDefinition {
        id: "memory_read".to_string(),
        description: "Read a JSON memory value.".to_string(),
        parameter_schema: schema(&["key"], &["key"]),
        dangerous: false,
        group: "memory".to_string(),
        execution_scope: ToolExecutionScope::Agnostic,
        execution_schema: readonly_schema("memory", false, false, None),
    });
    registry.register(ToolDefinition {
        id: "memory_write".to_string(),
        description: "Write a JSON memory value.".to_string(),
        parameter_schema: schema(&["key", "value"], &["key", "value"]),
        dangerous: false,
        group: "memory".to_string(),
        execution_scope: ToolExecutionScope::Agnostic,
        execution_schema: exclusive_schema(
            "memory",
            false,
            true,
            false,
            false,
            false,
            Some("memory:{key}"),
        ),
    });
    registry.register(ToolDefinition {
        id: "todowrite".to_string(),
        description: "Replace the session todo list.".to_string(),
        parameter_schema: schema(&["todos"], &["todos"]),
        dangerous: false,
        group: "todo".to_string(),
        execution_scope: ToolExecutionScope::Agnostic,
        execution_schema: exclusive_schema(
            "todo",
            false,
            true,
            false,
            false,
            false,
            Some("session:todos"),
        ),
    });
    registry.register(ToolDefinition {
        id: "todoread".to_string(),
        description: "Read the session todo list.".to_string(),
        parameter_schema: schema(&[], &[]),
        dangerous: false,
        group: "todo".to_string(),
        execution_scope: ToolExecutionScope::Agnostic,
        execution_schema: readonly_schema("todo", false, false, None),
    });
    registry.register(ToolDefinition {
        id: "question".to_string(),
        description: "Ask the user structured questions.".to_string(),
        parameter_schema: schema(&["questions"], &["questions"]),
        dangerous: false,
        group: "interactive".to_string(),
        execution_scope: ToolExecutionScope::Agnostic,
        execution_schema: exclusive_schema("interactive", false, true, false, false, true, None),
    });
}

#[must_use]
pub fn readonly_schema(
    batch_group: impl Into<String>,
    external_io: bool,
    mutates_session: bool,
    max_parallelism: Option<u64>,
) -> ToolExecutionSchema {
    ToolExecutionSchema {
        read_only: true,
        mutates_workspace: false,
        mutates_session,
        mutates_external: false,
        external_io,
        requires_user_interaction: false,
        concurrency: ToolConcurrency::Safe,
        batch_group: batch_group.into(),
        conflict_key_template: None,
        max_parallelism,
    }
}

#[must_use]
pub fn exclusive_schema(
    batch_group: impl Into<String>,
    mutates_workspace: bool,
    mutates_session: bool,
    mutates_external: bool,
    external_io: bool,
    requires_user_interaction: bool,
    conflict_key_template: Option<&str>,
) -> ToolExecutionSchema {
    ToolExecutionSchema {
        read_only: false,
        mutates_workspace,
        mutates_session,
        mutates_external,
        external_io,
        requires_user_interaction,
        concurrency: ToolConcurrency::Exclusive,
        batch_group: batch_group.into(),
        conflict_key_template: conflict_key_template.map(str::to_string),
        max_parallelism: None,
    }
}

#[must_use]
pub fn blocked_command(command: &str) -> Option<String> {
    let regex = Regex::new(
        r"(?i)(?:^|[;&|]\s*)(rm|rmdir|del|erase|deltree|remove-item|shred|unlink)(?:\s|$)",
    )
    .ok()?;
    regex
        .captures(command)
        .and_then(|captures| captures.get(1))
        .map(|matched| matched.as_str().to_string())
}

#[must_use]
pub fn benchmark_mode_value_allows_shell_command(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "terminal-bench" | "terminal_bench" | "terminalbench"
    )
}

#[must_use]
pub fn benchmark_mode_allows_shell_command(ctx: &ToolContext) -> bool {
    if env::var(BENCHMARK_MODE_ENV)
        .ok()
        .as_deref()
        .is_some_and(benchmark_mode_value_allows_shell_command)
    {
        return true;
    }

    let context_keys = ["benchmark_mode", "harness", "execution_mode"];
    context_keys.iter().any(|key| {
        ctx.execution_metadata
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(benchmark_mode_value_allows_shell_command)
            || ctx
                .agent_options
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(benchmark_mode_value_allows_shell_command)
    })
}

pub fn ensure_within_root(
    root: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> ToolResultValue<PathBuf> {
    let root_raw = root.as_ref();
    let target_raw = target.as_ref();
    let root_resolved = root_raw
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(root_raw));
    let target_joined = if target_raw.is_absolute() {
        target_raw.to_path_buf()
    } else {
        root_resolved.join(target_raw)
    };
    let target_resolved = target_joined
        .canonicalize()
        .unwrap_or_else(|_| normalize_path(&target_joined));
    if target_resolved == root_resolved || target_resolved.starts_with(&root_resolved) {
        Ok(target_resolved)
    } else {
        Err(format!(
            "Path escapes session root: {}",
            target_raw.display()
        ))
    }
}

pub fn resolve_path_in_root(root: impl AsRef<Path>, path: &str) -> ToolResultValue<PathBuf> {
    ensure_within_root(root, Path::new(path))
}

pub fn resolve_optional_path(
    root: impl AsRef<Path>,
    path: Option<&str>,
) -> ToolResultValue<PathBuf> {
    match path {
        Some(raw) => ensure_within_root(root, Path::new(raw)),
        None => ensure_within_root(&root, root.as_ref()),
    }
}

fn execute_builtin(name: &str, input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    match name {
        "read" => read_tool(input, ctx),
        "write" => write_tool(input, ctx),
        "edit" => edit_tool(input, ctx),
        "glob" => glob_tool(input, ctx),
        "grep" => grep_tool(input, ctx),
        "ls" => ls_tool(input, ctx),
        "bash" => bash_tool(input, ctx),
        "skill" => skill_tool(input, ctx),
        WEB_FETCH_TOOL_ID => web_fetch_tool(input),
        "code_search" => code_search_tool(input, ctx),
        "memory_read" => memory_read_tool(input, ctx),
        "memory_write" => memory_write_tool(input, ctx),
        "todowrite" => todo_write_tool(input, ctx),
        "todoread" => todo_read_tool(input, ctx),
        "question" => question_tool(input, ctx),
        _ => Err(format!("No Rust builtin implementation for tool: {name}")),
    }
}

fn read_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let file_path = string_arg(&input, "file_path")?;
    let offset = usize_arg(&input, "offset", 0)?;
    let limit = usize_arg(&input, "limit", DEFAULT_READ_LIMIT)?;
    let root = normalize_path(&ctx.session_root);
    let target = resolve_path_in_root(&root, &file_path)?;
    if !target.exists() {
        return Err(format!("File not found: {}", target.display()));
    }
    if target.is_dir() {
        return Err(format!(
            "Path is a directory, not a file: {}",
            target.display()
        ));
    }
    let text = read_text_checked(&target)?;
    let formatted = format_read_output_from_text(&text, offset, limit);
    ctx.remember_read(&target);
    let mut output = ToolOutput::new(display_path(&root, &target), formatted.output);
    output
        .metadata
        .insert("preview".to_string(), json!(formatted.preview));
    output.truncated = formatted.truncated;
    Ok(output)
}

fn write_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let file_path = string_arg(&input, "file_path")?;
    let content = string_arg(&input, "content")?;
    let root = normalize_path(&ctx.session_root);
    let target = resolve_path_in_root(&root, &file_path)?;
    let existed = target.exists();
    require_existing_file_was_read(ctx, &target, "writing")?;
    write_text(&target, &content)?;
    ctx.remember_read(&target);
    let mut output = ToolOutput::new(
        display_path(&root, &target),
        format!(
            "Wrote {} chars to {}",
            content.chars().count(),
            target.display()
        ),
    );
    output
        .metadata
        .insert("file_path".to_string(), json!(path_to_string(&target)));
    output.metadata.insert("exists".to_string(), json!(existed));
    Ok(output)
}

fn edit_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let file_path = string_arg(&input, "file_path")?;
    let old_string = string_arg(&input, "old_string")?;
    let new_string = string_arg(&input, "new_string")?;
    let replace_all = bool_arg(&input, "replace_all", false)?;
    let root = normalize_path(&ctx.session_root);
    let target = resolve_path_in_root(&root, &file_path)?;
    require_existing_file_was_read(ctx, &target, "editing")?;

    if old_string.is_empty() {
        write_text(&target, &new_string)?;
        ctx.remember_read(&target);
        return edited_output(&root, &target, replace_all);
    }
    if !target.exists() {
        return Err(format!("File not found: {}", target.display()));
    }
    if target.is_dir() {
        return Err(format!(
            "Path is a directory, not a file: {}",
            target.display()
        ));
    }
    let text = fs::read_to_string(&target).map_err(io_error)?;
    let new_text = replace_text(&text, &old_string, &new_string, replace_all)?;
    write_text(&target, &new_text)?;
    ctx.remember_read(&target);
    edited_output(&root, &target, replace_all)
}

fn glob_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let pattern = string_arg(&input, "pattern")?;
    let path = optional_string_arg(&input, "path")?;
    let root = normalize_path(&ctx.session_root);
    let base = resolve_optional_path(&root, path.as_deref())?;
    let mut matches = glob_paths(&root, &base, &pattern)?;
    let truncated = matches.len() > GLOB_LIMIT;
    matches.sort_by(|left, right| {
        path_mtime(right)
            .total_cmp(&path_mtime(left))
            .then_with(|| path_to_string(left).cmp(&path_to_string(right)))
    });
    matches.truncate(GLOB_LIMIT);
    let output_text = if matches.is_empty() {
        "No files found".to_string()
    } else {
        let mut lines = matches
            .iter()
            .map(|path| path_to_string(path))
            .collect::<Vec<_>>();
        if truncated {
            lines.push(String::new());
            lines.push(
                "(Results are truncated. Consider using a more specific path or pattern.)"
                    .to_string(),
            );
        }
        lines.join("\n")
    };
    let mut output = ToolOutput::new(display_path(&root, &base), output_text);
    output
        .metadata
        .insert("count".to_string(), json!(matches.len()));
    output.truncated = truncated;
    Ok(output)
}

fn grep_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let pattern = string_arg(&input, "pattern")?;
    let path = optional_string_arg(&input, "path")?;
    let include_glob =
        optional_string_arg(&input, "include")?.or(optional_string_arg(&input, "glob")?);
    let root = normalize_path(&ctx.session_root);
    let base = resolve_optional_path(&root, path.as_deref())?;
    let mut matches = grep_paths(&root, &base, &pattern, include_glob.as_deref())?;
    matches.sort_by(|left, right| {
        right
            .mtime
            .total_cmp(&left.mtime)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.line.cmp(&right.line))
    });
    let truncated = matches.len() > GREP_LIMIT;
    matches.truncate(GREP_LIMIT);
    let output_text = render_grep_output(&matches, truncated);
    let mut output = ToolOutput::new(pattern, output_text);
    output
        .metadata
        .insert("count".to_string(), json!(matches.len()));
    output.metadata.insert(
        "include".to_string(),
        json!(include_glob.unwrap_or_else(|| "*".to_string())),
    );
    output.truncated = truncated;
    Ok(output)
}

fn ls_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let path = optional_string_arg(&input, "path")?;
    let mut ignore = DEFAULT_LS_IGNORE
        .iter()
        .map(|item| (*item).to_string())
        .collect::<Vec<_>>();
    ignore.extend(string_list_arg(&input, "ignore")?);
    let root = normalize_path(&ctx.session_root);
    let base = resolve_optional_path(&root, path.as_deref())?;
    let (tree, count, truncated) = collect_ls_tree(&base, &ignore)?;
    let output_text = render_ls_tree(&format!("{}/", base.display()), &tree, truncated);
    let mut output = ToolOutput::new(display_path(&root, &base), output_text);
    output.metadata.insert("count".to_string(), json!(count));
    output.metadata.insert("ignore".to_string(), json!(ignore));
    output.truncated = truncated;
    Ok(output)
}

fn bash_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let command = string_arg(&input, "command")?;
    let timeout = u64_arg(&input, "timeout", 120_000)?;
    let workdir = optional_string_arg(&input, "workdir")?;
    let description = optional_string_arg(&input, "description")?.unwrap_or_default();
    if !benchmark_mode_allows_shell_command(ctx)
        && let Some(blocked) = blocked_command(&command)
    {
        return Err(format!(
            "{blocked} command is disabled for security reasons"
        ));
    }
    let runtime = LocalWorkspaceRuntime::new(&ctx.session_root);
    let command_result = runtime.run_command(&command, workdir.as_deref(), timeout)?;
    let combined = format!("{}{}", command_result.stdout, command_result.stderr)
        .trim()
        .to_string();
    let output_text = if combined.is_empty() {
        format!(
            "Command exited with return code {}.",
            command_result.returncode
        )
    } else {
        combined
    };
    let title = display_path(&ctx.session_root, Path::new(&command_result.cwd));
    let mut output = ToolOutput::new(title, output_text);
    output
        .metadata
        .insert("returncode".to_string(), json!(command_result.returncode));
    output
        .metadata
        .insert("description".to_string(), json!(description));
    output
        .metadata
        .insert("workdir".to_string(), json!(command_result.cwd));
    Ok(output)
}

fn skill_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let requested_name = optional_string_arg(&input, "name")?
        .unwrap_or_default()
        .trim()
        .to_string();
    let requested_query = optional_string_arg(&input, "query")?
        .unwrap_or_default()
        .trim()
        .to_string();
    let limit = optional_usize_arg(&input, "limit")?.filter(|value| *value > 0);
    let include_content = bool_arg(&input, "include_content", false)?;
    let include_diagnostics = bool_arg(&input, "include_diagnostics", false)?;
    let registry = SkillRegistry::new(
        Some(ctx.session_root.clone()),
        skill_roots(ctx),
        Option::<PathBuf>::None,
    );

    if requested_name.is_empty() {
        let report = registry.report(
            (!requested_query.is_empty()).then_some(requested_query.as_str()),
            limit,
        );
        let mut lines = if report.skills.is_empty() {
            if requested_query.is_empty() {
                vec!["No skills available.".to_string()]
            } else {
                vec![format!("No skills matched query \"{requested_query}\".")]
            }
        } else {
            let mut lines = vec![if requested_query.is_empty() {
                "Available skills:".to_string()
            } else {
                format!("Matched skills for \"{requested_query}\":")
            }];
            for skill in &report.skills {
                let score = skill
                    .score
                    .map(|score| format!(" score={score}"))
                    .unwrap_or_default();
                lines.push(format!(
                    "- `{}`:{} {}",
                    skill.name, score, skill.description
                ));
                if include_content && let Some(document) = registry.get(&skill.name) {
                    lines.push(render_skill_document(&document, false));
                }
            }
            lines
        };
        if include_diagnostics {
            lines.extend(diagnostic_lines(&report));
        }
        let mut output = ToolOutput::new("Available skills", lines.join("\n"));
        output.metadata = report_metadata(
            &report,
            (!requested_query.is_empty()).then_some(requested_query),
        );
        return Ok(output);
    }

    let Some(document) = registry.get(&requested_name) else {
        let skills = if requested_query.is_empty() {
            registry.all()
        } else {
            registry.search(&requested_query, limit)
        };
        let available = if skills.is_empty() {
            "none".to_string()
        } else {
            skills
                .iter()
                .map(|skill| skill.name.clone())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(format!(
            "Skill \"{requested_name}\" not found. Available skills: {available}"
        ));
    };

    let report = registry.report(None, None);
    let mut output = ToolOutput::new(
        format!("Loaded skill: {}", document.name),
        render_skill_document(&document, true),
    );
    output
        .metadata
        .insert("skill_name".to_string(), json!(document.name));
    output
        .metadata
        .insert("skill_location".to_string(), json!(document.location));
    output
        .metadata
        .insert("skill_dir".to_string(), json!(document.directory));
    output
        .metadata
        .insert("skill_count".to_string(), json!(report.loaded_count));
    output
        .metadata
        .insert("scanned_files".to_string(), json!(report.scanned_files));
    output
        .metadata
        .insert("invalid_count".to_string(), json!(report.invalid_count));
    output
        .metadata
        .insert("duplicate_count".to_string(), json!(report.duplicate_count));
    Ok(output)
}

fn web_fetch_tool(input: Value) -> ToolResultValue<ToolOutput> {
    let raw_url = string_arg(&input, "url")?;
    let url = reqwest::Url::parse(raw_url.trim())
        .map_err(|error| format!("invalid web_fetch url: {error}"))?;
    match url.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(format!(
                "web_fetch only supports http and https URLs, got: {scheme}"
            ));
        }
    }
    let timeout = Duration::from_millis(u64_arg(&input, "timeout", WEB_FETCH_DEFAULT_TIMEOUT_MS)?);
    let max_bytes = optional_usize_arg(&input, "max_bytes")?
        .unwrap_or(WEB_FETCH_MAX_BYTES)
        .clamp(1, WEB_FETCH_MAX_BYTES);
    let mut client_builder = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::limited(5));
    if url
        .host_str()
        .is_some_and(|host| matches!(host, "127.0.0.1" | "localhost" | "::1"))
    {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder
        .build()
        .map_err(|error| format!("failed to build web_fetch client: {error}"))?;
    let mut response = client
        .get(url.clone())
        .header(reqwest::header::USER_AGENT, "openagent-web-fetch/0.1")
        .send()
        .map_err(|error| format!("web_fetch request failed: {error:?}"))?;
    let status = response.status().as_u16();
    let final_url = response.url().to_string();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take((max_bytes + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read web_fetch response: {error}"))?;
    let truncated = bytes.len() > max_bytes;
    bytes.truncate(max_bytes);
    let body = String::from_utf8_lossy(&bytes).into_owned();
    let mut output = ToolOutput::new(final_url.clone(), body);
    output.truncated = truncated;
    output
        .metadata
        .insert("url".to_string(), json!(raw_url.trim()));
    output
        .metadata
        .insert("final_url".to_string(), json!(final_url));
    output.metadata.insert("status".to_string(), json!(status));
    output
        .metadata
        .insert("content_type".to_string(), json!(content_type));
    output
        .metadata
        .insert("max_bytes".to_string(), json!(max_bytes));
    Ok(output)
}

fn code_search_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let query = string_arg(&input, "query")?;
    let glob = string_arg_or(&input, "glob", "*")?;
    let path = optional_string_arg(&input, "path")?;
    let root = normalize_path(&ctx.session_root);
    let base = resolve_optional_path(&root, path.as_deref())?;
    let mut hits = Vec::new();
    let mut preview_hits = Vec::new();
    for file_path in walk_files(&base)? {
        let name = file_path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        let relative = file_path
            .strip_prefix(&base)
            .map(path_to_string)
            .unwrap_or_else(|_| path_to_string(&file_path));
        if !matches_glob(&glob, &relative, name) {
            continue;
        }
        let content = match fs::read_to_string(&file_path) {
            Ok(content) => content,
            Err(_) => continue,
        };
        for (index, line) in content.lines().enumerate() {
            if !line.contains(&query) {
                continue;
            }
            let clipped = clip_chars(line, CODE_SEARCH_MAX_LINE_CHARS);
            let hit = format!("{}:{}:{clipped}", file_path.display(), index + 1);
            hits.push(hit.clone());
            if preview_hits.len() < CODE_SEARCH_MAX_PREVIEW_HITS {
                preview_hits.push(hit);
            }
            if hits.len() >= CODE_SEARCH_MAX_HITS {
                return code_search_output(&root, &base, hits, preview_hits, true);
            }
        }
    }
    code_search_output(&root, &base, hits, preview_hits, false)
}

fn memory_read_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let key = string_arg(&input, "key")?;
    let value = ctx.memory.get(&key).cloned().unwrap_or(Value::Null);
    Ok(ToolOutput::new(key, value.to_string()))
}

fn memory_write_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let key = string_arg(&input, "key")?;
    let value = input
        .get("value")
        .cloned()
        .ok_or_else(|| "Missing required parameter: value".to_string())?;
    ctx.memory.insert(key.clone(), value);
    Ok(ToolOutput::new(key, "ok"))
}

fn todo_write_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let todos_value = input
        .get("todos")
        .and_then(Value::as_array)
        .ok_or_else(|| "Expected list input for todos".to_string())?;
    let todos = todos_value
        .iter()
        .map(todo_from_value)
        .collect::<ToolResultValue<Vec<_>>>()?;
    save_todos(ctx, todos.clone())?;
    todo_output(todos)
}

fn todo_read_tool(_input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let todos = load_todos(ctx)?;
    todo_output(todos)
}

fn question_tool(input: Value, ctx: &mut ToolContext) -> ToolResultValue<ToolOutput> {
    let questions = input
        .get("questions")
        .and_then(Value::as_array)
        .ok_or_else(|| "Expected list input for questions".to_string())?;
    let answers = ctx
        .question_answers
        .clone()
        .ok_or_else(|| "question tool requires configured answers".to_string())?;
    let mut formatted = Vec::new();
    let mut question_metadata = Vec::new();
    for (index, item) in questions.iter().enumerate() {
        let question = value_string(item, "question")?;
        let answer = answers
            .get(index)
            .map(|items| {
                if items.is_empty() {
                    "Unanswered".to_string()
                } else {
                    items.join(", ")
                }
            })
            .unwrap_or_else(|| "Unanswered".to_string());
        formatted.push(format!("\"{question}\"=\"{answer}\""));
        question_metadata.push(question_metadata_value(item)?);
    }
    let count = questions.len();
    let mut output = ToolOutput::new(
        title_for_questions(count),
        format!(
            "User has answered your questions: {}. You can now continue with the user's answers in mind.",
            formatted.join(", ")
        ),
    );
    output
        .metadata
        .insert("answers".to_string(), json!(answers));
    output
        .metadata
        .insert("questions".to_string(), Value::Array(question_metadata));
    output.metadata.insert(
        "request_id".to_string(),
        json!(format!("question_{}", ctx.call_id)),
    );
    output.metadata.insert("count".to_string(), json!(count));
    Ok(output)
}

fn tool_available(tool: &ToolDefinition, execution_mode: &str) -> bool {
    if execution_mode != "opensandbox" {
        return true;
    }
    matches!(
        tool.execution_scope,
        ToolExecutionScope::Workspace | ToolExecutionScope::Agnostic
    )
}

fn schema(required: &[&str], properties: &[&str]) -> Value {
    let props = properties
        .iter()
        .map(|name| ((*name).to_string(), property_schema(name)))
        .collect::<serde_json::Map<_, _>>();
    json!({
        "type": "object",
        "properties": props,
        "required": required,
    })
}

fn property_schema(name: &str) -> Value {
    match name {
        "offset" | "limit" | "timeout" | "max_bytes" => json!({"type": "integer"}),
        "replace_all"
        | "multiple"
        | "include_content"
        | "include_diagnostics"
        | "isolate_workspace" => json!({"type": "boolean"}),
        "ignore" | "questions" | "todos" | "options" => json!({"type": "array"}),
        "value" => json!({}),
        _ => json!({"type": "string"}),
    }
}

fn normalize_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
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
            Component::RootDir | Component::Prefix(_) => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn root_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| normalize_path(path))
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn display_path(root: &Path, target: &Path) -> String {
    normalize_path(target)
        .strip_prefix(normalize_path(root))
        .map(path_to_string)
        .unwrap_or_else(|_| path_to_string(&normalize_path(target)))
}

fn io_error(error: io::Error) -> String {
    error.to_string()
}

fn read_text_checked(path: &Path) -> ToolResultValue<String> {
    if is_binary_file(path)? {
        return Err(format!("Cannot read binary file: {}", path.display()));
    }
    fs::read_to_string(path).map_err(io_error)
}

fn is_binary_file(path: &Path) -> ToolResultValue<bool> {
    let suffix = path
        .extension()
        .and_then(OsStr::to_str)
        .map(|extension| format!(".{}", extension.to_lowercase()))
        .unwrap_or_default();
    if BINARY_EXTENSIONS.contains(&suffix.as_str()) {
        return Ok(true);
    }
    let data = fs::read(path).map_err(io_error)?;
    if data.is_empty() {
        return Ok(false);
    }
    let sample = data.iter().take(4096).copied().collect::<Vec<_>>();
    if sample.contains(&0) {
        return Ok(true);
    }
    let non_printable = sample
        .iter()
        .filter(|byte| **byte < 9 || (**byte > 13 && **byte < 32))
        .count();
    Ok((non_printable as f64 / sample.len() as f64) > 0.3)
}

fn write_text(path: &Path, content: &str) -> ToolResultValue<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    fs::write(path, content).map_err(io_error)
}

fn require_existing_file_was_read(
    ctx: &ToolContext,
    target: &Path,
    action: &str,
) -> ToolResultValue<()> {
    if ctx.require_read_before_write && target.exists() && !ctx.has_read_file(target) {
        Err(format!(
            "Must read existing file before {action} it: {}",
            target.display()
        ))
    } else {
        Ok(())
    }
}

fn replace_text(content: &str, old: &str, new: &str, replace_all: bool) -> ToolResultValue<String> {
    if old == new {
        return Err("old_string and new_string must be different".to_string());
    }
    if old.is_empty() {
        return Ok(new.to_string());
    }
    let count = content.matches(old).count();
    if count == 0 {
        return Err("old_string not found in content".to_string());
    }
    if count > 1 && !replace_all {
        return Err(
            "old_string found multiple times and requires more code context to uniquely identify the intended match"
                .to_string(),
        );
    }
    if replace_all {
        Ok(content.replace(old, new))
    } else {
        Ok(content.replacen(old, new, 1))
    }
}

fn edited_output(root: &Path, target: &Path, replace_all: bool) -> ToolResultValue<ToolOutput> {
    let mut output = ToolOutput::new(
        display_path(root, target),
        format!("Edited {}", target.display()),
    );
    output
        .metadata
        .insert("file_path".to_string(), json!(path_to_string(target)));
    output
        .metadata
        .insert("replace_all".to_string(), json!(replace_all));
    Ok(output)
}

fn walk_paths(base: &Path) -> ToolResultValue<Vec<PathBuf>> {
    let mut paths = Vec::new();
    if !base.exists() {
        return Ok(paths);
    }
    paths.push(base.to_path_buf());
    if base.is_dir() {
        let mut entries = fs::read_dir(base)
            .map_err(io_error)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                paths.extend(walk_paths(&path)?);
            } else {
                paths.push(path);
            }
        }
    }
    Ok(paths)
}

fn walk_files(base: &Path) -> ToolResultValue<Vec<PathBuf>> {
    Ok(walk_paths(base)?
        .into_iter()
        .filter(|path| path.is_file())
        .collect())
}

fn glob_paths(root: &Path, base: &Path, pattern: &str) -> ToolResultValue<Vec<PathBuf>> {
    let mut matches = BTreeSet::new();
    for path in walk_paths(base)? {
        let relative = path
            .strip_prefix(base)
            .map(path_to_string)
            .unwrap_or_else(|_| path_to_string(&path));
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        if matches_glob(pattern, &relative, name)
            && let Ok(resolved) = ensure_within_root(root, &path)
        {
            matches.insert(resolved);
        }
    }
    Ok(matches.into_iter().collect())
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct GrepMatch {
    pub path: String,
    pub line: usize,
    pub text: String,
    pub mtime: f64,
}

fn grep_paths(
    root: &Path,
    base: &Path,
    pattern: &str,
    include_glob: Option<&str>,
) -> ToolResultValue<Vec<GrepMatch>> {
    let regex = Regex::new(pattern).map_err(|error| error.to_string())?;
    let include = include_glob.unwrap_or("*");
    let mut matches = Vec::new();
    for file_path in walk_files(base)? {
        let name = file_path
            .file_name()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        let relative = file_path
            .strip_prefix(base)
            .map(path_to_string)
            .unwrap_or_else(|_| path_to_string(&file_path));
        if !matches_glob(include, &relative, name) {
            continue;
        }
        let Ok(resolved) = ensure_within_root(root, &file_path) else {
            continue;
        };
        let content = match fs::read_to_string(&resolved) {
            Ok(content) => content,
            Err(_) => continue,
        };
        for (line_index, line) in content.lines().enumerate() {
            if regex.is_match(line) {
                matches.push(GrepMatch {
                    path: path_to_string(&resolved),
                    line: line_index + 1,
                    text: line.to_string(),
                    mtime: path_mtime(&resolved),
                });
            }
        }
    }
    Ok(matches)
}

fn render_grep_output(matches: &[GrepMatch], truncated: bool) -> String {
    if matches.is_empty() {
        return "No files found".to_string();
    }
    let mut output_lines = vec![format!("Found {} matches", matches.len())];
    let mut current_file = String::new();
    for item in matches {
        if current_file != item.path {
            if !current_file.is_empty() {
                output_lines.push(String::new());
            }
            current_file = item.path.clone();
            output_lines.push(format!("{}:", item.path));
        }
        output_lines.push(format!(
            "  Line {}: {}",
            item.line,
            clip_chars(&item.text, MAX_LINE_LENGTH)
        ));
    }
    if truncated {
        output_lines.push(String::new());
        output_lines.push(
            "(Results are truncated. Consider using a more specific path or pattern.)".to_string(),
        );
    }
    output_lines.join("\n")
}

#[derive(Clone, Debug, Default)]
struct LsNode {
    dirs: BTreeMap<String, LsNode>,
    files: Vec<String>,
}

fn collect_ls_tree(base: &Path, ignore: &[String]) -> ToolResultValue<(LsNode, usize, bool)> {
    let mut root = LsNode::default();
    if base.is_file() {
        root.files.push(
            base.file_name()
                .and_then(OsStr::to_str)
                .unwrap_or_default()
                .to_string(),
        );
        return Ok((root, 1, false));
    }
    let mut file_count = 0usize;
    let mut truncated = false;
    collect_ls_tree_inner(
        base,
        base,
        ignore,
        &mut root,
        &mut file_count,
        &mut truncated,
    )?;
    Ok((root, file_count, truncated))
}

fn collect_ls_tree_inner(
    base: &Path,
    dir: &Path,
    ignore: &[String],
    node: &mut LsNode,
    file_count: &mut usize,
    truncated: &mut bool,
) -> ToolResultValue<()> {
    if *truncated || !dir.is_dir() {
        return Ok(());
    }
    let mut entries = fs::read_dir(dir)
        .map_err(io_error)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();
    for path in entries.iter().filter(|path| path.is_dir()) {
        let relative = path
            .strip_prefix(base)
            .map(path_to_string)
            .unwrap_or_default();
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        if should_ignore(&relative, name, ignore) {
            continue;
        }
        let child = node.dirs.entry(name.to_string()).or_default();
        collect_ls_tree_inner(base, path, ignore, child, file_count, truncated)?;
        if *truncated {
            return Ok(());
        }
    }
    for path in entries.into_iter().filter(|path| path.is_file()) {
        let relative = path
            .strip_prefix(base)
            .map(path_to_string)
            .unwrap_or_default();
        let name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        if should_ignore(&relative, name, ignore) {
            continue;
        }
        node.files.push(name.to_string());
        *file_count += 1;
        if *file_count >= LS_LIMIT {
            *truncated = true;
            return Ok(());
        }
    }
    Ok(())
}

fn render_ls_tree(label: &str, tree: &LsNode, truncated: bool) -> String {
    let mut lines = vec![label.to_string()];
    render_ls_tree_inner(tree, 0, &mut lines);
    if truncated {
        lines.push(String::new());
        lines.push("(Results are truncated. Consider using a more specific path.)".to_string());
    }
    lines.join("\n")
}

fn render_ls_tree_inner(node: &LsNode, depth: usize, lines: &mut Vec<String>) {
    for (dirname, child) in &node.dirs {
        lines.push(format!("{}{}{}", "  ".repeat(depth + 1), dirname, "/"));
        render_ls_tree_inner(child, depth + 1, lines);
    }
    let mut files = node.files.clone();
    files.sort();
    for filename in files {
        lines.push(format!("{}{}", "  ".repeat(depth + 1), filename));
    }
}

fn should_ignore(relative_path: &str, name: &str, patterns: &[String]) -> bool {
    let normalized = relative_path.replace('\\', "/");
    patterns.iter().any(|pattern| {
        let cleaned = pattern.replace('\\', "/");
        if cleaned.ends_with('/') {
            let prefix = cleaned.trim_end_matches('/');
            normalized == prefix || normalized.starts_with(&format!("{prefix}/")) || name == prefix
        } else {
            matches_glob(&cleaned, &normalized, name)
        }
    })
}

fn matches_glob(pattern: &str, relative: &str, name: &str) -> bool {
    let target = if pattern.contains('/') {
        relative
    } else {
        name
    };
    let regex_source = format!("^{}$", glob_to_regex(pattern));
    Regex::new(&regex_source)
        .map(|regex| regex.is_match(target))
        .unwrap_or(false)
}

fn glob_to_regex(pattern: &str) -> String {
    let mut regex = String::new();
    let mut chars = pattern.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '*' if chars.peek() == Some(&'*') => {
                let _ = chars.next();
                regex.push_str(".*");
            }
            '*' => regex.push_str("[^/]*"),
            '?' => regex.push_str("[^/]"),
            '.' | '+' | '(' | ')' | '|' | '^' | '$' | '[' | ']' | '{' | '}' | '\\' => {
                regex.push('\\');
                regex.push(ch);
            }
            other => regex.push(other),
        }
    }
    regex
}

fn path_mtime(path: &Path) -> f64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs_f64())
        .unwrap_or(0.0)
}

fn clip_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut clipped = text.chars().take(max_chars).collect::<String>();
    clipped.push_str("...");
    clipped
}

fn code_search_output(
    root: &Path,
    base: &Path,
    hits: Vec<String>,
    preview_hits: Vec<String>,
    truncated: bool,
) -> ToolResultValue<ToolOutput> {
    let mut output = ToolOutput::new(display_path(root, base), hits.join("\n"));
    output
        .metadata
        .insert("count".to_string(), json!(hits.len()));
    output
        .metadata
        .insert("returned_count".to_string(), json!(hits.len()));
    output
        .metadata
        .insert("preview".to_string(), json!(preview_hits.join("\n")));
    output.truncated = truncated;
    Ok(output)
}

fn save_todos(ctx: &mut ToolContext, todos: Vec<TodoItem>) -> ToolResultValue<()> {
    ctx.todos = todos.clone();
    let path = todo_storage_path(ctx);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error)?;
    }
    let encoded = serde_json::to_string_pretty(&todos).map_err(|error| error.to_string())?;
    fs::write(path, encoded).map_err(io_error)
}

fn load_todos(ctx: &mut ToolContext) -> ToolResultValue<Vec<TodoItem>> {
    if !ctx.todos.is_empty() {
        return Ok(ctx.todos.clone());
    }
    let path = todo_storage_path(ctx);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(path).map_err(io_error)?;
    let todos =
        serde_json::from_str::<Vec<TodoItem>>(&content).map_err(|error| error.to_string())?;
    ctx.todos = todos.clone();
    Ok(todos)
}

fn todo_storage_path(ctx: &ToolContext) -> PathBuf {
    let session_key = if ctx.session_id.is_empty() {
        "default"
    } else {
        &ctx.session_id
    };
    let safe_key = session_key
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '.' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    ctx.session_root
        .join(".openagent")
        .join("todo")
        .join(format!("{safe_key}.json"))
}

fn todo_from_value(value: &Value) -> ToolResultValue<TodoItem> {
    Ok(TodoItem::new(
        value_string(value, "content")?,
        value
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending"),
        value
            .get("priority")
            .and_then(Value::as_str)
            .unwrap_or("medium"),
        value.get("id").and_then(Value::as_str).unwrap_or_default(),
    ))
}

fn todo_output(todos: Vec<TodoItem>) -> ToolResultValue<ToolOutput> {
    let open_count = todos
        .iter()
        .filter(|todo| todo.status != "completed")
        .count();
    let output_text = serde_json::to_string_pretty(&todos).map_err(|error| error.to_string())?;
    let mut output = ToolOutput::new(format!("{open_count} todos"), output_text);
    output.metadata.insert("todos".to_string(), json!(todos));
    Ok(output)
}

fn question_metadata_value(value: &Value) -> ToolResultValue<Value> {
    let options = value
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| "Expected list input for options".to_string())?;
    let option_values = options
        .iter()
        .map(|option| {
            Ok(json!({
                "label": value_string(option, "label")?,
                "description": value_string(option, "description")?,
            }))
        })
        .collect::<ToolResultValue<Vec<_>>>()?;
    Ok(json!({
        "header": value_string(value, "header")?,
        "question": value_string(value, "question")?,
        "multiple": value.get("multiple").and_then(Value::as_bool).unwrap_or(false),
        "options": option_values,
    }))
}

fn skill_roots(ctx: &ToolContext) -> Option<Vec<String>> {
    let roots = ctx
        .agent_options
        .get("skill_roots")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    (!roots.is_empty()).then_some(roots)
}

fn report_metadata(
    report: &SkillDiscoveryReport,
    query: Option<String>,
) -> BTreeMap<String, Value> {
    let mut payload = BTreeMap::from([
        ("skill_count".to_string(), json!(report.skills.len())),
        ("loaded_count".to_string(), json!(report.loaded_count)),
        ("scanned_files".to_string(), json!(report.scanned_files)),
        ("invalid_count".to_string(), json!(report.invalid_count)),
        ("duplicate_count".to_string(), json!(report.duplicate_count)),
    ]);
    if let Some(query) = query {
        payload.insert("query".to_string(), json!(query));
    }
    if !report.issues.is_empty() {
        payload.insert(
            "issues".to_string(),
            Value::Array(
                report
                    .issues
                    .iter()
                    .map(|issue| {
                        json!({
                            "kind": issue.kind,
                            "path": issue.path,
                            "message": issue.message,
                            "duplicate_of": issue.duplicate_of,
                        })
                    })
                    .collect(),
            ),
        );
    }
    payload
}

fn diagnostic_lines(report: &SkillDiscoveryReport) -> Vec<String> {
    if report.issues.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![String::new(), "Diagnostics:".to_string()];
    for issue in &report.issues {
        let suffix = issue
            .duplicate_of
            .as_ref()
            .map(|path| format!(" duplicate_of={path}"))
            .unwrap_or_default();
        lines.push(format!(
            "- {}: {} - {}{}",
            issue.kind, issue.path, issue.message, suffix
        ));
    }
    lines
}

fn title_for_questions(count: usize) -> String {
    if count == 1 {
        "Asked 1 question".to_string()
    } else {
        format!("Asked {count} questions")
    }
}

fn string_arg(input: &Value, key: &str) -> ToolResultValue<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("Missing required parameter: {key}"))
}

fn string_arg_or(input: &Value, key: &str, default: &str) -> ToolResultValue<String> {
    input
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("Expected string input for {key}"))
        })
        .unwrap_or_else(|| Ok(default.to_string()))
}

fn optional_string_arg(input: &Value, key: &str) -> ToolResultValue<Option<String>> {
    input
        .get(key)
        .map(|value| {
            if value.is_null() {
                Ok(None)
            } else {
                value
                    .as_str()
                    .map(|text| Some(text.to_string()))
                    .ok_or_else(|| format!("Expected string input for {key}"))
            }
        })
        .unwrap_or(Ok(None))
}

fn value_string(input: &Value, key: &str) -> ToolResultValue<String> {
    input
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("Missing required parameter: {key}"))
}

fn bool_arg(input: &Value, key: &str, default: bool) -> ToolResultValue<bool> {
    input
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| format!("Expected boolean input for {key}"))
        })
        .unwrap_or(Ok(default))
}

fn usize_arg(input: &Value, key: &str, default: usize) -> ToolResultValue<usize> {
    input
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .and_then(|item| usize::try_from(item).ok())
                .ok_or_else(|| format!("Expected non-negative integer input for {key}"))
        })
        .unwrap_or(Ok(default))
}

fn optional_usize_arg(input: &Value, key: &str) -> ToolResultValue<Option<usize>> {
    input
        .get(key)
        .map(|value| {
            if value.is_null() {
                Ok(None)
            } else {
                value
                    .as_u64()
                    .and_then(|item| usize::try_from(item).ok())
                    .map(Some)
                    .ok_or_else(|| format!("Expected non-negative integer input for {key}"))
            }
        })
        .unwrap_or(Ok(None))
}

fn u64_arg(input: &Value, key: &str, default: u64) -> ToolResultValue<u64> {
    input
        .get(key)
        .map(|value| {
            value
                .as_u64()
                .ok_or_else(|| format!("Expected non-negative integer input for {key}"))
        })
        .unwrap_or(Ok(default))
}

fn string_list_arg(input: &Value, key: &str) -> ToolResultValue<Vec<String>> {
    let Some(value) = input.get(key) else {
        return Ok(Vec::new());
    };
    if value.is_null() {
        return Ok(Vec::new());
    }
    let items = value
        .as_array()
        .ok_or_else(|| format!("Expected list input for {key}"))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("Expected string item in {key}"))
        })
        .collect()
}

fn write_truncated_output(root: &Path, call_id: &str, content: &str) -> ToolResultValue<PathBuf> {
    let output_dir = root.join(".openagent").join("tool_output");
    fs::create_dir_all(&output_dir).map_err(io_error)?;
    let output_path = output_dir.join(format!("{call_id}.txt"));
    fs::write(&output_path, content).map_err(io_error)?;
    Ok(output_path)
}

#[cfg(unix)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("sh");
    shell.arg("-c").arg(command);
    shell
}

#[cfg(windows)]
fn shell_command(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_to_protocol_crate() {
        assert_eq!(crate_name(), "openagent-tools");
        assert_eq!(protocol_crate_name(), "openagent-protocol");
    }
}
