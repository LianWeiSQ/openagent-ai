# Operations

OpenAgent exposes enough runtime data to debug agent behavior without relying on external telemetry.

## Stream Events

`AgentLoop.run()` yields structured events:

| Event | Meaning |
| --- | --- |
| `text-start`, `text-delta`, `text-end` | Model text stream |
| `tool-call` | Model requested a tool |
| `tool-result` | Tool execution finished |
| `step-start`, `step-finish` | One loop step boundary |
| `question-request` | Agent needs user input |
| `patch` | Workspace files changed |
| `error` | Runtime error |

## Observability

Runtime metadata is stored under `Session.metadata["observability"]`:

- trace identifiers;
- recent events in a bounded buffer;
- model usage;
- context pack diagnostics;
- optional JSONL paths.

Runtime logs are separate from trace events. Logs are for operators; trace events are for replay, metrics, and regression analysis.

## Session Store

OpenAgent writes a local session ledger by default under `.openagent/sessions`. The store is meant for crash recovery, eval evidence, and trace debugging; `.openagent/` is ignored by git.

```python
config = AgentConfig(
    name="demo",
    options={
        "session_store": {
            "enabled": True,
            "root_dir": ".openagent/sessions",
        }
    },
)
```

Set `options={"session_store": {"enabled": False}}` to disable local persistence for a run.

File layout:

| Path | Purpose |
| --- | --- |
| `{root}/{session_id}/session.json` | Session metadata and latest run pointer |
| `{root}/{session_id}/transcript.jsonl` | Append-only message transcript |
| `{root}/{session_id}/state.latest.json` | Restorable `messages`, `todos`, and `metadata` snapshot |
| `{root}/{session_id}/runs/{run_id}/events.jsonl` | Append-only run ledger |
| `{root}/{session_id}/runs/{run_id}/summary.json` | Run status, counters, token usage, cost, and paths |

The ledger records `run.started`, `message.appended`, `step.started`, `step.finished`, `tool.call.requested`, `tool.call.started`, `tool.call.finished`, `tool.call.failed`, `model.usage`, `patch.detected`, `runtime.warning`, `run.finished`, and `run.failed`.

Restore a session from disk:

```python
from openagent.core.session import FileSessionStore

store = FileSessionStore(".openagent/sessions")
session = store.load_session(session_id)
```

`load_session(...)` restores `Session.messages`, `Session.todos`, and `Session.metadata` from `state.latest.json`. If the latest state file is missing, it falls back to the append-only transcript.

## Safety

Logs and traces should not include secrets. OpenAgent redacts common secret-bearing keys such as `api_key`, `authorization`, `cookie`, `password`, `secret`, and `token`.

Remote sandbox connection data is used to initialize the runtime, but is not emitted as tool metadata.

## Langfuse Export

OpenAgent can export P0 trace spans and eval scores to Langfuse. Local `trace.jsonl`, `summary.json`, and eval `report.json` remain the source of truth; Langfuse is an optional analysis view for traces, scores, latency, token usage, and regression review.

Install optional dependencies:

```bash
pip install "openagent-core[langfuse]"
```

Configure an agent or eval run:

```python
config = AgentConfig(
    name="eval",
    permission="FULL",
    options={
        "trace": {
            "enabled": True,
            "exporters": {
                "langfuse": {
                    "enabled": True,
                    "environment": "local",
                    "tags": ["openagent", "eval"],
                    "scores_enabled": True,
                }
            },
        }
    },
)
```

Required environment:

```bash
export LANGFUSE_PUBLIC_KEY=...
export LANGFUSE_SECRET_KEY=...
export LANGFUSE_BASE_URL=https://cloud.langfuse.com
```

The Langfuse exporter defaults to metadata-only export. It sends identifiers, span type, status, latency, model/tool names, tool source, token counts, cost, runtime warning metadata, eval score, eval status, runtime warning count, and trace-check status. It does not send prompts, model output, tool input/output, or workspace paths unless explicitly enabled with `include_content` or `include_workspace`.

Trace mapping:

| OpenAgent event | Langfuse observation |
| --- | --- |
| `run.started` / `run.finished` | `agent` |
| `step.started` / `step.finished` | `span` |
| `model.call.started` / `model.call.finished` | `generation` |
| `tool.call.started` / `tool.call.finished` | `tool` |
| `runtime.warning` | `span` |

The Langfuse trace id is deterministic: OpenAgent uses the local trace id as the seed for `create_trace_id(...)`, then stores the exported id in `Session.metadata["agent_trace"]["exporters"]["langfuse"]["trace_id"]`.

When `scores_enabled` is true, each eval case sends four trace-level scores after local scoring completes:

| Score name | Data type | Value |
| --- | --- | --- |
| `openagent.eval.score` | `NUMERIC` | `EvalResult.score` |
| `openagent.eval.status` | `CATEGORICAL` | `pass` or `fail` |
| `openagent.trace_check` | `BOOLEAN` | local trace integrity result |
| `openagent.runtime_warning_count` | `NUMERIC` | `EvalResult.runtime_warning_count` |

