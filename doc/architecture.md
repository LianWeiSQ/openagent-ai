# Architecture

OpenAgent is a Rust-only agent harness workspace for tool-using and coding
agents. The runtime is organized around a small set of durable concepts:
protocol types, an agent loop, context assembly, tool execution, permission
policy, session storage, trace/eval evidence, MCP integration, and product
surfaces such as CLI, TUI, Bridge API, and the HTTP runtime.

```text
User task
  -> Agent loop / turn runtime
  -> Context runtime
  -> Provider boundary
  -> Tool calls
  -> Permission policy
  -> Workspace / MCP / skill tools
  -> Session ledger / trace / parts
  -> CLI / TUI / HTTP runtime / Bridge API
```

## Workspace Modules

| Directory | Responsibility |
| --- | --- |
| `src` | Core agent loop, context, permission, policy, and skills |
| `src/protocol` | Shared serde protocol types and runtime contracts |
| `src/tools` | Tool registry, built-in tools, and workspace runtime |
| `src/provider` | Provider metadata and stream normalization |
| `src/session` | Session store, trace, observability, and replay evidence |
| `src/mcp` | MCP config, discovery, auth, and tool bridge contracts |
| `swarm` | Agent-agnostic swarm runner orchestration |
| `eval` | Eval runner, CI gate, and benchmark integrations |
| `cli` | `openagent` command-line binary |
| `skill` | Built-in prompts, tool descriptions, and skill libraries |
| `runtime/tui` | Local and remote terminal UI state |
| `runtime/bridge-server` | Bridge API server protocol and state |
| `runtime/bridge-server-client` | Bridge API client helpers |
| `runtime/http` | HTTP runtime binary and API contracts |

The HTTP runtime is split by service ownership:

| Module | Responsibility |
| --- | --- |
| `bridge_routes` | HTTP routing, protocol discovery, JSON/SSE transport |
| `turn_runtime` | turn queue, workers, leases, and restart recovery |
| `provider_runtime` | provider catalog, configuration, validation, retry/fallback |
| `mcp_runtime` | MCP server lifecycle, OAuth, discovery, and execution |
| `capability_runtime` | browser, computer, and terminal capability policy |
| `terminal_runtime` | persistent and one-shot terminal sessions |
| `git_runtime` | Git state, workflow summaries, and approved write actions |
| `plugin_runtime` | plugin and skill discovery, installation, and updates |
| `storage_runtime` | runtime-state audit, migration, and rollback |
| `performance_runtime` | source-only workspace probes and runtime metrics |

`http_runtime.rs` remains the composition boundary. It binds these services to
the shared session, context, provider, and tool contracts; it must not become a
second implementation of those subsystems.

## Tool Flow

The model receives tool schemas through the provider boundary. If it emits a
tool call, OpenAgent:

1. validates the call against registered tool definitions;
2. evaluates the active permission ruleset;
3. executes the tool through the appropriate runtime or bridge;
4. persists the tool result into session/trace records;
5. feeds the result back into the next model step or turn.

Workspace tools are responsible for file, shell, search, and edit operations.
MCP and skill tools are bridged through their own contracts while preserving a
common tool-call shape for permission checks, trace records, and eval replay.
Provider adapters are the only layer allowed to interpret provider-specific
tool-call wire formats. The agent loop consumes normalized calls and never
scans ordinary assistant text globally for tool syntax.

## Context Runtime

OpenAgent treats context as runtime state rather than a single prompt string.
The context path tracks instruction assets, file assets, context budget,
structured compaction, context pack snapshots, and session parts. The goal is
to make context selection recoverable and debuggable: the runtime should be
able to explain which items were included, which were dropped under budget
pressure, and which assets changed before a resumed turn.

See [`context.md`](context.md) for the current context and persistence model.

## Session And Trace

The append-only file ledger is the authoritative record for messages, parts,
events, and recovery evidence. Runtime-owned state also covers turn jobs,
queues and leases, task trees, approval/question waits, checkpoints, and
context receipts. Product surfaces project that state; they do not recreate a
parallel session state machine.

This layer is the bridge between raw model messages and product/runtime
observability: product surfaces can inspect the session, while eval tooling can
replay or score the same run evidence.

See [`operations.md`](operations.md) for runtime events and operational data.

## Provider Boundary

Provider-specific SDKs and wire formats should stay outside the agent loop.
The loop consumes normalized model text, tool calls, usage, and stream events.
This keeps provider changes from leaking into tool execution, context
assembly, permission policy, and session persistence.

## Repository Boundary

This repository owns the Rust harness and Bridge contracts. The React/Tauri
Desktop product lives in `../../app`; product UI state must not become an
alternate source of truth for runtime behavior.
