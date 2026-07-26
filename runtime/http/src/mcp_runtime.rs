use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::random;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};
use url::Url;

const MCP_OAUTH_SCHEMA_VERSION: u64 = 1;
const MCP_OAUTH_STATE_TTL_MS: u64 = 10 * 60 * 1000;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct McpOAuthPending {
    state: String,
    code_verifier: String,
    redirect_uri: String,
    created_at_ms: u64,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct McpOAuthCredential {
    schema_version: u64,
    server_name: String,
    server_url: String,
    authorization_server: String,
    authorization_endpoint: String,
    token_endpoint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    registration_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    revocation_endpoint: Option<String>,
    client_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    connected_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<McpOAuthPending>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_error: Option<String>,
}

#[derive(Clone, Debug)]
struct McpOAuthMetadata {
    authorization_server: String,
    authorization_endpoint: String,
    token_endpoint: String,
    registration_endpoint: Option<String>,
    revocation_endpoint: Option<String>,
    scopes_supported: Vec<String>,
}

#[derive(Clone, Debug)]
pub(super) struct McpConfigSource {
    pub(super) label: &'static str,
    pub(super) read_source: Option<String>,
    pub(super) writable_path: Option<PathBuf>,
    pub(super) readonly_reason: Option<&'static str>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum McpServersShape {
    Canonical,
    McpServers,
    McpNested,
    McpDirect,
    RootDirect,
}

pub(super) struct McpLifecycleEntry {
    pub(super) config_fingerprint: String,
    pub(super) session: StdioMcpSession,
    pub(super) descriptors: Vec<RemoteMcpToolDescriptor>,
    pub(super) started_at_ms: u64,
    pub(super) last_refreshed_at_ms: u64,
}

#[derive(Clone, Debug)]
pub(super) struct McpLifecycleSnapshot {
    pub(super) status: &'static str,
    pub(super) pid: Option<u32>,
    pub(super) started_at_ms: Option<u64>,
    pub(super) last_refreshed_at_ms: Option<u64>,
    pub(super) tool_count: usize,
}

#[derive(Debug)]
pub(super) struct McpConfigMutationError {
    pub(super) status: u16,
    pub(super) code: &'static str,
    pub(super) message: String,
}

pub(super) fn mcp_lifecycle_registry() -> &'static Mutex<BTreeMap<String, McpLifecycleEntry>> {
    static MCP_LIFECYCLE_REGISTRY: OnceLock<Mutex<BTreeMap<String, McpLifecycleEntry>>> =
        OnceLock::new();
    MCP_LIFECYCLE_REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn mcp_lifecycle_key(workspace: &Path, server_name: &str) -> String {
    format!("{}::{}", workspace.display(), server_name)
}

pub(super) fn mcp_server_fingerprint(server: &RemoteMcpServerConfig) -> String {
    let mut lifecycle_config = server.clone();
    lifecycle_config.enabled = true;
    serde_json::to_value(lifecycle_config)
        .map(|value| stable_json_dumps(&value))
        .unwrap_or_else(|_| server.name.clone())
}

pub(super) fn mcp_lifecycle_snapshot(
    server: &RemoteMcpServerConfig,
    workspace: &Path,
) -> McpLifecycleSnapshot {
    if server.server_type != McpServerType::Local {
        return McpLifecycleSnapshot {
            status: "not_applicable",
            pid: None,
            started_at_ms: None,
            last_refreshed_at_ms: None,
            tool_count: 0,
        };
    }
    let key = mcp_lifecycle_key(workspace, &server.name);
    let fingerprint = mcp_server_fingerprint(server);
    let Ok(mut registry) = mcp_lifecycle_registry().lock() else {
        return McpLifecycleSnapshot {
            status: "unavailable",
            pid: None,
            started_at_ms: None,
            last_refreshed_at_ms: None,
            tool_count: 0,
        };
    };
    let removal_snapshot = {
        let Some(entry) = registry.get_mut(&key) else {
            return McpLifecycleSnapshot {
                status: "stopped",
                pid: None,
                started_at_ms: None,
                last_refreshed_at_ms: None,
                tool_count: 0,
            };
        };
        if entry.config_fingerprint != fingerprint {
            Some(McpLifecycleSnapshot {
                status: "stale",
                pid: None,
                started_at_ms: None,
                last_refreshed_at_ms: None,
                tool_count: 0,
            })
        } else if entry.session.running() {
            return McpLifecycleSnapshot {
                status: "running",
                pid: Some(entry.session.pid()),
                started_at_ms: Some(entry.started_at_ms),
                last_refreshed_at_ms: Some(entry.last_refreshed_at_ms),
                tool_count: entry.descriptors.len(),
            };
        } else {
            Some(McpLifecycleSnapshot {
                status: "exited",
                pid: None,
                started_at_ms: Some(entry.started_at_ms),
                last_refreshed_at_ms: Some(entry.last_refreshed_at_ms),
                tool_count: entry.descriptors.len(),
            })
        }
    };
    if let Some(snapshot) = removal_snapshot {
        if let Some(entry) = registry.remove(&key) {
            drop(registry);
            entry.session.close();
        }
        return snapshot;
    }
    McpLifecycleSnapshot {
        status: "stopped",
        pid: None,
        started_at_ms: None,
        last_refreshed_at_ms: None,
        tool_count: 0,
    }
}

pub(super) fn refresh_mcp_lifecycle_server(
    server: &RemoteMcpServerConfig,
    workspace: &Path,
) -> Option<Result<Vec<RemoteMcpToolDescriptor>, String>> {
    if server.server_type != McpServerType::Local {
        return None;
    }
    let key = mcp_lifecycle_key(workspace, &server.name);
    let fingerprint = mcp_server_fingerprint(server);
    let Ok(mut registry) = mcp_lifecycle_registry().lock() else {
        return Some(Err("MCP lifecycle registry is unavailable".to_string()));
    };
    let remove_error = {
        let entry = registry.get_mut(&key)?;
        if entry.config_fingerprint != fingerprint {
            Some("MCP local lifecycle process was stopped because config changed".to_string())
        } else if !entry.session.running() {
            Some("MCP local lifecycle process exited".to_string())
        } else {
            match entry.session.tools_list() {
                Ok(tools) => {
                    let descriptors = build_tool_descriptors_from_values(server, &tools);
                    entry.last_refreshed_at_ms = now_ms();
                    entry.descriptors = descriptors.clone();
                    return Some(Ok(descriptors));
                }
                Err(error) => Some(error),
            }
        }
    };
    let entry = registry.remove(&key);
    drop(registry);
    if let Some(entry) = entry {
        entry.session.close();
    }
    remove_error.map(Err)
}

pub(super) fn stop_mcp_lifecycle_server(server: &RemoteMcpServerConfig, workspace: &Path) -> bool {
    let key = mcp_lifecycle_key(workspace, &server.name);
    let entry = mcp_lifecycle_registry()
        .lock()
        .ok()
        .and_then(|mut registry| registry.remove(&key));
    if let Some(entry) = entry {
        entry.session.close();
        true
    } else {
        false
    }
}

pub(super) fn apply_mcp_lifecycle_to_manager(manager: &mut RemoteMcpManager, workspace: &Path) {
    let updates = manager
        .config
        .servers
        .iter()
        .filter(|server| server.server_type == McpServerType::Local)
        .filter_map(|server| {
            let key = mcp_lifecycle_key(workspace, &server.name);
            let fingerprint = mcp_server_fingerprint(server);
            {
                let Ok(mut registry) = mcp_lifecycle_registry().lock() else {
                    return None;
                };
                let entry = registry.get_mut(&key)?;
                if entry.config_fingerprint == fingerprint && entry.session.running() {
                    Some((
                        server.name.clone(),
                        entry.descriptors.clone(),
                        Some(entry.last_refreshed_at_ms as f64 / 1000.0),
                    ))
                } else {
                    None
                }
            }
        })
        .collect::<Vec<_>>();
    for (server_name, descriptors, refreshed_at) in updates {
        let _ = manager.set_server_tools(
            &server_name,
            Some(McpTransport::Stdio),
            "connected",
            refreshed_at,
            descriptors,
        );
    }
}

pub(super) fn mcp_config_source(
    config: &HttpRuntimeConfig,
    env: &BTreeMap<String, String>,
) -> McpConfigSource {
    let default_workspace = workspace(config);
    mcp_config_source_for_workspace(config, env, &default_workspace)
}

pub(super) fn mcp_config_source_for_workspace(
    config: &HttpRuntimeConfig,
    env: &BTreeMap<String, String>,
    default_workspace: &Path,
) -> McpConfigSource {
    if let Some(raw) = config
        .mcp_config
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if looks_like_inline_json(raw) {
            return McpConfigSource {
                label: "config",
                read_source: Some(raw.to_string()),
                writable_path: None,
                readonly_reason: Some("inline_config_readonly"),
            };
        }
        let path = PathBuf::from(raw);
        return McpConfigSource {
            label: "config",
            read_source: path.exists().then(|| raw.to_string()),
            writable_path: Some(path),
            readonly_reason: None,
        };
    }
    if let Some(raw) = env
        .get("OPENAGENT_MCP_CONFIG")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if looks_like_inline_json(raw) {
            return McpConfigSource {
                label: "env",
                read_source: Some(raw.to_string()),
                writable_path: None,
                readonly_reason: Some("inline_env_readonly"),
            };
        }
        let path = PathBuf::from(raw);
        return McpConfigSource {
            label: "env",
            read_source: path.exists().then(|| raw.to_string()),
            writable_path: Some(path),
            readonly_reason: None,
        };
    }
    let default_path = default_workspace.join(".openagent").join("mcp.json");
    McpConfigSource {
        label: if default_path.exists() {
            "default"
        } else {
            "none"
        },
        read_source: default_path
            .exists()
            .then(|| default_path.to_string_lossy().to_string()),
        writable_path: Some(default_path),
        readonly_reason: None,
    }
}

