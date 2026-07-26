use super::*;
use std::ffi::OsStr;

const PLUGIN_STATE_SCHEMA: &str = "openagent.extensions.v1";
const PLUGIN_STATE_FILE: &str = ".openagent-runtime/extensions.json";
const PLUGIN_STORE_DIR: &str = ".openagent-runtime/plugins";
const PLUGIN_STAGE_DIR: &str = ".openagent-runtime/plugin-staging";
const MAX_PLUGIN_FILES: u64 = 4096;
const MAX_PLUGIN_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct ExtensionState {
    #[serde(default = "extension_state_schema")]
    schema_version: String,
    #[serde(default)]
    plugins: BTreeMap<String, ManagedPlugin>,
    #[serde(default)]
    skill_overrides: BTreeMap<String, bool>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ManagedPlugin {
    id: String,
    name: String,
    version: String,
    description: String,
    source: String,
    source_type: String,
    install_path: String,
    enabled: bool,
    skills: Vec<String>,
    skill_roots: Vec<String>,
    permissions: Vec<String>,
    installed_at_ms: u64,
    updated_at_ms: u64,
}

#[derive(Clone, Debug)]
struct InspectedPlugin {
    id: String,
    name: String,
    version: String,
    description: String,
    skills: Vec<String>,
    permissions: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct PluginRuntimeOptions {
    pub(super) extra_skill_roots: Vec<String>,
    pub(super) disabled_skills: Vec<String>,
    pub(super) enabled_plugins: Vec<String>,
}

fn extension_state_schema() -> String {
    PLUGIN_STATE_SCHEMA.to_string()
}

fn extension_state_path(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config).join(PLUGIN_STATE_FILE)
}

fn plugin_store(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config).join(PLUGIN_STORE_DIR)
}

fn plugin_stage_store(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config).join(PLUGIN_STAGE_DIR)
}

fn read_extension_state(config: &HttpRuntimeConfig) -> ExtensionState {
    fs::read_to_string(extension_state_path(config))
        .ok()
        .and_then(|raw| serde_json::from_str::<ExtensionState>(&raw).ok())
        .unwrap_or_else(|| ExtensionState {
            schema_version: extension_state_schema(),
            ..ExtensionState::default()
        })
}

fn write_extension_state(config: &HttpRuntimeConfig, state: &ExtensionState) -> Result<(), String> {
    let path = extension_state_path(config);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let temp = path.with_extension(format!("tmp-{}-{}", std::process::id(), now_ms()));
    fs::write(
        &temp,
        serde_json::to_vec_pretty(state).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    let backup = path.with_extension(format!("bak-{}-{}", std::process::id(), now_ms()));
    if path.exists() {
        fs::rename(&path, &backup).map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(&temp, &path) {
        let _ = fs::rename(&backup, &path);
        return Err(error.to_string());
    }
    let _ = fs::remove_file(backup);
    Ok(())
}

fn sanitize_plugin_id(value: &str) -> String {
    let mut output = value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>();
    while output.contains("--") {
        output = output.replace("--", "-");
    }
    output.trim_matches('-').to_string()
}

fn plugin_source(source: &str, workspace: &Path) -> Result<(String, String), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("plugin source is required".to_string());
    }
    if source.starts_with("https://") {
        let parsed = url::Url::parse(source).map_err(|error| error.to_string())?;
        if parsed.scheme() != "https" || parsed.host_str().is_none() {
            return Err("plugin Git source must use an absolute HTTPS URL".to_string());
        }
        return Ok((source.to_string(), "git".to_string()));
    }
    let raw_path = source.strip_prefix("file://").unwrap_or(source);
    let path = PathBuf::from(raw_path);
    let resolved = if path.is_absolute() {
        path
    } else {
        workspace.join(path)
    };
    let canonical = fs::canonicalize(&resolved).map_err(|error| {
        format!(
            "plugin source does not exist: {} ({error})",
            resolved.display()
        )
    })?;
    if !canonical.is_dir() && canonical.file_name().and_then(OsStr::to_str) != Some("SKILL.md") {
        return Err("local plugin source must be a directory or SKILL.md".to_string());
    }
    Ok((canonical.to_string_lossy().to_string(), "local".to_string()))
}