Score ids are stable idempotency keys:

```text
openagent:{run_id}:{case_id}:score
openagent:{run_id}:{case_id}:status
openagent:{run_id}:{case_id}:trace_check
openagent:{run_id}:{case_id}:runtime_warning_count
```

Score export is non-fatal. `EvalResult.langfuse_trace_id`, `EvalResult.langfuse_scores_sent`, and `EvalResult.langfuse_error` record the external export state while local `report.json` remains authoritative.

Local mock verification:

```bash
PYTHONPATH=src:src/tests python -m unittest \
  src/tests/test_trace.py \
  src/tests/test_eval_runner.py
```

Optional real smoke requires Langfuse env vars, then run a small eval case with the `langfuse` exporter enabled and confirm the trace plus four scores appear in the Langfuse UI.

## Step Budget Guidance

`max_steps` limits model-loop turns, not individual tool calls. One step can contain multiple tool calls, but the final step is text-only because `AgentLoop` sends `tools=[]` when `step_index >= max_steps`. In practice, usable tool rounds are `max_steps - 1`.

Observed on 2026-06-11 with a real `gpt-5.5` Langfuse smoke task:

| Run | Config | Outcome |
| --- | --- | --- |
| Complex investigation | `max_steps=10` | Reproduced failures and located likely fixes, but reached the final text-only step before editing. |
| Follow-up fix | `max_steps=12` | Edited code and passed tests, but reached the final text-only step before writing the report. |
| Closeout | `max_steps=5` | Wrote the report, verified tests, and completed in 3 steps. |

Recommended defaults:

| Task type | Suggested `max_steps` |
| --- | --- |
| Text-only answer or single quick command | 3-4 |
| Small file inspection or one bounded shell action | 6-8 |
| Small engineering fix with tests and a short report | 20-24 |
| Multi-file feature, benchmark adapter, or cross-module debugging | 30-40 |
| Terminal-Bench or long benchmark tasks | 80+ |

By default, OpenAgent now follows OpenCode semantics: no step cap is applied unless the request or agent profile sets `max_steps`/`steps`. Use explicit step caps for constrained evals, demos, or agents that should stop early. Treat a final answer that says a required artifact could not be written as a step-budget miss, not as a successful completion.

## Evaluation

The repository includes local eval/replay utilities plus benchmark adapters:

- local eval runner for deterministic regression cases;
- P0 trace-backed scoring for run integrity, required/forbidden trace events, model/tool call budgets, latency, tokens, and cost;
- trace/replay output for failure analysis;
- baseline regression reports for prompt/model/tool changes;
- Terminal-Bench adapter via `perform_task(instruction, session, logging_dir)`;
- Harbor adapter for benchmark environment execution.

An eval case can define deterministic expectations:

```yaml
id: smoke_case
input: continue
expected:
  files_changed: []
scoring:
  require_no_error: true
  require_final_answer_contains:
    - done
  require_trace_check: true
  required_trace_events:
    - run.started
    - model.call.finished
  forbidden_trace_events:
    - tool.call.finished
  max_runtime_warnings: 0
  max_model_calls: 1
  max_tool_calls: 0
  max_cost: 0.05
```

`run_eval_files(...)` writes:

- `report.json`: per-case result plus aggregate pass rate, token/cost, latency, trace check, tool-source metrics, `session_id`, `run_id`, session ledger path, and latest session state path;
- `summary.md`: human-readable summary for review;
- `runs/{run_id}/trace.jsonl` and `runs/{run_id}/summary.json`: P0 trace artifacts for every case;
- `regression.json` and `regression.md` when `baseline_report=...` is provided.

Baseline comparison tracks case additions/removals plus status, score, cost, duration, tool-call, and model-call deltas.

Runtime warnings are designed for budget gates and live review. Enable them through `AgentConfig.options["runtime_warnings"]`:

```python
options={
    "runtime_warnings": {
        "enabled": True,
        "context_usage_ratio": 0.75,
        "context_critical_ratio": 0.9,
        "max_step_total_tokens": 20000,
        "max_step_cost": 0.25,
    }
}
```

Benchmark adapters also read environment variables:

```bash
export OPENAGENT_CONTEXT_WARNING_RATIO=0.75
export OPENAGENT_MAX_STEP_TOTAL_TOKENS=20000
```

CI gate command:

```bash
PYTHONPATH=src python -m openagent.core.eval.ci_gate \
  --report path/to/report.json \
  --regression path/to/regression.json \
  --min-success-rate 1.0 \
  --max-runtime-warnings 0
```

The gate exits with `1` when success rate, trace integrity, runtime warning budget, status regressions, or budget regressions violate the configured policy.

Smoke command:

```bash
PYTHONPATH=src:src/tests python -m unittest \
  src/tests/test_eval_runner.py \
  src/tests/test_terminal_bench_adapter.py \
  src/tests/test_harbor_adapter.py \
  src/tests/test_loop.py
```

Full regression:

```bash
PYTHONPATH=src:src/tests python -m unittest discover -s src/tests -p "test_*.py"
```
