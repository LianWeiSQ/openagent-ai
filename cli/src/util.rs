use super::*;

pub(super) fn ok_text(text: impl Into<String>) -> CliRunResult {
    CliRunResult {
        exit_code: 0,
        stdout: ensure_trailing_newline(text.into()),
        stderr: String::new(),
    }
}

pub(super) fn err_text(exit_code: i32, text: impl Into<String>) -> CliRunResult {
    CliRunResult {
        exit_code,
        stdout: String::new(),
        stderr: ensure_trailing_newline(text.into()),
    }
}

pub(super) fn ensure_trailing_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

pub(super) fn is_help_flag(arg: &str) -> bool {
    matches!(arg, "--help" | "-h")
}

pub(super) fn run_external_json(program: &str, args: &[&str]) -> CliRunResult {
    match Command::new(program).args(args).output() {
        Ok(output) => CliRunResult {
            exit_code: output.status.code().unwrap_or(1),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        },
        Err(error) => err_text(1, format!("failed to run {program}: {error}")),
    }
}

pub(super) fn chat_message(role: Role, content: String) -> ChatMessage {
    ChatMessage {
        role,
        content,
        name: None,
        tool_call_id: None,
        metadata: BTreeMap::from([("message_id".to_string(), json!(new_cli_id("msg")))]),
    }
}

pub(super) fn cli_message_id(index: u64) -> String {
    format!("msg_{index}")
}

pub(super) fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub(super) fn url_encode(value: &str) -> String {
    value
        .bytes()
        .flat_map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                vec![byte as char]
            }
            b' ' => vec!['+'],
            other => format!("%{other:02X}").chars().collect(),
        })
        .collect()
}

pub(super) fn now_ms_cli() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or_default()
}

pub(super) fn new_cli_id(prefix: &str) -> String {
    format!("{prefix}_{}_{}", now_ms_cli(), std::process::id())
}

#[allow(dead_code)]
pub(super) fn opencode_gap_command(command: &str, args: &[String]) -> CliRunResult {
    let help = format!(
        "openagent {command} is tracked as an OpenCode parity backlog command.\n\
         The Rust rewrite exposes this boundary, but full behavior is not implemented yet."
    );
    if args.iter().any(|arg| is_help_flag(arg)) {
        ok_text(help)
    } else {
        err_text(2, help)
    }
}

pub(super) fn active_provider() -> String {
    env::var("OPENAGENT_PROVIDER")
        .or_else(|_| env::var("OPENAGENT_ACTIVE_PROVIDER"))
        .unwrap_or_else(|_| "openai".to_string())
}

