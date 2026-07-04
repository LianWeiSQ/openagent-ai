# Part 07 - MCP Capability Plane

## Problem Statement

MCP is not just another tool implementation. It introduces external capability
servers with their own configuration, authentication, lifecycle, diagnostics,
and execution contracts.

The full requirement is:

```text
config -> auth -> discovery -> lifecycle -> tool execution -> diagnostics -> UI
```

If only execution is implemented, operators cannot trust or manage MCP in a
long-running harness.

## Reference Direction

OpenCode treats MCP as a first-class capability domain. The useful reference
points are:

- CLI commands for MCP configuration and auth;
- debug/doctor workflows;
- remote OAuth and dynamic client registration;
- TUI/App visibility into server and tool state;
- tool execution through the normal provider loop.

OpenHarness has followed the same shape but with a local harness bias:
explicit files, redaction, App Bridge lifecycle control, and reproducible
tests.

## Current Architecture

MCP lives in the capability plane and is coordinated through App Bridge where
lifecycle is involved.

```text
CLI / TUI / Desktop
  -> MCP config/auth APIs
  -> App Bridge MCP lifecycle registry
  -> MCP discovery
  -> provider loop tool execution
  -> session/tool events
```

This avoids each surface maintaining a separate MCP runtime.

## Implemented Capabilities

### Config

The harness supports local and remote MCP configuration. Config handling
includes:

- server records;
- transport selection;
- timeout settings;
- headers;
- enabled/disabled state;
- redacted output for secrets and tokens.

### Auth

Current auth support includes:

- token storage/status;
- logout;
- redacted display;
- config-aware diagnostics.

Browser OAuth and dynamic client registration remain open parity work.

### Discovery

MCP discovery produces tool descriptors that can be registered into the
runtime tool registry. Discovery results are also usable for doctor/debug
commands and UI state.

### Lifecycle

Lifecycle control belongs to App Bridge:

- start;
- stop;
- restart;
- enable;
- disable;
- test.

This matters because Desktop and TUI need to see the same running server state
as CLI.

### Tool Execution

Once discovered, MCP tools enter the normal provider/tool call path. They are
subject to the same broad runtime concerns:

- permission;
- trace;
- tool result normalization;
- session event persistence;
- provider-loop continuation.

## Product Surface Integration

MCP appears in:

- CLI commands;
- HTTP/App Bridge endpoints;
- TUI controls;
- Desktop MCP panel;
- smoke tests.

The important design point is that UI surfaces do not create MCP state. They
read and operate on App Bridge MCP state.

## Development Process

MCP work progressed in phases:

1. File-based config and CLI command shape.
2. Auth token and redaction.
3. Discovery and doctor/debug.
4. Provider-loop MCP tool execution.
5. App Bridge lifecycle registry.
6. TUI remote MCP commands.
7. Desktop MCP panel and smoke coverage.

That order made the runtime increasingly operational. Each stage added a
missing operator concern rather than only expanding execution.

## Verification Evidence

Representative coverage:

```bash
cargo test -p openagent-cli --test cli_commands -q
cargo test -p openagent-http-runtime --test http_runtime -q
```

Desktop MCP UI smoke tests also cover product behavior, but those are separate
from the core MCP contract.

## Remaining Work

1. Browser OAuth flow.
2. Dynamic client registration.
3. Cross-restart lifecycle state recovery.
4. Better MCP picker/panel behavior in TUI.
5. Plugin-provided MCP server registration.
6. More complete remote error classification and retry behavior.
