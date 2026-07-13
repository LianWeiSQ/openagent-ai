# Context Runtime

OpenHarness assembles model input from structured runtime state instead of
building unrelated prompt strings in each product surface.

## Sources

The context pipeline can include:

- system and agent profile instructions;
- project instructions such as `AGENTS.md`, `CLAUDE.md`, and OpenAgent rules;
- durable session messages and compacted work state;
- todo state and current goal metadata when supported by the runtime;
- file-read state and explicitly attached text context;
- recent tool results, MCP descriptions, and loaded skills;
- workspace, permission, model, and execution metadata that is safe to expose.

Credentials, Bridge tokens, and provider connection secrets must never enter
provider messages or tool metadata.

## ContextPackBuilder

`ContextPackBuilder` is the common selection model. Each item carries a source,
priority, estimated size, stability, and inclusion decision. This lets traces
explain what was considered, retained, compacted, or dropped.

Budget pressure is handled in stages:

1. trim old or oversized tool output;
2. compact older conversation into structured work state;
3. reduce nonessential context detail;
4. preserve task intent, decisions, changed files, blockers, and next steps;
5. fail explicitly when a safe request still cannot fit.

## Durable State

Session data is stored below the configured session root, normally
`.openagent/sessions`. The durable model separates:

- messages: user/assistant conversation projected back to the provider;
- parts: timeline items such as text, tool calls, results, patches, usage, and
  context references;
- events: append-only runtime transitions for replay and UI synchronization;
- context assets: metadata for instruction and file snapshots;
- compacted state: a continuation packet for long sessions.

Product clients must restore this state through the Bridge API. They should
not reconstruct a session from local UI state.

## Invariants

- One runtime path owns provider-message assembly.
- New context sources are explicit typed items, not hidden string appendices.
- Compaction is resumable and records what information was removed.
- Persisted messages and events remain ordered and scoped to one session.
- Attachments are persisted as metadata plus safe content references.
- Secret values are redacted before persistence and diagnostics.
