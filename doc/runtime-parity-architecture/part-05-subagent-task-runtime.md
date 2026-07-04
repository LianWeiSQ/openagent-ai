# Part 05 - Subagent And Task Runtime

## Problem Statement

Large engineering work cannot be handled well by one flat agent context.
Search, planning, review, implementation, and external research often need
different context, tools, permissions, and model choices.

The runtime therefore needs subagents.

The key requirement is not just "start another prompt". The requirement is:

- independent context;
- scoped tools;
- independent permission policy;
- optional model/provider;
- child session metadata;
- parent/child lineage;
- resumability;
- nested guardrails;
- foreground and background execution;
- result projection back to the parent.

## Reference Direction

Claude Code treats subagent startup as an Agent tool. That is the most
important idea: delegation is model-visible and uses the normal tool channel.

OpenCode's Task tool follows the same basic shape. The model calls a task tool
with a description, prompt, and subagent type. The harness then creates the
child execution context.

The OpenHarness design combines the two:

```text
Task tool call
  -> resolve subagent profile
  -> create child session
  -> bind prompt/tools/skills/permissions
  -> execute child run
  -> return result and metadata to parent
```

## Current Capabilities

### Built-In And Project Subagents

OpenHarness supports built-in subagent profiles and project-defined agent
profiles. Profiles can define:

- id/name/description;
- mode;
- tools;
- model/provider;
- permissions;
- task permissions;
- skills and skill roots;
- workspace isolation;
- prompt.

OpenCode markdown-style agents are supported through frontmatter parsing.

### Task Tool

Task is registered as a tool when subagent descriptors are available. The
model can call it with:

- description;
- prompt;
- subagent type;
- background flag;
- resume/task identifiers where supported.

The Task tool is therefore part of the normal tool permission and trace path.

### Auto Routing

The harness can route direct prompts to matching subagents based on subagent
description. This is not meant to replace explicit Task tool calls. It is a
convenience path for obvious delegation requests.

### Nested Guardrails

The runtime tracks:

- task depth;
- task lineage;
- parent session id;
- root session id;
- subagent type.

This prevents self-calls, obvious recursion, and unbounded nesting.

### Workspace Isolation

Subagents can run in isolated workspaces. This is important for implementation
tasks because it allows independent work and review without immediately
mutating the parent workspace state.

### Skill Preloading

Subagent profiles can preload skills. The loaded skill content is injected
into the child system context, not the parent. This keeps parent context small
and preserves the child agent's specialized operating environment.

### Fork Skill To Task

Certain skills can request forked execution through a specific agent. This
joins the skill system and Task runtime:

```text
Skill metadata says: run this in agent X
Task runtime says: create isolated child session for agent X
```

## Current Limitation

The main missing piece is complete background lifecycle parity.

The desired lifecycle is:

```text
queued -> running -> completed
                 -> failed
                 -> cancelled
```

Current HTTP runtime has queue foundations and task tree APIs. CLI foreground
paths are more complete than CLI background paths. Wait/promote/cancel/resume
need to become stable runtime contracts, not surface-specific behavior.

## Architecture Direction

Subagent execution should eventually be owned by SessionRunner:

```text
SessionRunner::run_task
  -> validate parent
  -> resolve profile
  -> prepare child session
  -> bind system prompt and skills
  -> claim task run lock
  -> execute provider/tool loop
  -> update task status
  -> summarize result
```

The status update should be event-backed so TUI/Desktop can render task trees
without custom polling logic.

## Development Process

The current subagent/task path developed in stages:

1. Add reusable agent profile loading.
2. Add built-in subagents.
3. Register Task tool with available subagent descriptors.
4. Add explicit Task tool execution.
5. Add description-based routing.
6. Add nested lineage/depth guardrails.
7. Add workspace isolation metadata and behavior.
8. Add child session metadata and task tree payloads.
9. Add skill preloading.
10. Add fork-skill-to-task path.

The next step is not adding more profile fields. The next step is lifecycle
discipline.

## Verification Evidence

Representative tests cover:

- explicit Task tool execution;
- auto routing to matching subagent descriptions;
- nested tree and governance guards;
- isolated workspace execution;
- OpenCode markdown agent loading;
- preloaded subagent skills;
- HTTP task tree payloads.

Relevant commands:

```bash
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
```

## Remaining Work

1. Complete queued/running/completed/failed/cancelled state machine.
2. Add wait/promote/cancel/resume commands and HTTP endpoints.
3. Make background CLI execution first-class.
4. Add TUI/Desktop subagent panes and task tree navigation.
5. Move duplicated runner behavior into shared SessionRunner.
6. Add clearer event model for task lifecycle transitions.