pub(super) fn looks_like_inline_json(value: &str) -> bool {
    value.starts_with('{') || value.starts_with('[')
}

fn mcp_oauth_directory(config: &HttpRuntimeConfig) -> PathBuf {
    session_root(config)
        .join(".openagent-runtime")
        .join("mcp_oauth")
}

fn mcp_oauth_state_path(config: &HttpRuntimeConfig, server_name: &str) -> PathBuf {
    let slug = server_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let digest = Sha256::digest(server_name.as_bytes());
    let suffix = digest[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    mcp_oauth_directory(config).join(format!("{slug}-{suffix}.json"))
}

fn load_mcp_oauth_credential(
    config: &HttpRuntimeConfig,
    server_name: &str,
) -> Option<McpOAuthCredential> {
    let raw = fs::read_to_string(mcp_oauth_state_path(config, server_name)).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_mcp_oauth_credential(
    config: &HttpRuntimeConfig,
    credential: &McpOAuthCredential,
) -> Result<(), McpConfigMutationError> {
    let path = mcp_oauth_state_path(config, &credential.server_name);
    let parent = path.parent().ok_or_else(|| {
        mcp_mutation_error(500, "mcp_oauth_store_failed", "OAuth state path is invalid")
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        mcp_mutation_error(
            500,
            "mcp_oauth_store_failed",
            format!("failed to create private OAuth state directory: {error}"),
        )
    })?;
    let rendered = serde_json::to_vec_pretty(credential).map_err(|error| {
        mcp_mutation_error(
            500,
            "mcp_oauth_store_failed",
            format!("failed to serialize OAuth state: {error}"),
        )
    })?;
    let temporary = path.with_extension("json.tmp");
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temporary).map_err(|error| {
        mcp_mutation_error(
            500,
            "mcp_oauth_store_failed",
            format!("failed to open private OAuth state: {error}"),
        )
    })?;
    file.write_all(&rendered).map_err(|error| {
        mcp_mutation_error(
            500,
            "mcp_oauth_store_failed",
            format!("failed to write private OAuth state: {error}"),
        )
    })?;
    file.sync_all().map_err(|error| {
        mcp_mutation_error(
            500,
            "mcp_oauth_store_failed",
            format!("failed to sync private OAuth state: {error}"),
        )
    })?;
    fs::rename(&temporary, &path).map_err(|error| {
        mcp_mutation_error(
            500,
            "mcp_oauth_store_failed",
            format!("failed to replace private OAuth state: {error}"),
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            mcp_mutation_error(
                500,
                "mcp_oauth_store_failed",
                format!("failed to protect private OAuth state: {error}"),
            )
        })?;
    }
    Ok(())
}

fn remove_mcp_oauth_credential(config: &HttpRuntimeConfig, server_name: &str) {
    let _ = fs::remove_file(mcp_oauth_state_path(config, server_name));
}

fn random_oauth_token(byte_count: usize) -> String {
    let mut bytes = Vec::with_capacity(byte_count);
    while bytes.len() < byte_count {
        bytes.extend_from_slice(&random::<[u8; 32]>());
    }
    bytes.truncate(byte_count);
    URL_SAFE_NO_PAD.encode(bytes)
}

fn mcp_oauth_redirect_uri(config: &HttpRuntimeConfig) -> String {
    let host = match config.host.as_str() {
        "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        value => value,
    };
    format!("http://{host}:{}/api/mcp/oauth/callback", config.port)
}

fn oauth_json_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn oauth_http_error(label: &str, status: u16, body: &str) -> McpConfigMutationError {
    mcp_mutation_error(
        502,
        "mcp_oauth_upstream_failed",
        format!(
            "{label} returned HTTP {status}: {}",
            sanitize_mcp_status_error(body)
        ),
    )
}

fn oauth_get_json(
    client: &reqwest::blocking::Client,
    url: &str,
    label: &str,
) -> Result<Value, McpConfigMutationError> {
    let response = client.get(url).send().map_err(|error| {
        mcp_mutation_error(
            502,
            "mcp_oauth_discovery_failed",
            format!("{label} request failed: {error}"),
        )
    })?;
    let status = response.status();
    let raw = response.text().map_err(|error| {
        mcp_mutation_error(
            502,
            "mcp_oauth_discovery_failed",
            format!("{label} response could not be read: {error}"),
        )
    })?;
    if !status.is_success() {
        return Err(oauth_http_error(label, status.as_u16(), &raw));
    }
    serde_json::from_str(&raw).map_err(|error| {
        mcp_mutation_error(
            502,
            "mcp_oauth_discovery_failed",
            format!("{label} response is not valid JSON: {error}"),
        )
    })
}

fn oauth_origin_url(raw: &str) -> Result<Url, McpConfigMutationError> {
    let mut url = Url::parse(raw).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_oauth_invalid_url",
            format!("MCP OAuth URL is invalid: {error}"),
        )
    })?;
    url.set_path("/");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url)
}

fn www_authenticate_parameter(header: &str, target: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let trimmed = part.trim().trim_start_matches("Bearer").trim();
        let (key, value) = trimmed.split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case(target)
            .then(|| value.trim().trim_matches('"').to_string())
    })
}

