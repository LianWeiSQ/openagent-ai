# Part 01 - Demand Evolution

## Executive Summary

OpenHarness started as a tool-using coding-agent harness. The early demand was
straightforward: accept a user prompt, call a provider, execute workspace tools,
and return an answer. That shape was enough for a small CLI, but it was not
enough for a durable agent runtime.

The requirements evolved in stages:

1. Make tool execution reliable and inspectable.
2. Preserve session state across turns.
3. Add context engineering, compaction, and checkpoint recovery.
4. Add MCP and external capability integration.
5. Add App Bridge so CLI, HTTP, TUI, and Desktop can share runtime state.
6. Add subagents and Task tool semantics.
7. Add Skill as a first-class capability-routing object.
8. Consolidate duplicated profile/config parsing into shared schema.
9. Move toward a shared SessionRunner and planned tool concurrency.

The architectural direction is therefore not "more commands". The direction is
to make OpenHarness a local agent runtime with explicit state, explicit
capabilities, explicit permission boundaries, and product surfaces that all
consume the same contracts.

## Why The Requirements Changed

The first version of an agent harness can treat most things as transient:

```text
prompt -> provider -> tool call -> output
```

That model breaks down once the harness is used for real engineering work.
Engineering agents need resumability, auditability, permission gates, context
control, and multiple surfaces. They also need the ability to delegate work to
subagents and load specialized knowledge without flooding every prompt.

The demand moved from "can the agent answer" to "can the runtime explain,
resume, constrain, delegate, inspect, and recover the work".

That shift created the current architecture:

```text
User / CLI / TUI / Desktop
  -> App Bridge / HTTP runtime
  -> Session and event ledger
  -> Agent profile and runtime config
  -> Provider boundary
  -> Tool / Skill / MCP / Task execution
  -> Context and compaction
  -> Checkpoint / diff / trace / replay
```

## Reference Designs

### Claude Code

Claude Code is the stronger reference for subagent and skill semantics. The
important design ideas are:

- A subagent is not just a prompt prefix. It has its own context window, system
  prompt, tool access, permissions, optional model, and optional skills.
- Starting a subagent is exposed through an Agent tool instead of hidden router
  logic.
- The parent session receives a result or summary, not every intermediate child
  tool call.
- Skills and subagents can be combined: a skill can guide when specialized work
  should be delegated to a child context.

The OpenHarness interpretation is to make Task, Subagent, Skill, and
AgentProfile runtime objects, not string conventions.

### OpenCode

OpenCode is the stronger reference for product/runtime integration. The
important design ideas are:

- CLI, TUI, App, provider, MCP, permission, and session state are coordinated
  through common runtime contracts.
- Task is a tool-level abstraction, so the model can invoke delegation through
  the normal tool path.
- MCP is not only "call a remote tool"; it includes config, auth, lifecycle,
  discovery, diagnostics, and runtime execution.
- TUI and product surfaces are operational views over session state and event
  streams.

The OpenHarness interpretation is to route local CLI/HTTP/TUI/Desktop surfaces
through shared session and App Bridge contracts.

## Historical Development Path

### Stage 1: Tool-Using Agent Core

The first durable requirement was a stable tool execution path:

- workspace read/write/search/shell tools;
- permission rulesets;
- provider streaming normalization;
- tool result persistence;
- reproducible test fixtures.

At this stage, the architecture was still loop-centric. The main question was:
"Can one agent run one turn safely?"

### Stage 2: Session Ledger And Context Runtime

The next requirement was continuity. Real engineering work spans multiple
turns and requires evidence. That created the session ledger and context
runtime:

- append-only messages and parts;
- run/step/tool events;
- context budget and compaction;
- file context and instruction loading;
- checkpoint and restore evidence.

The runtime moved from "prompt string" to "context as state".

### Stage 3: Rust Runtime Consolidation

The Rust rewrite made the runtime boundary explicit. The repository became a
Rust-only harness workspace with crates for protocol, tools, provider, session,
MCP, HTTP runtime, TUI, and CLI.

The design benefit was ownership clarity:

- `src/protocol` owns wire-level shared types.
- `src/tools` owns tool registry and built-in tools.
- `src/session` owns persistence and replay evidence.
- `runtime/http` owns App Bridge HTTP runtime.
- `runtime/tui` owns terminal UI state.
- `cli` owns the command surface.

This made later cross-surface features possible.

### Stage 4: App Bridge And Product Surfaces

Once HTTP runtime and App Bridge existed, the harness stopped being a CLI-only
runtime. App Bridge became the contract between execution state and product
surfaces.

The demand expanded to:

- list sessions;
- start and stream turns;
- inspect events;
- resolve approvals/questions;
- inspect MCP state;
- render diffs and checkpoints;
- drive TUI/Desktop from the same state.

The system started to behave like an agent operating environment rather than a
command wrapper.

### Stage 5: MCP And External Capability Plane

MCP introduced external tools and servers. The requirement was not only "call
MCP tool". The full demand includes:

- config;
- auth and secret redaction;
- discovery;
- lifecycle control;
- diagnostics;
- execution through provider tool calls;
- UI and App Bridge visibility.

This pushed MCP into a dedicated capability plane.

### Stage 6: Subagent And Task Runtime

Complex engineering tasks need delegation. The Task tool and subagent profiles
introduced:

- built-in and project-defined agents;
- explicit subagent profile metadata;
- task routing by description;
- nested lineage and depth guards;
- workspace isolation;
- parent/child session metadata.

This moved delegation from a manual CLI option into the model-visible tool
runtime.

### Stage 7: Skill System

Skill closed the gap between "knowledge file" and "runtime capability". The
recent work made skill:

- discoverable;
- loadable by name;
- permission-gated;
- profile-bound;
- available in CLI/HTTP diagnostics;
- observable through session events;
- protected across compaction;
- optionally connected to Task/subagent fork execution.

This is the first point where knowledge, routing, permission, and delegation
are all connected in one path.

### Stage 8: Shared Schema

As AgentProfile, SkillConfig, and TaskConfig grew, duplication between CLI and
HTTP became a risk. The shared schema stage addressed that by moving common
profile parsing into `openagent-tools`.

The requirement is architectural hygiene:

- one interpretation of skill roots;
- one interpretation of task permissions;
- one model-options filtering rule;
- no accidental provider payload leakage;
- easier future SessionRunner extraction.

## Current North Star

The next architectural center is a shared SessionRunner:

```text
SessionRunner
  -> resolve profile
  -> bind system prompt and skills
  -> assemble provider messages
  -> execute provider step
  -> execute tools/tasks/skills/MCP
  -> append session events and messages
  -> return completed / paused / failed / cancelled
```

The goal is to remove CLI/HTTP loop drift and make TUI/Desktop behavior depend
on the same execution contract.