pub(super) fn provider_env_value(provider: &str, field: &str) -> Option<String> {
    let env = default_env_mapping(provider).ok()?;
    let env_name = env.get(field)?;
    env::var(env_name).ok().filter(|value| !value.is_empty())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ResolvedProviderConfig {
    pub(super) provider: String,
    pub(super) provider_label: String,
    pub(super) api_key_env: String,
    pub(super) api_key: Option<String>,
    pub(super) api_key_source: Option<String>,
    pub(super) base_url: String,
    pub(super) base_url_source: String,
    pub(super) model: String,
    pub(super) model_source: String,
    pub(super) wire_api: String,
    pub(super) wire_api_source: String,
    pub(super) native: bool,
    pub(super) requires_api_key: bool,
}

pub(super) fn resolve_provider_config(
    provider: &str,
    args: &[String],
) -> Result<ResolvedProviderConfig, String> {
    let provider = normalize_provider(Some(provider))?;
    let env_mapping = default_env_mapping(&provider)?;
    let auth_record = provider_auth_record_from_args(&provider, args);
    let env_file_values = provider_env_file_values(args);
    let api_key_env = env_mapping
        .get("api_key")
        .cloned()
        .unwrap_or_else(|| "OPENAI_API_KEY".to_string());
    let api_key = resolve_provider_field(
        "api_key",
        &["--api-key"],
        &api_key_env,
        &env_file_values,
        auth_record.as_ref(),
        args,
        None,
    );
    let base_url_env = env_mapping
        .get("base_url")
        .cloned()
        .unwrap_or_else(|| "OPENAI_BASE_URL".to_string());
    let default_base_url = provider_default_base_url(&provider)
        .ok()
        .flatten()
        .or_else(|| (provider == "anthropic").then(|| "https://api.anthropic.com/v1".to_string()))
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let base_url = resolve_provider_field(
        "base_url",
        &["--base-url"],
        &base_url_env,
        &env_file_values,
        auth_record.as_ref(),
        args,
        Some(default_base_url),
    )
    .expect("base_url has a default");
    let model_env = env_mapping
        .get("model")
        .cloned()
        .unwrap_or_else(|| "OPENAI_MODEL".to_string());
    let default_model = provider_default_model(&provider)
        .ok()
        .flatten()
        .unwrap_or_else(|| DEFAULT_MODEL.to_string());
    let model = resolve_provider_field(
        "model",
        &["--model", "-m"],
        &model_env,
        &env_file_values,
        auth_record.as_ref(),
        args,
        Some(default_model),
    )
    .expect("model has a default");
    let wire_api_env = env_mapping
        .get("wire_api")
        .cloned()
        .unwrap_or_else(|| "OPENAI_WIRE_API".to_string());
    let default_wire_api = match provider.as_str() {
        "anthropic" => "messages".to_string(),
        "gemini" | "google" => "generate_content".to_string(),
        _ => DEFAULT_WIRE_API.to_string(),
    };
    let wire_api = resolve_provider_field(
        "wire_api",
        &["--wire-api"],
        &wire_api_env,
        &env_file_values,
        auth_record.as_ref(),
        args,
        Some(default_wire_api),
    )
    .expect("wire_api has a default");
    Ok(ResolvedProviderConfig {
        provider_label: provider_label(&provider).unwrap_or_else(|_| provider.clone()),
        api_key_env,
        api_key: api_key.as_ref().map(|field| field.value.clone()),
        api_key_source: api_key.map(|field| field.source),
        base_url: base_url.value,
        base_url_source: base_url.source,
        model: model.value,
        model_source: model.source,
        wire_api: wire_api.value,
        wire_api_source: wire_api.source,
        native: matches!(provider.as_str(), "anthropic" | "gemini" | "google"),
        requires_api_key: provider_requires_api_key(&provider).unwrap_or(true),
        provider,
    })
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ResolvedProviderField {
    value: String,
    source: String,
}

fn resolve_provider_field(
    field: &str,
    arg_keys: &[&str],
    env_name: &str,
    env_file_values: &BTreeMap<String, String>,
    auth_record: Option<&Value>,
    args: &[String],
    default: Option<String>,
) -> Option<ResolvedProviderField> {
    value_for(args, arg_keys)
        .filter(|value| !value.is_empty())
        .map(|value| ResolvedProviderField {
            value,
            source: "cli".to_string(),
        })
        .or_else(|| {
            env::var(env_name)
                .ok()
                .filter(|value| !value.is_empty())
                .map(|value| ResolvedProviderField {
                    value,
                    source: "env".to_string(),
                })
        })
        .or_else(|| {
            env_file_values
                .get(env_name)
                .cloned()
                .map(|value| ResolvedProviderField {
                    value,
                    source: "env_file".to_string(),
                })
        })
        .or_else(|| {
            auth_record
                .and_then(|record| record.get(field))
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .map(|value| ResolvedProviderField {
                    value: value.to_string(),
                    source: "auth_file".to_string(),
                })
        })
        .or_else(|| {
            default.map(|value| ResolvedProviderField {
                value,
                source: "default".to_string(),
            })
        })
}

fn provider_auth_record_from_args(provider: &str, args: &[String]) -> Option<Value> {
    let auth = read_json_file(&auth_file_from_args(args));
    auth.get("providers")
        .and_then(Value::as_object)
        .and_then(|providers| providers.get(provider))
        .cloned()
}

fn provider_env_file_values(args: &[String]) -> BTreeMap<String, String> {
    provider_env_file_path(args)
        .and_then(|path| fs::read_to_string(path).ok())
        .map(|raw| parse_env_file_values(&raw))
        .unwrap_or_default()
}

fn provider_env_file_path(args: &[String]) -> Option<PathBuf> {
    value_for(args, &["--env-file"])
        .or_else(|| env::var("OPENAGENT_ENV_FILE").ok())
        .map(PathBuf::from)
}

fn parse_env_file_values(raw: &str) -> BTreeMap<String, String> {
    raw.lines()
        .filter_map(|line| {
            let mut line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            if let Some(stripped) = line.strip_prefix("export ") {
                line = stripped.trim();
            }
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            if key.is_empty() {
                return None;
            }
            let value = value.trim();
            let value = value
                .strip_prefix('"')
                .and_then(|item| item.strip_suffix('"'))
                .or_else(|| {
                    value
                        .strip_prefix('\'')
                        .and_then(|item| item.strip_suffix('\''))
                })
                .unwrap_or(value)
                .to_string();
            Some((key.to_string(), value))
        })
        .filter(|(_, value)| !value.is_empty())
        .collect()
}

pub(super) fn default_model_for_provider(provider: &str) -> String {
    if provider == "openai" {
        DEFAULT_MODEL.to_string()
    } else {
        provider_default_model(provider)
            .ok()
            .flatten()
            .unwrap_or_else(|| DEFAULT_MODEL.to_string())
    }
}

pub(super) fn workspace_from_args(args: &[String]) -> PathBuf {
    value_for(args, &["--workspace", "--dir"])
        .map(PathBuf::from)
        .unwrap_or_else(|| env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub(super) fn session_root_from_args(args: &[String]) -> PathBuf {
    value_for(args, &["--session-root"])
        .or_else(|| env::var("OPENAGENT_SESSION_ROOT").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_from_args(args).join(".openagent/sessions"))
}

pub(super) fn auth_file_from_args(args: &[String]) -> PathBuf {
    value_for(args, &["--auth-file"])
        .or_else(|| env::var("OPENAGENT_AUTH_FILE").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".config/openagent/auth.json"))
}

pub(super) fn mcp_config_path(args: &[String]) -> PathBuf {
    value_for(args, &["--mcp-config", "--config"])
        .or_else(|| env::var("OPENAGENT_MCP_CONFIG").ok())
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_from_args(args).join(".openagent/mcp.json"))
}

pub(super) fn home_dir() -> PathBuf {
    env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub(super) fn read_json_file(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

pub(super) fn write_json_file(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let raw = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
    fs::write(path, format!("{raw}\n")).map_err(|error| error.to_string())
}

pub(super) fn ensure_object_field<'a>(
    value: &'a mut Value,
    key: &str,
) -> &'a mut Map<String, Value> {
    if !value.is_object() {
        *value = json!({});
    }
    let object = value.as_object_mut().expect("object ensured");
    object.entry(key.to_string()).or_insert_with(|| json!({}));
    object
        .get_mut(key)
        .and_then(Value::as_object_mut)
        .expect("object field ensured")
}

#[cfg(unix)]
pub(super) fn chmod_private(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(metadata) = fs::metadata(path) {
        let mut permissions = metadata.permissions();
        permissions.set_mode(0o600);
        let _ = fs::set_permissions(path, permissions);
    }
}

#[cfg(not(unix))]
pub(super) fn chmod_private(_path: &Path) {}

pub(super) fn attached_files(
    workspace: &Path,
    files: &[String],
) -> Result<Vec<(String, String)>, String> {
    files
        .iter()
        .map(|file| {
            let path = workspace.join(file);
            fs::read_to_string(&path)
                .map(|content| (path.to_string_lossy().to_string(), content))
                .map_err(|error| {
                    format!("failed to read attached file {}: {error}", path.display())
                })
        })
        .collect()
}

pub(super) fn remote_turn_attachments(files: &[(String, String)]) -> Vec<RemoteTurnAttachment> {
    files
        .iter()
        .map(|(path, content)| RemoteTurnAttachment::text_file(path, content))
        .collect()
}

pub(super) fn remote_turn_options_from_args(args: &[String]) -> Value {
    let mut options = Map::new();
    for (flag, key) in [
        ("--provider", "provider"),
        ("--model", "model"),
        ("--agent", "agent"),
        ("--variant", "variant"),
        ("--permission", "permission"),
    ] {
        if let Some(value) = value_for(args, &[flag]).filter(|value| !value.trim().is_empty()) {
            options.insert(key.to_string(), json!(value));
        }
    }
    for (flag, key) in [
        ("--max-steps", "max_steps"),
        ("--max-output-tokens", "max_output_tokens"),
        ("--context-window", "context_window"),
    ] {
        if let Some(value) = value_for(args, &[flag]).and_then(|value| value.parse::<u64>().ok()) {
            options.insert(key.to_string(), json!(value));
        }
    }
    if has_flag(args, &["--thinking"]) {
        options.insert("thinking".to_string(), json!("high"));
    }
    if has_flag(args, &["--dangerously-skip-permissions"]) {
        options.insert("dangerously_skip_permissions".to_string(), json!(true));
    }
    Value::Object(options)
}

pub(super) fn parse_headers(headers: &[String]) -> Map<String, Value> {
    headers
        .iter()
        .filter_map(|header| {
            header
                .split_once('=')
                .map(|(key, value)| (key.trim().to_string(), json!(value.trim())))
        })
        .filter(|(key, _)| !key.is_empty())
        .collect()
}

pub(super) fn mcp_public_servers(config: &Value) -> Vec<Value> {
    mcp_servers_object(config)
        .map(|servers| {
            servers
                .iter()
                .map(|(name, server)| mcp_public_server(name, server))
                .collect()
        })
        .unwrap_or_default()
}

pub(super) fn mcp_public_server(name: &str, server: &Value) -> Value {
    let server_type = server
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| {
            if server.get("command").is_some() {
                "local"
            } else {
                "remote"
            }
        });
    let transport =
        server
            .get("transport")
            .and_then(Value::as_str)
            .unwrap_or(if server_type == "local" {
                "stdio"
            } else {
                "auto"
            });
    let headers = server
        .get("headers")
        .and_then(Value::as_object)
        .map(|items| {
            items
                .keys()
                .map(|key| (key.clone(), json!("[redacted]")))
                .collect::<Map<_, _>>()
        })
        .unwrap_or_default();
    let command = public_mcp_command(server);
    let url = redact_url(server.get("url").and_then(Value::as_str).unwrap_or(""));
    let endpoint = if command.is_empty() {
        url.clone()
    } else {
        command.join(" ")
    };
    json!({
        "name": name,
        "type": server_type,
        "url": url,
        "command": command,
        "endpoint": endpoint,
        "enabled": server.get("enabled").and_then(Value::as_bool).unwrap_or(true),
        "transport": transport,
        "configured_transport": transport,
        "selected_transport": null,
        "timeout_ms": server
            .get("timeout_ms")
            .or_else(|| server.get("timeout"))
            .and_then(Value::as_u64)
            .unwrap_or(30_000),
        "header_names": headers.keys().cloned().collect::<Vec<_>>(),
        "headers": headers,
    })
}

fn mcp_servers_object(config: &Value) -> Option<&Map<String, Value>> {
    config
        .get("mcpServers")
        .and_then(Value::as_object)
        .or_else(|| {
            config
                .get("mcp")
                .and_then(|mcp| mcp.get("servers"))
                .and_then(Value::as_object)
        })
        .or_else(|| config.get("mcp").and_then(Value::as_object))
}

fn public_mcp_command(server: &Value) -> Vec<String> {
    let mut command = match server.get("command") {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect::<Vec<_>>(),
        Some(Value::String(value)) => vec![value.clone()],
        _ => Vec::new(),
    };
    if let Some(args) = server.get("args").and_then(Value::as_array) {
        command.extend(args.iter().filter_map(Value::as_str).map(str::to_string));
    }
    command
}

pub(super) fn redact_url(url: &str) -> String {
    let mut redacted = url.to_string();
    if let Some((scheme, rest)) = redacted.split_once("://")
        && let Some((_credentials, host_rest)) = rest.split_once('@')
    {
        redacted = format!("{scheme}://[redacted]@{host_rest}");
    }
    for marker in ["token=", "api_key=", "apikey=", "secret="] {
        if let Some(index) = redacted.to_ascii_lowercase().find(marker) {
            let start = index + marker.len();
            let end = redacted[start..]
                .find('&')
                .map(|offset| start + offset)
                .unwrap_or(redacted.len());
            redacted.replace_range(start..end, "[redacted]");
        }
    }
    redacted
}

pub(super) fn public_auth_record(provider: &str, value: &Value, source: &str) -> Value {
    let api_key = value.get("api_key").and_then(Value::as_str).unwrap_or("");
    let present_env = env::vars().map(|(key, _)| key).collect::<BTreeSet<_>>();
    let auth_methods = provider_auth_methods(provider, &present_env).unwrap_or_default();
    json!({
        "provider": provider,
        "type": value.get("type").and_then(Value::as_str).unwrap_or("api"),
        "source": source,
        "api_key": mask_secret(api_key),
        "has_api_key": !api_key.is_empty(),
        "base_url": value.get("base_url").cloned().unwrap_or(Value::Null),
        "model": value.get("model").cloned().unwrap_or(Value::Null),
        "wire_api": value.get("wire_api").cloned().unwrap_or(Value::Null),
        "env": default_env_mapping(provider).unwrap_or_default(),
        "auth_methods": auth_methods,
        "methods": ["api_key"],
        "updated_at_ms": value.get("updated_at_ms").cloned().unwrap_or(Value::Null),
    })
}

pub(super) fn valid_session_id(session_id: &str) -> bool {
    !session_id.is_empty()
        && session_id
            .chars()
            .all(|item| item.is_ascii_alphanumeric() || matches!(item, '_' | '-'))
}

pub(super) fn sanitize_session_state(value: &mut Value) {
    if let Some(object) = value.as_object_mut() {
        object.insert("workspace".to_string(), json!("[redacted]"));
        if let Some(messages) = object.get_mut("messages").and_then(Value::as_array_mut) {
            for message in messages {
                if let Some(message_object) = message.as_object_mut() {
                    message_object.insert("content".to_string(), json!("[redacted]"));
                }
            }
        }
    }
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn mask_secret(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    if value.len() <= 8 {
        return "*".repeat(value.len());
    }
    format!(
        "{}{}{}",
        &value[..4],
        "*".repeat((value.len() - 8).max(4)),
        &value[value.len() - 4..]
    )
}

pub(super) fn looks_secret(value: &str) -> bool {
    value.len() >= 12
        || value.starts_with("sk-")
        || value.contains("token")
        || value.contains("secret")
        || value.contains("Bearer ")
}

pub(super) fn sanitize_identifier(value: &str) -> String {
    let mut output = value
        .trim()
        .chars()
        .map(|item| {
            if item.is_ascii_alphanumeric() || matches!(item, '-' | '_') {
                item.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    while output.contains("--") {
        output = output.replace("--", "-");
    }
    if output.is_empty() {
        "item".to_string()
    } else {
        output
    }
}

pub(super) fn copy_cli_options(args: &[String], names: &[&str], output: &mut Vec<String>) {
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        let name = arg.split_once('=').map_or(arg.as_str(), |(name, _)| name);
        if !names.contains(&name) {
            index += 1;
            continue;
        }
        output.push(arg.clone());
        if arg.contains('=') || matches!(name, "--skip-doctor" | "--stream") {
            index += 1;
            continue;
        }
        if let Some(value) = args.get(index + 1)
            && !value.starts_with('-')
        {
            output.push(value.clone());
            index += 2;
            continue;
        }
        index += 1;
    }
}

pub(super) fn value_after<'a>(argv: &'a [&'a str], flag: &str) -> Option<&'a str> {
    argv.windows(2)
        .find_map(|items| (items[0] == flag).then_some(items[1]))
}

pub(super) fn positional_after_options(argv: &[&str], skip: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    let mut index = 0;
    while index < argv.len() {
        let item = argv[index];
        if skip.contains(&item) {
            index += 1;
            continue;
        }
        if item.starts_with("--") {
            let takes_value = matches!(
                item,
                "--workspace" | "--dir" | "--format" | "--session" | "--session-root" | "--command"
            );
            index += if takes_value { 2 } else { 1 };
            continue;
        }
        values.push(item.to_string());
        index += 1;
    }
    values
}

pub(super) trait Pipe: Sized {
    fn pipe<T>(self, f: impl FnOnce(Self) -> T) -> T {
        f(self)
    }
}

impl<T> Pipe for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_command_boundary() {
        assert_eq!(crate_name(), "openagent-cli");
        assert_eq!(command_name(), "openagent");
        assert_eq!(core_crate_name(), "openagent-core");
    }
}
