# Part 04 - Skill System

## Problem Statement

Skills originally looked like instruction files. That is not enough for a
serious agent runtime. A skill needs to be:

- discoverable;
- permission-gated;
- loadable on demand;
- visible to the model without dumping full content into every prompt;
- able to carry supporting files;
- able to influence tool access;
- optionally able to fork work into a subagent;
- observable in session history;
- protected across compaction.

The demand is to turn skill from markdown into a runtime capability object.

## Reference Direction

### Claude Code

Claude Code's design treats skills and subagents as first-class concepts. A
subagent has independent context, tools, permissions, model choice, and skills.
Skill loading is not just prompt concatenation. It is part of runtime routing.

The useful idea for OpenHarness:

```text
skill = instruction package + metadata + routing hints + permission boundary
```

### OpenCode

OpenCode's skill direction emphasizes progressive disclosure. The model sees
available skills first, then asks to load a specific skill when needed. This
avoids paying context cost for every skill on every turn.

The useful idea for OpenHarness:

```text
available skills in system prompt
  -> model chooses
  -> skill tool loads full content
```

## Current OpenHarness Skill Architecture

The current skill path is:

```text
AgentProfile
  -> SkillConfig
  -> ToolContext
  -> SkillRegistry
  -> <available_skills>
  -> skill tool
  -> <skill_content>
  -> session event
  -> compaction protection
  -> optional fork to Task/subagent
```

## Implemented Capabilities

### Profile-Level Skill Configuration

Agent profiles can declare:

```yaml
skills:
  - review
skill_roots:
  - shared-skills
permission:
  skill:
    private-skill: deny
skill_permissions:
  public-skill: allow
```

This puts skill configuration at the same level as model, tools, and
permission. It is no longer an ad hoc tool parameter.

### ToolContext Injection

The active agent id, skill roots, active/preloaded skills, and skill
permissions are injected into `ToolContext`. The skill tool can then resolve
the correct registry and enforce the active profile's boundaries.

### Built-In Skill Root

The built-in skill root is registered as part of the skill registry. Workspace
and user skills can override built-ins by name.

The priority model is important:

```text
workspace skill
  > explicit roots
  > user skills
  > built-in skills
```

This gives project authors a controlled override path without losing bundled
capabilities.

### Available Skills Prompt

If an agent is allowed to use the `skill` tool, the system prompt includes an
`<available_skills>` block. It contains only:

- name;
- description;
- location.

It does not include full skill content. This keeps prompts smaller and makes
skill loading explicit.

### Permission Model

Skill permissions are enforced in two places:

1. denied skills are hidden from `<available_skills>`;
2. direct load attempts return permission errors.

This matches the principle that discovery and execution should use the same
policy boundary.

### Skill Tool V2

The primary path is now name-based loading:

```json
{"name": "review"}
```

The output includes:

- `<skill_content>`;
- base directory;
- sampled resource files;
- frontmatter-derived metadata;
- argument substitution where applicable.

List/search remains useful for diagnostics, but loading by name is the runtime
path.

### Subagent Preloading

An agent profile can preload skills into an independent subagent context.
Preloaded skills are written into the child session's system message and
metadata.

The parent context does not receive the child skill body. It only receives the
Task result and metadata. This preserves the subagent isolation model.

### Claude Frontmatter Subset

The supported subset includes:

- `when_to_use`;
- `paths`;
- `allowed-tools`;
- `disallowed-tools`;
- `user-invocable`;
- `disable-model-invocation`;
- `arguments`.

This gives skills enough metadata for routing and constraints without
requiring full Claude Code compatibility in the first pass.

### Fork Skill To Task/Subagent

A skill can indicate that execution should happen in a forked context with a
specific agent. When the model loads this kind of skill, the runtime can route
through the Task tool and return a summarized result to the parent.

This connects skill to delegation:

```text
skill says what specialized capability exists
Task/subagent says where it should run
```

### Observability

Skill use is recorded as session events:

- `skill.discovered`;
- `skill.loaded`.

The events include call id, input, metadata, skill name, location, base dir,
sampled files, and fork metadata where relevant.

### Compaction Protection

Loaded skill output is protected across compaction boundaries. Without this,
a session could load a skill, compact history, and then lose the instruction
that justified later behavior.

## Development Process

The skill work was deliberately staged:

1. Add profile config fields.
2. Inject config into ToolContext.
3. Add built-in root and override behavior.
4. Add available-skills prompt injection.
5. Enforce skill permissions.
6. Upgrade skill tool output shape.
7. Preload skills for subagents.
8. Support Claude frontmatter subset.
9. Add fork skill to Task/subagent.
10. Add CLI/HTTP APIs, events, golden tests, and compaction protection.

That order matters. If full content loading had been implemented before
permission and ToolContext, the runtime would have had a security and context
pollution problem. If events had been added before the tool output shape was
stable, the session contract would have churned.

## Verification Evidence

The current verification surface includes:

```bash
cargo test -p openagent-tools -q
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo test -p openagent-session --test session_trace -q
```

Important behaviors covered:

- profile parsing and provider payload non-leakage;
- CLI `skills list/show/doctor`;
- HTTP `/api/skills`;
- skill root injection for CLI and HTTP provider loops;
- denied skill hidden and denied on load;
- built-in skill discovery and workspace override;
- `skill.discovered` and `skill.loaded` events;
- compaction preserving loaded skill output.

## Remaining Work

Skill is now structurally complete enough to use. Remaining work is integration
depth:

1. Richer skill selection heuristics.
2. Stronger path-matching for large workspaces.
3. Skill install/update workflow.
4. Skill-aware TUI/Desktop surfaces.
5. Deeper integration with background task lifecycle.
6. Better observability around why a skill was selected.
