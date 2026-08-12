# Quality, Tool Governance, and Bad-Case Loop

OpenHarness treats operational health, product quality, and security as three
separate release signals. A run can be operationally successful but produce a
bad answer, or complete with a degraded outcome after provider fallback. The
release gate therefore consumes versioned eval evidence rather than inferring
quality from HTTP status or Grafana availability.

## Release evidence flow

```text
versioned eval suite
        |
        v
case results + run_id + trace_id + task-contract fingerprint
        |
        +---- baseline comparison (latency / tokens / cost)
        |
        +---- privacy and trace-completeness checks
        v
openharness.quality_gate.evidence.v1
        |
        v
eval/policies/release-v1.json
        |
        v
decision.json + exit 0 (pass) / 2 (policy fail) / 1 (invalid invocation)
```

Every candidate identifies the Harness, Agent, Prompt, Skill set, Tool set,
configuration fingerprint, eval dataset version, and dataset fingerprint.
Every case links back to one durable `run_id`, one W3C `trace_id`, and the
fingerprint of the task contract used to execute it. Missing evidence is a gate
failure, not an implicit pass.

The checked-in release policy is
`eval/policies/release-v1.json`. Its bootstrap thresholds are:

| Signal | Gate |
| --- | --- |
| Case success rate | at least 95% |
| Degraded case rate | at most 5% |
| Mean trace completeness | at least 95% |
| Privacy violations | zero |
| Critical case failures | zero |
| Status/budget regressions | zero |
| p95 duration increase | at most 20% versus baseline |
| Total token/cost increase | at most 15% versus baseline |

Run the machine gate after producing the candidate evidence:

```bash
cargo run -p openagent-eval --bin openharness-evidence -- \
  artifacts/release/evidence-request.json \
  artifacts/release/evidence.json
cargo run -p openagent-eval --bin openharness-quality-gate -- \
  eval/policies/release-v1.json \
  artifacts/release/evidence.json \
  artifacts/release/decision.json
```

The Evidence Assembler reads the versioned eval, baseline, regression, privacy,
dataset manifest, and durable Run/Trace artifacts. It rejects mixed candidate
versions, duplicate Run links, incomplete critical spans, case-set drift, and
missing privacy evidence. Input report fingerprints are retained as evidence
provenance.

The policy should be tuned from representative workload data. Do not loosen a
threshold in the same change that causes its regression without an explicit
risk decision and a new policy identifier.

## Tool governance

`GET /api/tool-governance` returns
`openharness.tool_governance.v1`, including a deterministic Tool-set
fingerprint, risk classification, execution schema, and default action for
FULL, READONLY, PLAN_ONLY, and NONE. The manifest fails audit when, for
example, a workspace/external mutator is not marked dangerous, READONLY allows
a state-changing tool, or PLAN_ONLY silently allows a dangerous tool.

Dynamic MCP catalogs are registered atomically through the same audit. An MCP
tool with an explicit read-only annotation is classified as external read;
missing safety annotations are conservatively treated as privileged external
mutation. A rejected batch exposes no partial catalog and cannot replace an
existing tool identifier. Each Task Contract fingerprints the final
profile/capability-filtered catalog after MCP and plugin skill discovery; the
runtime recomputes it immediately before execution and fails closed on drift.

Risk tiers are intentionally small and stable:

| Tier | Meaning | Typical examples |
| --- | --- | --- |
| `read_only` | local read without external effects | read, grep, LSP |
| `external_read` | read-only host or network access | web fetch, host search |
| `mutating` | session or workspace state change | write, edit, todo, question |
| `privileged` | external mutation, shell, or dangerous host access | bash, privileged MCP |

Every tool result carries a governance receipt with schema version, risk tier,
requested action, effective action, enforcement flag, approval-bypass flag,
group, and execution scope. The same bounded fields are copied to the
`tool.execute` trace event. Raw inputs and permission patterns are not metric
labels.

