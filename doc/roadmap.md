# Roadmap

OpenHarness is a usable Rust runtime. The current priority is to deepen shared
runtime contracts that improve Desktop, CLI, and TUI together.

## Stable Foundation

- multi-step provider/tool loop with streaming;
- built-in workspace tools and permission policy;
- durable sessions, messages, parts, events, checkpoints, and turn jobs;
- approval/question pause and resume;
- provider retry, fallback, failure, and manual retry;
- local and remote MCP configuration, lifecycle, discovery, and execution;
- Bridge HTTP/SSE API used by Desktop and TUI;
- CLI, TUI, LSP, swarm, and eval crates;
- restricted Bridge authentication, CORS defaults, and Desktop CSP;
- local P0 acceptance gate covering core, Desktop, and browser smokes.

## Completed V1 Baseline

- one `ContextPackBuilder` path for provider-facing context with typed
  attachments, receipts, replay, and compaction evidence;
- durable goal and plan contracts shared by Bridge and product clients;
- task trees, foreground/background subagents, lifecycle actions, and isolated
  child workspaces;
- provider catalog, validation, retry, fallback, recovery, and private
  configuration storage;
- MCP local/remote lifecycle and OAuth;
- plugins, skills, capability policy, persistent terminals, Git workflow,
  storage migration, and source-only performance probes;
- bounded HTTP service modules behind a single composition boundary.

## Completed Provider And Tool-Calling Plane

- first-class tool-call frames, strict fragmented-call assembly, and
  interleaved parallel-call isolation;
- native structured OpenAI Chat, OpenAI Responses, Anthropic, and Gemini
  adapters;
- explicit Hermes, Qwen XML, DeepSeek, and Pythonic compatibility dialects;
- negotiated tool choice, strict schema, output schema, and parallel-call
  capabilities;
- native Gemini request, response, streaming, authentication, and tool-result
  mapping;
- fail-closed malformed/truncated call handling and cross-surface fake-provider
  tests.

## Completed Durable Session And Turn Engine

- one canonical seven-state model for session, turn, task, approval, and
  question execution;
- phase-aware recovery decisions for provider, tool, approval, question,
  compaction, and subagent crashes;
- owner leases, heartbeats, attempts, turn idempotency keys, and at-most-once
  tool effect receipts;
- append-only lifecycle ledgers with cross-process append/read locking;
- rebuildable SQLite session history, execution tree, lease, and FTS catalog;
- sync/async turn parity, runtime-root queue isolation, restart reconciliation,
  and catalog service APIs.

## Active Harness Optimization Goal

Turn the existing durable runtime into a measurable PDCA loop without replacing
the append-only session ledger or the seven-state execution model. The active
work adds standard telemetry, versioned quality evidence, operational controls,
and bad-case feedback around the existing runtime.

The current baseline already provides local trace records, sanitized runtime
logs, usage and cost values, eval fixtures, retry and fallback, leases,
idempotency, cancellation, and restart recovery. The primary gap is a shared
telemetry contract and production export path, followed by version-aware quality
gates and an operational feedback loop.

### Definition Of Done

- A sampled run is inspectable as one distributed trace from its inbound Bridge,
  CLI, TUI, or swarm request through queueing, context assembly, provider calls,
  tools, MCP, approvals, subagents, persistence, and finalization.
- At least 95% of sampled runs contain a root span, a terminal outcome, and all
  applicable critical child spans. Error and degraded runs are retained at
  100%; successful-run sampling is configurable.
- Trace, metric, and log records share canonical trace and span identifiers.
  New traces use W3C-compatible identifiers and propagate `traceparent` and
  `tracestate` across HTTP, MCP, queued work, and subagent boundaries.
- Grafana shows traffic, terminal and degraded outcomes, SLO attainment, stage
  latency, tokens, cost, provider/tool failures, retries/fallback, and trace
  completeness. A trace can be used to find its correlated logs and durable run
  evidence.
- Metrics contain no run, session, task, user, path, prompt, or raw tool names as
  unbounded labels. Prompts, tool inputs/outputs, credentials, and workspace
  content are excluded from exported telemetry by default.
- Telemetry export is asynchronous and fail-open: a collector or Grafana outage
  cannot fail, delay, or change a user run beyond the agreed overhead budget.
- Golden cases, replay, fault injection, and version comparison prevent a
  release when quality, latency, token, cost, recovery, privacy, or trace
  completeness gates regress.
- Every production bad case can be classified, linked to durable evidence,
  replayed locally, converted into a regression fixture, and tracked to a
  verified release.

### Delivery Plan

