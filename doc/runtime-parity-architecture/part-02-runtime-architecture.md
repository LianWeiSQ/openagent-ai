# Part 02 - Runtime Architecture

## Purpose

This part describes the current OpenHarness runtime shape. It is the frame for
the later parts: Skill, MCP, Subagent, Session, Provider, and product surfaces
are not independent features. They are layers in one harness runtime.

## Architecture Layers

The current runtime can be described as six planes.

```text
Product Plane
  CLI / HTTP / App Bridge / TUI / Desktop

Routing Plane
  AgentProfile / SkillConfig / TaskConfig / Subagent descriptors

Capability Plane
  Built-in tools / MCP / Skills / Plugins / Provider catalog

Execution Plane
  Agent loop / provider loop / Task tool / future SessionRunner

State Plane
  Session store / messages / parts / events / checkpoints / compaction

Evidence Plane
  Golden fixtures / smoke tests / trace / eval / diagnostics
```

The most important design rule is that product surfaces should not invent
runtime state. They should project state that already exists in the session,
event, and App Bridge contracts.

## Product Plane

The product plane contains all user-facing surfaces:

- `cli`: compiled `openagent` command line.
- `runtime/http`: local HTTP runtime and App Bridge endpoints.
- `runtime/tui`: local and remote terminal UI state.
- `desktop`: packaged user interface on top of the App Bridge.

Earlier versions treated CLI as the primary runtime. That model no longer
holds. CLI is now one surface among several.

The design target is:

```text
CLI action       \
HTTP request      -> same session/runtime contract
TUI action       /
Desktop action  /
```

## Routing Plane

The routing plane decides what kind of agent or capability should handle a
task.

Current objects:

- AgentProfile;
- SkillConfig;
- TaskConfig;
- TaskSubagentDescriptor;
- TaskSubagentRoute;
- permission rulesets;
- per-skill and per-task permission rules.

The recent shared schema work moved common interpretation into
`openagent-tools`, so CLI and HTTP no longer independently decide what a
profile means.

This layer exists because routing is not provider logic. Provider payloads
should receive model options, messages, and tools; they should not receive
runtime-only fields such as `skill_roots`, `task_permissions`, or
`permission.skill`.

## Capability Plane

The capability plane contains things the model can call or the runtime can
load:

- workspace tools;
- MCP tools;
- skill tool;
- Task tool;
- plugin registry;
- provider/model catalog;
- debug and DB utilities.

The unifying shape is a tool call or a registry lookup. Even when the backing
implementation differs, capability execution should produce a stable result:

```text
input -> permission gate -> execution -> ToolResult -> session event
```

This is why Skill V2 and MCP execution both converge on normal tool-result
handling.

## Execution Plane

Execution is currently split:

- CLI has its own agent loop.
- HTTP runtime has its own provider loop and task runner paths.
- Task/subagent execution has nested session behavior.
- approval/question resume paths have their own branches.

This is functional but not ideal. The next architectural stage is to introduce
a shared SessionRunner facade.

The first extraction should avoid a risky full rewrite. A reasonable sequence:

1. Share tool-result-to-session append logic.
2. Share skill event recording and tool-call event projection.
3. Share system prompt/profile binding helpers.
4. Share provider-step finish/paused/error result model.
5. Move CLI and HTTP loops behind a common runner interface.

## State Plane

Session state is the runtime's durable memory:

- latest session state;
- messages;
- run records;
- parts;
- events;
- warnings;
- checkpoint metadata;
- task metadata;
- compaction boundaries.

State plane responsibilities:

- preserve enough context for resume;
- preserve enough evidence for UI and debugging;
- preserve enough structure for tests and replay;
- prevent important loaded content, such as skill output, from being compacted
  away.

The session store is therefore not just persistence. It is the contract between
execution and product surfaces.

## Evidence Plane

OpenHarness relies heavily on golden and smoke-style verification because the
runtime has many product-facing contracts.

Examples:

- CLI golden fixtures for command output;
- HTTP runtime golden fixtures for endpoint contracts;
- tool-runtime golden fixtures;
- App Bridge and TUI state tests;
- Desktop smoke tests;
- session trace tests.

This evidence style is deliberate. OpenCode parity work can easily regress
shape while preserving local behavior. Golden fixtures make shape changes
intentional.

## Architectural Invariants

The following invariants should guide future work:

1. Provider-specific details stay outside the agent loop.
2. Product surfaces consume session/App Bridge state instead of inventing it.
3. Runtime config fields do not leak into provider payloads.
4. Capability execution always passes through permission and trace boundaries.
5. Child/subagent work stays isolated from parent context except through
   result metadata and summary.
6. Loaded skill content is durable enough to survive compaction.
7. Golden tests protect public CLI/HTTP contracts.

## Development Process To Date

The architecture did not appear in one pass. It emerged through pressure:

- CLI parity exposed command-surface gaps.
- Rust rewrite forced crate ownership.
- App Bridge forced stable HTTP/session contracts.
- TUI/Desktop forced event-driven state projection.
- MCP forced capability lifecycle management.
- Subagents forced parent/child session modeling.
- Skill forced profile schema, permission, and compaction integration.

The next pressure point is duplicated loop behavior. That is why SessionRunner
is the next structural investment.