`dangerously_skip_permissions=true` only turns an `ask` decision into an
audited allow. It never overrides an explicit deny. Production entry points
should disable it; if a controlled automation enables it, alert on
`governance_approval_bypassed=true` in trace/log evidence.

## Bad-case lifecycle

Bad cases use a monotonic lifecycle:

```text
captured -> triaged -> fixture_ready -> fixed -> verified -> promoted
       \----------------------------------------------------> rejected
```

Capture a completed, failed, or degraded run through the Bridge:

```bash
curl -X POST "$BRIDGE/api/turns/$RUN_ID/bad-cases" \
  -H "Authorization: Bearer $BRIDGE_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{
    "title": "deadline allowed a provider request",
    "category": "deadline_or_budget",
    "severity": "high",
    "owner": "runtime-team",
    "expected_assertions": [{
      "kind": "json_path",
      "path": "$.reason_code",
      "operator": "eq",
      "expected": "deadline_exceeded"
    }],
    "tags": ["runtime", "deadline"]
  }'
```

The capture operation derives source versions, Run/Trace IDs, outcome and
reason from the durable run record. Sensitive keys and recognizable bearer,
OpenAI-style, and AWS access tokens are redacted recursively before storage.
The artifact is content-addressed; tampering or residual secret material makes
validation fail.

Operators can list and advance records with:

- `GET /api/bad-cases` and `GET /api/bad-cases/{id}`;
- `POST /api/bad-cases/{id}/transition` with `state`, `owner`, and `note`;
- `POST /api/bad-cases/{id}/promote` with `fixture_id`, `dataset_version`, and
  `owner` after verification.

Artifacts live below `<session-root>/.openagent-runtime/bad-cases/`. Promoted
fixtures are staged below
`<session-root>/.openagent-runtime/regression-fixtures/<dataset-version>/`.
Promotion requires at least one deterministic assertion. Review the staged
fixture, then intentionally copy it into the versioned eval dataset; the
runtime never commits it to source control automatically.

The same flow is available offline:

```bash
cargo run -p openagent-eval --bin openharness-bad-case -- capture capture.json bad-case.json
cargo run -p openagent-eval --bin openharness-bad-case -- validate bad-case.json
cargo run -p openagent-eval --bin openharness-bad-case -- transition \
  bad-case.json triaged runtime-team triaged.json "classified"
cargo run -p openagent-eval --bin openharness-bad-case -- promote \
  verified.json runtime.deadline.case-42 release-suite-v4 eval-team \
  promoted.json fixture.json
```

Promoted fixtures are indexed and replayed with:

```bash
cargo run -p openagent-eval --bin openharness-regression -- \
  index release-regressions release-suite-v4 dataset.json fixtures/*.json
cargo run -p openagent-eval --bin openharness-regression -- validate dataset.json
cargo run -p openagent-eval --bin openharness-regression -- \
  replay-bridge dataset.json "$BRIDGE" "$WORKSPACE" replay-report.json \
  OPENHARNESS_BRIDGE_TOKEN
```

For hermetic CI, `replay-observations` evaluates the same assertions against a
checked observation set. Bridge mode creates an isolated session per fixture,
executes the sanitized replay payload, and records only fingerprints,
assertions, and correlation IDs.

## Release acceptance

A release is acceptable only when all of the following are true:

1. Workspace format, Clippy, unit, HTTP integration, eval, and telemetry tests
   pass.
2. The checked fault-injection plan covers every critical policy case and its
   exact tests pass.
3. The candidate quality decision is `pass` and its evidence fingerprint is
   retained with release artifacts.
4. Tool governance returns `passed=true` for the deployed Tool set.
5. Grafana/Prometheus rules are provisioned, scrape authentication works, and
   an on-call engineer can pivot from an alert to Trace and durable Run.
6. Any newly found high/critical bad case is either promoted into the candidate
   dataset or explicitly rejected with an owner and reason.
7. The content-addressed readiness manifest seals quality, fault, regression,
   load/soak, and observability evidence before staged promotion. See
   `doc/release-rollout.md`.
