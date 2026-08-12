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
OPENAGENT_PROVIDER_MODELS=<comma-separated-selectable-models>
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

The Desktop Provider screen stores named connections in
`<session-root>/.openagent-runtime/provider.json` with owner-only permissions.
`PUT /api/providers/config` accepts a `config_id`; saving it preserves the
credentials and model catalog of every connection. Turns route by their model:
`gpt-*` models use `gpt`, while `kimi-k3` and `glm5.2` use `maas`; users only
need to choose a model in the Desktop picker. Existing single-provider state
is migrated to the `gpt` connection on the next save. Do not replace the
existing GPT client key or put an upstream-provider key in the Desktop app.
`GET /api/providers` returns redacted connection summaries and the active
fallback `config_id` only.

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

Storage upgrades use an exclusive cross-process lock, per-file backups, and a
durable migration manifest:

| Manifest state | Meaning | Next startup |
| --- | --- | --- |
| `prepared` / `applying` | the prior process may have stopped mid-upgrade | restore every target from the backup set before binding the Bridge port |
| `completed` | post-migration audit succeeded | keep the upgraded files and expose them as the rollback candidate |
| `failed_rolled_back` | apply/validation/startup recovery failed | keep the original files and report the stable error code |
| `rolled_back` | an operator requested rollback | keep the restored legacy files available to the old runtime |

`GET /api/storage` audits without exposing paths or content.
`POST /api/storage/migrate` upgrades known legacy schemas, and
`POST /api/storage/rollback` restores the latest completed backup. Unknown or
future session/transcript schemas block migration instead of being rewritten.
If an interrupted migration cannot be restored, Bridge startup fails closed
before serving session traffic.

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

Turns may provide `deadline_at_ms`, `max_steps`, `max_total_tokens`,
`max_cost_microunits`, and `max_tool_calls`. These values are persisted in the
task contract and enforced before a provider/tool boundary and again after
provider/tool settlement. Provider request timeout is capped by the remaining
deadline, retry backoff is cancellation-aware, MCP timeouts are capped by the
remaining deadline, and the same policy is inherited by foreground and queued
subagents. Budget exhaustion is a deterministic terminal failure with a
bounded `reason_code`; a tool-call budget is checked before side effects.

Provider fallback and partial tool failure do not add a new execution state.
They keep the durable terminal state compatible while recording the orthogonal
run outcome `degraded` with a bounded reason. Dashboards and release gates must
therefore use `outcome`, not infer health from terminal state alone.

## OpenHarness Observability

The Bridge exposes Prometheus text format at authenticated `GET /metrics` and
reports exporter state under `telemetry` in `GET /api/health`. The durable
session ledger is still authoritative; Prometheus, Grafana, Tempo, and Loki are
rebuildable operational views.

Configure the process with:

```dotenv
OPENHARNESS_PROMETHEUS_ENABLED=true
OPENHARNESS_TELEMETRY_ENABLED=true
OTEL_EXPORTER_OTLP_ENDPOINT=http://otel-collector:4318
OTEL_SERVICE_NAME=openharness-http
OPENHARNESS_SERVICE_VERSION=0.1.0
OPENHARNESS_ENVIRONMENT=production
OTEL_TRACES_SAMPLER_ARG=0.10
OTEL_EXPORTER_OTLP_TIMEOUT=3000
OTEL_BSP_SCHEDULE_DELAY=1000
OTEL_BSP_MAX_QUEUE_SIZE=2048
```

`OPENHARNESS_TELEMETRY_ENABLED=false` disables OTLP traces without disabling
the durable ledger. `OPENHARNESS_PROMETHEUS_ENABLED=false` disables `/metrics`.
Exporter initialization and export are fail-open; an unavailable collector is
reported by health and `openharness_telemetry_export_failures_total`, not as an
Agent run failure. Keep the exporter timeout and queue bounded.

Prometheus must use the same Bridge bearer credential when scraping:

```yaml
scrape_configs:
  - job_name: openharness
    metrics_path: /metrics
    authorization:
      type: Bearer
      credentials_file: /run/secrets/openharness-metrics-token
    static_configs:
      - targets: ["openharness:8787"]

rule_files:
  - /etc/prometheus/rules/openharness-alerts.yaml
```

Provision or import these checked assets:

- `observability/grafana/dashboards/openharness-runtime.json` — traffic,
  success/degradation, p95 run and stage latency, token/cost, provider/tool
  failures, queue capacity, retries/fallback, versions, and trace completeness;
- `observability/prometheus/openharness-alerts.yaml` — recording rules plus
  run-SLI, latency, queue, trace-completeness, exporter, and degradation alerts;
- `observability/grafana/provisioning/` — optional file provisioning and a
  replaceable Prometheus datasource example.
- `observability/deploy/` — pinned single-node Collector, Prometheus, Tempo,
  Loki, and Grafana acceptance topology. See
  `doc/observability-deployment.md` for secrets, startup, security boundaries,
  and production migration requirements.

The initial operational objectives are 99% successful-or-degraded terminal
runs, run p95 below 120 seconds, queue-wait p95 below 5 seconds, and at least
95% critical trace completeness. These are bootstrap thresholds, not universal
product promises. Establish a representative baseline, then tune them per
surface and workload before paging production on-call.

Use `openharness-load` for bounded Bridge/HTTP concurrency, staging soak, and
content-addressed p95/Token/cost baseline comparison. The checked CI workload
and 30-minute staging plan are documented in `doc/load-and-soak.md`.

Release stages, automatic stop conditions, sticky Run routing, config/image
rollback, and the production readiness manifest are documented in
`doc/release-rollout.md`.

Metrics deliberately exclude run, session, task, user, workspace, path,
prompt, input, output, and raw error labels. Use bounded dimensions for the
first diagnosis, then pivot to a trace. Trace and structured runtime-log events
share `trace_id` and `span_id`; the corresponding durable evidence is:

```text
<session-root>/<session_id>/runs/<run_id>/run.json
<session-root>/<session_id>/runs/<run_id>/events.jsonl
<session-root>/<session_id>/runs/<run_id>/summary.json
```

When sending structured logs to Loki, retain `trace_id` and `span_id` as JSON
fields but do not promote them to Loki labels. Configure a Grafana derived
field from the log `trace_id` to the Tempo datasource. This preserves
log-to-trace navigation without creating unbounded metric or log-label
cardinality.

Operational triage order:

1. Confirm traffic and the terminal/degraded ratio.
2. Compare queue, provider, tool, interaction, and step p95.
3. Filter bounded `reason_code`, provider, model family, tool class, and version.
4. Open one affected trace and verify its root, step, and provider-or-tool spans.
5. Use the trace/run IDs to inspect sanitized logs and the durable run ledger.
6. If the issue began with a version change, stop rollout and preserve the run
   as a bad-case candidate before rollback.

## Verification

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test -p openagent-telemetry
cargo test -p openagent-tools
cargo test -p openagent-eval
cargo test -p openagent-http-runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-tui
```

For the complete Desktop/core P0 gate:

```bash
npm --prefix ../app run ci:p0
```

Release-quality, Tool governance, bad-case promotion, failure drills, and
staged rollout are specified in [quality-governance.md](quality-governance.md)
and [fault-injection.md](fault-injection.md).
