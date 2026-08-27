# OpenHarness Modules

| Directory | Crate | Ownership |
| --- | --- | --- |
| `src` | `openagent-core` | Agent loop, context, policy, profiles, and skills |
| `src/protocol` | `openagent-protocol` | Shared serialized contracts |
| `src/provider` | `openagent-provider` | Provider configuration and stream events |
| `src/session` | `openagent-session` | Durable sessions, messages, parts, and trace data |
| `src/tools` | `openagent-tools` | Tool registry, permissions, and built-in tools |
| `src/mcp` | `openagent-mcp` | MCP configuration, discovery, and lifecycle |
| `src/lsp` | `openagent-lsp` | Language-server discovery and queries |
| `cli` | `openagent-cli` | `openagent` CLI |
| `runtime/bridge-server` | `openagent-bridge-server` | Bridge protocol and server state |
| `runtime/bridge-server-client` | `openagent-bridge-server-client` | Bridge client primitives |
| `runtime/http` | `openagent-http-runtime` | HTTP/SSE routes and durable turn scheduler |
| `runtime/tui` | `openagent-tui` | Terminal UI over the Bridge API |
| `swarm` | `openagent-swarm` | Agent-agnostic runner orchestration |
| `eval` | `openagent-eval` | Eval, replay, and benchmark contracts |

The React/Tauri Desktop app is intentionally outside this workspace at
`../app`. Cross-surface behavior should be implemented in a shared Rust crate
or the Bridge API, not duplicated in the app. The local `desktop/` directory is
legacy prototype material and is not a product ownership boundary.
