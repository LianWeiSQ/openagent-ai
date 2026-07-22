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
- Compaction is resumable, typed, parent-linked, and records what information
  was removed.
- Persisted messages and events remain ordered and scoped to one session.
- Attachments are persisted as metadata plus safe content references.
- Secret values are redacted before persistence and diagnostics.