fn discover_mcp_oauth_metadata(
    server: &RemoteMcpServerConfig,
) -> Result<McpOAuthMetadata, McpConfigMutationError> {
    let timeout = Duration::from_millis(server.timeout_ms.clamp(1_000, 30_000));
    let client = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| {
            mcp_mutation_error(
                500,
                "mcp_oauth_client_failed",
                format!("failed to create OAuth client: {error}"),
            )
        })?;
    let origin = oauth_origin_url(&server.url)?;
    let mut resource_metadata_urls = Vec::<String>::new();
    if let Ok(response) = client.get(&server.url).send() {
        if let Some(value) = response
            .headers()
            .get("www-authenticate")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| www_authenticate_parameter(value, "resource_metadata"))
        {
            resource_metadata_urls.push(value);
        }
    }
    let server_url = Url::parse(&server.url)
        .map_err(|error| mcp_mutation_error(400, "mcp_oauth_invalid_url", error.to_string()))?;
    let path = server_url.path().trim_end_matches('/');
    if !path.is_empty() {
        resource_metadata_urls.push(format!(
            "{}.well-known/oauth-protected-resource{}",
            origin.as_str(),
            path
        ));
    }
    resource_metadata_urls.push(
        origin
            .join(".well-known/oauth-protected-resource")
            .map_err(|error| mcp_mutation_error(400, "mcp_oauth_invalid_url", error.to_string()))?
            .to_string(),
    );
    resource_metadata_urls.sort();
    resource_metadata_urls.dedup();

    let mut resource_metadata = None::<Value>;
    let mut discovery_errors = Vec::new();
    for candidate in resource_metadata_urls {
        match oauth_get_json(&client, &candidate, "OAuth protected resource metadata") {
            Ok(value) => {
                resource_metadata = Some(value);
                break;
            }
            Err(error) => discovery_errors.push(error.message),
        }
    }
    let resource_metadata = resource_metadata.ok_or_else(|| {
        mcp_mutation_error(
            502,
            "mcp_oauth_discovery_failed",
            format!(
                "Remote MCP server did not expose OAuth protected resource metadata. {}",
                discovery_errors.join("; ")
            ),
        )
    })?;
    let authorization_server = resource_metadata
        .get("authorization_servers")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| origin.to_string().trim_end_matches('/').to_string());

    let auth_origin = oauth_origin_url(&authorization_server)?;
    let auth_url = Url::parse(&authorization_server)
        .map_err(|error| mcp_mutation_error(400, "mcp_oauth_invalid_url", error.to_string()))?;
    let auth_path = auth_url.path().trim_end_matches('/');
    let mut metadata_urls = Vec::new();
    if !auth_path.is_empty() {
        metadata_urls.push(format!(
            "{}.well-known/oauth-authorization-server{}",
            auth_origin.as_str(),
            auth_path
        ));
    }
    metadata_urls.push(
        auth_origin
            .join(".well-known/oauth-authorization-server")
            .map_err(|error| mcp_mutation_error(400, "mcp_oauth_invalid_url", error.to_string()))?
            .to_string(),
    );
    metadata_urls.sort();
    metadata_urls.dedup();
    let mut authorization_metadata = None::<Value>;
    let mut authorization_errors = Vec::new();
    for candidate in metadata_urls {
        match oauth_get_json(&client, &candidate, "OAuth authorization server metadata") {
            Ok(value) => {
                authorization_metadata = Some(value);
                break;
            }
            Err(error) => authorization_errors.push(error.message),
        }
    }
    let authorization_metadata = authorization_metadata.ok_or_else(|| {
        mcp_mutation_error(
            502,
            "mcp_oauth_discovery_failed",
            format!(
                "OAuth authorization server metadata could not be discovered. {}",
                authorization_errors.join("; ")
            ),
        )
    })?;
    let authorization_endpoint =
        oauth_json_string(&authorization_metadata, "authorization_endpoint").ok_or_else(|| {
            mcp_mutation_error(
                502,
                "mcp_oauth_metadata_invalid",
                "OAuth metadata is missing authorization_endpoint",
            )
        })?;
    let token_endpoint =
        oauth_json_string(&authorization_metadata, "token_endpoint").ok_or_else(|| {
            mcp_mutation_error(
                502,
                "mcp_oauth_metadata_invalid",
                "OAuth metadata is missing token_endpoint",
            )
        })?;
    let scopes_supported = authorization_metadata
        .get("scopes_supported")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Ok(McpOAuthMetadata {
        authorization_server,
        authorization_endpoint,
        token_endpoint,
        registration_endpoint: oauth_json_string(&authorization_metadata, "registration_endpoint"),
        revocation_endpoint: oauth_json_string(&authorization_metadata, "revocation_endpoint"),
        scopes_supported,
    })
}

fn register_mcp_oauth_client(
    metadata: &McpOAuthMetadata,
    redirect_uri: &str,
    timeout_ms: u64,
) -> Result<(String, Option<String>), McpConfigMutationError> {
    let endpoint = metadata.registration_endpoint.as_deref().ok_or_else(|| {
        mcp_mutation_error(
            409,
            "mcp_oauth_registration_unsupported",
            "OAuth server does not advertise dynamic client registration",
        )
    })?;
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms.clamp(1_000, 30_000)))
        .build()
        .map_err(|error| mcp_mutation_error(500, "mcp_oauth_client_failed", error.to_string()))?;
    let response = client
        .post(endpoint)
        .json(&json!({
            "client_name": "OpenAgent Desktop",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .map_err(|error| {
            mcp_mutation_error(
                502,
                "mcp_oauth_registration_failed",
                format!("OAuth client registration failed: {error}"),
            )
        })?;
    let status = response.status();
    let raw = response.text().map_err(|error| {
        mcp_mutation_error(502, "mcp_oauth_registration_failed", error.to_string())
    })?;
    if !status.is_success() {
        return Err(oauth_http_error(
            "OAuth client registration",
            status.as_u16(),
            &raw,
        ));
    }
    let payload = serde_json::from_str::<Value>(&raw).map_err(|error| {
        mcp_mutation_error(
            502,
            "mcp_oauth_registration_failed",
            format!("OAuth client registration response is invalid: {error}"),
        )
    })?;
    let client_id = oauth_json_string(&payload, "client_id").ok_or_else(|| {
        mcp_mutation_error(
            502,
            "mcp_oauth_registration_failed",
            "OAuth client registration response is missing client_id",
        )
    })?;
    Ok((client_id, oauth_json_string(&payload, "client_secret")))
}

fn mcp_oauth_server(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<RemoteMcpServerConfig, McpConfigMutationError> {
    let (_source, _manager, server, _workspace) = mcp_manager_for_server(config, encoded_name)?;
    if server.server_type == McpServerType::Remote {
        Ok(server)
    } else {
        Err(mcp_mutation_error(
            400,
            "mcp_oauth_local_unsupported",
            "OAuth is only available for remote MCP servers",
        ))
    }
}

fn mcp_oauth_status_for_server(
    config: &HttpRuntimeConfig,
    server: &RemoteMcpServerConfig,
) -> Value {
    if server.server_type != McpServerType::Remote {
        return json!({
            "supported": false,
            "status": "not_applicable",
            "connected": false,
            "pending": false,
            "refreshable": false,
        });
    }
    let Some(credential) = load_mcp_oauth_credential(config, &server.name) else {
        return json!({
            "supported": true,
            "status": "disconnected",
            "connected": false,
            "pending": false,
            "refreshable": false,
            "last_error": null,
        });
    };
    let matching_server = credential.server_url == server.url;
    let pending = credential.pending.as_ref().is_some_and(|pending| {
        now_ms().saturating_sub(pending.created_at_ms) <= MCP_OAUTH_STATE_TTL_MS
    });
    let expired = credential
        .expires_at_ms
        .is_some_and(|expires_at| expires_at <= now_ms());
    let connected = matching_server && credential.access_token.is_some() && !expired;
    let status = if !matching_server {
        "stale"
    } else if pending {
        "authorizing"
    } else if connected {
        "connected"
    } else if expired {
        "expired"
    } else if credential.last_error.is_some() {
        "error"
    } else {
        "disconnected"
    };
    json!({
        "supported": true,
        "status": status,
        "connected": connected,
        "pending": pending,
        "refreshable": credential.refresh_token.is_some(),
        "expires_at": credential.expires_at_ms.map(|value| value as f64 / 1000.0),
        "connected_at": credential.connected_at_ms.map(|value| value as f64 / 1000.0),
        "authorization_server": credential.authorization_server,
        "last_error": credential.last_error.as_deref().map(sanitize_mcp_status_error),
    })
}

pub(super) fn apply_mcp_oauth_credentials(
    config: &HttpRuntimeConfig,
    mcp_config: &mut openagent_mcp::McpConfig,
) {
    for server in &mut mcp_config.servers {
        if server.server_type != McpServerType::Remote {
            continue;
        }
        let Some(credential) = load_mcp_oauth_credential(config, &server.name) else {
            continue;
        };
        if credential.server_url != server.url
            || credential
                .expires_at_ms
                .is_some_and(|value| value <= now_ms())
        {
            continue;
        }
        let Some(access_token) = credential.access_token else {
            continue;
        };
        let token_type = credential.token_type.as_deref().unwrap_or("Bearer");
        server.headers.insert(
            "Authorization".to_string(),
            format!("{token_type} {access_token}"),
        );
    }
}

pub(super) fn mcp_oauth_login_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
    body: &str,
) -> Result<Value, McpConfigMutationError> {
    let server = mcp_oauth_server(config, encoded_name)?;
    let request = if body.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str::<Value>(body).map_err(|error| {
            mcp_mutation_error(400, "mcp_oauth_request_invalid", error.to_string())
        })?
    };
    let metadata = discover_mcp_oauth_metadata(&server)?;
    let redirect_uri = mcp_oauth_redirect_uri(config);
    let existing = load_mcp_oauth_credential(config, &server.name)
        .filter(|credential| credential.server_url == server.url);
    let requested_client_id = oauth_json_string(&request, "client_id");
    let requested_client_secret = oauth_json_string(&request, "client_secret");
    let (client_id, client_secret) = if let Some(client_id) = requested_client_id {
        (client_id, requested_client_secret)
    } else if let Some(existing) = existing.as_ref().filter(|credential| {
        credential.authorization_server == metadata.authorization_server
            && !credential.client_id.is_empty()
    }) {
        (existing.client_id.clone(), existing.client_secret.clone())
    } else {
        register_mcp_oauth_client(&metadata, &redirect_uri, server.timeout_ms)?
    };
    let code_verifier = random_oauth_token(64);
    let code_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
    let state = random_oauth_token(32);
    let requested_scope = oauth_json_string(&request, "scope");
    let scope = requested_scope.or_else(|| {
        (!metadata.scopes_supported.is_empty()).then(|| metadata.scopes_supported.join(" "))
    });
    let mut authorization_url = Url::parse(&metadata.authorization_endpoint).map_err(|error| {
        mcp_mutation_error(
            502,
            "mcp_oauth_metadata_invalid",
            format!("OAuth authorization endpoint is invalid: {error}"),
        )
    })?;
    {
        let mut query = authorization_url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &code_challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", &server.url);
        if let Some(scope) = scope.as_deref() {
            query.append_pair("scope", scope);
        }
    }
    let credential = McpOAuthCredential {
        schema_version: MCP_OAUTH_SCHEMA_VERSION,
        server_name: server.name.clone(),
        server_url: server.url.clone(),
        authorization_server: metadata.authorization_server,
        authorization_endpoint: metadata.authorization_endpoint,
        token_endpoint: metadata.token_endpoint,
        registration_endpoint: metadata.registration_endpoint,
        revocation_endpoint: metadata.revocation_endpoint,
        client_id,
        client_secret,
        access_token: existing
            .as_ref()
            .and_then(|value| value.access_token.clone()),
        refresh_token: existing
            .as_ref()
            .and_then(|value| value.refresh_token.clone()),
        token_type: existing.as_ref().and_then(|value| value.token_type.clone()),
        scope,
        expires_at_ms: existing.as_ref().and_then(|value| value.expires_at_ms),
        connected_at_ms: existing.as_ref().and_then(|value| value.connected_at_ms),
        pending: Some(McpOAuthPending {
            state: state.clone(),
            code_verifier,
            redirect_uri,
            created_at_ms: now_ms(),
        }),
        last_error: None,
    };
    write_mcp_oauth_credential(config, &credential)?;
    let mut payload = mcp_payload(config, "/api/mcp");
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            "oauth_login".to_string(),
            json!({
                "server_name": server.name,
                "authorization_url": authorization_url.to_string(),
                "status": "authorizing",
            }),
        );
    }
    Ok(payload)
}

