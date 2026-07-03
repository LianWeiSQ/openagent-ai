use std::{collections::BTreeMap, error::Error, fs, path::PathBuf};

use openagent_mcp::{
    McpConfig, McpServerType, McpTransport, RemoteMcpManager, RemoteMcpToolCallResult,
    bridge_tool_output, build_tool_descriptors_from_values, dynamic_tool_name, load_mcp_config,
    load_mcp_config_from_sources, load_mcp_config_from_value, mcp_tool_definition,
    normalize_tool_call_result, sanitize_mcp_observation_value, sanitize_mcp_trace_value,
    timeout_seconds, tool_allowed, transport_candidates, unavailable_tool_result,
};
use serde::Serialize;
use serde_json::{Value, json};

#[test]
fn mcp_runtime_fixture_matches_legacy_oracle() -> Result<(), Box<dyn Error>> {
    let fixture = read_fixture()?;
    let raw_config = raw_config();
    let parsed = load_mcp_config_from_value(&raw_config)?;
    assert_eq!(to_value(&parsed)?, fixture["config"]["parsed"]);
    assert_eq!(json!(parsed.enabled()), fixture["config"]["enabled"]);

    let env_config = env_config();
    let mut env = BTreeMap::new();
    env.insert(
        "OPENAGENT_MCP_CONFIG".to_string(),
        serde_json::to_string(&env_config)?,
    );
    let raw_config_string = serde_json::to_string(&raw_config)?;
    let source_cli = load_mcp_config_from_sources(Some(&raw_config_string), &env)?
        .ok_or("CLI MCP config should be loaded")?;
    assert_eq!(to_value(source_cli)?, fixture["config"]["source_cli"]);
    let source_env =
        load_mcp_config_from_sources(None, &env)?.ok_or("env MCP config should be loaded")?;
    assert_eq!(to_value(source_env)?, fixture["config"]["source_env"]);
    assert_eq!(
        to_value(load_mcp_config_from_sources(None, &BTreeMap::new())?)?,
        fixture["config"]["source_empty"]
    );
    assert_eq!(json!(config_errors()), fixture["config"]["errors"]);

    let primary = parsed
        .servers
        .first()
        .ok_or("fixture config should have a primary server")?;
    let descriptors = build_tool_descriptors_from_values(primary, &raw_tools());
    assert_eq!(to_value(&descriptors)?, fixture["discovery"]["descriptors"]);

    let mut manager = RemoteMcpManager::new(McpConfig {
        servers: vec![primary.clone()],
        refresh_ttl_s: parsed.refresh_ttl_s,
    });
    manager.set_server_tools(
        &primary.name,
        Some(McpTransport::Http),
        "ready",
        Some(1_781_840_000.25),
        descriptors.clone(),
    )?;
    assert_eq!(
        to_value(manager.list_tool_descriptors())?,
        fixture["discovery"]["listed"]
    );
    assert_eq!(
        to_value(manager.snapshot())?,
        fixture["discovery"]["snapshot"]
    );
    assert_eq!(
        json!({
            "dynamic_name": dynamic_tool_name("Demo Server", "Weather.Search"),
            "transport_auto": transport_candidates(McpTransport::Auto),
            "transport_http": transport_candidates(McpTransport::Http),
            "transport_sse": transport_candidates(McpTransport::Sse),
            "timeout_floor": timeout_seconds(500),
            "timeout_regular": timeout_seconds(45_000),
            "tool_allowed_weather": tool_allowed("Weather.Search", &primary.tools),
            "tool_allowed_denied": tool_allowed("Data-secret", &primary.tools),
        }),
        fixture["discovery"]["helpers"]
    );

    let descriptor = descriptors
        .first()
        .ok_or("fixture descriptor should exist")?;
    assert_eq!(
        to_value(normalize_tool_call_result(
            descriptor,
            Some(McpTransport::Http),
            &text_result()
        ))?,
        fixture["tool_call"]["text_non_text"]
    );
    assert_eq!(
        to_value(normalize_tool_call_result(
            descriptor,
            Some(McpTransport::Http),
            &empty_result()
        ))?,
        fixture["tool_call"]["empty"]
    );
    assert_eq!(
        to_value(normalize_tool_call_result(
            descriptor,
            Some(McpTransport::Sse),
            &error_result()
        ))?,
        fixture["tool_call"]["error"]
    );
    assert_eq!(
        to_value(unavailable_tool_result("mcp_tool_missing"))?,
        fixture["tool_call"]["unavailable"]
    );

    let bridge_definition = mcp_tool_definition(descriptor, "remote-mcp");
    assert_eq!(
        json!({
            "id": bridge_definition.id,
            "description": bridge_definition.description,
            "parameters_schema": bridge_definition.parameter_schema,
            "dangerous": bridge_definition.dangerous,
            "group": bridge_definition.group,
            "execution_scope": bridge_definition.execution_scope,
            "execution_schema": bridge_definition.execution_schema,
        }),
        fixture["bridge"]["definition"]
    );
    assert_eq!(
        json!([{
            "dynamic_name": descriptor.dynamic_name,
            "arguments": {"city": "Shanghai"},
        }]),
        fixture["bridge"]["calls"]
    );
    let bridge_result = RemoteMcpToolCallResult {
        output: "Bridge output".to_string(),
        error: None,
        metadata: BTreeMap::from([
            (
                "mcp_transport".to_string(),
                Value::String("sse".to_string()),
            ),
            ("custom".to_string(), Value::String("kept".to_string())),
        ]),
    };
    assert_eq!(
        to_value(bridge_tool_output(descriptor, bridge_result))?,
        fixture["bridge"]["output"]
    );

    assert_eq!(
        sanitize_mcp_trace_value(&auth_payload()),
        fixture["redaction"]["trace"]
    );
    assert_eq!(
        sanitize_mcp_observation_value(&auth_payload()),
        fixture["redaction"]["observability"]
    );

    Ok(())
}

