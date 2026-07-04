# Part 06 - Session, Context, And Events

## Problem Statement

Agent work is stateful. A useful harness must preserve more than the final
answer. It must preserve:

- prompts and messages;
- tool calls and results;
- approvals and questions;
- task/subagent metadata;
- skill discovery and loading;
- context and compaction boundaries;
- checkpoint and restore data;
- runtime warnings;
- usage and trace evidence.

This state is what lets the harness resume, explain, inspect, replay, and
render work across CLI, HTTP, TUI, and Desktop.

## Design Principle

Session state is the source of truth. Product surfaces are projections.

```text
runtime action
  -> session message / part / event
  -> App Bridge event
  -> CLI/TUI/Desktop projection
```

If a UI needs a state that does not exist in the session/event layer, the
runtime contract is incomplete.

## Current Session Model

The session layer stores:

- latest session state;
- messages;
- parts;
- run records;
- events;
- metadata;
- status;
- checkpoint references;
- task metadata;
- compaction boundaries.

Tool results are stored as tool messages with structured metadata. App Bridge
and TUI can then inspect the same state that the provider loop uses.

## Context Runtime

Context is treated as runtime state, not a one-off prompt string.

Current context-related capabilities include:

- instruction loading;
- file context;
- context budgets;
- ContextPackBuilder;
- structured compaction;
- compaction boundary messages;
- skill output preservation across compaction.

The design target is recoverability:

```text
Why did the model see this?
Why was that file included?
What was dropped?
What survived compaction?
```

The answer should exist in session/context evidence.

## Events

Session events serve multiple roles:

- debugging;
- UI state reconstruction;
- app bridge streaming;
- golden contract verification;
- eval/replay support.

Important event families include:

- run events;
- step started/finished;
- tool.call started/finished/failed;
- approval/question events;
- MCP discovery/lifecycle events;
- skill.discovered / skill.loaded;
- checkpoint/restore events;
- task lifecycle events.

The current event model is functional but not fully unified. Some event
families still have surface-specific shapes.

## Skill And Compaction

Skill introduced a concrete session/context requirement: loaded skill content
must survive compaction.

Without protection, the runtime could load a skill, compact the prior tool
message, and continue without the instruction body that was supposed to guide
behavior. The session store now protects loaded skill tool output across
compaction boundaries.

This is an important precedent: compaction cannot be a generic truncation
mechanism. It must respect semantic anchors.

## Checkpoint And Diff State

Engineering work needs rollback and inspection. The session layer carries
checkpoint/restore metadata and diff/patch parts so product surfaces can
display:

- what changed;
- what checkpoint exists;
- whether restore happened;
- which files were affected;
- what event caused the state transition.

The Desktop restore history work depends on this session metadata being
durable.

## Development Process

The session model evolved in response to concrete failures:

1. Tool results needed to be inspectable after a run.
2. Multi-turn sessions needed latest state.
3. App Bridge needed streamable events.
4. TUI needed remote transcript and control state.
5. Approvals/questions needed pause/resume state.
6. Checkpoint/restore needed durable metadata.
7. Skills needed event and compaction protection.
8. Subagents needed parent/child task metadata.

Each new product surface exposed missing session semantics. The correct
pattern has been to add state to the session layer, then project it outward.

## Verification Evidence

Representative commands:

```bash
cargo test -p openagent-session --test session_trace -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo test -p openagent-cli --test cli_commands -q
```

Important coverage includes:

- session trace persistence;
- compaction boundary behavior;
- loaded skill output preservation;
- HTTP runtime event contracts;
- CLI event output;
- approval/question resume paths;
- checkpoint and restore flows.

## Remaining Work

1. Unify permission/question/approval/diff/checkpoint event shapes.
2. Make task lifecycle events first-class.
3. Make ContextPackBuilder the single model-message assembly path.
4. Improve crash recovery and session resume semantics.
5. Add stronger long-term indexes for session history.
6. Ensure SessionRunner writes one consistent event sequence across CLI/HTTP.
