# Operations

## Start The Bridge

```bash
cargo run -p openagent-http-runtime --bin openagent-http-runtime -- \
  --host 127.0.0.1 \
  --port 8787 \
  --workspace /path/to/workspace \
  --session-root /path/to/session-root
```

Use `GET /api/health` for health and provider summary, and
`GET /api/protocol` for the runtime route manifest.

The Desktop app normally manages this process. Manual startup is useful for
CLI/TUI development and API diagnostics.

## Provider Configuration

The runtime accepts OpenAgent or OpenAI-compatible environment names:

```dotenv
OPENAGENT_API_KEY=<local-secret>
OPENAGENT_BASE_URL=<provider-base-url>
OPENAGENT_MODEL=<model-id>
OPENAGENT_WIRE_API=responses
OPENAGENT_PROVIDER_STREAM=1
OPENAGENT_PROVIDER_RETRIES=2
OPENAGENT_PROVIDER_FALLBACK_MODELS=<comma-separated-models>
```

Equivalent `OPENAI_*` variables are supported. Keep them in ignored local env
files or the process environment; do not put real values in docs or fixtures.

Native Anthropic routing uses `ANTHROPIC_API_KEY`, `ANTHROPIC_BASE_URL`, and
`ANTHROPIC_MODEL`. Native Gemini routing uses `GOOGLE_API_KEY`,
`GOOGLE_BASE_URL`, and `GOOGLE_MODEL`. Gemini credentials are sent in the
`x-goog-api-key` header rather than a URL query parameter.

Agent profile `model_options` may include runtime provider controls:

```json
{
  "tool_call_dialect": "native",
  "tool_choice": "auto",
  "parallel_tool_calls": true
}
```

`native` resolves to the provider's structured tool-call protocol. Text
dialects such as `hermes`, `qwen_xml`, `deepseek`, and `pythonic` are opt-in
compatibility modes. These control keys are consumed by the provider plane and
are not copied into the provider payload as arbitrary model parameters.

## Authentication And Origins

Prefer `--auth-token-file <path>` or
`OPENAGENT_BRIDGE_AUTH_TOKEN_FILE=<path>`. The file should be readable only by
the current user. Avoid command-line token values because process arguments
are observable.

The default CORS policy is restricted to Tauri origins and configured local
development origins. Add exact origins with `--cors-origin`; do not use `*` for
an authenticated local runtime.

## Runtime State

The configured session root contains the append-only session ledgers, turn
jobs, queued turn payloads, events, messages, checkpoints, and trace evidence.
The ledger remains authoritative.

Runtime service state lives under `<session-root>/.openagent-runtime/`. It
contains rebuildable turn indexes, queue leases, provider configuration,
capability state, plugin metadata, OAuth records, performance samples, and
storage migration receipts. Public API summaries are constructed from
allowlisted fields and never expose stored credentials.

The rebuildable session catalog is
`<session-root>/.openagent-runtime/session_catalog.sqlite3`. The Bridge rebuilds
it from session lifecycle and transcript ledgers at startup. Use
`POST /api/session-catalog/rebuild` for an explicit repair and
`GET /api/session-catalog` to inspect counts and search history. Deleting the
catalog does not delete session data.

Treat both areas as local application data:

- do not commit it;
- do not edit it while the Bridge is running;
- retain it when testing restart recovery;
- remove it only when intentionally resetting local state.

## Failure Semantics

Queued turns run through bounded workers and filesystem leases. On restart the
runtime reclaims stale leases and reconciles queued or interrupted work from
the persisted turn records. Approval and question waits are durable waiting
states, so reconnecting clients can discover and resolve them.

Provider retries and fallback emit durable `turn/retrying` and `turn/fallback`
events. Exhausted failures become terminal failed turns with a user-facing
error and resumability metadata. A retryable failed turn can be resubmitted
with `POST /api/turns/{turn_id}/retry`.

Completed, failed, cancelled, and interrupted are terminal states. Queue
timeout is reported as `cancelled` with `terminal_reason=queue_timeout`.
Clients should render canonical state and recovery fields directly instead of
inferring completion from a closed connection. A claimed but uncommitted tool
effect is intentionally reported as uncertain and is not retried
automatically.

## Verification

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test -p openagent-http-runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-tui
```

For the complete Desktop/core P0 gate:

```bash
npm --prefix ../app run ci:p0
```
