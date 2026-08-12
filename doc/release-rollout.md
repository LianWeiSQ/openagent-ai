# Release, Canary, and Rollback Runbook

OpenHarness releases are promoted by evidence and immutable identity, not by a
green build alone. Each production candidate must retain a content-addressed
`openharness.release_readiness.manifest.v1` containing:

- recomputed passing quality policy, Release Evidence, and decision;
- passing critical fault-injection execution report;
- passing promoted Bad Case regression replay report;
- passing load report and a distinct passing staging soak report;
- p95, Token, and cost baseline identity;
- fingerprints of Collector, Prometheus, Tempo, Loki, Grafana, dashboard, and
  alert assets;
- source revision, candidate VersionIdentity, and immutable `sha256` image
  digest.

The CI-tier manifest exercises the same contract but does not claim production
readiness: it may omit the 30-minute soak and image digest. Production-tier
assembly fails closed when either is missing.

## Preflight

Before sending candidate traffic:

1. Freeze Agent, Prompt, Skill set, Tool set, configuration, dataset, and image
   versions. Do not use mutable image tags.
2. Run the full workspace gates, critical fault plan, regression replay, CI
   load, and staging soak.
3. Validate the observability deployment and confirm dashboard-to-Trace-to-Run
   navigation using one synthetic failure.
4. Confirm the previous image/config bundle is still deployable and its
   session/storage schema can read candidate-created state.
5. Assign release commander, on-call owner, start/end time, rollback authority,
   and incident channel.

Do not combine a runtime rollout with an observability-backend migration or a
new baseline unless each change has an independent canary and rollback point.

## Staged promotion

| Stage | Candidate traffic | Minimum observation | Entry/exit condition |
| --- | ---: | ---: | --- |
| Shadow | 0% user traffic | 30 min | Telemetry/config loads; synthetic Runs, faults, and alerts work |
| Internal | allowlisted users only | 60 min | No critical Bad Case; SLI, p95/p99, Token, cost, queue, and trace completeness within gate |
| Canary | 1% | 60 min and 100 Runs | Version-isolated metrics meet policy; no duplicate effects or permission bypass |
| Ramp 1 | 10% | 2 h and 1,000 Runs | Error-budget burn, degraded ratio, latency, Token/cost, and exporter health stable |
| Ramp 2 | 50% | 4 h | No new high/critical Bad Case; capacity and ledger growth within forecast |
| General | 100% | 24 h heightened watch | Release manifest archived; alerts and on-call handoff complete |

Traffic allocation belongs in the deployment/router layer and must be sticky
for a Run. Never split one Run between Harness versions. Dashboard every stage
by `harness_version`, `agent_version`, and surface so candidate and control are
not averaged together.

## Automatic stop and rollback triggers

Stop promotion immediately when any of these occurs:

- critical safety/privacy/permission failure or duplicated external effect;
- task contract, Tool Set, Run, or Trace fingerprint mismatch;
- successful-or-degraded ratio below the active SLO or critical burn alert;
- degraded ratio above 5%, trace completeness below 95%, or sustained exporter
  loss that prevents diagnosis;
- p95/p99, average Token, or average cost exceeds the accepted plan/baseline;
- queue timeouts, stuck approvals/questions, lease recovery, or storage errors
  materially exceed control;
- a high/critical Bad Case cannot be triaged and owned inside the stage window.

One transient alert can pause a stage for investigation; safety, privacy,
duplicate-effect, contract-integrity, or unrecoverable-state failures trigger
rollback without waiting for a time window.

## Rollback procedure

1. Set candidate allocation to zero and keep existing sticky Runs on their
   current worker unless safety requires cancellation.
2. Stop new work, drain queued candidate jobs, and classify running Runs as
   completed, safely cancellable, or outcome-unknown. Never blindly replay an
   outcome-unknown external mutation.
3. Restore the previous immutable image plus its Agent/Prompt/Skill/Tool/config
   bundle. Disable newly introduced MCP servers/plugins before resuming traffic
   if they are implicated.
4. If only telemetry is failing, set
   `OPENHARNESS_TELEMETRY_ENABLED=false` while retaining durable Run evidence;
   do not disable the Run ledger.
5. Verify health, one read-only synthetic Run, queue recovery, approval resume,
   and idempotency receipts on the previous version.
6. Reopen traffic at the last known-good stage and watch the same isolated
   version panels for at least one observation window.
7. Capture the failed candidate as a Bad Case, attach the release manifest and
   Run/Trace links, then promote a regression fixture before retrying rollout.

Rollback is a configuration/image switch, not deletion. Preserve candidate
session directories, Run contracts, effect receipts, events, checkpoints, load
reports, and telemetry evidence for incident analysis.

## Production manifest

Generate the request from the release pipeline using the actual commit, image
digest, passing report paths, and staging soak path, then seal it:

```bash
cargo run -p openagent-eval --bin openharness-release-bundle -- \
  artifacts/readiness-request.json artifacts/readiness-manifest.json
```

Archive the request, manifest, every referenced report, the exact policy and
baseline, image signature/SBOM, and observability assets. A manifest is valid
only as a set; copying `passed=true` without the referenced fingerprints is not
a release approval.
