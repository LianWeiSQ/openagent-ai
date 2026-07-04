use super::*;

#[derive(Clone, Debug, Default)]
pub(crate) struct RunAgentProfile {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) description: Option<String>,
    pub(super) mode: String,
    pub(super) model: Option<String>,
    pub(super) provider: Option<String>,
    pub(super) permission: Option<String>,
    pub(super) task_permissions: Vec<TaskPermissionRule>,
    pub(super) skills: Vec<String>,
    pub(super) skill_roots: Vec<String>,
    pub(super) skill_permissions: Vec<SkillPermissionRule>,
    pub(super) prompt: Option<String>,
    pub(super) tools: Vec<String>,
    pub(super) max_steps: Option<u64>,
    pub(super) temperature: Option<f64>,
    pub(super) top_p: Option<f64>,
    pub(super) color: Option<String>,
    pub(super) disabled: bool,
    pub(super) model_options: BTreeMap<String, Value>,
    pub(super) workspace_isolation: bool,
    pub(super) hidden: bool,
    pub(super) source_path: Option<PathBuf>,
    pub(super) loaded: bool,
}

const BUILD_AGENT_PROMPT: &str = include_str!("../../../skill/prompts/build.txt");
const EXPLORE_AGENT_PROMPT: &str = include_str!("../../../skill/prompts/explore.txt");
const PLAN_AGENT_PROMPT: &str = include_str!("../../../skill/prompts/plan.txt");
const SCOUT_AGENT_PROMPT: &str = include_str!("../../../skill/prompts/scout.txt");

pub(super) fn provider_and_model_from_args(
    args: &[String],
    agent_profile: Option<&RunAgentProfile>,
) -> (String, String) {
    if let Some(raw) = value_for(args, &["--model", "-m"])
        && let Some((provider, model)) = raw.split_once('/')
        && !provider.is_empty()
        && !model.is_empty()
    {
        let provider = normalize_provider(Some(provider)).unwrap_or_else(|_| provider.to_string());
        return (provider, model.to_string());
    }
    if value_for(args, &["--model", "-m"]).is_none()
        && let Some(raw) = agent_profile.and_then(|profile| profile.model.as_deref())
        && let Some((provider, model)) = raw.split_once('/')
        && !provider.is_empty()
        && !model.is_empty()
    {
        let provider = normalize_provider(Some(provider)).unwrap_or_else(|_| provider.to_string());
        return (provider, model.to_string());
    }
    let provider = value_for(args, &["--provider"])
        .or_else(|| agent_profile.and_then(|profile| profile.provider.clone()))
        .unwrap_or_else(active_provider);
    let provider = normalize_provider(Some(&provider)).unwrap_or(provider);
    let model = value_for(args, &["--model", "-m"])
        .or_else(|| agent_profile.and_then(|profile| profile.model.clone()))
        .or_else(|| provider_env_value(&provider, "model"))
        .unwrap_or_else(|| default_model_for_provider(&provider));
    (provider, model)
}

pub(super) fn provider_and_model_for_subagent(
    parent_provider: &str,
    parent_model: &str,
    agent_profile: &RunAgentProfile,
) -> (String, String) {
    if let Some(raw) = agent_profile.model.as_deref()
        && let Some((provider, model)) = raw.split_once('/')
        && !provider.is_empty()
        && !model.is_empty()
    {
        let provider = normalize_provider(Some(provider)).unwrap_or_else(|_| provider.to_string());
        return (provider, model.to_string());
    }
    let provider = agent_profile
        .provider
        .as_deref()
        .map(str::to_string)
        .unwrap_or_else(|| parent_provider.to_string());
    let provider = normalize_provider(Some(&provider)).unwrap_or(provider);
    let model = agent_profile
        .model
        .clone()
        .unwrap_or_else(|| parent_model.to_string());
    (provider, model)
}

pub(super) fn load_agent_profile_from_args(
    args: &[String],
    _workspace: &Path,
) -> Result<Option<RunAgentProfile>, String> {
    let Some(raw_name) = value_for(args, &["--agent"]) else {
        return Ok(None);
    };
    Ok(Some(load_agent_profile_by_name(args, &raw_name)?))
}