#[test]
fn load_mcp_config_rejects_empty_or_invalid_sources() {
    assert_eq!(
        load_mcp_config("")
            .err()
            .unwrap_or_else(|| "missing error".to_string()),
        "MCP config must be a valid JSON string or a path to a JSON file."
    );
    assert_eq!(
        load_mcp_config("[]")
            .err()
            .unwrap_or_else(|| "missing error".to_string()),
        "MCP config JSON must be an object."
    );
}

#[test]
fn load_mcp_config_accepts_stdio_and_opencode_shapes() -> Result<(), Box<dyn Error>> {
    let arbor_style = json!({
        "mcpServers": {
            "arbor-review": {
                "command": "uvx",
                "args": ["--from", "arbor-agent", "arbor-mcp-server"],
                "cwd": "tools",
                "env": {"ARBOR_TOKEN": "secret"},
                "timeout": 10_000,
            },
        },
    });
    let parsed = load_mcp_config_from_value(&arbor_style)?;
    let arbor = parsed.servers.first().ok_or("missing arbor server")?;
    assert_eq!(arbor.name, "arbor-review");
    assert_eq!(arbor.server_type, McpServerType::Local);
    assert_eq!(arbor.transport, McpTransport::Stdio);
    assert_eq!(
        arbor.command,
        ["uvx", "--from", "arbor-agent", "arbor-mcp-server"]
    );
    assert_eq!(arbor.cwd.as_deref(), Some("tools"));
    assert_eq!(
        arbor.environment.get("ARBOR_TOKEN").map(String::as_str),
        Some("secret")
    );
    assert_eq!(arbor.timeout_ms, 10_000);

    let opencode_style = json!({
        "mcp": {
            "timeout": 5_000,
            "servers": {
                "local": {
                    "type": "local",
                    "command": ["node", "./mcp/server.js"],
                    "environment": {"API_KEY": "secret"},
                    "disabled": false,
                },
                "remote": {
                    "type": "remote",
                    "url": "https://mcp.example.test/mcp",
                    "disabled": true,
                },
            },
        },
    });
    let parsed = load_mcp_config_from_value(&opencode_style)?;
    let local = parsed
        .servers
        .iter()
        .find(|server| server.name == "local")
        .ok_or("missing local server")?;
    assert_eq!(local.server_type, McpServerType::Local);
    assert_eq!(local.command, ["node", "./mcp/server.js"]);
    assert_eq!(local.timeout_ms, 5_000);
    assert!(local.enabled);
    let remote = parsed
        .servers
        .iter()
        .find(|server| server.name == "remote")
        .ok_or("missing remote server")?;
    assert_eq!(remote.server_type, McpServerType::Remote);
    assert!(!remote.enabled);
    assert_eq!(remote.timeout_ms, 5_000);
    Ok(())
}

fn read_fixture() -> Result<Value, Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/golden/rust_rewrite/mcp_runtime.json");
    let raw = fs::read_to_string(path)?;
    Ok(serde_json::from_str(&raw)?)
}

