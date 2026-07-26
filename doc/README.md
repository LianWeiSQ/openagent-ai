# OpenHarness Documentation

Project documentation is intentionally small and describes the current Rust
implementation. Completed migration plans, session receipts, and duplicated
comparison reports do not live here.

| Document | Purpose |
| --- | --- |
| [Architecture](architecture.md) | Runtime boundaries and data flow |
| [Bridge API](bridge-api.md) | HTTP/SSE contract consumed by Desktop, CLI, and TUI |
| [Context](context.md) | Context assembly, compaction, and persistence |
| [Session Engine](session-engine.md) | Durable execution states, recovery, idempotency, effects, and catalog |
| [Operations](operations.md) | Startup, local state, security, diagnostics, and verification |
| [TUI](tui.md) | Terminal UI usage and responsibility boundary |
| [Swarm](swarm.md) | Rust swarm runner contract and CLI |
| [Roadmap](roadmap.md) | Current gaps and ordered next work |

Desktop product status and planning live in
[`../../app/docs/desktop-agentic-workspace-plan.md`](../../app/docs/desktop-agentic-workspace-plan.md).

When behavior changes, update one of these documents instead of adding a new
phase-specific progress file.