pub(crate) fn load_agent_profile_by_name(
    args: &[String],
    raw_name: &str,
) -> Result<RunAgentProfile, String> {
    let agent_id = sanitize_identifier(raw_name);
    for path in agent_profile_path_candidates(args, &agent_id) {
        if let Some(profile) = load_agent_profile_from_path(&path, &agent_id, raw_name)? {
            if profile.disabled {
                return Err(format!("agent profile {raw_name} is disabled"));
            }
            return Ok(profile);
        }
    }
    available_agent_profiles(args)
        .into_iter()
        .find(|profile| {
            profile.id == agent_id
                || sanitize_identifier(&profile.name) == agent_id
                || profile.name.eq_ignore_ascii_case(raw_name)
        })
        .ok_or_else(|| format!("agent profile not found: {raw_name}"))
}

pub(crate) fn available_agent_profiles(args: &[String]) -> Vec<RunAgentProfile> {
    let mut profiles = builtin_agent_profiles()
        .into_iter()
        .map(|profile| (profile.id.clone(), profile))
        .collect::<BTreeMap<_, _>>();
    let mut paths = agent_registry_dirs(args)
        .into_iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| agent_profile_file_kind(path).is_some())
        .collect::<Vec<_>>();
    paths.sort();
    for path in paths {
        let fallback_id = path
            .file_stem()
            .and_then(|value| value.to_str())
            .map(sanitize_identifier)
            .unwrap_or_else(|| "agent".to_string());
        if let Ok(Some(profile)) = load_agent_profile_from_path(&path, &fallback_id, &fallback_id)
            && !profile.disabled
        {
            profiles.insert(profile.id.clone(), profile);
        }
    }
    profiles.into_values().collect()
}

fn agent_registry_dirs(args: &[String]) -> Vec<PathBuf> {
    let workspace = workspace_from_args(args);
    vec![
        agent_registry_dir(args),
        workspace.join(".opencode/agents"),
        workspace.join(".opencode/agent"),
    ]
}

fn agent_profile_path_candidates(args: &[String], agent_id: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for dir in agent_registry_dirs(args) {
        for extension in ["json", "md", "markdown"] {
            paths.push(dir.join(format!("{agent_id}.{extension}")));
        }
    }
    paths
}

fn agent_profile_file_kind(path: &Path) -> Option<&'static str> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("json") => Some("json"),
        Some("md" | "markdown") => Some("markdown"),
        _ => None,
    }
}

fn load_agent_profile_from_path(
    path: &Path,
    fallback_id: &str,
    fallback_name: &str,
) -> Result<Option<RunAgentProfile>, String> {
    let Some(kind) = agent_profile_file_kind(path) else {
        return Ok(None);
    };
    let value = if kind == "json" {
        read_json_file(path)
    } else {
        match markdown_agent_profile_value(path) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        }
    };
    if value.as_object().is_none_or(Map::is_empty) {
        return Ok(None);
    }
    agent_profile_from_value(
        &value,
        Some(path.to_path_buf()),
        fallback_id,
        fallback_name,
        true,
    )
    .map(Some)
}

fn markdown_agent_profile_value(path: &Path) -> Result<Value, String> {
    let raw = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut value = json!({});
    let mut body = raw.as_str();
    if let Some(rest) = raw.trim_start_matches('\u{feff}').strip_prefix("---")
        && let Some((frontmatter, tail)) = rest.split_once("---")
    {
        value = serde_yaml::from_str::<Value>(frontmatter).unwrap_or_else(|_| json!({}));
        body = tail.trim_start_matches('\n');
    }
    if value.as_object().is_none() {
        value = json!({});
    }
    if let Some(object) = value.as_object_mut() {
        let prompt = body.trim_start_matches('\n').trim_end();
        if !prompt.trim().is_empty() && !object.contains_key("prompt") {
            object.insert("prompt".to_string(), json!(prompt));
        }
    }
    Ok(value)
}

pub(super) fn available_subagent_profiles(
    args: &[String],
    include_hidden: bool,
) -> Vec<RunAgentProfile> {
    available_agent_profiles(args)
        .into_iter()
        .filter(|profile| is_subagent_mode(&profile.mode))
        .filter(|profile| include_hidden || !profile.hidden)
        .collect()
}

