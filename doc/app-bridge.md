# OpenAgent App Bridge

OpenAgent App Bridge is the first thin UI integration layer for OpenAgent. It follows the shape of Codex app-server without copying the whole Codex runtime or UI. The first version keeps the interface small: a local HTTP server, an SSE event stream, and a static console bundled inside the OpenAgent package.

## Goal

The goal is to let a UI, CLI, desktop shell, or IDE client drive OpenAgent through stable session and turn primitives:

- create or resume a session;
- start a turn with user input;
- stream model text, tool calls, tool results, patches, runtime warnings, and completion state;
- expose trace/session identifiers for later inspection.

This is intentionally not a UI rewrite. The UI is a minimal console for validating the bridge.

## Reference Shape

Codex app-server uses three core primitives:

| Codex app-server | OpenAgent mapping |
| --- | --- |
| Thread | `Session` |
| Turn | one `AgentLoop.run(...)` invocation |
| Item | `StreamEvent` projected into UI events |

OpenAgent keeps the existing Python runtime as the source of truth. The bridge only adapts the runtime for clients.

## First Version Scope

Implemented endpoints:

| Endpoint | Purpose |
| --- | --- |
| `GET /` | Static OpenAgent console |
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

Approvals are driven by the existing permission system. When `OPENAGENT_APP_PERMISSION=PLAN_ONLY` or a custom ask rule requires confirmation, the loop pauses and emits `turn/approval_requested` with:

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

Install the package in editable mode:

```bash
python -m pip install -e .
```

Configure the OpenAI-compatible provider:

```bash
openagent config init \
  --api-key "$OPENAI_API_KEY" \
  --base-url http://localhost:8080/v1 \
  --model gpt-5.5 \
  --wire-api responses

openagent config show
```

Start the app bridge:

```bash
openagent serve --host 127.0.0.1 --port 8787 --workspace .
```

Then open:

```text
http://127.0.0.1:8787
```

Equivalent module form:

```bash
PYTHONPATH=src python -m openagent.app_server.server --host 127.0.0.1 --port 8787
```

For Desktop, IDE, or other non-browser clients, run the same App Bridge as a headless API/SSE service:

```bash
export OPENAGENT_SERVER_TOKEN="replace-with-a-local-secret"
openagent serve --host 127.0.0.1 --port 8787 --workspace . --headless
```

`--session-root` can pin session ledger storage for clients that need stable resume paths.
When `OPENAGENT_SERVER_TOKEN` is set, all `/api/*` JSON and SSE endpoints require `Authorization: Bearer <token>`.
The bundled static console does not yet include a token prompt, so token-protected mode is best used with `--headless` or custom clients.

Send a one-shot turn to an already running App Bridge:

```bash
openagent client --server-url http://127.0.0.1:8787 "summarize this repository"
openagent client --server-url http://127.0.0.1:8787 --continue "continue the latest server session"
openagent client --server-url http://127.0.0.1:8787 --format json "stream events as JSON"
openagent client --server-url http://127.0.0.1:8787 --server-token "$OPENAGENT_SERVER_TOKEN" "run through a secured bridge"
```

`openagent client` uses the App Bridge protocol directly:

1. `POST /api/sessions` or `GET /api/sessions` for session selection.
2. `POST /api/sessions/{session_id}/turns` to start a turn.
3. `GET /api/turns/{turn_id}/events` to consume the SSE stream.

## Runtime Defaults

The bridge reads:

| Env var | Default | Purpose |
| --- | --- | --- |
| `OPENAGENT_WORKSPACE` | current working directory | Session workspace |
| `OPENAGENT_SESSION_ROOT` | `.openagent/sessions` | File session store root |
| `OPENAGENT_APP_AGENT_NAME` | `openagent-app` | Agent name |
| `OPENAGENT_APP_MAX_STEPS` | `0` | Max AgentLoop steps; `0` means OpenCode-style unbounded unless an agent/profile/request sets `steps` |
| `OPENAGENT_APP_PERMISSION` | `FULL` | Permission ruleset |
| `OPENAGENT_APP_TOOLS` | `all` | Tool allowlist |
| `OPENAGENT_TRACE_ROOT` | `.openagent/traces` | Local trace root |
| `OPENAGENT_SERVER_TOKEN` | unset | Optional Bearer token for App Bridge API/SSE |

## CLI Entrypoints

| Command | Purpose |
| --- | --- |
| `openagent web` | Start the bundled browser console |
| `openagent serve` | Start the App Bridge HTTP server |
| `openagent serve --headless` | Start API/SSE endpoints without the static console |
| `openagent client` | Send a turn to an already running App Bridge |
| `openagent client --server-token ...` | Connect to a token-protected App Bridge |
| `openagent-app` | Lower-level compatibility entrypoint for the same server |

## Non-goals

First version deliberately does not implement:

- full Codex app-server protocol compatibility;
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
5. Add Web UI panes for context pack, tool batch, and Langfuse trace.

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

These commands read the same file-backed session store used by the App Bridge runtime. By default the store is resolved from `OPENAGENT_SESSION_ROOT` or `.openagent/sessions` under the selected workspace.

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