fn exchange_mcp_oauth_token(
    credential: &McpOAuthCredential,
    form: &mut BTreeMap<String, String>,
    timeout_ms: u64,
) -> Result<Value, McpConfigMutationError> {
    form.insert("client_id".to_string(), credential.client_id.clone());
    if let Some(secret) = credential.client_secret.as_ref() {
        form.insert("client_secret".to_string(), secret.clone());
    }
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(timeout_ms.clamp(1_000, 30_000)))
        .build()
        .map_err(|error| mcp_mutation_error(500, "mcp_oauth_client_failed", error.to_string()))?;
    let response = client
        .post(&credential.token_endpoint)
        .form(form)
        .send()
        .map_err(|error| {
            mcp_mutation_error(
                502,
                "mcp_oauth_token_failed",
                format!("OAuth token request failed: {error}"),
            )
        })?;
    let status = response.status();
    let raw = response
        .text()
        .map_err(|error| mcp_mutation_error(502, "mcp_oauth_token_failed", error.to_string()))?;
    if !status.is_success() {
        return Err(oauth_http_error(
            "OAuth token endpoint",
            status.as_u16(),
            &raw,
        ));
    }
    serde_json::from_str(&raw).map_err(|error| {
        mcp_mutation_error(
            502,
            "mcp_oauth_token_failed",
            format!("OAuth token response is invalid: {error}"),
        )
    })
}

fn apply_oauth_token_response(
    credential: &mut McpOAuthCredential,
    payload: &Value,
) -> Result<(), McpConfigMutationError> {
    let access_token = oauth_json_string(payload, "access_token").ok_or_else(|| {
        mcp_mutation_error(
            502,
            "mcp_oauth_token_failed",
            "OAuth token response is missing access_token",
        )
    })?;
    credential.access_token = Some(access_token);
    if let Some(refresh_token) = oauth_json_string(payload, "refresh_token") {
        credential.refresh_token = Some(refresh_token);
    }
    credential.token_type =
        oauth_json_string(payload, "token_type").or_else(|| Some("Bearer".to_string()));
    credential.scope = oauth_json_string(payload, "scope").or_else(|| credential.scope.clone());
    credential.expires_at_ms = payload
        .get("expires_in")
        .and_then(Value::as_u64)
        .map(|seconds| now_ms().saturating_add(seconds.saturating_mul(1_000)));
    credential.connected_at_ms = Some(now_ms());
    credential.pending = None;
    credential.last_error = None;
    Ok(())
}

fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn mcp_oauth_html_response(status: u16, title: &str, message: &str) -> HttpResponseSpec {
    HttpResponseSpec {
        status,
        content_type: Some("text/html; charset=utf-8".to_string()),
        headers: Map::new(),
        body: None,
        body_text: Some(format!(
            "<!doctype html><meta charset=\"utf-8\"><title>{}</title><style>body{{font:16px system-ui;margin:48px;color:#202124}}main{{max-width:560px}}p{{color:#5f6368;line-height:1.6}}</style><main><h1>{}</h1><p>{}</p><p>You can close this window and return to OpenAgent.</p></main>",
            html_escape(title),
            html_escape(title),
            html_escape(message),
        )),
    }
}

pub(super) fn mcp_oauth_callback_response(
    config: &HttpRuntimeConfig,
    request_path: &str,
) -> HttpResponseSpec {
    let state = query_param(request_path, "state").unwrap_or_default();
    if state.is_empty() {
        return mcp_oauth_html_response(400, "OAuth connection failed", "Missing OAuth state.");
    }
    let directory = mcp_oauth_directory(config);
    let credential = fs::read_dir(&directory).ok().and_then(|entries| {
        entries.filter_map(Result::ok).find_map(|entry| {
            let raw = fs::read_to_string(entry.path()).ok()?;
            let credential = serde_json::from_str::<McpOAuthCredential>(&raw).ok()?;
            credential
                .pending
                .as_ref()
                .is_some_and(|pending| pending.state == state)
                .then_some(credential)
        })
    });
    let Some(mut credential) = credential else {
        return mcp_oauth_html_response(
            400,
            "OAuth connection expired",
            "This login request is no longer active. Start Connect again in OpenAgent Settings.",
        );
    };
    let Some(pending) = credential.pending.clone() else {
        return mcp_oauth_html_response(400, "OAuth connection failed", "OAuth state is invalid.");
    };
    if now_ms().saturating_sub(pending.created_at_ms) > MCP_OAUTH_STATE_TTL_MS {
        credential.pending = None;
        credential.last_error = Some("OAuth login expired before callback".to_string());
        let _ = write_mcp_oauth_credential(config, &credential);
        return mcp_oauth_html_response(
            400,
            "OAuth connection expired",
            "Start Connect again in OpenAgent Settings.",
        );
    }
    if let Some(error) = query_param(request_path, "error") {
        let description = query_param(request_path, "error_description").unwrap_or(error);
        credential.pending = None;
        credential.last_error = Some(sanitize_mcp_status_error(&description));
        let _ = write_mcp_oauth_credential(config, &credential);
        return mcp_oauth_html_response(400, "OAuth connection denied", &description);
    }
    let Some(code) = query_param(request_path, "code") else {
        return mcp_oauth_html_response(
            400,
            "OAuth connection failed",
            "Missing authorization code.",
        );
    };
    let mut form = BTreeMap::from([
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code),
        ("redirect_uri".to_string(), pending.redirect_uri),
        ("code_verifier".to_string(), pending.code_verifier),
    ]);
    match exchange_mcp_oauth_token(&credential, &mut form, 15_000)
        .and_then(|payload| apply_oauth_token_response(&mut credential, &payload))
        .and_then(|()| write_mcp_oauth_credential(config, &credential))
    {
        Ok(()) => mcp_oauth_html_response(
            200,
            "MCP connected",
            &format!("{} is now connected securely.", credential.server_name),
        ),
        Err(error) => {
            credential.pending = None;
            credential.last_error = Some(sanitize_mcp_status_error(&error.message));
            let _ = write_mcp_oauth_credential(config, &credential);
            mcp_oauth_html_response(502, "OAuth connection failed", &error.message)
        }
    }
}

