# Part 09 - Product Surfaces

## Problem Statement

A harness that only exposes a CLI is limited. Engineering agents need both
automation and inspection. The product surfaces therefore evolved into:

- CLI for scripting and direct operation;
- HTTP runtime for API and App Bridge;
- TUI for terminal operation;
- Desktop for richer project workflows.

The architectural problem is keeping these surfaces consistent.

## Design Principle

Product surfaces should be thin projections over shared runtime state.

```text
CLI command
HTTP endpoint
TUI action
Desktop action
  -> shared session/App Bridge/runtime contract
```

If each surface implements its own state model, parity collapses.

## CLI

The CLI now covers:

- run;
- session;
- models;
- agent;
- plugin;
- MCP;
- auth/providers;
- debug/db;
- import/export;
- skills;
- attach;
- approvals/questions and related runtime actions.

The CLI is still the fastest way to verify command contracts. Golden tests
protect output shape.

## HTTP Runtime And App Bridge

HTTP runtime owns:

- sessions;
- turns;
- global and turn events;
- approvals/questions;
- models;
- agents;
- MCP;
- skills;
- task trees;
- checkpoint/diff/restore APIs.

App Bridge is the contract that lets TUI/Desktop operate the same runtime
state as CLI.

## TUI

The TUI has grown from display state into a runtime control surface:

- session picker;
- file picker;
- model picker;
- agent picker;
- variant/thinking picker;
- approval/question docks;
- diff/checkpoint rendering;
- App Bridge attach/control.

Remaining work is mainly around richer panes:

- subagent panes;
- task tree navigation;
- plugin panes;
- command palette;
- configurable keymap.

## Desktop

Desktop is a richer product surface over the same App Bridge:

- workspace shell;
- approval dock;
- MCP panel;
- checkpoint restore workflow;
- history/detail cards;
- packaged app smoke workflows.

The Desktop direction is important because it exposes runtime gaps that CLI
does not reveal, especially around persisted UI state, reload behavior, and
long-running workflows.

## Development Process

Surface development happened in layers:

1. Restore CLI command surface and golden tests.
2. Build HTTP runtime and health/routes.
3. Add App Bridge session and event contracts.
4. Add TUI attach and control routes.
5. Add MCP lifecycle and UI.
6. Add approvals/questions.
7. Add diff/checkpoint/restore.
8. Add Desktop workflow smokes.
9. Add skills and task tree visibility.

Each product surface forced runtime contracts to become more explicit.

## Verification Evidence

Representative commands:

```bash
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
cargo test -p openagent-tui -q
```

Desktop smoke tests provide additional product evidence, especially for MCP,
approval dock, and checkpoint restore workflows.

## Remaining Work

1. Subagent panes and task tree navigation.
2. Attachments beyond local file mentions.
3. Plugin panes and plugin runtime controls.
4. More terminal automation coverage for long-running TUI behavior.
5. Product-level model/agent/provider consistency across all surfaces.
