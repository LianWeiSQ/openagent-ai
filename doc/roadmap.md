# Roadmap

OpenHarness is a usable Rust runtime. The current priority is to deepen shared
runtime contracts that improve Desktop, CLI, and TUI together.

## Stable Foundation

- multi-step provider/tool loop with streaming;
- built-in workspace tools and permission policy;
- durable sessions, messages, parts, events, checkpoints, and turn jobs;
- approval/question pause and resume;
- provider retry, fallback, failure, and manual retry;
- local and remote MCP configuration, lifecycle, discovery, and execution;
- Bridge HTTP/SSE API used by Desktop and TUI;
- CLI, TUI, LSP, swarm, and eval crates;
- restricted Bridge authentication, CORS defaults, and Desktop CSP;
- local P0 acceptance gate covering core, Desktop, and browser smokes.

## Next Priorities

1. Make context assembly fully single-path across CLI and Bridge execution.
2. Harden attachment persistence and context projection for large text files,
   folders, images, and documents.
3. Add durable goal and read-only plan-mode contracts in Rust, then expose them
   in Desktop.
4. Productize task trees, background tasks, subagent navigation, and crash
   recovery without creating client-only state.
5. Improve provider catalog capability metadata and native provider streaming.
6. Complete MCP OAuth/dynamic registration and lifecycle persistence.
7. Add controlled concurrent execution for independent read-only tool calls.
8. Validate packaged Desktop behavior on Windows after the macOS gate remains
   stable.

## Documentation Rule

Update this roadmap when priorities change. Do not create phase receipts or
one-off parity documents; Git history and tests are the implementation record.