pub(super) fn mcp_oauth_refresh_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let server = mcp_oauth_server(config, encoded_name)?;
    let mut credential = load_mcp_oauth_credential(config, &server.name).ok_or_else(|| {
        mcp_mutation_error(
            404,
            "mcp_oauth_not_connected",
            "MCP server is not connected with OAuth",
        )
    })?;
    if credential.server_url != server.url {
        return Err(mcp_mutation_error(
            409,
            "mcp_oauth_stale",
            "MCP server URL changed; connect again",
        ));
    }
    let refresh_token = credential.refresh_token.clone().ok_or_else(|| {
        mcp_mutation_error(
            409,
            "mcp_oauth_refresh_unavailable",
            "OAuth server did not provide a refresh token; connect again",
        )
    })?;
    let mut form = BTreeMap::from([
        ("grant_type".to_string(), "refresh_token".to_string()),
        ("refresh_token".to_string(), refresh_token),
    ]);
    if let Some(scope) = credential.scope.clone() {
        form.insert("scope".to_string(), scope);
    }
    match exchange_mcp_oauth_token(&credential, &mut form, server.timeout_ms)
        .and_then(|payload| apply_oauth_token_response(&mut credential, &payload))
    {
        Ok(()) => write_mcp_oauth_credential(config, &credential)?,
        Err(error) => {
            credential.last_error = Some(sanitize_mcp_status_error(&error.message));
            write_mcp_oauth_credential(config, &credential)?;
            return Err(error);
        }
    }
    Ok(mcp_payload(config, "/api/mcp"))
}

pub(super) fn mcp_oauth_revoke_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let server = mcp_oauth_server(config, encoded_name)?;
    let Some(credential) = load_mcp_oauth_credential(config, &server.name) else {
        return Ok(mcp_payload(config, "/api/mcp"));
    };
    if let (Some(endpoint), Some(token)) = (
        credential.revocation_endpoint.as_deref(),
        credential
            .refresh_token
            .as_ref()
            .or(credential.access_token.as_ref()),
    ) {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_millis(
                server.timeout_ms.clamp(1_000, 30_000),
            ))
            .build()
            .map_err(|error| {
                mcp_mutation_error(500, "mcp_oauth_client_failed", error.to_string())
            })?;
        let mut form = BTreeMap::from([
            ("token".to_string(), token.clone()),
            ("client_id".to_string(), credential.client_id.clone()),
        ]);
        if let Some(secret) = credential.client_secret.as_ref() {
            form.insert("client_secret".to_string(), secret.clone());
        }
        let response = client.post(endpoint).form(&form).send().map_err(|error| {
            mcp_mutation_error(
                502,
                "mcp_oauth_revoke_failed",
                format!("OAuth revoke request failed: {error}"),
            )
        })?;
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response.text().unwrap_or_default();
            return Err(oauth_http_error("OAuth revoke endpoint", status, &body));
        }
    }
    remove_mcp_oauth_credential(config, &server.name);
    Ok(mcp_payload(config, "/api/mcp"))
}

pub(super) fn mcp_oauth_status_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let server = mcp_oauth_server(config, encoded_name)?;
    Ok(json!({
        "server_name": server.name,
        "oauth": mcp_oauth_status_for_server(config, &server),
    }))
}

pub(super) fn mcp_payload(config: &HttpRuntimeConfig, request_path: &str) -> Value {
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    let refresh = query_flag(request_path, "refresh") || query_flag(request_path, "check");
    let source = mcp_config_source(config, &env);
    let loaded = match source
        .read_source
        .as_deref()
        .map(load_mcp_config)
        .transpose()
    {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "configured": false,
                "enabled": false,
                "server_count": 0,
                "tool_count": 0,
                "refresh_ttl_s": null,
                "source": source.label,
                "writable": source.writable_path.is_some(),
                "config_path": source.writable_path.as_ref().map(|path| path.to_string_lossy().to_string()),
                "readonly_reason": source.readonly_reason,
                "status": "error",
                "error": error,
                "servers": [],
            });
        }
    };
    let Some(mut mcp_config) = loaded else {
        return json!({
            "configured": false,
            "enabled": false,
            "server_count": 0,
            "tool_count": 0,
            "refresh_ttl_s": null,
            "source": source.label,
            "writable": source.writable_path.is_some(),
            "config_path": source.writable_path.as_ref().map(|path| path.to_string_lossy().to_string()),
            "readonly_reason": source.readonly_reason,
            "status": "unconfigured",
            "error": null,
            "servers": [],
        });
    };
    apply_mcp_oauth_credentials(config, &mut mcp_config);
    let workspace_root = workspace(config);
    let mut manager = RemoteMcpManager::new(mcp_config);
    if refresh && manager.enabled() {
        refresh_mcp_manager_tools(&mut manager, &workspace_root);
    }
    apply_mcp_lifecycle_to_manager(&mut manager, &workspace_root);
    mcp_manager_payload(config, &source, &manager, &workspace_root)
}

pub(super) fn mcp_manager_payload(
    config: &HttpRuntimeConfig,
    source: &McpConfigSource,
    manager: &RemoteMcpManager,
    workspace: &Path,
) -> Value {
    let mut tool_count = 0usize;
    let servers = manager
        .servers
        .values()
        .map(|state| {
            let lifecycle = mcp_lifecycle_snapshot(&state.config, workspace);
            let tools = state
                .tools_by_dynamic_name
                .values()
                .map(|descriptor| {
                    json!({
                        "name": descriptor.dynamic_name,
                        "original_name": descriptor.original_name,
                        "title": descriptor.title,
                        "description": descriptor.description,
                    })
                })
                .collect::<Vec<_>>();
            tool_count = tool_count.saturating_add(tools.len());
            json!({
                "name": &state.config.name,
                "type": state.config.server_type.as_str(),
                "enabled": state.config.enabled,
                "transport": state.config.transport.as_str(),
                "selected_transport": state.selected_transport.map(|transport| transport.as_str()),
                "status": &state.status,
                "tool_count": tools.len(),
                "tools": tools,
                "remote_url_configured": !state.config.url.is_empty(),
                "command": state.config.command.first().cloned().unwrap_or_default(),
                "args_count": state.config.command.len().saturating_sub(1),
                "cwd_configured": state.config.cwd.is_some(),
                "env_count": state.config.environment.len(),
                "header_count": state.config.headers.len(),
                "oauth": mcp_oauth_status_for_server(config, &state.config),
                "timeout_ms": state.config.timeout_ms,
                "lifecycle_status": lifecycle.status,
                "lifecycle_pid": lifecycle.pid,
                "lifecycle_started_at": lifecycle.started_at_ms.map(|value| value as f64 / 1000.0),
                "lifecycle_last_refreshed_at": lifecycle.last_refreshed_at_ms.map(|value| value as f64 / 1000.0),
                "lifecycle_tool_count": lifecycle.tool_count,
                "last_error": &state.last_error,
                "last_refreshed_at": state.last_refreshed_at,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "configured": !manager.servers.is_empty(),
        "enabled": manager.enabled(),
        "server_count": servers.len(),
        "tool_count": tool_count,
        "refresh_ttl_s": manager.config.refresh_ttl_s,
        "source": source.label,
        "writable": source.writable_path.is_some(),
        "config_path": source.writable_path.as_ref().map(|path| path.to_string_lossy().to_string()),
        "readonly_reason": source.readonly_reason,
        "status": mcp_manager_status(manager),
        "error": null,
        "servers": servers,
    })
}

pub(super) fn mcp_add_server_payload(
    config: &HttpRuntimeConfig,
    body: &str,
) -> Result<Value, McpConfigMutationError> {
    mutate_mcp_config(config, |servers| {
        let payload = parse_mcp_server_request_body(body)?;
        let name = mcp_server_name_from_body(&payload)?;
        let server = build_mcp_server_config_value(&payload, None)?;
        servers.insert(name, server);
        Ok(())
    })
}

pub(super) fn mcp_update_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
    body: &str,
) -> Result<Value, McpConfigMutationError> {
    let name = mcp_server_name_from_path(encoded_name)?;
    mutate_mcp_config(config, |servers| {
        let existing = servers.get(&name).cloned().ok_or_else(|| {
            mcp_mutation_error(404, "mcp_server_not_found", "MCP server not found")
        })?;
        let payload = parse_mcp_server_request_body(body)?;
        let server = build_mcp_server_config_value(&payload, Some(&existing))?;
        servers.insert(name, server);
        Ok(())
    })
}

pub(super) fn mcp_delete_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let name = mcp_server_name_from_path(encoded_name)?;
    let payload = mutate_mcp_config(config, |servers| {
        if servers.remove(&name).is_none() {
            return Err(mcp_mutation_error(
                404,
                "mcp_server_not_found",
                "MCP server not found",
            ));
        }
        Ok(())
    })?;
    remove_mcp_oauth_credential(config, &name);
    Ok(payload)
}

pub(super) fn mcp_test_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let (source, mut manager, _server, workspace_root) =
        mcp_manager_for_server(config, encoded_name)?;
    let name = mcp_server_name_from_path(encoded_name)?;
    refresh_mcp_manager_server_tools(&mut manager, &name, &workspace_root);
    apply_mcp_lifecycle_to_manager(&mut manager, &workspace_root);
    Ok(mcp_manager_payload(
        config,
        &source,
        &manager,
        &workspace_root,
    ))
}

