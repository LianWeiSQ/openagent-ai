# Fault Injection and Rollout Runbook

The executable source of truth is `eval/fault-injection-v1.json`. Its
fingerprint is audited against `eval/policies/release-v1.json`; every
release-critical case must have one deterministic `cargo test ... -- --exact`
command.

## Critical matrix

| Case | Injection | Required behavior |
| --- | --- | --- |
| `runtime.cancel` | interrupt an open provider stream | cancel promptly; commit no late completion |
| `runtime.deadline` | start after deadline with a pending side effect | fail before provider/tool boundary |
| `runtime.provider-retry` | provider returns retryable 503 | bounded retry; one terminal result |
| `runtime.tool-permission-deny` | deny a mutating tool approval | no tool effect and no provider resume |
| `recovery.restart` | restart after committed tool effect | recover receipt; never duplicate effect |
| `telemetry.collector-outage` | make OTLP collector unreachable | Agent stays available; exporter failure counted |

Queue timeout and approval-wait restart are also in the plan as noncritical
coverage. Run the full plan before a release candidate, after changing Runtime,
provider, persistence, queue, permission, or telemetry code, and during a
quarterly recovery drill.

The tests inject failures with local fake providers, process restarts, bounded
deadlines, and unreachable loopback endpoints. They must not target a shared
production provider or collector.

## Evidence to retain

For each scenario retain:

- exact test command and exit status;
- Harness/Agent/Tool/config versions;
- `run_id`, `trace_id`, terminal outcome, and reason code where a Run exists;
- proof that forbidden side effects did not occur;
- exporter/queue/retry counters relevant to the scenario;
- the fault-plan fingerprint and release-gate decision fingerprint.

If a scenario fails, capture its Run as a bad case before changing the test or
rolling back. A fixed test alone is not enough: promote the sanitized fixture
and make it pass through the same release gate.

## Staged rollout

1. **Shadow:** enable Prometheus and durable correlation everywhere; export a
   sampled Trace stream. Do not page on new SLO alerts yet.
2. **Canary 5%:** require the release gate, tool-governance audit, and all six
   critical fault cases. Compare success/degraded ratio, p95, Token, cost,
   exporter failures, and Trace completeness with the prior version.
3. **Canary 25%:** enable alert routing with warning severity. Hold long enough
   to cover representative provider and Tool traffic.
4. **Full rollout:** page only on tuned SLOs. Preserve version dimensions so a
   regression can be isolated to Harness/Agent/Prompt/Tool configuration.

Stop rollout on a critical-case failure, privacy violation, missing Trace
evidence, duplicate Tool effect, unbounded queue growth, or release-gate fail.
Rollback is configuration/deployment based: restore the prior candidate while
retaining the append-only ledger and bad-case artifacts. Telemetry can be
disabled independently with `OPENHARNESS_TELEMETRY_ENABLED=false`; collector
failure alone must never require stopping Agent traffic.

## Operator drill

At least once per quarter, an operator who did not implement the feature must:

1. trigger one provider retry and one collector outage in a test environment;
2. find the affected bounded metric in Grafana;
3. open the Trace and identify the provider/export boundary;
4. pivot to the durable Run by `run_id`;
5. capture and triage a sanitized bad case;
6. verify that the release gate rejects deliberately regressed evidence.

The drill passes only if this can be completed without reading raw prompts from
Prometheus labels or depending on the telemetry backend as recovery state.