fn raw_config() -> Value {
    json!({
        "refresh_ttl_s": "12.5",
        "mcp": {
            "Demo Server": {
                "type": "remote",
                "url": "https://mcp.example.test/demo",
                "transport": "auto",
                "enabled": true,
                "headers": {
                    "Authorization": "Bearer secret-token",
                    "X-Token": "token-secret",
                    "X-Client": "openagent-fixture",
                },
                "timeout_ms": 500,
                "tools": {
                    "allow": ["Weather*", "weather search", "Data-*"],
                    "deny": ["Data-secret"],
                },
            },
            "Event Server": {
                "type": "sse",
                "url": "https://mcp.example.test/sse",
                "enabled": false,
            },
            "Stream Server": {
                "type": "streamableHttp",
                "url": "https://mcp.example.test/stream",
                "headers": {"Authorization": "Bearer stream-secret"},
            },
        },
    })
}

fn env_config() -> Value {
    json!({
        "mcpServers": {
            "Env Server": {
                "type": "streamable_http",
                "url": "https://mcp.example.test/env",
                "transport": "http",
            },
        },
    })
}

fn config_errors() -> BTreeMap<String, String> {
    [
        (
            "invalid_type",
            json!({"mcp": {"bad": {"type": "stdio", "url": "https://example.test"}}}),
        ),
        (
            "invalid_transport",
            json!({"mcp": {"bad": {"url": "https://example.test", "transport": "websocket"}}}),
        ),
        (
            "invalid_headers",
            json!({"mcp": {"bad": {"url": "https://example.test", "headers": ["nope"]}}}),
        ),
        (
            "invalid_tools",
            json!({"mcp": {"bad": {"url": "https://example.test", "tools": ["nope"]}}}),
        ),
    ]
    .into_iter()
    .map(|(key, value)| {
        (
            key.to_string(),
            load_mcp_config_from_value(&value)
                .err()
                .unwrap_or_else(|| "missing error".to_string()),
        )
    })
    .collect()
}

fn raw_tools() -> Vec<Value> {
    vec![
        json!({
            "name": "Weather.Search",
            "title": "Weather Search",
            "description": "Find a forecast for a city.",
            "inputSchema": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            },
            "annotations": {"readOnlyHint": true},
            "execution": {"read_only": true},
        }),
        json!({
            "name": "weather search",
            "title": null,
            "description": "Duplicate sanitized name.",
            "inputSchema": null,
            "annotations": null,
            "execution": null,
        }),
        json!({
            "name": "Data-List",
            "title": "Data List",
            "description": "Schema starts as an array and must be wrapped.",
            "inputSchema": {"type": "array", "items": {"type": "string"}},
            "annotations": {"dangerous": false},
            "execution": {"external_io": true},
        }),
        json!({
            "name": "Data-secret",
            "title": "Denied",
            "description": "This tool is denied by filter.",
            "inputSchema": {"type": "object"},
            "annotations": {},
            "execution": null,
        }),
        json!({
            "name": "",
            "title": "Empty",
            "description": "This tool is ignored because it has no name.",
            "inputSchema": {"type": "object"},
            "annotations": {},
            "execution": null,
        }),
    ]
}

fn text_result() -> Value {
    json!({
        "content": [
            {"type": "text", "text": "Weather summary\nCloudy with light wind."},
            {"type": "image"},
            {"type": "image"},
            {"type": "resource"},
            {"type": "blob"},
            {"type": "weird"},
        ],
        "structuredContent": {"city": "Shanghai", "temperature": 24},
        "isError": false,
    })
}

fn empty_result() -> Value {
    json!({
        "content": [],
        "structuredContent": {"only": "structured"},
        "isError": false,
    })
}

fn error_result() -> Value {
    json!({
        "content": [{"type": "text", "text": "Remote MCP rejected the request."}],
        "structuredContent": {"debug": "ignored"},
        "isError": true,
    })
}

fn auth_payload() -> Value {
    json!({
        "headers": {
            "Authorization": "Bearer secret-token",
            "X-Token": "token-secret",
            "X-Client": "openagent-fixture",
        },
        "api_key": "secret-api-key",
        "nested": {
            "session_token": "secret-session-token",
            "input_tokens": 123,
            "prompt": "visible",
        },
    })
}

fn to_value<T: Serialize>(value: T) -> Result<Value, serde_json::Error> {
    serde_json::to_value(value)
}
