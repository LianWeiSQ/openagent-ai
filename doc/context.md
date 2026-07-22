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

## Semantic Anchors

`openagent.semantic_anchor.v1` separates continuation-critical facts from the
summary prose that happens to describe them. This follows OpenCode's practice
of carrying goal, constraints, progress, decisions, next steps, and critical
context through compaction, together with Claude Code's emphasis on preserving
user intent, edited files, errors, fixes, and the exact continuation point.
OpenHarness turns those conventions into a typed runtime contract rather than
depending on a model to reproduce section headings consistently.

An anchor has a stable ID, kind, session or epoch scope, authority, source,
content hash, priority, and optional durable references. Supported kinds are
goal, constraint, decision, progress, file, critical context, blocker, next
step, and recovery point. IDs are independent of storage location: the primary
goal has a fixed ID, todo and checkpoint anchors use their durable business
identity, files preserve case-sensitive paths, and content-addressed facts use
a normalized key.

`openagent.semantic_anchor_registry.v1` is the deterministic conflict resolver.
It rejects malformed candidates, merges equal IDs by authority and priority,
uses the latest candidate only as the final tie-breaker, sorts the resulting
snapshot canonically, and hashes the full anchor set. Explicit input outranks a
context epoch, which outranks structured work state, todos, checkpoints, and
message-derived evidence. The original transcript and typed epoch remain the
authority; the registry is a resumable semantic index over those records.

At compaction time the HTTP runtime merges the previous registry with current
structured work state, active todos, selected checkpoints, and a recovery
anchor for the new boundary. Live todo and checkpoint items are not duplicated
as anchor messages before compaction. Each completed epoch stores the complete
registry, while event diagnostics expose only schema, hashes, counts, and kind
distribution. Restart materialization copies the epoch anchors into typed work
state, and receipt replay rebuilds the same registry without calling a provider
or executing a tool.

`ContextPackBuilder` renders each registered anchor as an independent pinned
`semantic_anchor` item. Keeping anchors independent allows per-kind budget
allocation. Registry provenance and references
stay in private trace metadata; provider input receives only XML-escaped ID,
kind, and content. Public HTTP diagnostics expose safe hashes and counts, and
sanitize the source. An unregistered anchor item, a missing registry item, or a
metadata mismatch invalidates the pack. Empty registries are omitted from the
pack hash payload so pre-anchor receipts retain their historical hash.

## Layered Budget Allocation

`openagent.context_budget_allocation.v1` replaces the former global
`pinned/priority` greedy selector. The old selector could retain an older
message before a newer one with the same priority and could split an assistant
tool call from its result. It also provided no explanation for how different
context families competed for the remaining window.

The allocator consumes typed `ContextItem` records after semantic deduplication,
token estimation, tool micro-compaction, and required-item fitting. Its policy
is `openagent.context_budget_policy.v1`; the complete policy is part of
`ContextPackBuildOptions`, so HTTP receipt replay and runtime restart use the
same policy rather than reconstructing defaults at an entry point. The default
recent-tail boundary is derived from `prune_keep_recent_user_turns`, matching
the existing public context-budget configuration.

Allocation has three phases:

1. hard reserve selects pinned instructions, the latest user request,
   semantic anchors, active session state, runtime state, and required
   attachments;
2. soft quota gives recent conversation, tool observations, session state,
   attachments, historical conversation, and extension items independent
   weighted opportunities to fit;
3. borrowing returns every unused or rounding remainder to one shared pool and
   makes a deterministic second pass, so an empty class never strands tokens.

Ranking uses taxonomy category and scope, anchor kind, user-turn recency,
recoverability, explicit priority, source sequence, and a stable group ID.
Goal and constraint anchors are hard-required and rank ahead of continuation
anchors; the full anchor tie-break order begins with goal, constraint, blocker,
decision, and critical context. Current-turn and recent records rank ahead of
historical records. Inline-only facts receive more protection than equivalent
facts recoverable from the session ledger, a durable reference, or a rebuildable
authority.

