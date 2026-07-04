# OpenHarness Runtime Parity Architecture

This folder records the harness-level demand evolution behind the Rust
runtime, CLI, HTTP runtime, App Bridge, TUI/Desktop surfaces, MCP, skills, and
subagent/task execution.

It is intentionally broader than one implementation session. The purpose is to
explain why the runtime moved in this direction, what OpenCode and Claude Code
patterns were used as reference points, how the current OpenHarness design is
layered, and what remains to be built.

## Scope

This is an architecture record for maintainers. It is not a sprint receipt,
not a marketing overview, and not a direct translation of OpenCode or Claude
Code feature lists. Each part describes:

- the harness-level requirement that forced the change;
- the reference architecture idea used from OpenCode or Claude Code;
- the OpenHarness design adopted for local runtime constraints;
- the development sequence that made the design safe to land;
- the verification evidence and the next engineering boundary.

## Reading Order

| Part | Document | Scope |
| --- | --- | --- |
| 01 | [Demand Evolution](part-01-demand-evolution.md) | Overall product and runtime demand evolution |
| 02 | [Runtime Architecture](part-02-runtime-architecture.md) | Current layered architecture and ownership model |
| 03 | [Profile Schema](part-03-profile-schema.md) | Shared AgentProfile, SkillConfig, and TaskConfig schema |
| 04 | [Skill System](part-04-skill-system.md) | Skill registry, loading, permissions, events, and compaction |
| 05 | [Subagent And Task Runtime](part-05-subagent-task-runtime.md) | Task tool, subagent sessions, isolation, and background lifecycle |
| 06 | [Session, Context, And Events](part-06-session-context-events.md) | Session ledger, context, event model, compaction, checkpointing |
| 07 | [MCP Capability Plane](part-07-mcp-capability-plane.md) | MCP config, auth, discovery, lifecycle, execution, and UI |
| 08 | [Provider And Model Runtime](part-08-provider-model-runtime.md) | Provider-aware auth, model catalog, native providers, payload boundary |
| 09 | [Product Surfaces](part-09-product-surfaces.md) | CLI, HTTP, App Bridge, TUI, and Desktop surfaces |
| 10 | [Extension And Operations](part-10-extension-operations.md) | Plugins, GitHub helpers, debug, DB, lifecycle, operations |
| 11 | [Execution Planner Roadmap](part-11-execution-planner-roadmap.md) | SessionRunner, task lifecycle, event unification, ToolBatchPlanner |

## Design References

The comparison target is not a feature checklist only. The important reference
points are architectural:

- Claude Code treats subagents and skills as first-class runtime objects with
  independent context, permissions, tool access, optional model selection, and
  resumable/nested execution.
- OpenCode treats CLI, TUI, App runtime, task tools, MCP, providers, and
  session state as one integrated runtime contract rather than isolated command
  implementations.

OpenHarness follows the same direction but keeps a local-harness bias: file
system evidence, reproducible golden tests, explicit session state, and product
surfaces that can be run locally without hidden service dependencies.
