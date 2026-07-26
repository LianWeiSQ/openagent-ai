# Durable Session And Turn Engine

OpenHarness treats execution state as durable runtime data. A process, HTTP
connection, or Desktop window may disappear without becoming the owner of
session truth.

The engine has two storage layers:

```text
session/transcript/lifecycle JSONL
  -> authoritative append-only history
  -> deterministic projection
  -> .openagent-runtime/session_catalog.sqlite3
  -> HTTP history, execution-tree, lease, and full-text queries
```

SQLite is never the only copy of an execution transition or message. It can be
deleted and rebuilt from the session ledgers.

## Execution Model

`DurableExecutionRecord` is shared by five first-class runtime objects:

| Kind | Identity | Parent |
| --- | --- | --- |
| `session` | session ID | optional fork source |
| `turn` | turn/run ID | session |
| `task` | subagent task/session ID | parent turn |
| `approval` | approval request ID | waiting turn |
| `question` | question request ID | waiting turn |

Every kind uses the same seven persisted states:

```text
queued -> running -> waiting -> completed
   |         |          |       failed
   |         |          |       cancelled
   |         |          |       interrupted
   +---------+----------+
```

Terminal executions may not restart in place. A retry moves a failed,
cancelled, or interrupted record to `queued`, increments `attempt`, clears its
lease, and records the recovery reason. `completed` is immutable.

Legacy input aliases are accepted while reading:

- `canceled` and `expired` become `cancelled`;
- `interrupting`, `pending_approval`, and `pending_question` become `waiting`;
- `streaming`, `retrying`, and `in_progress` become `running`.

New public values only use the canonical seven states. Queue timeout is
`cancelled` with `terminal_reason=queue_timeout`; cancel intent is represented
by `cancel_requested`, not by inventing another state.

## Phase And Recovery

State says whether work can progress. Phase says where a crash occurred:

- `scheduling`
- `provider`
- `tool`
- `approval`
- `question`
- `compaction`
- `subagent`
- `finalize`

`RecoveryPolicy` combines state, phase, attempt, lease, and effect receipt:

| Crash point | Recovery |
| --- | --- |
| durable queue | resume |
| approval/question wait | resume without calling the model |
| provider request | retry while under the attempt limit |
| compaction | retry because the boundary write is atomic |
| subagent wait | resume from the child session |
| tool with committed effect | resume using the stored result |
| tool with no committed effect | retry if no effect was claimed |
| tool with claimed but uncommitted effect | interrupt as ambiguous |
| terminal state | ignore |

HTTP startup reconciles persisted turn jobs before serving them. A live
approval or question remains `waiting`; an orphaned running turn becomes
`interrupted` and retains `recovery=retry|resume|interrupt` so a client can
offer the correct next action.

## Leases And Heartbeats

Running and queued turns carry an owner, claim time, heartbeat time, and
expiry. Async workers renew their execution lease and all queue leases owned
by the same runtime. Queue scheduling and worker quota are scoped by session
root, so independent runtimes and parallel tests cannot consume one another's
capacity.

An unexpired lease is not stolen. After expiry, another runtime may reclaim a
persisted queued payload. Lease files are coordination data under
`.openagent-runtime/`; lifecycle records remain the recovery evidence.

## Idempotency And Side Effects

Turn creation accepts `idempotency_key`. Repeating the same key in a session
returns the original turn, status, and attempt without calling the provider
again. Manual retry derives a new attempt key while retaining the retry root.

Tool execution has a stricter at-most-once boundary:

1. create an effect receipt with `create_new`;
2. append `effect.claimed` before invoking the tool;
3. execute the tool;
4. atomically persist the result and append `effect.committed`;
5. replay a committed result instead of executing again.

If a process dies after step 3 but before step 4, the receipt remains
`claimed`. The runtime reports `effect_uncertain` and refuses automatic
re-execution. This favors at-most-once behavior over silently duplicating a
shell, file, MCP, Git, or external side effect.

Permission probing and approved execution use separate effect scopes. A
permission-required result therefore cannot be mistaken for the approved
tool's committed effect.

## Ledger Layout

For session `session_x`:

```text
<session-root>/
  session_x/
    session.json
    state.latest.json
    transcript.jsonl
    lifecycle.jsonl
    lifecycle.lock
    runs/<turn-id>/
      run.json
      events.jsonl
      parts.jsonl
      summary.json
  .openagent-runtime/
    session_catalog.sqlite3
    effects/<sha256(idempotency-key)>.json
    turn_jobs.json
    turn_queue/
    turn_queue_leases/
    turn_retry/
```

`lifecycle.jsonl` stores full execution snapshots. Each row has a stable event
ID, monotonic per-session sequence, event type, timestamp, and record. Append
and read operations share an advisory file lock, so independent runtime
processes cannot interleave rows or observe a partially written line. Every
append calls `sync_data` before success. `lifecycle.lock` is coordination data,
not execution history.

`state.latest.json`, `turn_jobs.json`, lease files, and SQLite are projections
or coordination caches. They are useful, but they do not replace the ledger.

## SQLite Catalog

The catalog contains:

- session history and metadata;
- current execution records and parent relationships;
- lifecycle events;
- lease owner and expiry;
- FTS5 message content.

Indexes support session/status, parent task tree, and idempotency queries. A
unique `(session_id, kind, idempotency_key)` constraint provides a second
defense against duplicate execution projection.

Rebuild reads `state.latest.json`, projects messages from the append-only
transcript, and replays every lifecycle event. Rebuild is deterministic and
safe after deleting the database.

## HTTP Service

```text
GET  /api/session-catalog?query=<text>&limit=<n>
POST /api/session-catalog/rebuild
GET  /api/sessions/{session_id}/executions
POST /api/sessions/{session_id}/turns
```

The catalog endpoint returns session matches and FTS message hits. The
execution endpoint returns the session/turn/task/approval/question tree with
canonical status, phase, attempt, idempotency key, lease, and recovery fields.

Turn request example:

```json
{
  "input": "inspect the workspace",
  "async": true,
  "idempotency_key": "request-2026-07-26-001"
}
```

A duplicate response contains `deduplicated=true` and the original `turn_id`.

## Verification

The session tests cover:

- all seven states and invalid terminal transitions;
- duplicate execution creation;
- provider, tool, approval, compaction, and subagent crash classification;
- ambiguous and committed effect receipts;
- concurrent writers in independent processes;
- catalog deletion, rebuild, task-tree query, lease query, and FTS search.

HTTP tests cover:

- sync and async turns through one durable registry;
- root-isolated worker quotas;
- queue expiry and stale/live lease behavior;
- persisted queue recovery after restart;
- approval/question pause and resume;
- idempotent turn submission with one provider request;
- catalog rebuild and search before and after runtime restart.