Assistant messages carrying tool calls and their matching tool-result messages
form an atomic allocation group. A group is either selected or dropped as a
whole, preventing malformed provider history. Required fitting still runs
before allocation and can shrink allowed source types; when required content
cannot fit even at its hard minimum, the allocator reports
`required_budget_exhausted` and exact hard overflow tokens instead of silently
discarding the condition.

Every budgeted message receives a `ContextBudgetItemDecision` in the shared
trace. Decisions record class, phase (`hard_reserve`, `soft_quota`, `borrowed`,
or dropped), recency, recoverability, group identity, group size, class quota,
and deterministic rank. `ContextPackBudget` and the public redacted receipt
carry aggregate class accounting, policy hash, selected, borrowed, dropped,
and overflow token counts. The HTTP projection exposes the same fields after
sanitizing the group label. They contain no item content, provider credentials,
or model-option values. Pack validation checks schema versions, phase/inclusion
agreement, class accounting, and total budget before a provider adapter accepts
the pack.

The design combines OpenCode's bounded recent tail and anchored older-state
summary with Codex's preference for retaining recent history and canonical
initial context when trimming overflow. OpenHarness adds a typed allocator at
the shared builder boundary so CLI, Bridge, replay, and restart all make the
same selection and expose the same explanation.

## Compaction Evaluation

Compaction quality is a release contract, not an assertion that a smaller
prompt is automatically better. The eval crate defines the versioned
`openagent.eval.context_compaction_corpus.v1` corpus and
`openagent.eval.context_compaction.v1` observation/result contracts. A rubric
can require anchor IDs and kinds, continuation terms, removed noise markers,
minimum token savings, a maximum post-compaction input size, intact tool
exchange groups, typed epochs, a valid epoch parent chain, append-only ledger
preservation, and replay/restart pack and registry parity.

The scorer produces a 100-point breakdown:

- semantic anchor recall: 25 points;
- continuation-term recall: 20 points;
- historical-noise removal: 10 points;
- token reduction: 15 points;
- model-budget fit: 10 points;
- required-item and tool-group integrity: 5 points;
- replay/restart consistency: 10 points;
- typed epoch and ledger durability: 5 points.

Rubric requirements remain hard gates even when the weighted score would be
high enough. The regression comparison also rejects score loss, anchor or term
recall loss, noise/integrity/recovery/durability loss, token-savings regression,
post-compaction token growth beyond tolerance, and a passing baseline becoming
a failure.

For an overflowing pre-compaction receipt, source size is reconstructed from
the allocator's candidate tokens plus fixed tool/model overhead. Using only the
already-selected provider projection would undercount discarded history and
could incorrectly report zero savings. The post-compaction side always uses
the actual provider input estimate and its model limit.

The checked-in golden at
`tests/golden/rust_rewrite/context_compaction_eval.json` contains a passing
baseline, a deliberately degraded counterexample, and the exact regression
reasons. A Core integration builds a noisy multi-turn session, replaces old
history with a typed epoch and semantic registry, and verifies deterministic
replay/restart packs. The HTTP integration scores a real automatic compaction,
public receipt/trace projection, zero-side-effect receipt replay, runtime
restart, typed epoch chain, and durable transcript. Eval artifacts retain only
expected synthetic terms, IDs, hashes, booleans, counts, ratios, and failure
reasons; they do not persist provider prompts or user content.

## Invariants

- One runtime path owns provider-message assembly.
- New context sources are explicit typed items, not hidden string appendices.
- Tool micro-compaction changes only the model projection; the session ledger
  retains the complete source result and a typed recovery reference.
- Compaction is resumable, typed, parent-linked, and records what information
  was removed.
- Continuation-critical semantics are versioned, conflict-resolved, and survive
  compaction, restart, and receipt replay independently of summary wording.
- Budget selection is layered, deterministic, dependency-aware, and replayed
  from a versioned policy; unused soft quota is always borrowable.
- Compaction releases are gated by semantic fidelity, measured reduction,
  provider integrity, and durable recovery rather than token reduction alone.
- Persisted messages and events remain ordered and scoped to one session.
- Attachments are persisted as metadata plus safe content references.
- Secret values are redacted before persistence and diagnostics.
