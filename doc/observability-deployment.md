# OpenHarness Observability Deployment

The checked bundle under `observability/deploy/` is a single-node acceptance
and small-production topology for OpenHarness. It provides an OTLP gateway,
Prometheus, Tempo, Loki, and provisioned Grafana. Durable Run artifacts remain
the source of truth; this stack is disposable and rebuildable.

## Data flow

```text
OpenHarness -- OTLP/HTTP traces --> Collector --> Tempo
OpenHarness -- GET /metrics -----> Prometheus --> Grafana
events.jsonl -- filelog receiver -> Collector --> Loki
OTLP logs ------------------------> Collector --> Loki
Grafana <--------- Prometheus + Tempo + Loki
```

The Collector deletes prompt, completion, authorization-header, and end-user
attributes before export. Durable ledger JSON is tailed read-only. `trace_id`
and `span_id` remain fields/structured metadata and are never configured as
Loki labels. Grafana provisions log-to-trace navigation through a derived
field.

## Start the acceptance stack

From `observability/deploy/`:

```bash
cp .env.example .env
cp secrets/metrics-token.example secrets/metrics-token
cp secrets/grafana-admin-password.example secrets/grafana-admin-password
chmod 600 secrets/metrics-token secrets/grafana-admin-password
docker compose config --quiet
docker compose up -d
```

Replace both example secret values before starting. `metrics-token` must equal
the Bridge bearer token. Set `OPENHARNESS_SESSION_ROOT` to the absolute durable
session root when ledger log ingestion is required. The checked default points
at an empty local acceptance directory.

Configure a host-run Bridge with:

```dotenv
OPENHARNESS_PROMETHEUS_ENABLED=true
OPENHARNESS_TELEMETRY_ENABLED=true
OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318
OTEL_SERVICE_NAME=openharness-http
OPENHARNESS_ENVIRONMENT=staging
```

The Prometheus template scrapes `host.docker.internal:8787`. Change this target
when the Bridge is another Compose/Kubernetes service. In that topology, join
the service to a private telemetry network and use
`http://otel-collector:4318` for OTLP.

Open these loopback-only endpoints after startup:

- Grafana: `http://127.0.0.1:3000`
- Prometheus: `http://127.0.0.1:9090`
- OTLP/gRPC: `127.0.0.1:4317`
- OTLP/HTTP: `127.0.0.1:4318`

## Acceptance checks

```bash
curl -fsS -H "Authorization: Bearer $(cat secrets/metrics-token)" \
  http://127.0.0.1:8787/metrics | grep openharness_runs_total
curl -fsS http://127.0.0.1:9090/-/ready
curl -fsS http://127.0.0.1:3000/api/health
docker compose logs --since=5m otel-collector
```

Then execute one deterministic Agent run and verify:

1. `openharness_runs_total` increases in Prometheus;
2. the OpenHarness Runtime & SLO dashboard shows the run;
3. Tempo can find the Run `trace_id`;
4. Loki contains the matching durable event and its TraceID link opens Tempo;
5. stopping the Collector does not fail an Agent run, while exporter-failure
   health/metrics become non-zero after it returns.

## Production security boundary

The Compose stack is deliberately bound to `127.0.0.1`, uses an internal
network, disables anonymous Grafana access, loads credentials from files, pins
component versions, mounts configs read-only, and disables Grafana/Loki usage
reporting. It is not an internet-facing authentication layer.

For multi-host production:

- terminate TLS/mTLS and authenticate OTLP at an ingress or service mesh;
- put Prometheus, Tempo, Loki, and Collector APIs on private networks;
- replace local Tempo/Loki filesystem storage with replicated object storage;
- source secrets from the platform secret manager, not bind-mounted files;
- configure backups and retention as explicit data policies;
- verify image signatures/digests and scan them before promotion;
- canary Collector and backend upgrades separately from the Agent runtime;
- keep the Run ledger even if telemetry export is unavailable.

The versions in `.env.example` are compatibility pins, not an instruction to
auto-upgrade. Review upstream migration/security notes, update one component at
a time, run the checks above, then record the accepted image digests in the
release evidence.
