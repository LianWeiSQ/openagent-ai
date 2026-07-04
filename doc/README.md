# Documentation

OpenAgent keeps the public docs intentionally small. Start with the README, then use these pages when you need implementation-level context.

| Document | What it covers |
| --- | --- |
| [Architecture](architecture.md) | Runtime shape, core modules, tool flow, provider boundary |
| [Context Engineering](context.md) | Context budget, compaction, instructions, file context, ContextPackBuilder |
| [Operations](operations.md) | Observability, runtime logs, eval/replay, Terminal-Bench/Harbor adapters |
| [OpenCode CLI/TUI Parity Matrix](opencode-cli-tui-parity-matrix.md) | Auditable CLI/TUI gap ledger, row issues, priorities, and verification evidence |
| [Runtime Parity Architecture](runtime-parity-architecture/README.md) | Harness-level demand evolution and architecture notes for skills, subagents, MCP, sessions, product surfaces, and runner roadmap |
| [Roadmap](roadmap.md) | Current gaps and the next engineering milestones |
| [Rust Rewrite Plan](rust-rewrite-plan.md) | Goal-gated Python-to-Rust rewrite plan and terminal acceptance criteria |
| [Rust Rewrite Parity Matrix](rust-rewrite-parity-matrix.md) | Final Rust crate ownership map and golden fixture gates |
| [Rust Rewrite Progress](rust-rewrite-progress.md) | Goal receipts, verification evidence, and next migration action |
| [Step Budget Optimization](step-budget-optimization.md) | Adaptive `max_steps`, closeout protection, and runtime warnings discovered from real traces |
| [Tool Optimization](tool-optimization.md) | Tool execution schema, builtin tool classification, and batch scheduling roadmap |
| [Swarm Function Kernel](swarm.md) | Decoupled function-mode swarm kernel, YAML config, and runner boundary |

## Maintainer Rule

Keep docs short and current. Prefer updating one of the four pages above over adding a new long design note.
