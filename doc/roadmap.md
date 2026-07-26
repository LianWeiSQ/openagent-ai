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
