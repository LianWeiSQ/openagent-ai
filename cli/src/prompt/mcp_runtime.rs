use super::*;
use openagent_mcp::{discover_mcp_server_tools, mcp_json_rpc};

#[derive(Clone, Debug)]
pub(super) struct McpRuntime {
    pub(super) manager: RemoteMcpManager,
    pub(super) descriptors: BTreeMap<String, RemoteMcpToolDescriptor>,
    pub(super) snapshot: Value,
    pub(super) workspace: PathBuf,
}

pub(super) fn load_mcp_runtime(
    args: &[String],
    toolkit: &mut Toolkit,
) -> Result<Option<McpRuntime>, String> {
    let Some(source) = mcp_runtime_source(args) else {
        return Ok(None);
    };
    let config = load_mcp_config(&source)?;
    let workspace = workspace_from_args(args);
    if !config.enabled() {
        return Ok(Some(McpRuntime {
            manager: RemoteMcpManager::new(config),
            descriptors: BTreeMap::new(),
            snapshot: json!({}),
            workspace,
        }));
    }
    let mut manager = RemoteMcpManager::new(config.clone());
    let mut descriptors_by_name = BTreeMap::new();
    for server in config.servers.iter().filter(|server| server.enabled) {
        let (transport, tools) = discover_mcp_server_tools(server, &workspace)?;
        let descriptors = build_tool_descriptors_from_values(server, &tools);
        for descriptor in &descriptors {
            toolkit
                .registry
                .register(mcp_tool_definition(descriptor, "remote-mcp"));
            descriptors_by_name.insert(descriptor.dynamic_name.clone(), descriptor.clone());
        }
        manager.set_server_tools(
            &server.name,
            Some(transport),
            "connected",
            Some(now_ms_cli() as f64 / 1000.0),
            descriptors,
        )?;
    }
    let snapshot = serde_json::to_value(manager.snapshot()).unwrap_or_else(|_| json!({}));
    Ok(Some(McpRuntime {
        manager,
        descriptors: descriptors_by_name,
        snapshot,
        workspace,
    }))
}

fn mcp_runtime_source(args: &[String]) -> Option<String> {
    value_for(args, &["--mcp-config"])
        .or_else(|| env::var("OPENAGENT_MCP_CONFIG").ok())
        .or_else(|| {
            let path = mcp_config_path(args);
            path.exists().then(|| path.to_string_lossy().to_string())
        })
}

pub(super) fn execute_mcp_tool(
    mcp_runtime: Option<&McpRuntime>,
    tool_call: &ToolCall,
) -> Option<ToolResult> {
    let runtime = mcp_runtime?;
    let descriptor = runtime.descriptors.get(&tool_call.name)?;
    let Some(state) = runtime.manager.servers.get(&descriptor.server_name) else {
        let result = unavailable_tool_result(&tool_call.name);
        let bridge = bridge_tool_output(descriptor, result);
        return Some(mcp_bridge_to_tool_result(tool_call, bridge));
    };
    let transport = state.selected_transport.unwrap_or(McpTransport::Http);
    let result = match mcp_json_rpc(
        &state.config,
        transport,
        "tools/call",
        json!({
            "name": descriptor.original_name,
            "arguments": tool_call.input.clone(),
        }),
        &runtime.workspace,
    ) {
        Ok(value) => normalize_tool_call_result(descriptor, Some(transport), &value),
        Err(error) => {
            let mut result = unavailable_tool_result(&tool_call.name);
            result.error = Some(error);
            result
        }
    };
    Some(mcp_bridge_to_tool_result(
        tool_call,
        bridge_tool_output(descriptor, result),
    ))
}

fn mcp_bridge_to_tool_result(
    tool_call: &ToolCall,
    bridge: openagent_mcp::McpBridgeOutput,
) -> ToolResult {
    ToolResult {
        call_id: tool_call.call_id.clone(),
        output: bridge.output,
        error: bridge.error,
        metadata: bridge.metadata,
    }
}