| Milestone | Scope | Deliverables and exit gate | Estimate |
| --- | --- | --- | --- |
| M0: Baseline and contracts | Inventory the CLI, Bridge, HTTP/SSE, provider, tool, MCP, subagent, and persistence paths. Record current tests, latency, token/cost, failure modes, and trace coverage. Define SLIs before choosing SLO targets. | Critical-flow map, risk register, baseline report, metric cardinality policy, and an accepted v1 run/outcome contract. Existing format, trace, and HTTP runtime gates are reproducible. | 2-3 engineer-days |
| M1: Telemetry foundation | Add a dependency-light `openagent-telemetry` workspace crate. Define resource attributes, canonical IDs, redaction, sampling, exporter health, and version attributes for harness, agent, prompt, skill set, tool set, model, and configuration fingerprint. | OTel initialization is owned by composition roots; disabled telemetry is near-zero cost; old durable records remain readable; exporter-outage tests pass. | 3-4 engineer-days |
| M2: Trace and context propagation | Instrument stable boundaries: `agent.run`, `turn.queue`, `agent.step`, `context.build`, `gen_ai.request`, `tool.execute`, `mcp.call`, `approval.wait`, `subagent.run`, `session.persist`, and `run.finalize`. Extract/inject W3C context on inbound/outbound requests and carry context through queues and subagents. | One integration fixture proves a single parent/child trace across the full critical path. Local `TraceEvent` remains durable evidence and records the OTel IDs needed for correlation. | 5-6 engineer-days |
| M3: Metrics and Grafana | Export Prometheus-compatible metrics for run count and duration, queue wait, stage duration, terminal/degraded outcomes, provider requests, retries/fallback, token/cost, tool/MCP calls, active workers, exporter failures, and trace completeness. Provision the Grafana dashboard and SLO alerts. | Cardinality tests reject forbidden labels. Dashboard top row answers usage, success/SLO, latency, token, and cost; diagnostic rows locate the failing provider, stage, tool class, or version. | 3-4 engineer-days |
| M4: Runtime gap closure | Audit rather than rebuild existing retry, fallback, lease, idempotency, cancellation, and recovery behavior. Close only verified gaps in deadline propagation, retry classification, loop/token/tool budgets, backpressure, degraded outcomes, MCP/subagent cancellation, and telemetry backpressure. | Provider, tool, MCP, queue, process-crash, and collector fault-injection tests have deterministic terminal outcomes and do not duplicate committed tool effects. | 4-5 engineer-days |
| M5: Quality gates | Extend the eval harness with versioned golden cases, durable-run replay, rule assertions, model scoring where needed, human-review hooks, and before/after comparison by configuration fingerprint. Separate product quality from operational success. | CI blocks critical-case regressions and enforces agreed pass-rate, latency, token, cost, privacy, and trace-completeness budgets. Each score links to its run and trace. | 4-6 engineer-days |
| M6: Governance and bad-case loop | Add outcome and degradation taxonomy without adding execution states. Enforce minimum tool permission, approval evidence, redaction tests, budget/loop circuit breakers, and a bad-case intake record containing classification, evidence IDs, owner, fixture, fix version, and verification state. | A failed or degraded production run can be promoted to a sanitized fixture and replayed through the same eval gate. Security review confirms that telemetry contains no default content export. | 3-4 engineer-days |
| M7: Rollout and operations | Run load and soak tests, validate sampling and overhead, publish Grafana and alert rules, document configuration and incident playbooks, and roll out behind feature flags with staged percentages and rollback switches. | Full workspace gates and Desktop P0 pass; on-call can diagnose a synthetic failure from dashboard to trace to durable evidence; rollback is config-only. | 2-3 engineer-days |

The expected critical path is about 26-35 engineer-days for one engineer. Two
engineers can overlap M2/M3 with M4/M5 after M1, but the contracts and redaction
policy must remain single-owner to avoid incompatible telemetry schemas.

### Stable Telemetry Contract

Run state and run outcome are separate. The current seven execution states stay
unchanged. A terminal run adds an outcome of `success`, `degraded`, `failed`,
`cancelled`, or `interrupted`, plus bounded reason codes such as provider
fallback, context truncation, partial tool failure, budget exhaustion, or user
cancel.

Required bounded dimensions include surface, phase, status, outcome, reason
code, agent name and version, harness version, provider, model family, tool
class, retry attempt, and deployment environment. High-cardinality evidence
stays on traces or durable run records, not metric labels.

Initial metric families are:

- `openharness_runs_total` and `openharness_run_duration_seconds`;
- `openharness_stage_duration_seconds` and `openharness_queue_wait_seconds`;
- `openharness_provider_requests_total` and
  `openharness_provider_duration_seconds`;
- `openharness_llm_tokens_total` and `openharness_llm_cost_total`;
- `openharness_tool_calls_total`, `openharness_tool_duration_seconds`, and
  `openharness_mcp_calls_total`;
- `openharness_retries_total`, `openharness_fallbacks_total`, and
  `openharness_degraded_runs_total`;
- `openharness_active_workers`, `openharness_queue_depth`,
  `openharness_telemetry_export_failures_total`, and
  `openharness_trace_completeness_ratio`.

### Implementation Guardrails

- Keep the append-only ledger authoritative; OTel backends are operational
  projections, not recovery state.
- Initialize exporters at process composition roots and keep instrumentation
  provider-neutral. Business modules must not depend on Grafana, Tempo,
  Langfuse, or another vendor API.
- Use one semantic adapter for local trace events and OTel attributes; do not
  hand-build a second event vocabulary in each runtime module.
- Prefer hashes and immutable version identifiers over mutable names when
  comparing prompts, agents, skills, tools, and configuration.
- Do not add a `degraded` execution state. Record degradation as an orthogonal
  outcome so state recovery remains compatible.
- Do not place exporter retries on the user request path. Bound queues, count
  drops, and degrade telemetry rather than the agent run.
- Land every milestone behind configuration flags with tests and a documented
  rollback path.

## Enhancement Sequence

1. Context operating system: deterministic assembly, typed assets, semantic
   compaction, and cross-client prompt parity.
2. Concurrent tool scheduler: dependency-aware read parallelism, write
   serialization, cancellation, and resource limits.
3. Subagent control plane: durable background lifecycle, nested routing,
   worktree isolation, resume, promotion, and task-tree UX.
4. MCP service plane: dynamic client registration, hardened remote OAuth,
   reconnect policy, server logs, and capability negotiation.
5. LSP service plane: managed clients, diagnostics, lifecycle control, logs,
   and server capability discovery.
6. Product hardening: packaged-platform gates, accessibility, observability,
   migration compatibility, and release automation.

## Documentation Rule

Update this roadmap when priorities change. Do not create phase receipts or
one-off parity documents; Git history and tests are the implementation record.
