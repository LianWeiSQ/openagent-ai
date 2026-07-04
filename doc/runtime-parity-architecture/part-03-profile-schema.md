# Part 03 - AgentProfile, SkillConfig, And TaskConfig Schema

## Problem Statement

Agent profile parsing grew organically in two places:

- CLI profile loading for `openagent run --agent ...`;
- HTTP runtime profile loading for App Bridge and subagent execution.

As profile fields expanded, this duplication became unsafe. The same markdown
agent could be interpreted differently by CLI and HTTP. Worse, runtime-only
fields could accidentally pass into provider payloads through `model_options`.

The risk increased when profiles gained:

- `skills`;
- `skill_roots`;
- `skill_permissions`;
- `task_permissions`;
- permission subtrees;
- model options;
- workspace isolation;
- hidden/disabled state.

## Reference Direction

OpenCode's configuration model is not just a CLI parser. The runtime, TUI, and
tool execution all rely on common interpretation of agent configuration.

Claude Code similarly treats agent/subagent configuration as a runtime
contract: tool access, model choice, permissions, and skills belong to the
agent definition, not to a single entry point.

The OpenHarness design response is to treat profile parsing as shared runtime
schema.

## Current Shared Schema

The shared schema lives in `openagent-tools` because both CLI and HTTP already
depend on that crate and it already owns task/skill permission rule types.

Core structures:

```text
AgentProfileSchema
  - id
  - name
  - description
  - mode
  - model
  - provider
  - permission
  - TaskConfig
  - SkillConfig
  - prompt
  - tools
  - max_steps
  - temperature
  - top_p
  - color
  - disabled
  - hidden
  - workspace_isolation
  - model_options

SkillConfig
  - skills
  - roots
  - permissions

TaskConfig
  - permissions
```

This is intentionally not a full runner abstraction. It is the shared input
contract that makes a runner abstraction safe to build later.

## Design Decisions

### Keep File Discovery Local To Each Surface

CLI and HTTP still decide where to look for agent files:

- `.openagent/agents`;
- `.opencode/agents`;
- `.opencode/agent`;
- built-in profiles.

The shared schema only parses a `serde_json::Value` into a normalized runtime
shape. This keeps the first extraction low-risk.

### Strip Runtime Config From Provider Options

The parser filters known runtime fields out of `model_options`. This includes:

- `skills`;
- `skill_roots`;
- `skill_permissions`;
- `task_permissions`;
- `task_permission`;
- `permission`;
- `tools`;
- `workspace_isolation`;
- `hidden`;
- `disabled`.

Only model-facing options such as `temperature`, `top_p`, or provider-specific
options remain.

### Merge Permission Sources

Skill permissions and task permissions can come from structured `permission`
subtrees or top-level fields:

```yaml
permission:
  skill:
    review: allow
  task:
    planner: ask

skill_permissions:
  debugger: deny

task_permissions:
  reviewer: allow
```

The parser merges these sources so profile authors can use either style
without CLI/HTTP diverging.

### Preserve Markdown Agent Compatibility

Markdown frontmatter remains supported. The caller still extracts frontmatter
and body, then passes the value to the shared parser. This preserves OpenCode
style markdown agent definitions while keeping parsing rules centralized.

## Development Process

The implementation sequence was intentionally conservative:

1. Define shared schema and parser in `openagent-tools`.
2. Move task/skill permission parsing into shared helpers.
3. Add a focused parser test that verifies:
   - skill config parses;
   - task config parses;
   - permission subtree and top-level rules merge;
   - model options keep model fields;
   - runtime config fields do not leak.
4. Replace CLI `agent_profile_from_value` with a thin adapter.
5. Replace HTTP `runtime_agent_profile_from_value` with a thin adapter.
6. Delete duplicated CLI/HTTP parser helpers.
7. Run CLI and HTTP integration tests to prove behavior stayed stable.

## Verification Evidence

The following tests cover the schema stage:

```bash
cargo test -p openagent-tools -q
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo fmt --all -- --check
git diff --check
```

The most important contract test is the shared parser test in
`src/tools/tests/tool_runtime.rs`. It directly checks that runtime-only config
does not remain in `model_options`.

## Remaining Work

The shared schema is a foundation, not the final runner architecture.

Next steps:

1. Add typed public conversion helpers for CLI/HTTP public profile values.
2. Move built-in profile definitions into a shared registry or shared profile
   descriptor format.
3. Share system-prompt binding logic where possible.
4. Feed the schema into a SessionRunner facade.
5. Extend schema to cover provider catalog and plugin-provided agents if those
   become first-class runtime objects.