pub(super) fn mcp_lifecycle_start_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let (source, mut manager, server, workspace_root) =
        mcp_manager_for_server(config, encoded_name)?;
    ensure_local_mcp_server(&server)?;
    if let Some(result) = refresh_mcp_lifecycle_server(&server, &workspace_root) {
        match result {
            Ok(descriptors) => {
                let _ = manager.set_server_tools(
                    &server.name,
                    Some(McpTransport::Stdio),
                    "connected",
                    Some(now_ms() as f64 / 1000.0),
                    descriptors,
                );
                return Ok(mcp_manager_payload(
                    config,
                    &source,
                    &manager,
                    &workspace_root,
                ));
            }
            Err(error) => {
                let _ = manager.set_server_error(
                    &server.name,
                    "error",
                    sanitize_mcp_status_error(&error),
                    Some(now_ms() as f64 / 1000.0),
                );
                return Ok(mcp_manager_payload(
                    config,
                    &source,
                    &manager,
                    &workspace_root,
                ));
            }
        }
    }

    let started_at = now_ms();
    match StdioMcpSession::start(&server, &workspace_root) {
        Ok(mut session) => match session.tools_list() {
            Ok(tools) => {
                let descriptors = build_tool_descriptors_from_values(&server, &tools);
                stop_mcp_lifecycle_server(&server, &workspace_root);
                let key = mcp_lifecycle_key(&workspace_root, &server.name);
                if let Ok(mut registry) = mcp_lifecycle_registry().lock() {
                    registry.insert(
                        key,
                        McpLifecycleEntry {
                            config_fingerprint: mcp_server_fingerprint(&server),
                            session,
                            descriptors: descriptors.clone(),
                            started_at_ms: started_at,
                            last_refreshed_at_ms: started_at,
                        },
                    );
                    let _ = manager.set_server_tools(
                        &server.name,
                        Some(McpTransport::Stdio),
                        "connected",
                        Some(started_at as f64 / 1000.0),
                        descriptors,
                    );
                } else {
                    session.close();
                    let _ = manager.set_server_error(
                        &server.name,
                        "error",
                        "MCP lifecycle registry is unavailable",
                        Some(now_ms() as f64 / 1000.0),
                    );
                }
            }
            Err(error) => {
                session.close();
                let _ = manager.set_server_error(
                    &server.name,
                    "error",
                    sanitize_mcp_status_error(&error),
                    Some(now_ms() as f64 / 1000.0),
                );
            }
        },
        Err(error) => {
            let _ = manager.set_server_error(
                &server.name,
                "error",
                sanitize_mcp_status_error(&error),
                Some(now_ms() as f64 / 1000.0),
            );
        }
    }
    Ok(mcp_manager_payload(
        config,
        &source,
        &manager,
        &workspace_root,
    ))
}

pub(super) fn mcp_lifecycle_stop_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let (source, mut manager, server, workspace_root) =
        mcp_manager_for_server(config, encoded_name)?;
    ensure_local_mcp_server(&server)?;
    stop_mcp_lifecycle_server(&server, &workspace_root);
    let _ = manager.set_server_tools(
        &server.name,
        Some(McpTransport::Stdio),
        "stopped",
        Some(now_ms() as f64 / 1000.0),
        Vec::new(),
    );
    Ok(mcp_manager_payload(
        config,
        &source,
        &manager,
        &workspace_root,
    ))
}

pub(super) fn mcp_lifecycle_restart_server_payload(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<Value, McpConfigMutationError> {
    let (_source, _manager, server, workspace_root) = mcp_manager_for_server(config, encoded_name)?;
    ensure_local_mcp_server(&server)?;
    stop_mcp_lifecycle_server(&server, &workspace_root);
    mcp_lifecycle_start_server_payload(config, encoded_name)
}

pub(super) fn mcp_manager_for_server(
    config: &HttpRuntimeConfig,
    encoded_name: &str,
) -> Result<
    (
        McpConfigSource,
        RemoteMcpManager,
        RemoteMcpServerConfig,
        PathBuf,
    ),
    McpConfigMutationError,
> {
    let name = mcp_server_name_from_path(encoded_name)?;
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    let source = mcp_config_source(config, &env);
    let loaded = source
        .read_source
        .as_deref()
        .map(load_mcp_config)
        .transpose()
        .map_err(|error| mcp_mutation_error(400, "mcp_config_invalid", error))?;
    let mut mcp_config = loaded.ok_or_else(|| {
        mcp_mutation_error(
            404,
            "mcp_config_unconfigured",
            "MCP config is not configured.",
        )
    })?;
    apply_mcp_oauth_credentials(config, &mut mcp_config);
    let manager = RemoteMcpManager::new(mcp_config);
    let server = manager
        .config
        .servers
        .iter()
        .find(|server| server.name == name)
        .cloned()
        .ok_or_else(|| mcp_mutation_error(404, "mcp_server_not_found", "MCP server not found"))?;
    Ok((source, manager, server, workspace(config)))
}

pub(super) fn ensure_local_mcp_server(
    server: &RemoteMcpServerConfig,
) -> Result<(), McpConfigMutationError> {
    if server.server_type == McpServerType::Local {
        Ok(())
    } else {
        Err(mcp_mutation_error(
            400,
            "mcp_lifecycle_remote_unsupported",
            "MCP lifecycle is only supported for local stdio servers.",
        ))
    }
}

pub(super) fn mcp_config_response(
    result: Result<Value, McpConfigMutationError>,
) -> HttpResponseSpec {
    match result {
        Ok(payload) => json_response(200, payload),
        Err(error) => json_response(
            error.status,
            json!({
                "error": error.message,
                "error_code": error.code,
            }),
        ),
    }
}

pub(super) fn mutate_mcp_config<F>(
    config: &HttpRuntimeConfig,
    mutation: F,
) -> Result<Value, McpConfigMutationError>
where
    F: FnOnce(&mut Map<String, Value>) -> Result<(), McpConfigMutationError>,
{
    let env = std::env::vars().collect::<BTreeMap<_, _>>();
    let source = mcp_config_source(config, &env);
    let path = source.writable_path.ok_or_else(|| {
        mcp_mutation_error(
            409,
            "mcp_config_readonly",
            source
                .readonly_reason
                .unwrap_or("MCP config source is read-only"),
        )
    })?;
    let mut raw = read_mutable_mcp_config(&path)?;
    let shape = detect_mcp_servers_shape(&raw)?;
    {
        let servers = mcp_servers_object_mut(&mut raw, shape)?;
        mutation(servers)?;
    }
    load_mcp_config_from_value(&raw)
        .map_err(|error| mcp_mutation_error(400, "mcp_config_invalid", error))?;
    write_mcp_config_file(&path, &raw)?;
    Ok(mcp_payload(config, "/api/mcp"))
}

pub(super) fn read_mutable_mcp_config(path: &Path) -> Result<Value, McpConfigMutationError> {
    if !path.exists() {
        return Ok(json!({"mcp": {"servers": {}}}));
    }
    let raw = fs::read_to_string(path).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_config_read_failed",
            format!("failed to read MCP config: {error}"),
        )
    })?;
    let value = serde_json::from_str::<Value>(&raw).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_config_invalid_json",
            format!("MCP config is not valid JSON: {error}"),
        )
    })?;
    if !value.is_object() {
        return Err(mcp_mutation_error(
            400,
            "mcp_config_invalid",
            "MCP config JSON must be an object.",
        ));
    }
    Ok(value)
}

pub(super) fn write_mcp_config_file(
    path: &Path,
    value: &Value,
) -> Result<(), McpConfigMutationError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            mcp_mutation_error(
                400,
                "mcp_config_write_failed",
                format!("failed to create MCP config directory: {error}"),
            )
        })?;
    }
    let rendered = serde_json::to_string_pretty(value).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_config_write_failed",
            format!("failed to serialize MCP config: {error}"),
        )
    })?;
    fs::write(path, format!("{rendered}\n")).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_config_write_failed",
            format!("failed to write MCP config: {error}"),
        )
    })
}

