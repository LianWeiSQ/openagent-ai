# OpenAgent TUI

The TUI is a terminal client for the Bridge API. It does not call providers or
tools directly; sessions, turns, events, approvals, and questions remain owned
by the shared Rust runtime.

## Start

Start a Bridge:

```bash
cargo run -p openagent-cli --bin openagent -- \
  serve --host 127.0.0.1 --port 8787
```

Attach the TUI:

```bash
cargo run -p openagent-tui --bin openagent-tui -- \
  --attach http://127.0.0.1:8787 \
  --workspace /path/to/workspace
```

Useful options:

- `--session <id>` resumes a specific session;
- `--continue` resumes the latest matching session;
- `--fork` forks before continuing;
- `--server-token <token>` or Basic auth credentials authenticate a remote
  Bridge;
- `--permission <ruleset>` selects a permission ruleset;
- `--dangerously-skip-permissions` enables unrestricted execution.

Prefer an environment variable or protected token file around the Bridge
process instead of placing secrets in reusable shell history.

## Responsibilities

The TUI renders:

- session selection and transcript history;
- streamed assistant text;
- tool calls, results, patches, and task state;
- approval and question queues;
- model, agent, file, variant, and thinking pickers;
- interrupt and remote control actions.

The Bridge remains the source of truth. TUI state is a projection and must be
safe to discard and rebuild from session/event APIs.

## Verify

```bash
cargo test -p openagent-tui
cargo test -p openagent-http-runtime --test http_runtime
```
