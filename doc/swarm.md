# Swarm Runtime

`openagent-swarm` is the Rust runner-orchestration crate. It coordinates agents
through the shared protocol without making OpenAgent the only possible worker.

## Contract

An `AgentRunner` exposes an `AgentDescriptor` and starts work from an
`AgentSpec` plus `RunContext`. It returns normalized events and an
`AgentResult`. The registry can select a runner by id or supported role.

Implemented runner types include:

- in-process Rust function handlers;
- subprocess commands;
- HTTP endpoints;
- A2A-compatible HTTP endpoints.

`FanoutBudget` and run limits bound concurrency, depth, timeout, and resource
use. Runner-specific transport details stay outside the coordinator.

## CLI

The CLI loads a YAML configuration and runs one declared task:

```bash
cargo run -p openagent-swarm --bin openagent-swarm -- \
  run swarm.yaml \
  --task compare \
  --run-id compare-demo \
  --pretty
```

The result is JSON. Exit code `0` represents `completed` or `partial`; failed
runs return a nonzero status.

## Boundaries

- Swarm owns coordination, runner selection, fanout limits, and normalized
  results.
- Individual agents own model prompts, tools, permissions, and workspace
  behavior.
- Durable session/task presentation belongs to the Bridge and product clients.
- New transports should implement `AgentRunner` rather than branching the core
  agent loop.
