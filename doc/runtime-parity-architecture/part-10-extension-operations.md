# Part 10 - Extension And Operations

## Problem Statement

The core runtime is not enough for a full harness. Operators also need:

- plugin registration;
- GitHub and PR workflows;
- debug and DB inspection;
- lifecycle commands;
- eval and replay evidence;
- operational diagnostics.

These capabilities should not be mixed into the core agent loop. They belong
around the runtime as extension and operations surfaces.

## Plugin Layer

Current plugin support includes:

- install/register local, module, and remote entries;
- list/show;
- enable/disable;
- remove;
- manifest-backed dry-run dispatch.

The important current limitation is that plugin `run` is not yet a real plugin
runtime execution path. It is registry/config scaffolding.

The future design should treat plugins as providers of:

- skills;
- MCP servers;
- commands;
- UI panes;
- maybe provider catalog extensions.

## GitHub And PR Helpers

GitHub/PR support currently behaves like local workflow scaffolding:

- status;
- issue;
- PR list/view/checkout/template/review;
- workflow helpers.

OpenCode-level GitHub agent install/run is not complete.

The design direction should avoid hardwiring GitHub behavior into the agent
loop. GitHub should be an extension capability that can supply tools,
commands, or task templates.

## Debug And DB

Debug and DB commands exist to inspect runtime state:

- paths;
- env;
- sessions;
- files;
- search;
- bundle;
- DB path/summary/rebuild/query/schema/export.

OpenCode snapshot-level debug parity is broader. The remaining need is a more
complete snapshot/replay artifact that captures enough state for external
inspection.

## Lifecycle

Upgrade/uninstall commands currently behave as explicit dry-run plans. This is
intentional until packaging owns destructive lifecycle semantics.

The rule should remain:

```text
source-tree harness
  -> no destructive lifecycle mutation
packaged distribution
  -> can own upgrade/uninstall behavior
```

## Operations And Eval

Operations work includes:

- JSONL-friendly observability;
- eval/replay support;
- Terminal-Bench and Harbor adapters;
- Langfuse export paths;
- runtime logs and diagnostics.

These are evidence systems. They should consume session and trace artifacts
instead of adding another state model.

## Development Process

Operations and extension work has followed this pattern:

1. Add explicit command boundary instead of unknown command.
2. Add safe local scaffolding.
3. Add machine-readable output.
4. Add golden or integration tests.
5. Add runtime execution only after the state and permission model is clear.

This is why plugin execution and lifecycle mutation are intentionally not
fully active yet.

## Remaining Work

1. Real plugin runtime execution.
2. Plugin-provided skills and MCP registration.
3. GitHub agent install/run parity.
4. Snapshot-grade debug bundles.
5. Packaging-owned lifecycle commands.
6. More complete operational runbooks for provider/MCP/task failures.