pub(super) fn detect_mcp_servers_shape(
    value: &Value,
) -> Result<McpServersShape, McpConfigMutationError> {
    let object = value.as_object().ok_or_else(|| {
        mcp_mutation_error(
            400,
            "mcp_config_invalid",
            "MCP config JSON must be an object.",
        )
    })?;
    if let Some(mcp_servers) = object.get("mcpServers") {
        if !mcp_servers.is_object() {
            return Err(mcp_mutation_error(
                400,
                "mcp_config_invalid",
                "mcpServers must be an object.",
            ));
        }
        return Ok(McpServersShape::McpServers);
    }
    if let Some(mcp) = object.get("mcp") {
        let Some(mcp_object) = mcp.as_object() else {
            return Err(mcp_mutation_error(
                400,
                "mcp_config_invalid",
                "mcp must be an object.",
            ));
        };
        if let Some(servers) = mcp_object.get("servers") {
            if !servers.is_object() {
                return Err(mcp_mutation_error(
                    400,
                    "mcp_config_invalid",
                    "mcp.servers must be an object.",
                ));
            }
            return Ok(McpServersShape::McpNested);
        }
        return Ok(McpServersShape::McpDirect);
    }
    if object.is_empty() {
        Ok(McpServersShape::Canonical)
    } else {
        Ok(McpServersShape::RootDirect)
    }
}

pub(super) fn mcp_servers_object_mut(
    value: &mut Value,
    shape: McpServersShape,
) -> Result<&mut Map<String, Value>, McpConfigMutationError> {
    match shape {
        McpServersShape::Canonical => {
            let object = value.as_object_mut().ok_or_else(|| {
                mcp_mutation_error(
                    400,
                    "mcp_config_invalid",
                    "MCP config JSON must be an object.",
                )
            })?;
            let mcp = object.entry("mcp").or_insert_with(|| json!({}));
            let mcp_object = mcp.as_object_mut().ok_or_else(|| {
                mcp_mutation_error(400, "mcp_config_invalid", "mcp must be an object.")
            })?;
            let servers = mcp_object.entry("servers").or_insert_with(|| json!({}));
            servers.as_object_mut().ok_or_else(|| {
                mcp_mutation_error(400, "mcp_config_invalid", "mcp.servers must be an object.")
            })
        }
        McpServersShape::McpServers => value
            .as_object_mut()
            .and_then(|object| object.get_mut("mcpServers"))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                mcp_mutation_error(400, "mcp_config_invalid", "mcpServers must be an object.")
            }),
        McpServersShape::McpNested => value
            .as_object_mut()
            .and_then(|object| object.get_mut("mcp"))
            .and_then(Value::as_object_mut)
            .and_then(|mcp| mcp.get_mut("servers"))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| {
                mcp_mutation_error(400, "mcp_config_invalid", "mcp.servers must be an object.")
            }),
        McpServersShape::McpDirect => value
            .as_object_mut()
            .and_then(|object| object.get_mut("mcp"))
            .and_then(Value::as_object_mut)
            .ok_or_else(|| mcp_mutation_error(400, "mcp_config_invalid", "mcp must be an object.")),
        McpServersShape::RootDirect => value.as_object_mut().ok_or_else(|| {
            mcp_mutation_error(
                400,
                "mcp_config_invalid",
                "MCP config JSON must be an object.",
            )
        }),
    }
}

pub(super) fn parse_mcp_server_request_body(
    body: &str,
) -> Result<Map<String, Value>, McpConfigMutationError> {
    let value = serde_json::from_str::<Value>(body).map_err(|error| {
        mcp_mutation_error(
            400,
            "mcp_request_invalid_json",
            format!("request body is not valid JSON: {error}"),
        )
    })?;
    value.as_object().cloned().ok_or_else(|| {
        mcp_mutation_error(
            400,
            "mcp_request_invalid",
            "request body must be a JSON object.",
        )
    })
}

pub(super) fn mcp_server_name_from_body(
    payload: &Map<String, Value>,
) -> Result<String, McpConfigMutationError> {
    let raw = payload
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| mcp_mutation_error(400, "mcp_server_name_missing", "name is required"))?;
    validate_mcp_server_name(raw)
}

pub(super) fn mcp_server_name_from_path(
    encoded_name: &str,
) -> Result<String, McpConfigMutationError> {
    let decoded = percent_decode(encoded_name);
    validate_mcp_server_name(&decoded)
}

pub(super) fn validate_mcp_server_name(raw: &str) -> Result<String, McpConfigMutationError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(mcp_mutation_error(
            400,
            "mcp_server_name_invalid",
            "MCP server name must be non-empty.",
        ));
    }
    if name.len() > 128 || name.contains('/') || name.contains('\\') || name.contains('?') {
        return Err(mcp_mutation_error(
            400,
            "mcp_server_name_invalid",
            "MCP server name contains unsupported path characters.",
        ));
    }
    Ok(name.to_string())
}

pub(super) fn build_mcp_server_config_value(
    payload: &Map<String, Value>,
    existing: Option<&Value>,
) -> Result<Value, McpConfigMutationError> {
    let mut server = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for key in [
        "type",
        "url",
        "transport",
        "enabled",
        "disabled",
        "command",
        "cwd",
        "env",
        "environment",
        "headers",
        "timeout_ms",
        "timeout",
        "tools",
    ] {
        if let Some(value) = payload.get(key) {
            server.insert(key.to_string(), value.clone());
        }
    }
    if let Some(command_text) = payload.get("command").and_then(Value::as_str) {
        let mut command = Vec::new();
        let command_text = command_text.trim();
        if !command_text.is_empty() {
            command.push(Value::String(command_text.to_string()));
        }
        if let Some(args) = payload.get("args").and_then(Value::as_array) {
            command.extend(
                args.iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|item| !item.is_empty())
                    .map(|item| Value::String(item.to_string())),
            );
        }
        server.insert("command".to_string(), Value::Array(command));
    }
    if !server.contains_key("enabled") && !server.contains_key("disabled") {
        server.insert("enabled".to_string(), Value::Bool(true));
    }
    Ok(Value::Object(server))
}

pub(super) fn mcp_mutation_error(
    status: u16,
    code: &'static str,
    message: impl Into<String>,
) -> McpConfigMutationError {
    McpConfigMutationError {
        status,
        code,
        message: message.into(),
    }
}

pub(super) fn refresh_mcp_manager_tools(manager: &mut RemoteMcpManager, workspace: &Path) {
    let server_names = manager
        .config
        .servers
        .iter()
        .filter(|server| server.enabled)
        .map(|server| server.name.clone())
        .collect::<Vec<_>>();
    for server_name in server_names {
        refresh_mcp_manager_server_tools(manager, &server_name, workspace);
    }
}

pub(super) fn refresh_mcp_manager_server_tools(
    manager: &mut RemoteMcpManager,
    server_name: &str,
    workspace: &Path,
) {
    let Some(server) = manager
        .config
        .servers
        .iter()
        .find(|server| server.name == server_name)
        .cloned()
    else {
        return;
    };
    let refreshed_at = Some(now_ms() as f64 / 1000.0);
    if let Some(result) = refresh_mcp_lifecycle_server(&server, workspace) {
        match result {
            Ok(descriptors) => {
                let _ = manager.set_server_tools(
                    &server.name,
                    Some(McpTransport::Stdio),
                    "connected",
                    refreshed_at,
                    descriptors,
                );
            }
            Err(error) => {
                let _ = manager.set_server_error(
                    &server.name,
                    "error",
                    sanitize_mcp_status_error(&error),
                    refreshed_at,
                );
            }
        }
        return;
    }
    match discover_mcp_server_tools(&server, workspace) {
        Ok((transport, tools)) => {
            let descriptors = build_tool_descriptors_from_values(&server, &tools);
            let _ = manager.set_server_tools(
                &server.name,
                Some(transport),
                "connected",
                refreshed_at,
                descriptors,
            );
        }
        Err(error) => {
            let _ = manager.set_server_error(
                &server.name,
                "error",
                sanitize_mcp_status_error(&error),
                refreshed_at,
            );
        }
    }
}

pub(super) fn mcp_manager_status(manager: &RemoteMcpManager) -> &'static str {
    if !manager.enabled() {
        return "disabled";
    }
    if manager
        .servers
        .values()
        .filter(|state| state.config.enabled)
        .any(|state| state.status == "error")
    {
        return "error";
    }
    if manager
        .servers
        .values()
        .filter(|state| state.config.enabled)
        .any(|state| state.status == "connected")
    {
        return "connected";
    }
    "idle"
}

