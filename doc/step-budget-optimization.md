# Step Budget Optimization

This track records runtime improvements discovered from real Langfuse traces where the model understood the task but exhausted tool-enabled steps before completing all deliverables.

## Evidence

On 2026-06-11, a real `gpt-5.5` OpenAgent smoke task showed:

| Run | Langfuse trace | Config | Outcome |
| --- | --- | --- | --- |
| Complex investigation | `e1e104b82cdc449c7cd4a08a4191c437` | `max_steps=10` | Reproduced failures and located likely fixes, but reached the final text-only step before editing. |
| Follow-up fix | `db54886a34a9716dec037588f6c127a5` | `max_steps=12` | Edited code and passed tests, but reached the final text-only step before writing the report. |
| Report closeout | `4f85f2d63dd7db31a7bcb40f56bc087f` | `max_steps=5` | Wrote the report, verified tests, and completed in 3 steps. |

`max_steps` counts model-loop turns. The final step is text-only because tools are disabled on the final step, so usable tool rounds are roughly `max_steps - 1`.

## Operating Rule

OpenAgent now follows OpenCode semantics by default: no step cap is applied unless the request or agent profile sets `max_steps`/`steps`. Use explicit step caps for constrained evals, demos, or agents that should stop early. Treat a final answer that describes missing required work as a step-budget miss, not a successful completion.

## Evolution Issues

Each direction should move independently: one GitHub issue, one implementation slice, one commit, one push.

| Direction | GitHub issue | Goal |
| --- | --- | --- |
| Adaptive defaults | [#1](https://github.com/LianWeiSQ/openagent-ai/issues/1) | Pick default `max_steps` by task complexity while preserving explicit overrides. |
| Remaining-step warnings | [#2](https://github.com/LianWeiSQ/openagent-ai/issues/2) | Emit runtime warnings before tool-enabled rounds are exhausted. |
| Closeout protection | [#3](https://github.com/LianWeiSQ/openagent-ai/issues/3) | Do not report success when required reports, verification, or artifacts are still missing. |
| Read-heavy loop detection | [#4](https://github.com/LianWeiSQ/openagent-ai/issues/4) | Detect repeated inspection rounds without patch or verification progress. |

## Commit Discipline

- Start each slice from the matching GitHub issue.
- Keep the change narrow enough to verify locally.
- Add or update tests in the same commit.
- Push after each verified slice.
- Record residual risk in the issue if the slice is intentionally partial.
