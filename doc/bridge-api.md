# OpenAgent Bridge API

OpenAgent Bridge API is the local API/SSE contract used by the Desktop app,
CLI, TUI, and future IDE clients. The Rust core workspace owns this runtime
contract; product UI assets live outside the core workspace in `../../app`.

## Goal

The goal is to let a UI, CLI, desktop shell, or IDE client drive OpenAgent through stable session and turn primitives:

- create or resume a session;
- start a turn with user input;
- stream model text, tool calls, tool results, patches, runtime warnings, and completion state;
- expose trace/session identifiers for later inspection.

This is intentionally not a UI rewrite. The core runtime exposes API/SSE routes only; product clients live outside this workspace.

## Reference Shape

Codex runtime services use three core primitives:

| Codex runtime | OpenAgent mapping |
| --- | --- |
| Thread | `Session` |
| Turn | one `AgentLoop.run(...)` invocation |
| Item | `StreamEvent` projected into UI events |

OpenAgent keeps the Rust runtime as the source of truth. The bridge adapts
runtime sessions, turns, events, approvals, questions, MCP, diff, and
checkpoint APIs for clients.

## First Version Scope

Implemented endpoints:

| Endpoint | Purpose |
| --- | --- |
| `GET /api/health` | Service health |
| `GET /api/models` | Current OpenAI-compatible model catalog |
| `GET /api/sessions` | List known sessions |
| `POST /api/sessions` | Create a new session |
| `GET /api/sessions/{session_id}` | Read a session snapshot |
| `POST /api/sessions/{session_id}/turns` | Start a turn |
| `GET /api/turns/{turn_id}/events` | Stream turn events through SSE |
| `POST /api/turns/{turn_id}/interrupt` | Request cooperative turn interruption |
| `POST /api/turns/{turn_id}/approvals/{approval_id}` | Resolve a pending tool approval with `{"action":"allow"}` or `{"action":"deny"}` |

The event stream uses Codex-like method names:

- `turn/started`
- `item/step/started`
- `item/agentMessage/delta`
- `item/toolCall/started`
- `item/toolCall/completed`
- `runtime/warning`
- `item/patch/detected`
- `turn/completed`
- `turn/failed`
- `turn/interrupt_requested`
- `turn/interrupted`
- `turn/approval_requested`
- `turn/approval_resolved`

Interrupt is cooperative: the running turn is marked as interrupting immediately, and the background OpenAgent loop stops at the next model/tool event boundary. A blocking provider request or tool process may still need to return control before the final `turn/interrupted` event is emitted.

Approvals are driven by the existing permission system. When `OPENAGENT_BRIDGE_PERMISSION=PLAN_ONLY` or a custom ask rule requires confirmation, the loop pauses and emits `turn/approval_requested` with:

```json
{
  "approval": {
    "request_id": "approval_...",
    "turn_id": "turn_...",
    "tool_name": "write",
    "tool_input": {"file_path": "example.txt"},
    "call_id": "call_..."
  }
}
```

Clients resume the loop by posting `allow` or `deny` to the approval endpoint. If the turn is interrupted while waiting for approval, pending approvals are resolved as `deny` with reason `interrupt` so the background run can unwind cleanly.

## Run Locally

Build the Rust command surfaces:

```bash
cargo build -p openagent-cli -p openagent-http-runtime
```

Configure the OpenAI-compatible provider:

```bash
cargo run -p openagent-cli --bin openagent -- config init \
  --api-key "$OPENAI_API_KEY" \
  --base-url http://localhost:8080/v1 \
  --model gpt-5.5 \
  --wire-api responses

cargo run -p openagent-cli --bin openagent -- config show
```

Start the bridge API:

```bash
cargo run -p openagent-cli --bin openagent -- \
  serve --host 127.0.0.1 --port 8787 --workspace .
```

For Desktop, IDE, CLI, TUI, or other clients, run the Bridge API service:

```bash
umask 077
printf '%s' "$OPENAGENT_SERVER_TOKEN" > .openagent/bridge-token
cargo run -p openagent-http-runtime --bin openagent-http-runtime -- \
  --host 127.0.0.1 \
  --port 8787 \
  --workspace . \
  --auth-token-file .openagent/bridge-token
```

`--session-root` can pin session ledger storage for clients that need stable resume paths.
When authentication is configured, all `/api/*` JSON and SSE endpoints require
`Authorization: Bearer <token>`. Keep the token file ignored and readable only
by the current user.
The core runtime no longer serves a web UI. Use the Desktop product app from
`../../app` or a custom client against the API/SSE routes.

Send a one-shot turn to an already running Bridge API service:

```bash
openagent client --server-url http://127.0.0.1:8787 "summarize this repository"
openagent client --server-url http://127.0.0.1:8787 --continue "continue the latest server session"
openagent client --server-url http://127.0.0.1:8787 --format json "stream events as JSON"
openagent client --server-url http://127.0.0.1:8787 --server-token "$OPENAGENT_SERVER_TOKEN" "run through a secured bridge"
```

`openagent client` uses the Bridge API protocol directly:

1. `POST /api/sessions` or `GET /api/sessions` for session selection.
2. `POST /api/sessions/{session_id}/turns` to start a turn.
3. `GET /api/turns/{turn_id}/events` to consume the SSE stream.

## Runtime Defaults

The bridge reads:

| Env var | Default | Purpose |
| --- | --- | --- |
| `OPENAGENT_WORKSPACE` | current working directory | Session workspace |
| `OPENAGENT_SESSION_ROOT` | `.openagent/sessions` | File session store root |
| `OPENAGENT_BRIDGE_MAX_STEPS` | `0` | Max AgentLoop steps; `0` means OpenCode-style unbounded unless an agent/profile/request sets `steps` |
| `OPENAGENT_BRIDGE_PERMISSION` | `FULL` | Permission ruleset |
| `OPENAGENT_BRIDGE_DANGEROUSLY_SKIP_PERMISSIONS` | unset | Auto-approve permission prompts for trusted local smoke runs |
| `OPENAGENT_TRACE_ROOT` | `.openagent/traces` | Local trace root |
| `OPENAGENT_SERVER_TOKEN` | unset | Optional Bearer token for Bridge API/SSE |

## CLI Entrypoints

| Command | Purpose |
| --- | --- |
| `openagent serve` | Start the Bridge API HTTP server |
| `openagent client` | Send a turn to an already running Bridge API service |
| `openagent client --server-token ...` | Connect to a token-protected Bridge API service |

## Non-goals

First version deliberately does not implement:

- full Codex runtime protocol compatibility;
- marketplace/plugin UI;
- remote control pairing;
- complex permission approval UI;
- WebRTC/realtime voice;
- a redesigned product UI.

## Next Steps

1. Add `turn/interrupt` and cooperative cancellation.
2. Make session listing read run summaries and trace links more directly.
3. Add a thin CLI client that talks to the same bridge.
4. Connect sandbox binding state to the right-side inspector.
5. Expose context pack, tool batch, and trace panes through product clients
   outside the core runtime.

## TUI Client

The same runtime now supports a terminal UI:

```bash
openagent-tui --workspace .
```

See [`doc/tui.md`](tui.md) for the Codex TUI mapping and current capability matrix.

## Non-interactive CLI

The top-level `openagent` command can also run one prompt without opening the TUI:

```bash
openagent run "summarize this repository"
```

Useful scripting flags:

```bash
openagent run --file README.md --format json "review the attached file"
openagent run --continue "continue the last session"
openagent run --session session_abc123 "resume this session"
openagent client --server-url http://127.0.0.1:8787 --file README.md "review through the running server"
```

The same CLI also exposes local session management and usage inspection:

```bash
openagent session list
openagent session list --format json
openagent session export session_abc123 --sanitize
openagent session delete session_abc123
openagent models
openagent stats
```

These commands read the same file-backed session store used by the Bridge API runtime. By default the store is resolved from `OPENAGENT_SESSION_ROOT` or `.openagent/sessions` under the selected workspace.

## Provider Auth

`openagent auth` stores local OpenAI-compatible credentials in `~/.config/openagent/auth.json` by default. The file is written with `0600` permissions. Values from real environment variables and `.openagent/openagent.env` still take precedence; auth file values are only used when the corresponding environment variable is missing.

```bash
openagent auth login \
  --api-key "$OPENAI_API_KEY" \
  --base-url http://localhost:8080 \
  --model gpt-5.5 \
  --wire-api responses

openagent auth list
openagent auth logout
```

For tests or isolated local setups, pass `--auth-file /path/to/auth.json`.

## Custom Commands

Custom command files mirror the OpenCode command-file workflow. Place markdown files in:

- project scope: `.openagent/commands/*.md`
- global scope: `~/.config/openagent/commands/*.md`

Example:

```markdown
---
description: Review recent changes
model: gpt-5.5
---

Recent commits:
!`git log --oneline -5`

Review $ARGUMENTS and inspect @README.md.
```

Use the command from the CLI:

```bash
openagent command list
openagent command show review
openagent command render review "the current branch"
openagent run --command review "the current branch"
```

Supported template features:

- `$ARGUMENTS` for the full argument string.
- `$1`, `$2`, ... for positional arguments.
- `!` shell blocks to inject command output from the workspace.
- `@path` file references to inline file content.