pub(super) fn sanitize_mcp_status_error(error: &str) -> String {
    let lower = error.to_ascii_lowercase();
    if [
        "authorization",
        "bearer ",
        "api_key",
        "apikey",
        "password",
        "secret",
        "access_token",
        "refresh_token",
        "client_secret",
        "code_verifier",
        "token=",
        "token:",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
    {
        return "MCP discovery failed with sensitive details redacted".to_string();
    }
    truncate_mcp_status_error(error, 500)
}

pub(super) fn truncate_mcp_status_error(error: &str, max_chars: usize) -> String {
    if error.chars().count() <= max_chars {
        return error.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    format!("{}...", error.chars().take(keep).collect::<String>())
}

#[cfg(test)]
mod oauth_tests {
    use super::*;
    use std::net::{TcpListener, TcpStream};

    fn respond(stream: &mut TcpStream, status: &str, extra_headers: &[(&str, String)], body: &str) {
        let mut response = format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
            body.len()
        );
        for (name, value) in extra_headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");
        response.push_str(body);
        stream
            .write_all(response.as_bytes())
            .expect("mock OAuth response write");
    }

    #[test]
    fn remote_oauth_login_refresh_revoke_and_restart_round_trip() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("mock OAuth bind");
        let port = listener.local_addr().expect("mock OAuth addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let server_base = base.clone();
        let server = thread::spawn(move || {
            for step in 0..8 {
                let (mut stream, _) = listener.accept().expect("mock OAuth accept");
                let request = read_http_request(&mut stream).expect("mock OAuth request");
                match step {
                    0 => respond(
                        &mut stream,
                        "401 Unauthorized",
                        &[(
                            "www-authenticate",
                            format!(
                                "Bearer resource_metadata=\"{server_base}/.well-known/oauth-protected-resource\""
                            ),
                        )],
                        "{}",
                    ),
                    1 => {
                        assert_eq!(request.path, "/.well-known/oauth-protected-resource");
                        respond(
                            &mut stream,
                            "200 OK",
                            &[],
                            &stable_json_dumps(&json!({
                                "resource": format!("{server_base}/mcp"),
                                "authorization_servers": [server_base],
                            })),
                        );
                    }
                    2 => {
                        assert_eq!(request.path, "/.well-known/oauth-authorization-server");
                        respond(
                            &mut stream,
                            "200 OK",
                            &[],
                            &stable_json_dumps(&json!({
                                "issuer": server_base,
                                "authorization_endpoint": format!("{server_base}/authorize"),
                                "token_endpoint": format!("{server_base}/token"),
                                "registration_endpoint": format!("{server_base}/register"),
                                "revocation_endpoint": format!("{server_base}/revoke"),
                                "scopes_supported": ["mcp:tools"],
                            })),
                        );
                    }
                    3 => {
                        assert_eq!(request.path, "/register");
                        assert!(request.body.contains("token_endpoint_auth_method"));
                        respond(
                            &mut stream,
                            "201 Created",
                            &[],
                            r#"{"client_id":"desktop-oauth-client"}"#,
                        );
                    }
                    4 => {
                        assert_eq!(request.path, "/token");
                        assert!(request.body.contains("grant_type=authorization_code"));
                        assert!(request.body.contains("code_verifier="));
                        respond(
                            &mut stream,
                            "200 OK",
                            &[],
                            r#"{"access_token":"oauth-access-alpha","refresh_token":"oauth-refresh-alpha","token_type":"Bearer","expires_in":3600}"#,
                        );
                    }
                    5 => {
                        assert_eq!(request.path, "/mcp");
                        assert_eq!(
                            request.headers.get("authorization").map(String::as_str),
                            Some("Bearer oauth-access-alpha")
                        );
                        let rpc = serde_json::from_str::<Value>(&request.body).expect("MCP RPC");
                        assert_eq!(rpc["method"], "tools/list");
                        respond(
                            &mut stream,
                            "200 OK",
                            &[],
                            &stable_json_dumps(&json!({
                                "jsonrpc": "2.0",
                                "id": rpc["id"],
                                "result": {"tools": [{
                                    "name": "oauth_lookup",
                                    "description": "OAuth protected lookup",
                                    "inputSchema": {"type": "object"},
                                }]},
                            })),
                        );
                    }
                    6 => {
                        assert_eq!(request.path, "/token");
                        assert!(request.body.contains("grant_type=refresh_token"));
                        assert!(request.body.contains("refresh_token=oauth-refresh-alpha"));
                        respond(
                            &mut stream,
                            "200 OK",
                            &[],
                            r#"{"access_token":"oauth-access-beta","token_type":"Bearer","expires_in":7200}"#,
                        );
                    }
                    7 => {
                        assert_eq!(request.path, "/revoke");
                        assert!(request.body.contains("token=oauth-refresh-alpha"));
                        respond(&mut stream, "200 OK", &[], "{}");
                    }
                    _ => unreachable!(),
                }
            }
        });

        let root = std::env::temp_dir().join(format!("openagent-mcp-oauth-{}", now_ms()));
        let workspace = root.join("workspace");
        let session_store_root = root.join("sessions");
        fs::create_dir_all(&workspace).expect("OAuth workspace");
        fs::create_dir_all(&session_store_root).expect("OAuth sessions");
        let config = HttpRuntimeConfig {
            host: "127.0.0.1".to_string(),
            port: 18787,
            workspace: Some(workspace.to_string_lossy().to_string()),
            session_store_root: Some(session_store_root.to_string_lossy().to_string()),
            auth_token: Some("bridge-oauth-test-token".to_string()),
            mcp_config: Some(stable_json_dumps(&json!({
                "mcp": {"servers": {"oauth-tools": {
                    "url": format!("{base}/mcp"),
                    "transport": "http",
                    "timeout_ms": 3000,
                    "enabled": true,
                }}}
            }))),
            ..HttpRuntimeConfig::default()
        };

        let disconnected = mcp_payload(&config, "/api/mcp");
        assert_eq!(
            disconnected["servers"][0]["oauth"]["status"],
            "disconnected"
        );
        let login = mcp_oauth_login_payload(&config, "oauth-tools", "{}").expect("OAuth login");
        assert_eq!(login["servers"][0]["oauth"]["status"], "authorizing");
        let authorization_url = login["oauth_login"]["authorization_url"]
            .as_str()
            .expect("authorization URL");
        assert!(authorization_url.contains("code_challenge_method=S256"));
        let state = Url::parse(authorization_url)
            .expect("authorization URL parse")
            .query_pairs()
            .find_map(|(key, value)| (key == "state").then(|| value.into_owned()))
            .expect("OAuth state");
        let login_json = stable_json_dumps(&login);
        assert!(!login_json.contains("oauth-access"));
        assert!(!login_json.contains("oauth-refresh"));
        assert!(!login_json.contains("code_verifier"));

        let callback = route_http_request(
            &HttpRequest {
                method: "GET".to_string(),
                path: format!("/api/mcp/oauth/callback?code=oauth-code&state={state}"),
                headers: BTreeMap::new(),
                body: String::new(),
            },
            &config,
        );
        assert_eq!(callback.status, 200);
        let callback_html = callback.body_text.unwrap_or_default();
        assert!(callback_html.contains("MCP connected"));
        assert!(!callback_html.contains("oauth-access-alpha"));

        let connected = mcp_payload(&config, "/api/mcp");
        assert_eq!(connected["servers"][0]["oauth"]["status"], "connected");
        assert_eq!(connected["servers"][0]["oauth"]["refreshable"], true);
        let connected_json = stable_json_dumps(&connected);
        assert!(!connected_json.contains("oauth-access-alpha"));
        assert!(!connected_json.contains("oauth-refresh-alpha"));

        let tested = mcp_test_server_payload(&config, "oauth-tools").expect("OAuth MCP test");
        assert_eq!(tested["servers"][0]["tool_count"], 1);
        assert_eq!(
            tested["servers"][0]["tools"][0]["original_name"],
            "oauth_lookup"
        );

        let restarted_config = config.clone();
        let after_restart = mcp_payload(&restarted_config, "/api/mcp");
        assert_eq!(after_restart["servers"][0]["oauth"]["status"], "connected");
        let credential_path = fs::read_dir(mcp_oauth_directory(&config))
            .expect("OAuth private directory")
            .next()
            .expect("OAuth private state")
            .expect("OAuth private entry")
            .path();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&credential_path)
                    .expect("OAuth state metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }

        let refreshed = mcp_oauth_refresh_payload(&config, "oauth-tools").expect("OAuth refresh");
        assert_eq!(refreshed["servers"][0]["oauth"]["status"], "connected");
        assert!(!stable_json_dumps(&refreshed).contains("oauth-access-beta"));
        let revoked = mcp_oauth_revoke_payload(&config, "oauth-tools").expect("OAuth revoke");
        assert_eq!(revoked["servers"][0]["oauth"]["status"], "disconnected");
        assert!(!credential_path.exists());
        assert!(
            !config
                .mcp_config
                .as_deref()
                .unwrap_or_default()
                .contains("oauth-access")
        );

        server.join().expect("mock OAuth join");
        let _ = fs::remove_dir_all(root);
    }
}
