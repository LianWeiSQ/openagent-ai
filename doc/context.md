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

Every item also carries the versioned `openagent.context_item_taxonomy.v1`
contract. The taxonomy is intentionally separate from the compatibility
`kind` and `source` labels and classifies four policy dimensions:

- category: the semantic family, such as instruction, conversation, tool
  observation, attachment, skill, tool manifest, runtime state, or session
  state;
- origin: the owning source, such as an agent profile, instruction file,
  session message, turn attachment, registry, todo, checkpoint, or work state;
- scope: whether the item is stable, session-scoped, or turn-scoped;
- compaction: whether the item must be preserved, summarized, truncated,
  rebuilt from its authority, or dropped.

Builder normalization upgrades legacy items that do not yet carry taxonomy.
Trace and receipt diagnostics expose taxonomy without copying item content into
public diagnostics or provider payloads. Compaction and budget policy must use
this taxonomy instead of matching free-form `kind` strings.

Budget pressure is handled in stages:

1. project oversized tool output through typed micro-compaction;
2. compact older conversation into structured work state;
3. reduce nonessential context detail;
4. preserve task intent, decisions, changed files, blockers, and next steps;
5. fail explicitly when a safe request still cannot fit.

## Tool Output Micro-Compaction

Large tool results use `openagent.context_micro_compaction.v1`. This is a
lossy provider projection over durable source data, not a transcript rewrite
and not a context epoch. The session message part remains the authority and
keeps the complete result. `ContextPackBuilder` derives a bounded head/tail
preview immediately before budget selection and provider projection.

The v1 contract records:

- the deterministic `tool_output_head_tail_v1` strategy and content hash;
- original, preview, projected, omitted, and estimated token sizes;
- the reason for projection and the estimated tokens saved;
- a durable session message/part reference when one is available;
- a non-durable tool-call reference only for legacy or in-memory input.

The projection is applied when either the configured byte or line threshold is
exceeded and only when the projected item is smaller by token estimate. It
preserves sampled leading and trailing lines, preserves both ends of an
oversized single line, and is UTF-8 safe. The original result is removed from
the projected item's nested metadata so a provider adapter cannot recover the
dropped middle through a second field. Loaded skill results are protected
because their complete instructions are active model context rather than
diagnostic tool output.

Configuration continues to use the existing context-budget fields:
`prune_old_tool_outputs`, `tool_context_preview_bytes`,
`tool_context_preview_lines`, and `tool_context_line_max_chars`. These values
are converted into `ContextPackBuildOptions`, persisted in the private replay
specification, and never sent to the provider.

The design follows the same authority/view split used by OpenCode and Claude
Code: retain recoverable tool state while reducing the model-visible result.
OpenHarness places the operation at the shared `ContextPackBuilder` boundary so
CLI and HTTP runtimes cannot implement different truncation rules. Receipt and
trace data expose only hashes, counts, strategy, savings, and recovery
references. Retry and receipt replay rebuild the same pack without executing a
tool or calling a provider.

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

## Context Epochs

Every successful compaction that removes transcript messages creates an
`openagent.context_epoch.v1` record. `ContextEpoch` is the canonical compaction
boundary shared by manual compaction, automatic budget compaction, the session
ledger, restart/replay materialization, and context diagnostics. It records:

- stable epoch, session, run, parent epoch, and boundary message identities;
- whether the trigger was manual or automatic and the policy reason;
- the last compacted message and the number of compacted messages;
- summary format, source, timestamp, and optional structured work state;
- automatic-compaction provenance such as the prior pack hash, step, and
  summary token estimate.

The complete epoch is stored in the compaction message part and in the latest
session state. Session events receive only redacted diagnostics: identities,
policy fields, counts, and summary length, never the summary or structured
state body. On restart, the latest valid epoch becomes a typed work-state item
for `ContextPackBuilder`; its boundary message is not projected as an ordinary
system message.

Epochs form a parent-linked append-only chain. The transcript reader still
recognizes the older free-form `compaction_boundary` shape so existing sessions
remain usable, but all new writes use `ContextEpoch` exclusively. A manual
compaction of an empty session can update session metadata without inventing a
message boundary.

## Invariants

- One runtime path owns provider-message assembly.
- New context sources are explicit typed items, not hidden string appendices.
- Tool micro-compaction changes only the model projection; the session ledger
  retains the complete source result and a typed recovery reference.
- Compaction is resumable, typed, parent-linked, and records what information
  was removed.
- Persisted messages and events remain ordered and scoped to one session.
- Attachments are persisted as metadata plus safe content references.
- Secret values are redacted before persistence and diagnostics.
