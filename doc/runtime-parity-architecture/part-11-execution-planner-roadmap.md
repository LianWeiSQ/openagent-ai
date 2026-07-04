# Part 11 - Execution Planner Roadmap

## Problem Statement

The runtime has grown enough that two structural issues are now visible:

1. CLI and HTTP still have separate execution loops.
2. Tool execution is still mostly serial even when calls are independent.

These are not cosmetic issues. They affect behavior consistency, task
lifecycle, event shape, and performance.

The roadmap has two major components:

- SessionRunner facade;
- ToolBatchPlanner integration.

## SessionRunner Facade

### Goal

SessionRunner should become the shared execution interface for CLI and HTTP.

Target shape:

```text
SessionRunner
  -> resolve profile
  -> prepare ToolContext
  -> bind system prompt and preloaded skills
  -> assemble provider messages
  -> execute provider step
  -> execute tools/tasks/skills/MCP
  -> append messages/events/parts
  -> finish completed / failed / paused / cancelled
```

### Why It Is Needed

Current duplication causes drift:

- CLI and HTTP each bind profile state.
- CLI and HTTP each execute tool results into session state.
- CLI and HTTP each record skill events.
- Task/subagent paths have separate finish/error behavior.
- Approval/question resume logic has surface-specific branches.

The shared schema stage removed one source of drift. SessionRunner should
remove the next one.

### Recommended Extraction Sequence

Do not move the entire loop at once. The safer sequence is:

1. Extract shared tool-result append logic.
2. Extract shared skill event recording.
3. Extract shared step/run finish result model.
4. Extract shared profile/system-prompt binding.
5. Extract shared provider-message assembly.
6. Wrap CLI/HTTP loops behind SessionRunner.

The first slice should be small and verifiable.

## Task Background Lifecycle

Once SessionRunner exists, Task background lifecycle should become a stable
state machine:

```text
queued
  -> running
  -> completed
  -> failed
  -> cancelled
```

Required operations:

- wait;
- promote;
- cancel;
- resume;
- inspect;
- list task tree.

The state transitions should be written as session events so TUI/Desktop can
render them without custom state reconstruction.

## Unified Event Model

The event model should converge across:

- permissions;
- approvals;
- questions;
- tool calls;
- skill events;
- MCP lifecycle;
- diff/checkpoint/restore;
- task lifecycle.

The goal is not identical payloads for every event. The goal is consistent
envelope and predictable status/attributes.

Recommended event shape:

```text
event
kind
status
run_id
step
attributes
created_at_ms
```

## ToolBatchPlanner

### Goal

ToolBatchPlanner should improve runtime efficiency without breaking
permission, trace, or file safety.

### Staged Rollout

| Stage | Behavior |
| --- | --- |
| Trace-only | planner emits what could run in parallel, execution remains serial |
| Read-only concurrency | read/glob/grep/ls/code_search can run concurrently |
| Keyed concurrency | tools declare resource keys; conflicting writes serialize |
| Permission-aware | approvals/questions pause the batch correctly |
| Session-aware | results are persisted in deterministic projection order |

### Design Rule

Concurrency must be controlled by the runner, not by individual tools.

```text
provider tool calls
  -> planner
  -> permission gate
  -> scheduler
  -> execution
  -> ordered session projection
```

## Development Process For The Roadmap

The right development cadence:

1. Pick one small shared runner slice.
2. Add tests that compare CLI and HTTP behavior.
3. Keep golden fixtures stable unless the contract intentionally changes.
4. Commit and push each coherent phase.
5. Only then move to broader lifecycle or planner changes.

This avoids the common failure mode where a runner refactor changes multiple
contracts at once and makes regression diagnosis difficult.

## Acceptance Criteria

SessionRunner phase is not complete until:

- CLI and HTTP both use shared runner code for at least tool-result/session
  append behavior;
- skill event recording is no longer duplicated;
- CLI and HTTP tests still pass;
- HTTP golden fixtures still pass;
- no provider payload includes runtime config fields.

Task lifecycle phase is not complete until:

- queued/running/completed/failed/cancelled are all represented;
- wait/promote/cancel/resume are tested;
- task tree APIs expose state transitions;
- TUI/Desktop can consume the same contract.

ToolBatchPlanner phase is not complete until:

- trace-only mode is observable;
- read-only true concurrency is tested;
- keyed concurrency prevents conflicting writes;
- permission and session event ordering remain deterministic.