pub(super) fn task_subagent_descriptors(
    args: &[String],
    agent_profile: Option<&RunAgentProfile>,
    parent_session: Option<&Session>,
) -> Vec<TaskSubagentDescriptor> {
    available_subagent_profiles(args, false)
        .into_iter()
        .filter(|profile| {
            agent_profile.is_none_or(|parent| {
                task_subagent_is_visible(&parent.task_permissions, &profile.id)
            })
        })
        .filter(|profile| {
            parent_session
                .is_none_or(|session| subagent_task_governance_error(session, profile).is_none())
        })
        .map(|profile| TaskSubagentDescriptor {
            id: profile.id,
            name: profile.name,
            description: profile.description.unwrap_or_default(),
        })
        .collect()
}

pub(super) fn max_subagent_depth_cli() -> u64 {
    std::env::var("OPENAGENT_MAX_SUBAGENT_DEPTH")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(3)
        .max(1)
}

pub(super) fn child_task_depth(parent_session: &Session) -> u64 {
    if parent_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        parent_session
            .metadata
            .get("task_depth")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .saturating_add(1)
    } else {
        1
    }
}

pub(super) fn task_root_session_id(parent_session: &Session) -> String {
    if parent_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        parent_session
            .metadata
            .get("task_root_session_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(parent_session.id.as_str())
            .to_string()
    } else {
        parent_session.id.clone()
    }
}

