# OpenHarness

OpenHarness is the Rust agent runtime behind OpenAgent. It owns provider
streaming, the multi-step agent loop, tools, permissions, sessions, MCP, the
Bridge HTTP/SSE API, CLI, TUI, swarm orchestration, and eval contracts.

The sole production Desktop product is the separate repository at `../app`. It consumes the
Bridge API and bundles the HTTP runtime as a Tauri sidecar; UI code and desktop
packaging do not belong in this core workspace. The checked-in `desktop/`
directory is a legacy prototype only; see `desktop/README.md`.

## Run

```bash
# Inspect local configuration and provider health.
cargo run -p openagent-cli --bin openagent -- doctor --format json

# Run one agent turn from the CLI.
cargo run -p openagent-cli --bin openagent -- run "summarize this repository"

# Start the Bridge API.
cargo run -p openagent-cli --bin openagent -- serve --host 127.0.0.1 --port 8787

# Attach the terminal UI to a running Bridge.
cargo run -p openagent-tui --bin openagent-tui -- --attach http://127.0.0.1:8787
```

Provider credentials must stay in environment variables or ignored local env
files. Never commit API keys, Bridge tokens, or private provider endpoints.

## Workspace

```text
src/                         Core runtime and internal Rust crates
  protocol/                  Shared protocol types
  provider/                  Provider catalog and stream normalization
  session/                   Durable session, message, part, and trace state
  tools/                     Built-in tools and tool execution
  mcp/                       MCP configuration and lifecycle
  lsp/                       Language-server integration
cli/                         `openagent` command-line surface
runtime/bridge-server/       Bridge protocol and server state
runtime/bridge-server-client/ Bridge client helpers
runtime/http/                HTTP/SSE runtime
runtime/tui/                 Terminal UI
swarm/                       Agent-runner orchestration
eval/                        Eval and benchmark contracts
skill/                       Runtime prompts, tools, and skills
```

See [MODULES.md](MODULES.md) for crate ownership and [doc/README.md](doc/README.md)
for implementation documentation.

## Verify

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The Desktop P0 acceptance gate lives in the app repository:

```bash
npm --prefix ../app run ci:p0
```

## License

UNLICENSED.
