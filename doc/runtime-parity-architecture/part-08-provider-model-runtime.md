# Part 08 - Provider And Model Runtime

## Problem Statement

Provider support cannot be a single OpenAI-compatible environment variable
path. A real harness needs:

- provider-aware auth;
- provider-specific environment defaults;
- native provider routing where needed;
- model listing and catalog behavior;
- health diagnostics;
- payload isolation;
- redaction;
- stable test contracts.

The most important architectural requirement is boundary control. Provider
wire format should not leak into runtime logic, and runtime-only config should
not leak into provider payloads.

## Reference Direction

OpenCode's provider layer includes provider login, list, logout, methods,
model listing, and provider-specific behavior. The design lesson is that
providers are part of the operator experience, not only a model API endpoint.

OpenHarness adopts that direction while keeping the provider boundary explicit:

```text
runtime profile/config
  -> provider resolver
  -> normalized provider config
  -> provider payload builder
  -> normalized stream events
```

## Current Capabilities

### Provider-Aware Auth

The CLI supports `auth` and `providers` flows:

- login;
- list;
- methods;
- logout;
- provider-specific env metadata;
- auth-file routing;
- redaction.

### Model Runtime

Model listing supports:

- provider filtering;
- refresh;
- offline/catalog mode;
- verbose capability metadata;
- cache TTL;
- snapshot fallback.

### Native Provider Routing

Native provider support exists for Anthropic-style routing without forcing
every provider through OpenAI-compatible `/models` assumptions.

### HTTP Provider Health

HTTP runtime can expose provider health and model diagnostics without leaking
secrets.

## Payload Boundary

Provider payloads should include:

- messages;
- model;
- tools;
- allowed provider options;
- temperature/top_p or equivalent model parameters.

Provider payloads should not include:

- `skills`;
- `skill_roots`;
- `skill_permissions`;
- `task_permissions`;
- `permission`;
- workspace isolation;
- hidden/disabled flags;
- runtime-only metadata.

The shared `AgentProfileSchema` stage exists partly to enforce this boundary
consistently across CLI and HTTP.

## Development Process

Provider work evolved through the following sequence:

1. OpenAI-compatible provider path.
2. Streaming normalization.
3. CLI auth/provider commands.
4. Provider-specific environment defaults.
5. Auth-file runtime routing.
6. Models catalog/cache/verbose output.
7. Native provider routing.
8. HTTP provider diagnostics.
9. Shared profile schema to prevent provider payload leaks.

This sequence reflects a shift from "call a model" to "operate multiple model
providers safely".

## Verification Evidence

Representative commands:

```bash
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
```

Important checks include:

- provider-specific env/model behavior;
- auth-file provider routing;
- native provider diagnostics;
- model catalog output;
- no provider payload leakage from skill/task config.

## Remaining Work

1. Well-known provider URL login.
2. Fuller provider catalog and login model.
3. More native providers.
4. Better provider capability mapping.
5. Better operator diagnostics for provider-specific failures.
6. Provider selection in product surfaces tied more tightly to session state.