fn stage_plugin(
    config: &HttpRuntimeConfig,
    source: &str,
    source_type: &str,
) -> Result<PathBuf, String> {
    let stage_root = plugin_stage_store(config);
    fs::create_dir_all(&stage_root).map_err(|error| error.to_string())?;
    let stage = stage_root.join(format!("plugin-{}-{}", std::process::id(), now_ms()));
    if source_type == "git" {
        let output = Command::new("git")
            .args(["clone", "--depth", "1", "--", source])
            .arg(&stage)
            .output()
            .map_err(|error| format!("failed to launch git: {error}"))?;
        if !output.status.success() {
            let _ = fs::remove_dir_all(&stage);
            return Err(format!(
                "plugin clone failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    } else {
        let source_path = PathBuf::from(source);
        if source_path.is_dir() {
            copy_plugin_tree(&source_path, &stage, &mut 0, &mut 0)?;
        } else {
            fs::create_dir_all(&stage).map_err(|error| error.to_string())?;
            fs::copy(&source_path, stage.join("SKILL.md")).map_err(|error| error.to_string())?;
        }
    }
    Ok(stage)
}

fn copy_plugin_tree(
    source: &Path,
    destination: &Path,
    files: &mut u64,
    bytes: &mut u64,
) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| error.to_string())?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| error.to_string())?
        .flatten()
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        if file_type.is_symlink() {
            return Err(format!(
                "plugin packages cannot contain symbolic links: {}",
                entry.path().display()
            ));
        }
        let target = destination.join(entry.file_name());
        if file_type.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            copy_plugin_tree(&entry.path(), &target, files, bytes)?;
        } else if file_type.is_file() {
            *files = files.saturating_add(1);
            *bytes =
                bytes.saturating_add(entry.metadata().map_err(|error| error.to_string())?.len());
            if *files > MAX_PLUGIN_FILES || *bytes > MAX_PLUGIN_BYTES {
                return Err("plugin package exceeds the 4096 file / 64 MB safety limit".to_string());
            }
            fs::copy(entry.path(), target).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

fn plugin_skill_files(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return result;
    };
    let mut entries = entries.flatten().collect::<Vec<_>>();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.file_name().and_then(OsStr::to_str) == Some(".git") {
            continue;
        }
        if path.is_dir() {
            result.extend(plugin_skill_files(&path));
        } else if path.file_name().and_then(OsStr::to_str) == Some("SKILL.md") {
            result.push(path);
        }
    }
    result
}

fn manifest_value(root: &Path) -> Result<Option<Value>, String> {
    for path in [
        root.join(".codex-plugin/plugin.json"),
        root.join("plugin.json"),
    ] {
        if !path.is_file() {
            continue;
        }
        let raw = fs::read_to_string(&path).map_err(|error| error.to_string())?;
        let value = serde_json::from_str::<Value>(&raw)
            .map_err(|error| format!("invalid plugin manifest {}: {error}", path.display()))?;
        if !value.is_object() {
            return Err("plugin manifest must be a JSON object".to_string());
        }
        return Ok(Some(value));
    }
    Ok(None)
}

fn manifest_text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_string()
}

