# Eval P1 Progress

## Goal

Build a generic eval regression layer on top of the P0 Agent Run Trace. P1 stays domain-agnostic: product-specific behavior is expressed through cases, tools, skills, MCP servers, and scoring rules.

## Scope

1. Use P0 trace artifacts as the primary eval evidence.
2. Record trace integrity, event counts, model/tool counts, latency, token, cost, and tool-source metrics per case.
3. Add scoring rules for trace closure, required/forbidden trace events, model/tool call budgets, latency, and cost.
4. Add baseline regression comparison for prompt/model/tool changes.
5. Keep old observability JSONL replay compatible while making P0 `trace.jsonl` the preferred path.

## Receipts

- 2026-06-09: P1 started after P0 was pushed to GitHub at commit `44861ee`.
- 2026-06-09: Confirmed existing eval runner wrote `report.json` and `summary.md`, but result tracing still preferred old observability JSONL.
- 2026-06-09: Updated eval runner to enable P0 trace by default under the eval output directory and attach trace path, summary path, trace check result, model/tool/MCP/Skill/local counts, error count, artifact count, and latency to every `EvalResult`.
- 2026-06-09: Added trace-backed scoring rules: `require_trace_check`, `required_trace_events`, `forbidden_trace_events`, `required_tool_sources`, `max_model_calls`, `max_tool_calls`, `max_duration_ms`, `max_total_latency_ms`, and existing `max_cost`.
- 2026-06-09: Added baseline regression output through `baseline_report=...`, writing `regression.json` and `regression.md`.
- 2026-06-09: Updated replay summary to read both old observability events (`name`) and P0 trace events (`event`).
- 2026-06-30: Hardened the Harbor Terminal-Bench 2.0 path after P0 wiring: added offline adapter tests, broader OpenAgent JSON event usage parsing, Langfuse/trace env passthrough, 1/5/10 task Harbor job configs, and a Harbor job summarizer that emits `summary.json` and `summary.md`.

## Remaining Later Work

- Add a CLI wrapper for eval suites if repeated local runs need a stable command-line interface.
- Add HTTP record/replay cassettes only when model/tool integration tests need deterministic external traffic.
- Add database-backed eval history when cross-run querying becomes necessary.

## P2 Cost / Budget Governance Slice

- 2026-06-11: Added optional baseline regression thresholds for cost, duration, input tokens, output tokens, total tokens, model calls, and tool calls.
- Regression reports now include token deltas and `budget_regressions`, making cost/token drift visible before it becomes a Langfuse dashboard or production issue.

## P3-P5 Runtime Warning / Dashboard / CI Slice

- 2026-06-11: Added configurable `runtime-warning` stream events and `runtime.warning` trace observations for context usage, step token budgets, and step cost budgets.
- 2026-06-11: Added real-time display payloads for warning events, plus Terminal-Bench and Harbor `runtime-warnings.txt` outputs.
- 2026-06-11: Added runtime warning scoring in eval reports, Langfuse `runtime_warning_count` scores, Langfuse warning spans, and a local `openagent.core.eval.ci_gate` command for CI budget gates.