pub(super) fn parent_task_lineage(parent_session: &Session) -> Vec<String> {
    parent_session
        .metadata
        .get("task_lineage_subagents")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| {
            parent_session
                .metadata
                .get("agent")
                .and_then(Value::as_str)
                .filter(|_| {
                    parent_session
                        .metadata
                        .get("subagent")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .map(|agent| vec![agent.to_string()])
                .unwrap_or_default()
        })
}

pub(super) fn child_task_lineage(parent_session: &Session, child_agent: &str) -> Vec<String> {
    let mut lineage = parent_task_lineage(parent_session);
    lineage.push(child_agent.to_string());
    lineage
}

pub(super) fn subagent_task_governance_error(
    parent_session: &Session,
    profile: &RunAgentProfile,
) -> Option<String> {
    let lineage = parent_task_lineage(parent_session);
    let parent_agent = parent_session
        .metadata
        .get("agent")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if parent_session
        .metadata
        .get("subagent")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && parent_agent == profile.id
    {
        return Some(format!("subagent {} cannot call itself", profile.id));
    }
    if lineage.iter().any(|agent| agent == &profile.id) {
        return Some(format!(
            "subagent {} is already in task lineage",
            profile.id
        ));
    }
    let next_depth = child_task_depth(parent_session);
    let max_depth = max_subagent_depth_cli();
    if next_depth > max_depth {
        return Some(format!(
            "subagent nesting depth {next_depth} exceeds max subagent depth {max_depth}"
        ));
    }
    None
}

fn agent_profile_from_value(
    value: &Value,
    source_path: Option<PathBuf>,
    fallback_id: &str,
    fallback_name: &str,
    loaded: bool,
) -> Result<RunAgentProfile, String> {
    let mode = value
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("primary")
        .to_ascii_lowercase();
    if !matches!(mode.as_str(), "primary" | "subagent" | "all") {
        return Err(format!(
            "agent profile {fallback_id} has invalid mode '{mode}'; expected primary, subagent, or all"
        ));
    }
    Ok(RunAgentProfile {
        id: value
            .get("id")
            .and_then(Value::as_str)
            .map(sanitize_identifier)
            .unwrap_or_else(|| sanitize_identifier(fallback_id)),
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(fallback_name)
            .to_string(),
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(str::to_string),
        mode,
        model: value
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        provider: value
            .get("provider")
            .and_then(Value::as_str)
            .map(str::to_string),
        permission: profile_permission_ruleset_value(value).map(str::to_string),
        task_permissions: profile_task_permissions(value),
        skills: profile_string_list(value.get("skills")),
        skill_roots: profile_string_list(value.get("skill_roots")),
        skill_permissions: profile_skill_permissions(value),
        prompt: value
            .get("prompt")
            .and_then(Value::as_str)
            .map(str::to_string),
        tools: profile_string_list(value.get("tools")),
        max_steps: value
            .get("max_steps")
            .or_else(|| value.get("steps"))
            .or_else(|| value.get("maxSteps"))
            .and_then(Value::as_u64),
        temperature: value.get("temperature").and_then(Value::as_f64),
        top_p: value
            .get("top_p")
            .or_else(|| value.get("topP"))
            .and_then(Value::as_f64),
        color: value
            .get("color")
            .and_then(Value::as_str)
            .map(str::to_string),
        disabled: value
            .get("disabled")
            .or_else(|| value.get("disable"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        model_options: profile_model_options(value),
        workspace_isolation: value
            .get("workspace_isolation")
            .or_else(|| value.get("isolate_workspace"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        hidden: value
            .get("hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        source_path,
        loaded,
    })
}

fn profile_string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(item)) => item
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn profile_model_options(value: &Value) -> BTreeMap<String, Value> {
    const KNOWN_KEYS: &[&str] = &[
        "id",
        "name",
        "description",
        "mode",
        "model",
        "provider",
        "permission",
        "task",
        "task_permissions",
        "skill",
        "skills",
        "skill_roots",
        "skill_permissions",
        "skill_permission",
        "tools",
        "prompt",
        "steps",
        "max_steps",
        "maxSteps",
        "hidden",
        "color",
        "disabled",
        "disable",
        "temperature",
        "top_p",
        "topP",
        "model_options",
        "options",
        "workspace_isolation",
        "isolate_workspace",
        "source_path",
        "loaded",
    ];
    let mut options = BTreeMap::new();
    if let Some(object) = value.as_object() {
        for (key, item) in object {
            if !KNOWN_KEYS.contains(&key.as_str()) {
                options.insert(key.clone(), item.clone());
            }
        }
    }
    if let Some(temperature) = value.get("temperature").and_then(Value::as_f64) {
        options.insert("temperature".to_string(), json!(temperature));
    }
    if let Some(top_p) = value
        .get("top_p")
        .or_else(|| value.get("topP"))
        .and_then(Value::as_f64)
    {
        options.insert("top_p".to_string(), json!(top_p));
    }
    for key in ["model_options", "options"] {
        if let Some(object) = value.get(key).and_then(Value::as_object) {
            for (option_key, option_value) in object {
                if !profile_model_option_key_reserved(option_key) {
                    options.insert(option_key.clone(), option_value.clone());
                }
            }
        }
    }
    options
}

fn profile_model_option_key_reserved(key: &str) -> bool {
    matches!(
        key,
        "skill" | "skills" | "skill_roots" | "skill_permissions" | "skill_permission"
    )
}

fn builtin_agent_profiles() -> Vec<RunAgentProfile> {
    vec![
        builtin_agent_profile(
            "build",
            "Build",
            "Primary implementation agent for coding, testing, and general project work.",
            "primary",
            Some("PLAN_ONLY"),
            BUILD_AGENT_PROMPT,
            &[],
        ),
        builtin_agent_profile(
            "general",
            "General",
            "General-purpose subagent for complex multi-step implementation, debugging, and research tasks.",
            "subagent",
            Some("PLAN_ONLY"),
            BUILD_AGENT_PROMPT,
            &[],
        ),
        builtin_agent_profile(
            "explore",
            "Explore",
            "Read-only code exploration subagent for fast search, mapping, and evidence gathering.",
            "subagent",
            Some("READONLY"),
            EXPLORE_AGENT_PROMPT,
            &[
                "read",
                "glob",
                "grep",
                "ls",
                "code_search",
                "skill",
                "todoread",
            ],
        ),
        builtin_agent_profile(
            "scout",
            "Scout",
            "External documentation and dependency research subagent with read-only web fetch access.",
            "subagent",
            Some("READONLY"),
            SCOUT_AGENT_PROMPT,
            &[
                "web_fetch",
                "read",
                "glob",
                "grep",
                "ls",
                "code_search",
                "skill",
                "todoread",
            ],
        ),
        builtin_agent_profile(
            "plan",
            "Plan",
            "Planning subagent for architecture analysis, implementation strategy, and task breakdowns.",
            "subagent",
            Some("PLAN_ONLY"),
            PLAN_AGENT_PROMPT,
            &[
                "read",
                "glob",
                "grep",
                "ls",
                "code_search",
                "skill",
                "todoread",
                "todowrite",
                "question",
            ],
        ),
    ]
}

fn builtin_agent_profile(
    id: &str,
    name: &str,
    description: &str,
    mode: &str,
    permission: Option<&str>,
    prompt: &str,
    tools: &[&str],
) -> RunAgentProfile {
    RunAgentProfile {
        id: id.to_string(),
        name: name.to_string(),
        description: Some(description.to_string()),
        mode: mode.to_string(),
        model: None,
        provider: None,
        permission: permission.map(str::to_string),
        task_permissions: Vec::new(),
        skills: Vec::new(),
        skill_roots: Vec::new(),
        skill_permissions: Vec::new(),
        prompt: Some(prompt.trim_start_matches('\u{feff}').to_string()),
        tools: tools.iter().map(|item| (*item).to_string()).collect(),
        max_steps: None,
        temperature: None,
        top_p: None,
        color: None,
        disabled: false,
        model_options: BTreeMap::new(),
        workspace_isolation: false,
        hidden: false,
        source_path: None,
        loaded: true,
    }
}

pub(crate) fn agent_profile_public_value(profile: &RunAgentProfile) -> Value {
    json!({
        "id": profile.id.clone(),
        "name": profile.name.clone(),
        "description": profile.description.clone(),
        "mode": profile.mode.clone(),
        "model": profile.model.clone(),
        "provider": profile.provider.clone(),
        "permission": profile.permission.clone(),
        "task_permissions": profile.task_permissions.clone(),
        "skills": profile.skills.clone(),
        "skill_roots": profile.skill_roots.clone(),
        "skill_permissions": profile.skill_permissions.clone(),
        "tools": profile.tools.clone(),
        "max_steps": profile.max_steps,
        "steps": profile.max_steps,
        "temperature": profile.temperature,
        "top_p": profile.top_p,
        "color": profile.color.clone(),
        "disabled": profile.disabled,
        "model_options": profile.model_options.clone(),
        "workspace_isolation": profile.workspace_isolation,
        "hidden": profile.hidden,
        "loaded": profile.loaded,
        "source_path": profile.source_path.as_ref().map(|path| path.to_string_lossy().to_string()),
    })
}

pub(super) fn bind_agent_profile_system_prompt(
    session: &mut Session,
    store: &FileSessionStore,
    run_id: &str,
    profile: Option<&RunAgentProfile>,
) -> Result<(), String> {
    let Some(profile) = profile else {
        return Ok(());
    };
    let already_bound = session.messages.iter().any(|message| {
        message.role == Role::System
            && message
                .metadata
                .get("agent_profile")
                .and_then(Value::as_str)
                == Some(profile.id.as_str())
    });
    if already_bound {
        return Ok(());
    }
    let mut prompt_parts = Vec::new();
    if let Some(prompt) = profile
        .prompt
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        prompt_parts.push(prompt.to_string());
    }
    let preloaded_skills = profile_preloaded_skill_documents(profile, &session.directory);
    let preloaded_skill_names = preloaded_skills
        .iter()
        .map(|skill| skill.name.clone())
        .collect::<Vec<_>>();
    if let Some(skills) = render_preloaded_skills(&preloaded_skills) {
        prompt_parts.push(skills);
    }
    if agent_allows_tool(profile, "skill") {
        let registry = SkillRegistry::new_with_options(
            Some(session.directory.clone()),
            (!profile.skill_roots.is_empty()).then_some(profile.skill_roots.clone()),
            Option::<PathBuf>::None,
            SkillRegistryOptions {
                include_builtin_skills: true,
            },
        );
        let skills = registry
            .all()
            .into_iter()
            .filter(|skill| skill_is_visible(&profile.skill_permissions, &skill.name))
            .collect::<Vec<_>>();
        if let Some(skills) = render_available_skills(&skills) {
            prompt_parts.push(skills);
        }
    }
    if prompt_parts.is_empty() {
        return Ok(());
    }
    if !profile.skills.is_empty() {
        session
            .metadata
            .insert("skills".to_string(), json!(profile.skills.clone()));
    }
    if !preloaded_skill_names.is_empty() {
        session.metadata.insert(
            "preloaded_skills".to_string(),
            json!(preloaded_skill_names.clone()),
        );
    }
    let mut message = chat_message(Role::System, prompt_parts.join("\n\n"));
    message
        .metadata
        .insert("agent_profile".to_string(), json!(profile.id.clone()));
    message
        .metadata
        .insert("agent_mode".to_string(), json!(profile.mode.clone()));
    if !preloaded_skill_names.is_empty() {
        message
            .metadata
            .insert("preloaded_skills".to_string(), json!(preloaded_skill_names));
    }
    let index = session.messages.len() as u64;
    session.add(message.clone());
    store
        .append_message(session, &message, run_id, index)
        .map_err(|error| format!("failed to record agent system prompt: {error}"))
}

pub(super) fn filter_tools_for_agent(
    tools: Vec<ToolSchema>,
    agent_profile: Option<&RunAgentProfile>,
) -> Vec<ToolSchema> {
    let Some(profile) = agent_profile else {
        return tools;
    };
    if profile.tools.is_empty() {
        return tools;
    }
    tools
        .into_iter()
        .filter(|tool| {
            profile
                .tools
                .iter()
                .any(|pattern| wildcard_match(pattern, &tool.name))
        })
        .collect()
}

fn profile_preloaded_skill_documents(
    profile: &RunAgentProfile,
    session_root: &Path,
) -> Vec<SkillDocument> {
    if profile.skills.is_empty() {
        return Vec::new();
    }
    let registry = SkillRegistry::new_with_options(
        Some(session_root.to_path_buf()),
        (!profile.skill_roots.is_empty()).then_some(profile.skill_roots.clone()),
        Option::<PathBuf>::None,
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    );
    let mut seen = BTreeSet::new();
    profile
        .skills
        .iter()
        .filter_map(|name| {
            let name = name.trim();
            if name.is_empty()
                || !seen.insert(name.to_string())
                || !skill_is_visible(&profile.skill_permissions, name)
            {
                return None;
            }
            registry.get(name).filter(skill_document_model_invocable)
        })
        .collect()
}

pub(super) fn agent_tool_options(
    agent_profile: Option<&RunAgentProfile>,
) -> BTreeMap<String, Value> {
    let mut options = BTreeMap::new();
    if let Some(profile) = agent_profile {
        options.insert("agent_id".to_string(), json!(profile.id.clone()));
        options.insert("agent".to_string(), json!(profile.id.clone()));
        if !profile.skills.is_empty() {
            options.insert("skills".to_string(), json!(profile.skills.clone()));
        }
        if !profile.skill_roots.is_empty() {
            options.insert(
                "skill_roots".to_string(),
                json!(profile.skill_roots.clone()),
            );
        }
        if !profile.skill_permissions.is_empty() {
            options.insert(
                "skill_permissions".to_string(),
                json!(profile.skill_permissions.clone()),
            );
        }
    }
    options
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
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

fn agent_allows_tool(profile: &RunAgentProfile, tool_name: &str) -> bool {
    profile.tools.is_empty()
        || profile
            .tools
            .iter()
            .any(|pattern| wildcard_match(pattern, tool_name))
}

pub(super) fn permission_ruleset_from_args(
    args: &[String],
    agent_profile: Option<&RunAgentProfile>,
) -> Result<PermissionRuleset, String> {
    let raw = value_for(args, &["--permission"])
        .or_else(|| agent_profile.and_then(|profile| profile.permission.clone()))
        .unwrap_or_else(|| "PLAN_ONLY".to_string());
    parse_permission_ruleset(&raw)
}

pub(super) fn permission_manager_for_agent(
    ruleset: PermissionRuleset,
    agent_profile: Option<&RunAgentProfile>,
) -> PermissionManager {
    let mut manager = PermissionManager::new();
    manager.set_ruleset(ruleset);
    if let Some(profile) = agent_profile {
        for rule in &profile.task_permissions {
            manager.add_rule(permission_rule(
                TASK_TOOL_ID,
                rule.action.clone(),
                Some(&rule.pattern),
            ));
        }
        for rule in &profile.skill_permissions {
            manager.add_rule(permission_rule(
                "skill",
                rule.action.clone(),
                Some(&rule.pattern),
            ));
        }
    }
    manager
}

pub(super) fn permission_ruleset_for_profile(
    agent_profile: &RunAgentProfile,
    fallback: PermissionRuleset,
) -> Result<PermissionRuleset, String> {
    agent_profile
        .permission
        .as_deref()
        .map(parse_permission_ruleset)
        .transpose()
        .map(|value| value.unwrap_or(fallback))
}

pub(super) fn is_subagent_mode(mode: &str) -> bool {
    matches!(mode, "subagent" | "all")
}

pub(super) fn parse_permission_ruleset(raw: &str) -> Result<PermissionRuleset, String> {
    match raw.trim().to_ascii_uppercase().replace('-', "_").as_str() {
        "FULL" | "ALLOW" | "AUTO" => Ok(PermissionRuleset::Full),
        "READONLY" | "READ_ONLY" => Ok(PermissionRuleset::Readonly),
        "PLAN_ONLY" | "ASK" => Ok(PermissionRuleset::PlanOnly),
        "NONE" | "DENY" => Ok(PermissionRuleset::None),
        _ => Err("permission must be FULL, READONLY, PLAN_ONLY, or NONE".to_string()),
    }
}

fn profile_permission_ruleset_value(value: &Value) -> Option<&str> {
    let permission = value.get("permission")?;
    if let Some(raw) = permission.as_str() {
        return Some(raw);
    }
    permission
        .get("ruleset")
        .or_else(|| permission.get("default"))
        .or_else(|| permission.get("mode"))
        .and_then(Value::as_str)
}

fn profile_task_permissions(value: &Value) -> Vec<TaskPermissionRule> {
    let Some(task) = value
        .get("permission")
        .and_then(|permission| permission.get("task"))
        .or_else(|| value.get("task_permissions"))
        .or_else(|| value.get("task_permission"))
    else {
        return Vec::new();
    };
    parse_task_permission_rules(task)
}

fn profile_skill_permissions(value: &Value) -> Vec<SkillPermissionRule> {
    let mut rules = Vec::new();
    for skill in [
        value
            .get("permission")
            .and_then(|permission| permission.get("skill")),
        value.get("skill_permissions"),
        value.get("skill_permission"),
    ]
    .into_iter()
    .flatten()
    {
        rules.extend(parse_skill_permission_rules(skill));
    }
    rules
}

fn parse_task_permission_rules(value: &Value) -> Vec<TaskPermissionRule> {
    if let Some(object) = value.as_object() {
        return object
            .iter()
            .filter_map(|(pattern, action)| {
                task_permission_action(action).map(|action| TaskPermissionRule {
                    pattern: pattern.clone(),
                    action,
                })
            })
            .collect();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let pattern = item
                .get("pattern")
                .or_else(|| item.get("subagent"))
                .or_else(|| item.get("agent"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let action = item.get("action").and_then(task_permission_action)?;
            Some(TaskPermissionRule {
                pattern: pattern.to_string(),
                action,
            })
        })
        .collect()
}

fn parse_skill_permission_rules(value: &Value) -> Vec<SkillPermissionRule> {
    if let Some(object) = value.as_object() {
        return object
            .iter()
            .filter_map(|(pattern, action)| {
                task_permission_action(action).map(|action| SkillPermissionRule {
                    pattern: pattern.clone(),
                    action,
                })
            })
            .collect();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let pattern = item
                .get("pattern")
                .or_else(|| item.get("skill"))
                .or_else(|| item.get("name"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            let action = item.get("action").and_then(task_permission_action)?;
            Some(SkillPermissionRule {
                pattern: pattern.to_string(),
                action,
            })
        })
        .collect()
}

fn task_permission_action(value: &Value) -> Option<PermissionAction> {
    let raw = value.as_str()?;
    match raw.trim().to_ascii_lowercase().as_str() {
        "allow" | "allowed" => Some(PermissionAction::Allow),
        "deny" | "denied" => Some(PermissionAction::Deny),
        "ask" | "prompt" => Some(PermissionAction::Ask),
        _ => None,
    }
}