fn metadata_string_list(value: &Value, keys: &[&str]) -> Vec<String> {
    for key in keys {
        let Some(raw) = value.get(*key) else {
            continue;
        };
        if let Some(items) = raw.as_array() {
            return items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect();
        }
        if let Some(items) = raw.as_str() {
            return items
                .split([',', ' '])
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    Vec::new()
}

fn inspect_plugin(root: &Path) -> Result<InspectedPlugin, String> {
    let manifest = manifest_value(root)?;
    let mut documents = Vec::new();
    for path in plugin_skill_files(root) {
        documents.push(openagent_core::load_skill_document(&path)?);
    }
    if documents.is_empty() {
        return Err("plugin contains no valid SKILL.md files".to_string());
    }
    let name = manifest
        .as_ref()
        .map(|value| manifest_text(value, "name"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| documents[0].name.clone());
    let id = sanitize_plugin_id(&name);
    if id.is_empty() {
        return Err("plugin name must contain letters or numbers".to_string());
    }
    let version = manifest
        .as_ref()
        .map(|value| manifest_text(value, "version"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "0.0.0-local".to_string());
    let description = manifest
        .as_ref()
        .map(|value| manifest_text(value, "description"))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| documents[0].description.clone());
    let mut skill_names = BTreeSet::new();
    let mut permissions = BTreeSet::from(["skills:read".to_string()]);
    for document in &documents {
        if !skill_names.insert(document.name.clone()) {
            return Err(format!("duplicate skill name in plugin: {}", document.name));
        }
        for tool in metadata_string_list(
            &json!(document.metadata),
            &["allowed-tools", "allowed_tools"],
        ) {
            permissions.insert(format!("tool:{tool}"));
        }
        for tool in metadata_string_list(
            &json!(document.metadata),
            &["disallowed-tools", "disallowed_tools"],
        ) {
            permissions.insert(format!("tool-denied:{tool}"));
        }
    }
    if let Some(manifest) = manifest.as_ref() {
        if manifest.get("mcpServers").is_some() {
            permissions.insert("mcp:configure".to_string());
        }
        if manifest.get("apps").is_some() {
            permissions.insert("apps:connect".to_string());
        }
        if let Some(capabilities) = manifest
            .get("interface")
            .and_then(|value| value.get("capabilities"))
            .and_then(Value::as_array)
        {
            for capability in capabilities.iter().filter_map(Value::as_str) {
                permissions.insert(format!(
                    "capability:{}",
                    capability.trim().to_ascii_lowercase()
                ));
            }
        }
    }
    Ok(InspectedPlugin {
        id,
        name,
        version,
        description,
        skills: skill_names.into_iter().collect(),
        permissions: permissions.into_iter().collect(),
    })
}

fn validate_plugin_skill_names(
    config: &HttpRuntimeConfig,
    state: &ExtensionState,
    inspected: &InspectedPlugin,
    replacing: Option<&str>,
) -> Result<(), String> {
    let builtins = SkillRegistry::new_with_options(
        Some(workspace(config)),
        Option::<Vec<String>>::None,
        Option::<PathBuf>::None,
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    )
    .all()
    .into_iter()
    .map(|skill| skill.name)
    .collect::<BTreeSet<_>>();
    for skill in &inspected.skills {
        if builtins.contains(skill) {
            return Err(format!(
                "skill name already exists in workspace or built-ins: {skill}"
            ));
        }
        if state.plugins.iter().any(|(id, plugin)| {
            replacing != Some(id.as_str()) && plugin.skills.iter().any(|name| name == skill)
        }) {
            return Err(format!(
                "skill name already belongs to another plugin: {skill}"
            ));
        }
    }
    Ok(())
}

fn public_plugin(plugin: &ManagedPlugin, state: &ExtensionState) -> Value {
    let enabled_skills = plugin
        .skills
        .iter()
        .filter(|skill| {
            plugin.enabled && state.skill_overrides.get(*skill).copied().unwrap_or(true)
        })
        .count();
    json!({
        "id": plugin.id,
        "name": plugin.name,
        "version": plugin.version,
        "description": plugin.description,
        "source": plugin.source,
        "source_type": plugin.source_type,
        "enabled": plugin.enabled,
        "skills": plugin.skills,
        "skill_count": plugin.skills.len(),
        "enabled_skill_count": enabled_skills,
        "permissions": plugin.permissions,
        "installed_at_ms": plugin.installed_at_ms,
        "updated_at_ms": plugin.updated_at_ms,
    })
}

pub(super) fn plugin_runtime_options(config: &HttpRuntimeConfig) -> PluginRuntimeOptions {
    let state = read_extension_state(config);
    let mut options = PluginRuntimeOptions::default();
    let mut disabled = BTreeSet::new();
    for plugin in state.plugins.values() {
        if plugin.enabled {
            options.enabled_plugins.push(plugin.id.clone());
            options.extra_skill_roots.extend(plugin.skill_roots.clone());
        } else {
            disabled.extend(plugin.skills.clone());
        }
    }
    disabled.extend(
        state
            .skill_overrides
            .iter()
            .filter(|(_, enabled)| !**enabled)
            .map(|(name, _)| name.clone()),
    );
    options.extra_skill_roots.sort();
    options.extra_skill_roots.dedup();
    options.enabled_plugins.sort();
    options.disabled_skills = disabled.into_iter().collect();
    options
}

pub(super) fn sync_plugin_runtime_metadata(config: &HttpRuntimeConfig, session: &mut Session) {
    let options = plugin_runtime_options(config);
    session.metadata.insert(
        "extra_skill_roots".to_string(),
        json!(options.extra_skill_roots),
    );
    session.metadata.insert(
        "disabled_skills".to_string(),
        json!(options.disabled_skills),
    );
    session.metadata.insert(
        "enabled_plugins".to_string(),
        json!(options.enabled_plugins),
    );
}

pub(super) fn plugins_payload(config: &HttpRuntimeConfig) -> Value {
    let state = read_extension_state(config);
    let options = plugin_runtime_options(config);
    let registry = SkillRegistry::new_with_options(
        Some(workspace(config)),
        Option::<Vec<String>>::None,
        Option::<PathBuf>::None,
        SkillRegistryOptions {
            include_builtin_skills: true,
        },
    )
    .with_extra_roots(options.extra_skill_roots.clone());
    let report = registry.report(None, None);
    let plugin_by_skill = state
        .plugins
        .values()
        .flat_map(|plugin| {
            plugin
                .skills
                .iter()
                .map(move |skill| (skill.clone(), plugin.id.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    let skills = report
        .skills
        .into_iter()
        .filter(skill_info_model_invocable)
        .map(|skill| {
            let plugin_id = plugin_by_skill.get(&skill.name).cloned();
            let plugin_enabled = plugin_id
                .as_ref()
                .and_then(|id| state.plugins.get(id))
                .is_none_or(|plugin| plugin.enabled);
            let enabled = plugin_enabled
                && state
                    .skill_overrides
                    .get(&skill.name)
                    .copied()
                    .unwrap_or(true);
            json!({
                "name": skill.name,
                "description": skill.description,
                "enabled": enabled,
                "plugin_id": plugin_id,
                "source": if plugin_id.is_some() { "plugin" } else { "workspace_or_builtin" },
                "metadata": skill.metadata,
            })
        })
        .collect::<Vec<_>>();
    let plugins = state
        .plugins
        .values()
        .map(|plugin| public_plugin(plugin, &state))
        .collect::<Vec<_>>();
    json!({
        "schema_version": PLUGIN_STATE_SCHEMA,
        "plugins": plugins,
        "plugin_count": state.plugins.len(),
        "enabled_plugin_count": state.plugins.values().filter(|plugin| plugin.enabled).count(),
        "skills": skills,
        "skill_count": report.loaded_count,
        "issues": report.issues,
        "runtime": {
            "enabled_plugins": options.enabled_plugins,
            "extra_skill_root_count": options.extra_skill_roots.len(),
            "disabled_skills": options.disabled_skills,
        },
    })
}

pub(super) fn install_plugin_payload(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let raw_source = payload
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let (source, source_type) = plugin_source(raw_source, &workspace(config))?;
    let stage = stage_plugin(config, &source, &source_type)?;
    let inspected = match inspect_plugin(&stage) {
        Ok(inspected) => inspected,
        Err(error) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(error);
        }
    };
    let mut state = read_extension_state(config);
    if state.plugins.contains_key(&inspected.id) {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!(
            "plugin {} is already installed; use Update instead",
            inspected.id
        ));
    }
    if let Err(error) = validate_plugin_skill_names(config, &state, &inspected, None) {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    let final_path = plugin_store(config).join(&inspected.id);
    fs::create_dir_all(plugin_store(config)).map_err(|error| error.to_string())?;
    fs::rename(&stage, &final_path).map_err(|error| error.to_string())?;
    let timestamp = now_ms();
    state.plugins.insert(
        inspected.id.clone(),
        ManagedPlugin {
            id: inspected.id,
            name: inspected.name,
            version: inspected.version,
            description: inspected.description,
            source,
            source_type,
            install_path: final_path.to_string_lossy().to_string(),
            enabled: payload
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            skills: inspected.skills,
            skill_roots: vec![final_path.to_string_lossy().to_string()],
            permissions: inspected.permissions,
            installed_at_ms: timestamp,
            updated_at_ms: timestamp,
        },
    );
    if let Err(error) = write_extension_state(config, &state) {
        let _ = fs::remove_dir_all(&final_path);
        return Err(error);
    }
    Ok(plugins_payload(config))
}

pub(super) fn update_plugin_payload(
    config: &HttpRuntimeConfig,
    plugin_id: &str,
) -> Result<Value, String> {
    let mut state = read_extension_state(config);
    let current = state
        .plugins
        .get(plugin_id)
        .cloned()
        .ok_or_else(|| format!("plugin not found: {plugin_id}"))?;
    let stage = stage_plugin(config, &current.source, &current.source_type)?;
    let inspected = match inspect_plugin(&stage) {
        Ok(inspected) => inspected,
        Err(error) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(error);
        }
    };
    if inspected.id != current.id {
        let _ = fs::remove_dir_all(&stage);
        return Err(format!(
            "updated plugin id changed from {} to {}",
            current.id, inspected.id
        ));
    }
    validate_plugin_skill_names(config, &state, &inspected, Some(plugin_id))?;
    let final_path = PathBuf::from(&current.install_path);
    let backup = plugin_stage_store(config).join(format!("backup-{plugin_id}-{}", now_ms()));
    if final_path.exists() {
        fs::rename(&final_path, &backup).map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(&stage, &final_path) {
        let _ = fs::rename(&backup, &final_path);
        return Err(error.to_string());
    }
    state.plugins.insert(
        plugin_id.to_string(),
        ManagedPlugin {
            name: inspected.name,
            version: inspected.version,
            description: inspected.description,
            skills: inspected.skills,
            skill_roots: vec![final_path.to_string_lossy().to_string()],
            permissions: inspected.permissions,
            updated_at_ms: now_ms(),
            ..current
        },
    );
    if let Err(error) = write_extension_state(config, &state) {
        let _ = fs::remove_dir_all(&final_path);
        let _ = fs::rename(&backup, &final_path);
        return Err(error);
    }
    let _ = fs::remove_dir_all(&backup);
    Ok(plugins_payload(config))
}

pub(super) fn mutate_plugin_payload(
    config: &HttpRuntimeConfig,
    plugin_id: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let mut state = read_extension_state(config);
    let plugin = state
        .plugins
        .get_mut(plugin_id)
        .ok_or_else(|| format!("plugin not found: {plugin_id}"))?;
    if let Some(enabled) = payload.get("enabled").and_then(Value::as_bool) {
        plugin.enabled = enabled;
        plugin.updated_at_ms = now_ms();
    }
    write_extension_state(config, &state)?;
    Ok(plugins_payload(config))
}

pub(super) fn delete_plugin_payload(
    config: &HttpRuntimeConfig,
    plugin_id: &str,
) -> Result<Value, String> {
    let mut state = read_extension_state(config);
    let plugin = state
        .plugins
        .remove(plugin_id)
        .ok_or_else(|| format!("plugin not found: {plugin_id}"))?;
    let managed_root =
        fs::canonicalize(plugin_store(config)).unwrap_or_else(|_| plugin_store(config));
    let install_path = fs::canonicalize(&plugin.install_path)
        .unwrap_or_else(|_| PathBuf::from(&plugin.install_path));
    if !install_path.starts_with(&managed_root) {
        return Err("refusing to remove a plugin outside the managed store".to_string());
    }
    for skill in plugin.skills {
        state.skill_overrides.remove(&skill);
    }
    write_extension_state(config, &state)?;
    if install_path.exists() {
        fs::remove_dir_all(&install_path).map_err(|error| error.to_string())?;
    }
    Ok(plugins_payload(config))
}

pub(super) fn mutate_skill_payload(
    config: &HttpRuntimeConfig,
    skill_name: &str,
    body: &str,
) -> Result<Value, String> {
    let payload: Value = serde_json::from_str(body).map_err(|error| error.to_string())?;
    let enabled = payload
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "skill update requires enabled=true|false".to_string())?;
    let current = plugins_payload(config);
    let exists = current
        .get("skills")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|skill| skill.get("name").and_then(Value::as_str) == Some(skill_name));
    if !exists {
        return Err(format!("skill not found: {skill_name}"));
    }
    let mut state = read_extension_state(config);
    state
        .skill_overrides
        .insert(skill_name.to_string(), enabled);
    write_extension_state(config, &state)?;
    Ok(plugins_payload(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("{name}-{}-{}", std::process::id(), now_ms()));
        fs::create_dir_all(&root).expect("create plugin test root");
        root
    }

    fn test_config(root: &Path) -> HttpRuntimeConfig {
        HttpRuntimeConfig {
            workspace: Some(root.join("workspace").to_string_lossy().to_string()),
            session_store_root: Some(root.join("sessions").to_string_lossy().to_string()),
            ..HttpRuntimeConfig::default()
        }
    }

    fn write_plugin(root: &Path, version: &str) {
        fs::create_dir_all(root.join(".codex-plugin")).expect("create plugin manifest directory");
        fs::create_dir_all(root.join("skills/demo")).expect("create plugin skill directory");
        fs::write(
            root.join(".codex-plugin/plugin.json"),
            stable_json_dumps(&json!({
                "name": "demo-plugin",
                "version": version,
                "description": "Demo plugin",
                "interface": {"capabilities": ["Read"]}
            })),
        )
        .expect("write plugin manifest");
        fs::write(
            root.join("skills/demo/SKILL.md"),
            "---\nname: demo-managed-skill\ndescription: Managed demo skill\nallowed-tools:\n  - read\n---\nUse the managed demo.\n",
        )
        .expect("write plugin skill");
    }

    #[test]
    fn plugin_install_toggle_update_and_remove_round_trip() {
        let root = temp_root("openagent-plugin-runtime");
        let source = root.join("source");
        write_plugin(&source, "1.0.0");
        let config = test_config(&root);
        fs::create_dir_all(workspace(&config)).expect("create plugin test workspace");

        let installed = install_plugin_payload(
            &config,
            &stable_json_dumps(&json!({"source": source.to_string_lossy()})),
        )
        .expect("install plugin");
        assert_eq!(installed["plugin_count"], json!(1));
        assert_eq!(installed["plugins"][0]["enabled"], json!(true));
        assert_eq!(
            installed["skills"]
                .as_array()
                .expect("installed skills")
                .iter()
                .find(|skill| skill["name"] == "demo-managed-skill")
                .expect("installed demo skill")["enabled"],
            json!(true)
        );
        assert!(
            installed["plugins"][0]["permissions"]
                .as_array()
                .expect("plugin permissions")
                .iter()
                .any(|permission| permission == "tool:read")
        );

        let disabled = mutate_plugin_payload(&config, "demo-plugin", r#"{"enabled":false}"#)
            .expect("disable plugin");
        assert_eq!(disabled["plugins"][0]["enabled"], json!(false));
        assert!(plugin_runtime_options(&config).extra_skill_roots.is_empty());

        write_plugin(&source, "1.1.0");
        let updated = update_plugin_payload(&config, "demo-plugin").expect("update plugin");
        assert_eq!(updated["plugins"][0]["version"], json!("1.1.0"));
        assert_eq!(updated["plugins"][0]["enabled"], json!(false));

        let enabled = mutate_plugin_payload(&config, "demo-plugin", r#"{"enabled":true}"#)
            .expect("enable plugin");
        assert_eq!(enabled["plugins"][0]["enabled"], json!(true));
        let skill_disabled =
            mutate_skill_payload(&config, "demo-managed-skill", r#"{"enabled":false}"#)
                .expect("disable plugin skill");
        assert_eq!(
            skill_disabled["skills"]
                .as_array()
                .expect("plugin skills")
                .iter()
                .find(|skill| skill["name"] == "demo-managed-skill")
                .expect("demo plugin skill")["enabled"],
            json!(false)
        );

        let removed = delete_plugin_payload(&config, "demo-plugin").expect("remove plugin");
        assert_eq!(removed["plugin_count"], json!(0));
        assert!(
            !extension_state_path(&config)
                .to_string_lossy()
                .contains(&source.to_string_lossy().to_string())
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn enabled_plugin_skill_is_visible_to_the_runtime_tool_context() {
        let root = temp_root("openagent-plugin-runtime-tool");
        let source = root.join("source");
        write_plugin(&source, "1.0.0");
        let config = test_config(&root);
        let workspace = workspace(&config);
        fs::create_dir_all(&workspace).expect("create plugin tool workspace");
        install_plugin_payload(
            &config,
            &stable_json_dumps(&json!({"source": source.to_string_lossy()})),
        )
        .expect("install plugin for tool context");

        let mut session = Session::new("session-plugin", &workspace);
        sync_plugin_runtime_metadata(&config, &mut session);
        let mut ctx =
            runtime_session_runner_facade(&session, None, PermissionRuleset::PlanOnly, true)
                .tool_context();
        let toolkit = Toolkit::with_builtins();
        let loaded = toolkit.execute(
            "skill",
            json!({"name": "demo-managed-skill"}),
            "call-plugin-skill",
            &mut ctx,
        );
        assert!(loaded.error.is_none(), "{:?}", loaded.error);
        assert!(loaded.output.contains("Use the managed demo."));

        mutate_skill_payload(&config, "demo-managed-skill", r#"{"enabled":false}"#)
            .expect("disable managed skill");
        sync_plugin_runtime_metadata(&config, &mut session);
        let mut disabled_ctx =
            runtime_session_runner_facade(&session, None, PermissionRuleset::PlanOnly, true)
                .tool_context();
        let disabled = toolkit.execute(
            "skill",
            json!({"name": "demo-managed-skill"}),
            "call-plugin-skill-disabled",
            &mut disabled_ctx,
        );
        assert!(disabled.error.is_some());
        let _ = fs::remove_dir_all(root);
    }
}
