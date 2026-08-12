# Load, Soak, Token, and Cost Acceptance

`openharness-load` runs bounded HTTP or isolated Bridge-turn workloads and
writes `openharness.load_test.report.v1`. It keeps only aggregate metrics and
at most 50 failure samples; request/response bodies and bearer credentials are
never written to the report.

The runner measures:

- request count, successful/failed count, success rate, and requests/second;
- p50, p95, and p99 end-to-end latency;
- input, output, and total Token usage;
- total and per-request cost in microunits;
- baseline regression ratios for p95, average Token use, and average cost;
- session, Run, and Trace correlation IDs only for bounded failure samples.

## CI load contract

The checked CI plan executes six manual read-only Agent turns across two
isolated sessions:

```bash
OPENHARNESS_LOAD_TOKEN=... cargo run -p openagent-eval \
  --bin openharness-load -- \
  run eval/load/ci-bridge-turn.json artifacts/load-report.json \
  eval/load/ci-bridge-turn-baseline.json
```

The accepted baseline records p95 plus 13 Tokens and zero cost per request for
this deterministic Tool-only path. CI rejects an availability, latency, Token,
or cost regression according to the plan. The baseline is content-addressed;
editing a metric without regenerating and explicitly accepting the baseline
makes validation fail.

Create a new baseline only from a passing report:

```bash
cargo run -p openagent-eval --bin openharness-load -- \
  baseline artifacts/load-report.json bridge-turn-v2 \
  artifacts/load-baseline.json
```

Do not update plan thresholds and the baseline in the same regression fix
without an explicit capacity review. Keep the old report, new report, baseline,
Harness version, machine shape, and runtime configuration together in release
evidence.

## Staging soak

`eval/load/staging-soak.json` runs four isolated sessions for 30 minutes with a
one-second think time. Point `base_url` and `workspace` at staging, set the
token environment variable, and run it outside the latency-sensitive CI job:

```bash
OPENHARNESS_LOAD_TOKEN=... cargo run -p openagent-eval \
  --bin openharness-load -- \
  run eval/load/staging-soak.json artifacts/staging-soak.json
```

For a representative production candidate, add workload plans for provider
streaming, MCP reads, mutating tools with approval, queue contention, and
subagent fan-out. Provider workloads must use a dedicated capped account and
set explicit Token/cost limits. Never point an unbounded duration plan at
production; the schema caps concurrency, duration, request count, timeout, and
response size.

## Acceptance sequence

1. Run the deterministic CI workload against the candidate binary and the
   checked baseline.
2. Run the staging soak while watching queue depth, active workers, p95/p99,
   memory, exporter failures, and durable ledger growth.
3. Execute `eval/fault-injection-v1.json`, including Collector outage and
   restart/idempotency cases.
4. Verify every load failure sample pivots to a durable Run/Trace and that no
   side effect was duplicated.
5. Retain load, soak, fault, quality-gate, and regression-replay reports in the
   release artifact before canary promotion.
