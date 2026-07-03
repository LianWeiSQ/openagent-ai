# Project Progress

> Append new session notes at the top. Use `tasks.json` as the active task queue and `doc/maintenance.md` as the human-readable issue report.

---

## 2026-07-03 HTTP Runtime Choke Point Split

Changed:
- Frozen mainline to Rust Desktop Agentic Coding Workspace.
- Split `runtime/http/src/http_runtime.rs` into `app_bridge_routes.rs`, `mcp_runtime.rs`, and `turn_runtime.rs`.
- Kept behavior unchanged; only adjusted `pub(super)` boundaries needed by existing routes/tests.

Verified:
- `cargo fmt --all -- --check`
- `cargo check -p openagent-http-runtime`
- `cargo test -p openagent-http-runtime app_bridge_mcp_local_stdio_lifecycle_start_stop_restart --lib -- --nocapture`
- `cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint --lib -- --nocapture`

Next:
- Do a user-visible big loop: Desktop MCP panel or approval dock.

## 2026-07-03 CLI MCP Local Test And App Bridge Lifecycle Guard Slice

Product alignment:

- 收口 CLI standalone MCP 语义：本地 config 可以做一次性连通测试，但真正的 start/stop/restart/enable/disable lifecycle 必须走 Rust App Bridge registry。
- 本轮不推 GitHub、不做 Python、不实现 CLI 自己的常驻进程池、不做跨 App Bridge 重启恢复；重点避免 CLI lifecycle 命令误解析本地 config 或给用户造成“CLI 进程退出后还能保活 local MCP”的错觉。

Implemented:

- `cli/src/util.rs`
  - `mcp_config_path(...)` 同时支持 `--mcp-config` 与 `--config`，与 `run --mcp-config` 主路径对齐。
- `cli/src/mcp.rs`
  - 新增本地一次性 `openagent mcp test <server> --mcp-config <file>`：读取本地 MCP config，执行一次 `discover_mcp_server_tools` / `tools/list`，返回脱敏 snapshot、`selected_transport`、`tool_count` 和 tools。
  - `mcp start|stop|restart|enable|disable` 如果带本地 `--mcp-config`/`--config`/`--workspace`/`--dir`，会在发网络请求前失败，并提示先启动 Rust App Bridge 后用 `--server-url`/`--server-token`。
  - 显式 remote 模式同时传本地 config/workspace flag 会失败，避免客户端悄悄忽略本地配置。
  - Remote positional parser 增加本地 config/workspace flags，防止配置路径被误当成 server name。
  - 其他 mcp 子命令的 positional parser 同步认识 `--mcp-config`。
- `cli/src/help.rs`
  - MCP help 拆成 `local one-shot test` 与 `App Bridge lifecycle` 两条路径。
- `cli/tests/cli_commands.rs`
  - 新增 `binary_mcp_test_uses_local_mcp_config_alias_once`。
  - 新增 `binary_mcp_lifecycle_rejects_local_config_without_app_bridge`。

Verification:

```bash
cargo test -p openagent-cli binary_mcp_test_uses_local_mcp_config_alias_once --test cli_commands -- --nocapture
cargo test -p openagent-cli binary_mcp_lifecycle_rejects_local_config_without_app_bridge --test cli_commands -- --nocapture
cargo test -p openagent-cli binary_mcp_remote_lifecycle_controls_app_bridge --test cli_commands -- --nocapture
cargo test -p openagent-cli binary_help_smoke_covers_legacy_command_surface --test cli_commands -- --nocapture
cargo fmt --all -- --check
cargo check -p openagent-cli
git diff --check -- cli/src/mcp.rs cli/src/util.rs cli/src/help.rs cli/tests/cli_commands.rs .goal/state.md progress.md
rg -n "[ \t]+$|\t" cli/src/mcp.rs cli/src/util.rs cli/src/help.rs cli/tests/cli_commands.rs .goal/state.md progress.md || true
pgrep -fl 'openagent-http-runtime|openagent serve|stdio_mcp_server|python3 .*stdio_mcp_server|vite --host 127.0.0.1' || true
```

Evidence:

- Local one-shot test 通过：`mcp test local-tools --mcp-config <file> --workspace <dir> --format json` 对临时 disabled stdio MCP server 返回 `ok:true`、`status:connected`、`selected_transport:stdio`、`tool_count:1`。
- Lifecycle guard 通过：`mcp start local-tools --mcp-config <file>` 直接失败并提示 `App Bridge lifecycle registry` / `--server-url <url>`，且不出现 `Connection refused`。
- Remote lifecycle 回归仍通过：真实 `openagent serve` 下 `list -> start -> enable -> test -> stop` 保持 App Bridge lifecycle 语义。
- Help smoke、format、`cargo check`、diff hygiene、trailing whitespace/tab 检查通过；无 openagent runtime / stdio MCP / Vite 残留进程。

Residual risk:

- CLI 不提供自己的常驻 local MCP lifecycle registry，这是有意保持 App Bridge-first。
- App Bridge lifecycle registry 仍是进程内状态，不跨 runtime 重启恢复。
- TUI `/mcp` 还没有 picker/side panel，仍是 slash command。

## 2026-07-03 TUI Remote MCP Lifecycle Controls Via App Bridge Slice

Product alignment:

- 把上一段 CLI remote MCP lifecycle 控制推进到 TUI 接入层，让 CLI/TUI/Desktop 更接近共用同一 Rust App Bridge MCP API。
- 本轮不推 GitHub、不做 Python、不做 Desktop UI 改动、不做跨 App Bridge 重启持久化；只完成 TUI slash command 到 App Bridge MCP lifecycle 的可验证闭环。

Implemented:

- `runtime/tui/src/commands.rs`
  - `/help` 增加 `/mcp [list|show|doctor|test|start|stop|restart|enable|disable]`。
- `runtime/tui/src/server.rs`
  - `AppBridgeTerminalHandler` 新增 `/mcp` command branch。
  - `/mcp` / `/mcp list` / `/mcp doctor` 调 `RemoteRuntimeClient::mcp_status(...)`。
  - `/mcp show <server>` 展示单 server 状态和 tools。
  - `/mcp test <server>` 调 App Bridge `/api/mcp/servers/{name}/test`。
  - `/mcp start|stop|restart <server>` 调 App Bridge lifecycle routes。
  - `/mcp enable|disable <server>` 调 App Bridge PATCH server config。
  - Timeline 输出 enabled/status/transport/tool_count/lifecycle_status/PID/endpoint。
- `runtime/tui/src/tests.rs`
  - Fake App Bridge 新增 `/api/mcp`、`/api/mcp/servers/local-tools/start|stop|restart|test`、`PATCH /api/mcp/servers/local-tools`。
  - 新增 `app_bridge_terminal_mcp_lifecycle_controls_remote_bridge`，走真实 TUI keyflow：`/mcp` -> `/mcp start local-tools` -> `/mcp enable local-tools` -> `/mcp test local-tools` -> `/mcp show local-tools` -> `/mcp stop local-tools`。
- `tests/golden/rust_rewrite/app_bridge_tui.json`
  - 同步 TUI help golden 中新增的 `/mcp` 行。

Verification:

```bash
cargo test -p openagent-tui tui_control_matches_legacy_oracle --test tui_control -- --nocapture
cargo test -p openagent-tui app_bridge_terminal_mcp_lifecycle_controls_remote_bridge --lib -- --nocapture
cargo fmt --all -- --check
cargo test -p openagent-tui --lib -- --nocapture
cargo check -p openagent-tui -p openagent-app-server-client
git diff --check -- runtime/tui/src/commands.rs runtime/tui/src/server.rs runtime/tui/src/tests.rs tests/golden/rust_rewrite/app_bridge_tui.json .goal/state.md progress.md
rg -n "[ \t]+$|\t" runtime/tui/src/commands.rs runtime/tui/src/server.rs runtime/tui/src/tests.rs tests/golden/rust_rewrite/app_bridge_tui.json .goal/state.md progress.md || true
```

Evidence:

- TUI keyflow test 通过：timeline 显示 `local-tools enabled=no`、`lifecycle=stopped`、start 后 `lifecycle=running` / `pid=4242`、enable 后 `enabled=yes`、test 后 `tools=1`、show 后 `tools: stdio_echo`、stop 后 `lifecycle=stopped`。
- Fake App Bridge request assertions 覆盖 `GET /api/mcp`、`POST /api/mcp/servers/local-tools/start`、`PATCH /api/mcp/servers/local-tools`、`POST /api/mcp/servers/local-tools/test`、`POST /api/mcp/servers/local-tools/stop`。
- TUI lib 52 个 tests 全部通过；TUI golden 通过；`cargo check`、format、diff hygiene、trailing whitespace/tab 检查通过。

Residual risk:

- TUI 现在有 `/mcp` slash command，但还不是完整 MCP picker/side panel。
- CLI standalone `--mcp-config` direct stdio path 仍未共享 App Bridge lifecycle registry。
- MCP lifecycle registry 仍是 App Bridge 进程内状态，不跨 runtime 重启。

## 2026-07-03 CLI Remote MCP Lifecycle Controls Via App Bridge Slice

Product alignment:

- 把 MCP lifecycle 控制从 Desktop/UI 与 packaged workflow 继续推进到 CLI/TUI/Desktop 共用的 Rust App Bridge client 路径。
- 本轮不推 GitHub、不做 Python runtime、不做 Desktop UI 改动；重点让 `openagent mcp ... --server-url/--attach` 能控制远端/App Bridge MCP lifecycle。

Implemented:

- `runtime/app-server-client/src/app_bridge_client.rs`
  - `RemoteRuntimeClient` 新增 `mcp_status(refresh)`、`mcp_server_test(name)`、`mcp_server_lifecycle(name,start|stop|restart)`、`mcp_server_update(name, body)`。
  - `app_bridge_client_fixture` 与 `tests/golden/rust_rewrite/app_bridge_tui.json` 记录 MCP status/start/test/enable request shapes。
- `cli/src/mcp.rs`
  - `openagent mcp list/show/doctor/test/start/stop/restart/enable/disable` 支持远端 App Bridge 模式。
  - 当命令带 `--server-url`/`--attach`，或使用 lifecycle-only 命令时，复用 `RemoteRuntimeClient` 访问 `/api/mcp` 与 `/api/mcp/servers/{name}/...`。
  - JSON 模式保留完整 App Bridge payload 并补 `remote/server_url`；text 模式展示远端 MCP 状态表。
- `cli/src/remote.rs`
  - 暴露内部 `app_bridge_client` helper 供 MCP 命令复用统一 bearer/basic auth。
- `cli/src/help.rs`
  - MCP help 增加远端 App Bridge lifecycle 命令提示。
- `cli/tests/cli_commands.rs`
  - 新增真实 `openagent serve` 集成测试，覆盖 disabled local stdio MCP server 的 list/start/enable/test/stop 远端控制闭环。

Verification:

```bash
cargo fmt --all -- --check
cargo test -p openagent-app-server-client app_bridge_client_matches_legacy_oracle --test remote_runtime -- --nocapture
cargo test -p openagent-cli binary_mcp_remote_lifecycle_controls_app_bridge --test cli_commands -- --nocapture
cargo test -p openagent-cli binary_help_smoke_covers_legacy_command_surface --test cli_commands -- --nocapture
cargo test -p openagent-app-server-client --lib -- --nocapture
cargo check -p openagent-app-server-client -p openagent-cli
git diff --check -- runtime/app-server-client/src/app_bridge_client.rs cli/src/mcp.rs cli/src/remote.rs cli/src/help.rs cli/tests/cli_commands.rs tests/golden/rust_rewrite/app_bridge_tui.json .goal/state.md progress.md
rg -n "[ \t]+$|\t" runtime/app-server-client/src/app_bridge_client.rs cli/src/mcp.rs cli/src/remote.rs cli/src/help.rs cli/tests/cli_commands.rs tests/golden/rust_rewrite/app_bridge_tui.json .goal/state.md progress.md || true
```

Evidence:

- App Bridge client golden test 通过。
- CLI remote MCP lifecycle 集成测试通过：真实 `openagent serve` 下，`mcp list` 看到 `local-tools` stopped/disabled；`mcp start local-tools` 启动 lifecycle；`mcp enable local-tools` 保持同一 PID；`mcp test local-tools` 发现 1 个 stdio tool 且仍复用同一 PID；`mcp stop local-tools` 正常停止。
- CLI help smoke、app-server-client lib tests、`cargo check`、format/diff/trailing whitespace 检查通过。
- 无本轮 openagent runtime / stdio MCP / Vite 残留进程。

Residual risk:

- CLI 已能控制 App Bridge MCP lifecycle，但 standalone `--mcp-config` direct stdio path 仍是短连接，还未共享 App Bridge 进程内 lifecycle registry。
- TUI 还没有 interactive `/mcp start` / `/mcp stop` slash command，只补了 CLI command surface 和 shared client。
- Lifecycle registry 仍是 App Bridge 进程内状态，不跨 runtime 重启。

## 2026-07-03 Desktop Real Local MCP Lifecycle UI Smoke Slice

Product alignment:

- 把 local stdio MCP lifecycle 从 packaged API/workflow smoke 继续推进到真实 Desktop UI 路径。
- 本轮不推 GitHub、不做跨 App Bridge 重启持久化；重点验证用户可在 Desktop MCP 面板 Start/Enable local MCP server，并在 timeline/Latest call 中看到真实 MCP tool call 的 lifecycle reused/PID。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 修复 `mcp_server_fingerprint`：lifecycle fingerprint 现在忽略 `enabled` 开关，因为 enabled 不属于本地 stdio 子进程启动配置。
  - 新增 `app_bridge_mcp_lifecycle_survives_enable_toggle` 回归：workspace 默认 `.openagent/mcp.json` 中 local server 初始 disabled，`GET /api/mcp?refresh=true` 不启动进程，`POST /start` 启动 lifecycle，`PATCH enabled:true` 不让 lifecycle stale，随后 direct MCP tool call 仍复用同 PID。
- `desktop/scripts/smoke-local-mcp-ui.mjs`
  - 新增真实 Desktop UI smoke：启动 Rust App Bridge + Vite Desktop + headless Chrome。
  - 临时 workspace 写入 disabled local stdio MCP config 和 fake MCP stdio server。
  - 浏览器打开 Desktop details，点击 `Start MCP server local-tools`，再点击 `Enable MCP server local-tools`。
  - 通过 App Bridge direct `tool_call` 调用 `mcp_tool_local_tools_stdio_echo`，等待 timeline `[data-testid="mcp-tool-card"]` 和右侧 `[data-testid="mcp-latest-call"]` 显示输出、lifecycle reused 和 PID。
  - 读取 fake MCP request log，验证全程只有一个 stdio PID。
- `desktop/package.json`
  - 新增 `smoke:local-mcp-ui`。

Verification:

```bash
node --check desktop/scripts/smoke-local-mcp-ui.mjs
npm --prefix desktop run build
cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture
npm --prefix desktop run smoke:local-mcp-ui
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs .goal/state.md progress.md
rg -n "[ \t]+$|\t" runtime/http/src/http_runtime.rs .goal/state.md progress.md desktop/scripts/smoke-local-mcp-ui.mjs desktop/package.json || true
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-local-mcp-ui|desktop-local-mcp-ui|vite --host 127.0.0.1' || true
```

Evidence:

- App Bridge MCP 9 个目标测试通过。
- Desktop production build 通过。
- `npm --prefix desktop run smoke:local-mcp-ui` 通过：UI 路径 Start -> Enable -> MCP tool card -> Latest call 完整闭环；输出 `desktop stdio echo: ui-lifecycle`，`lifecycle_pid=22264`，`request_pid_count=1`，request methods 为 `initialize` / `notifications/initialized` / `tools/list` / `tools/list` / `tools/call` / `shutdown` / `exit`。
- 截图 artifact：`/var/folders/6h/_xdqdq9177lcf_s0lf4kt8440000gn/T/openagent-desktop-local-mcp-ui-1783044648899.png`。
- format/diff/trailing whitespace 检查通过；无本轮 runtime/Vite/MCP 残留进程。

Residual risk:

- 这是 Vite/Desktop UI + external Rust App Bridge smoke，不是 packaged Tauri GUI click smoke。
- Lifecycle registry 仍是 App Bridge 进程内状态，不跨 runtime 重启。
- CLI standalone MCP path 仍未复用 Desktop/App Bridge lifecycle。

## 2026-07-03 Packaged Tauri Local MCP Lifecycle Workflow Smoke Slice

Product alignment:

- 把 local stdio MCP lifecycle 从“后端 API + Desktop 可视化”推进到真实 packaged `OpenAgent.app` 的 workflow 验收。
- 本轮不推 GitHub、不做跨进程重启持久化；重点验证 bundled Rust App Bridge 在 macOS `.app` 形态下能启动 local MCP server、注册工具、执行工具、持久化消息，并证明整条链路复用同一个 stdio PID。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `mcp_config_source` 拆出 `mcp_config_source_for_workspace`，让 Agent Loop MCP 注册使用和 `/api/mcp` 一致的配置来源。
  - `register_runtime_mcp_tools` 支持 session workspace 默认 `.openagent/mcp.json`，避免 packaged workflow 中 `/api/mcp` 能看到配置但 Agent Loop 注册不到工具。
  - `refresh_mcp_manager_server_tools` 对 local stdio server 优先复用 running lifecycle session，避免 Desktop/UI 背景 refresh 或手动 refresh 再短启动 discovery。
- `desktop/scripts/smoke-packaged-app.mjs`
  - 新增 `local-mcp-lifecycle` workflow：临时生成 workspace `.openagent/mcp.json` 和 fake stdio MCP server。
  - workflow 会调用 `/api/mcp/servers/local-tools/start`，再通过 turn `tool_call` 调用 `mcp_tool_local_tools_stdio_echo`。
  - 断言 turn event 与 persisted `messages_v2` 都包含 MCP tool 输出，metadata 标记 `mcp_lifecycle_reused=true` 且 PID 一致。
  - 读取 fake MCP request log，验证 initialize/tools/list/tools/call/shutdown 全部来自同一个 stdio PID。
- `desktop/package.json`
  - 新增 `smoke:packaged-app:local-mcp-lifecycle`。

Verification:

```bash
cargo fmt --all -- --check
cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture
npm --prefix desktop run build
npm --prefix desktop run tauri -- build --bundles app
node desktop/scripts/smoke-packaged-app.mjs --launch=direct --workflow=local-mcp-lifecycle
npm --prefix desktop run smoke:packaged-app:local-mcp-lifecycle
git diff --check -- runtime/http/src/http_runtime.rs .goal/state.md progress.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|packaged-local-mcp' || true
```

Evidence:

- App Bridge MCP 8 个目标测试通过。
- Desktop production build 通过，Tauri macOS `OpenAgent.app` 重新打包成功。
- Direct packaged smoke 通过：`output` 为 `packaged stdio echo: packaged-lifecycle`，`lifecycle_reused=true`，`request_pid_count=1`，request methods 为 `initialize` / `notifications/initialized` / `tools/list` / `tools/list` / `tools/call` / `shutdown` / `exit`。
- LaunchServices packaged smoke 通过，同样 `request_pid_count=1`，证明真实 macOS `.app` 启动路径下 bundled bridge 和 local MCP lifecycle 复用闭环可用。
- `git diff --check` 通过；无本轮 OpenAgent runtime/Desktop/packaged MCP 残留进程。

Residual risk:

- Screenshot artifact 仍因为当前 macOS 前台窗口不是 OpenAgent 被降级为 `null`，不影响 API/workflow 验收。
- Lifecycle registry 仍是 App Bridge 进程内状态，不跨 runtime 重启。
- CLI standalone MCP path 仍未复用 Desktop/App Bridge lifecycle。

## 2026-07-02 Desktop MCP Lifecycle Trace Visibility Slice

Product alignment:

- 在 App Bridge runtime 已经真正复用 local stdio MCP lifecycle session 后，把这条链路变成 Desktop 可见状态：用户能在 MCP tool card 和 Latest call 中看到是否复用了已启动的 local MCP 进程和 PID。
- 本轮不改 Rust runtime 语义、不做 packaged Tauri local MCP workflow、不推 GitHub；只完成 Desktop 可视承接和 fake App Bridge 渲染验证。

Implemented:

- `desktop/src/App.tsx`
  - `McpToolTrace` 增加 `lifecycleReused` 和 `lifecyclePid`。
  - 从 tool part metadata 解析 `mcp_lifecycle_reused` 与 `mcp_lifecycle_pid`。
  - timeline MCP tool card trace strip 增加 `lifecycle reused` 与 `pid <pid>`。
  - 右侧 MCP `Latest call` 增加 Lifecycle/PID 两行。

Verification:

```bash
npm --prefix desktop run build
node --input-type=module  # inline fake App Bridge + Playwright rendered smoke
git diff --check -- desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
rg -n "[ \t]+$|\t" desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
```

Evidence:

- `npm --prefix desktop run build` 通过。
- Fake App Bridge + Playwright smoke 通过：fake persisted `messages_v2` 返回带 `mcp_lifecycle_reused:true`、`mcp_lifecycle_pid:4242` 的 MCP tool part；浏览器中 `[data-testid="mcp-tool-card"]` 显示 `MCP: stdio_echo`、`lifecycle reused`、`pid 4242`；打开 details 后 `[data-testid="mcp-latest-call"]` 显示 `Lifecycle reused` 和 `PID 4242`。
- `git diff --check` 与显式 trailing whitespace/tab 检查通过；无本轮 Vite/Node 残留进程，只看到既有 sub2api 前端 Vite。

Residual risk:

- 本轮是 fake App Bridge rendered smoke，不是真实 packaged Tauri local MCP workflow。
- `desktop/` 当前仍是 untracked 目录，未改变 git 跟踪状态。
- CLI standalone MCP path 仍未复用 App Bridge lifecycle。

## 2026-07-02 Agent MCP Lifecycle Session Reuse Slice

Product alignment:

- 在 App Bridge 和 Desktop 都已经支持 local stdio MCP start/stop/restart 之后，把 running lifecycle session 接进 Agent/runtime MCP tool call 路径，避免用户启动的 local MCP server 在真实工具调用时被绕开。
- 本轮不做跨 App Bridge 重启持久化、不改 CLI standalone MCP 短连接路径、不做 packaged Tauri local MCP smoke、不推 GitHub；只收口 Rust App Bridge runtime 路径。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `register_runtime_mcp_tools` 优先调用 `refresh_mcp_lifecycle_server`，已启动的 local stdio MCP server 会复用 running session 发现 tools 并注册到 toolkit。
  - `execute_runtime_mcp_tool` 在 local stdio + running lifecycle session 存在时直接调用同一个 `StdioMcpSession::request("tools/call", ...)`。
  - MCP tool result metadata 增加 `mcp_lifecycle_reused=true` 和 `mcp_lifecycle_pid`，用于 trace/UI 证明复用路径。
  - fake stdio MCP server 增加 `tools/call` 响应和可选 `LOCAL_REQUEST_LOG` 请求日志。
  - 新增 `app_bridge_mcp_tool_call_reuses_local_stdio_lifecycle_session` 回归测试。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_mcp_tool_call_reuses_local_stdio_lifecycle_session --lib -- --nocapture
cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture
cargo test -p openagent-mcp --tests -- --nocapture
cargo check -p openagent-http-runtime -p openagent-mcp
git diff --check -- runtime/http/src/http_runtime.rs .goal/state.md progress.md
rg -n "[ \t]+$|\t" runtime/http/src/http_runtime.rs .goal/state.md progress.md
```

Evidence:

- 新增回归通过：先 `POST /api/mcp/servers/local-tools/start`，再通过 App Bridge turn `tool_call` 调用 `mcp_tool_local_tools_stdio_echo`，输出包含 `stdio echo: from-lifecycle`。
- Tool result metadata 包含 `mcp_lifecycle_reused=true` 与同一个 `mcp_lifecycle_pid`。
- fake stdio 请求日志证明 initialize / tools/list / tools/call 全部来自同一个 PID，没有另起短连接。
- MCP HTTP runtime 8 个 MCP tests 通过；MCP crate 3 个 tests 通过；`cargo check` 通过；hygiene 检查通过。

Residual risk:

- Lifecycle registry 仍是 App Bridge 进程内状态，不跨 runtime 重启。
- CLI standalone MCP path 仍使用短连接，尚未共用 App Bridge lifecycle registry。
- 尚未做 packaged Tauri 真实 local MCP UI smoke。

## 2026-07-02 Desktop MCP Lifecycle Controls Slice

Product alignment:

- 在 Rust App Bridge 已有 local stdio MCP lifecycle API 之后，把 start/stop/restart 接入 Desktop MCP 面板，让用户不再只能通过 API 或 CLI 触发本地 MCP server 进程控制。
- 本轮不改 Rust lifecycle registry 语义、不做 packaged Tauri local MCP smoke、不推 GitHub；只收口 Desktop UI 接线和 fake bridge 交互验证。

Implemented:

- `desktop/src/App.tsx`
  - `McpServerSummary` 增加 `lifecycle_status`、`lifecycle_pid`、`lifecycle_started_at`、`lifecycle_last_refreshed_at`、`lifecycle_tool_count`。
  - MCP local server row 增加 lifecycle strip，展示 running/stopped、pid、started time、runtime tools。
  - MCP local server row 增加 Start/Stop/Restart icon buttons，分别调用 `/api/mcp/servers/{name}/start|stop|restart`。
  - Test/Enable/Delete buttons 补 `aria-label`，便于可访问操作和 Playwright 精确定位。
- `desktop/src/styles.css`
  - 修正 MCP server action 区的 mini button spacing。
  - 新增 lifecycle strip 样式，保持 MCP row 紧凑、可扫描。

Verification:

```bash
npm --prefix desktop run build
node --input-type=module  # inline fake App Bridge + Playwright smoke
```

Evidence:

- `npm --prefix desktop run build` 通过。
- Fake App Bridge + Playwright smoke 通过：打开 Desktop details 后看到 `local-tools`，点击 Start 收到 `POST /api/mcp/servers/local-tools/start`，UI 显示 `Stdio Echo` 和 `pid 4242`；点击 Stop 收到 `POST /api/mcp/servers/local-tools/stop`，UI 显示 `pid -`；点击 Restart 收到 `POST /api/mcp/servers/local-tools/restart`，UI 显示 `pid 4243`。

Residual risk:

- Lifecycle registry 仍是 App Bridge 进程内状态，不跨 runtime 重启。
- Agent MCP tool call 仍未复用 running stdio lifecycle session。
- 尚未做 packaged Tauri 真实 local MCP UI smoke。

## 2026-07-02 MCP Local Stdio Lifecycle API Slice

Product alignment:

- 在 local stdio discovery/test 之后，补齐 App Bridge 后端生命周期控制：local MCP server 不再只能每次短启动 test，也可以 start 后保留在 App Bridge 进程内，由 `/api/mcp` 暴露 running/pid/started/tools 状态。
- 本轮不改 Desktop 前端、不改 Agent tool call 复用路径、不做跨重启 daemon、不做 packaged app smoke、不推 GitHub；只完成 Rust App Bridge lifecycle API 竖切。

Implemented:

- `src/mcp/src/mcp_bridge.rs`
  - `StdioMcpSession` 从私有一次性 helper 提升为可复用 session。
  - 新增/暴露 `start`、`request`、`tools_list`、`running`、`pid`、`close`。
  - 保留 stdio MCP handshake：initialize -> `notifications/initialized` -> request -> shutdown/exit。
- `runtime/http/src/http_runtime.rs`
  - 新增进程内 MCP lifecycle registry，key 为 workspace + server name。
  - 新增 `POST /api/mcp/servers/{name}/start`、`stop`、`restart`。
  - `/api/mcp` local server payload 增加 `lifecycle_status`、`lifecycle_pid`、`lifecycle_started_at`、`lifecycle_last_refreshed_at`、`lifecycle_tool_count`。
  - `refresh/test` 优先复用 running stdio session；配置变更或进程退出会清理 registry。
  - protocol manifest MCP endpoint 文案同步更新。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_mcp_local_stdio_lifecycle_start_stop_restart --lib -- --nocapture
cargo fmt --all -- --check
cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture
cargo test -p openagent-mcp --tests -- --nocapture
cargo check -p openagent-mcp -p openagent-http-runtime
git diff --check -- src/mcp/src/mcp_bridge.rs runtime/http/src/http_runtime.rs .goal/state.md progress.md
rg -n "[ \t]+$|\t" src/mcp/src/mcp_bridge.rs runtime/http/src/http_runtime.rs .goal/state.md progress.md
pgrep -fl "fake_stdio_mcp|openagent-http-runtime|openagent-desktop|vite|smoke-packaged-app|node --input-type=module"
```

Evidence:

- 新增 `app_bridge_mcp_local_stdio_lifecycle_start_stop_restart` 通过：临时 rustc fake stdio MCP server，start 后 running/pid/tool 可见；普通 `GET /api/mcp` 保持 running；`refresh=true` 复用同一 pid；stop 后 stopped/tool_count 0；restart 后重新 running；响应不泄露 env secret。
- `cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture` 通过 7 个 MCP tests。
- `cargo test -p openagent-mcp --tests -- --nocapture` 通过 3 个 MCP crate tests。
- `cargo check -p openagent-mcp -p openagent-http-runtime` 通过。
- `cargo fmt --all -- --check`、`git diff --check`、显式 trailing whitespace/tab 检查通过。
- 进程检查没有本轮 fake stdio/OpenAgent runtime 残留；只看到既有 sub2api 前端 Vite。

Residual risk:

- Lifecycle registry 仍是 App Bridge 进程内状态，不跨 runtime 重启。
- Agent MCP tool call 仍使用 `mcp_json_rpc` 短连接，没有复用 running stdio session。
- Desktop MCP 面板还没接 start/stop/restart 按钮。
- 尚未做 packaged app 真实 local MCP smoke。

## 2026-07-02 MCP Local Stdio Discovery/Test Slice

Product alignment:

- 在 local stdio MCP 配置编辑之后，补齐真实连通验证：App Bridge 的 `/api/mcp?refresh=true` 和 `/api/mcp/servers/{name}/test` 都能实际拉起本地 stdio MCP server，完成 initialize / initialized / tools/list。
- 本轮不做常驻 MCP 进程池、不做 OAuth/插件市场、不做 packaged app local MCP smoke、不推 GitHub；只收口一次性 discovery/test 和失败诊断。

Implemented:

- `src/mcp/src/mcp_bridge.rs`
  - stdio MCP session 从丢弃 stderr 改为 pipe stderr。
  - initialize/tools/list 超时、stdout 关闭或 frame 读取失败时，错误消息会追加截断后的 stderr 摘要。
  - 成功路径仍保持一次性 stdio session：spawn -> initialize -> `notifications/initialized` -> request -> shutdown/exit。
- `runtime/http/src/http_runtime.rs`
  - 新增 `app_bridge_mcp_refresh_and_test_discover_local_stdio_tools`。
  - 测试用临时 Rust fake stdio MCP server，不依赖 Node；通过 `rustc` 编译后作为 local MCP command 执行。
  - 测试覆盖 command args、relative cwd、env 注入、`GET /api/mcp?refresh=true`、`POST /api/mcp/servers/local-tools/test` 和响应脱敏。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_mcp_refresh_and_test_discover_local_stdio_tools --lib -- --nocapture
cargo fmt --all -- --check
cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture
cargo test -p openagent-mcp --tests -- --nocapture
cargo check -p openagent-mcp -p openagent-http-runtime
git diff --check -- src/mcp/src/mcp_bridge.rs runtime/http/src/http_runtime.rs .goal/state.md progress.md
rg -n "[ \t]+$|\t" src/mcp/src/mcp_bridge.rs runtime/http/src/http_runtime.rs .goal/state.md progress.md
pgrep -fl "fake_stdio_mcp|openagent-http-runtime|openagent-desktop|vite|smoke-packaged-app|node --input-type=module"
```

Evidence:

- 新增 stdio 回归单测通过。
- `cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture` 通过 6 个 MCP tests。
- `cargo test -p openagent-mcp --tests -- --nocapture` 通过 3 个 MCP crate tests。
- `cargo check -p openagent-mcp -p openagent-http-runtime` 通过。
- `cargo fmt --all -- --check`、`git diff --check`、显式 trailing whitespace/tab 检查通过。
- 进程检查没有本轮 fake stdio/OpenAgent runtime 残留；只看到既有 sub2api 前端 Vite。

Residual risk:

- 这是一次性 stdio discovery/test，不是 MCP 常驻进程 lifecycle。
- 每次 refresh/test 仍会短启动本地 server；还没有 start/stop/reconnect daemon、健康缓存、进程保活或 packaged app 真实 local MCP smoke。

## 2026-07-02 MCP Local Stdio Config Editor Slice

Product alignment:

- 把 MCP 配置管理从 remote-only 表单推进到 remote/local 双模式：Desktop 可以新增 local stdio MCP server，并把 command/args/cwd/env/headers/timeout 通过 Rust App Bridge 写入配置。
- 本轮不做 MCP 进程 lifecycle、OAuth/插件市场、真实 packaged MCP server smoke、不推 GitHub；只收口 local stdio 配置编辑、落盘和 UI payload。

Implemented:

- `desktop/src/App.tsx`
  - `McpServerDraft` 扩展为 mode-aware draft，支持 `remote` / `local`。
  - Remote mode 保留 URL/transport；Local mode 新增 command、args、cwd。
  - 两种模式共享 timeout、env、headers，并在提交前校验必填和 timeout 正数。
  - `parseMcpList(...)` 把 textarea 中的 args 拆成数组；`parseMcpMap(...)` 支持 env/header 的 `KEY=value` 或 `Header: value` 输入。
  - `POST /api/mcp/servers` local body 使用 `type:"local"`、`command`、`args`、`cwd`、`env`、`headers`、`timeout_ms`。
- `desktop/src/styles.css`
  - MCP form 支持 textarea、full-width field、local mode 的双列/全宽布局和 submit button 跨列。
- `runtime/http/src/http_runtime.rs`
  - 新增 `app_bridge_mcp_server_config_crud_writes_local_stdio_fields`，锁住 local stdio 字段写入、脱敏响应和默认 `.openagent/mcp.json` 落盘行为。

Verification:

```bash
cargo fmt --all -- --check
cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture
npm --prefix desktop run build
node --input-type=module <<'NODE'
# inline Playwright fake App Bridge smoke:
# open Desktop -> details -> MCP form -> Local stdio
# fill local-tools/node/server.js --stdio/cwd/env/header/timeout -> Add
# assert posted JSON matches local stdio config exactly
NODE
```

Evidence:

- `cargo fmt --all -- --check` 通过。
- `cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture` 通过 5 个 MCP tests。
- 新增 `app_bridge_mcp_server_config_crud_writes_local_stdio_fields` 覆盖 local stdio server 写入 command/args/cwd/env/headers/timeout，并断言 `/api/mcp` 响应不泄露 env/header key/value。
- `npm --prefix desktop run build` 通过。
- inline Node + Playwright fake App Bridge smoke 通过：浏览器切到 Local stdio，提交 `local-tools`，fake bridge 收到精确 payload：

```json
{"name":"local-tools","type":"local","command":"node","args":["server.js","--stdio"],"cwd":"/tmp/local-tools","env":{"LOCAL_SECRET":"local-secret-value"},"headers":{"X-Local-Token":"local-header-secret"},"timeout_ms":3000,"enabled":true}
```

Residual risk:

- 这是 local stdio 配置编辑和落盘，不是 MCP 子进程 lifecycle。
- 还没有 start/stop/reconnect daemon、进程保活、真实 local MCP server packaged smoke、OAuth 或插件市场安装。

## 2026-07-02 MCP Per-Server Health/Test Slice

Product alignment:

- 在 MCP config CRUD 之后，补上单个 MCP server 的手动 Test/Reconnect 产品闭环：用户不必刷新全部 server，也不必先启用 disabled server，就能单独探测某个 MCP endpoint 是否能连通、能发现哪些 tools。
- 本轮不做 MCP 常驻进程池、不做 OAuth/插件市场、不做真实 packaged MCP smoke、不推 GitHub；只做 per-server health/test API + Desktop 操作面。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - protocol manifest 的 MCP endpoint 描述新增 `POST /api/mcp/servers/{name}/test`。
  - 动态路由新增 `POST /api/mcp/servers/{name}/test`。
  - 新增 `mcp_test_server_payload(...)`，加载当前 MCP config、定位指定 server、执行一次 `discover_mcp_server_tools`，并返回脱敏后的 `/api/mcp` payload。
  - `refresh_mcp_manager_tools(...)` 拆成全量 refresh + `refresh_mcp_manager_server_tools(...)` 单 server helper，保持 `/api/mcp?refresh=true` 与单 server test 的 discovery/错误脱敏一致。
  - disabled server 也允许被显式 test；test 不写配置、不改变 enabled 状态。
- `desktop/src/App.tsx`
  - MCP server row 新增 Test 按钮，调用 `/api/mcp/servers/{name}/test`。
  - server meta 新增 Checked 字段，兼容后端秒级 `last_refreshed_at`。
  - Test 成功后更新 row status/tools/checked；错误仍显示在 MCP card 内。
  - 修正 MCP row 文本区/action 区类名，避免按钮布局被通用 grid 规则覆盖。
- `desktop/src/styles.css`
  - MCP row meta 从 4 列扩成 5 列，新增 Checked 展示空间。

Verification:

```bash
cargo fmt --all -- --check
cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture
npm --prefix desktop run build
node --input-type=module <<'NODE'
# inline Playwright fake App Bridge smoke:
# open details -> click Test MCP server -> expect connected + Lookup + Checked
# observed POST /api/mcp/servers/remote-tools/test
NODE
git diff --check -- runtime/http/src/http_runtime.rs desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
rg -n "[ \t]+$|\t" runtime/http/src/http_runtime.rs desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
pgrep -fl "vite|openagent-http-runtime|openagent-desktop|smoke-packaged-app|node --input-type=module" || true
```

Evidence:

- `cargo fmt --all -- --check` 通过。
- `cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture` 通过 4 个 MCP tests。
- 新增 `app_bridge_mcp_server_test_discovers_disabled_server_tools` 覆盖 disabled `remote-tools` 手动 test 后 server row 为 `connected`、发现 `mcp_tool_remote_tools_lookup`，同时全局 `enabled:false/status:disabled` 保持不变，并断言 payload 不泄露 `test-secret` / `token=`。
- `npm --prefix desktop run build` 通过。
- inline Node + Playwright fake App Bridge smoke 通过：点击 `Test MCP server` 后 fake bridge 收到 `POST /api/mcp/servers/remote-tools/test`，UI 显示 `connected`、`Lookup`、`Checked`，browser console/page errors 为 0。
- `git diff --check`、`cargo fmt --all -- --check` 和显式 trailing whitespace/tab 检查通过。
- 无本轮残留 OpenAgent runtime/Desktop/smoke/node 进程；`pgrep` 只看到既有 sub2api 前端 Vite 和 william-kb Vite preview。

Residual risk:

- 这是手动 health/test，不是 MCP server 常驻 lifecycle；还没有 start/stop/reconnect daemon、自动重试、健康缓存或后台保活。
- local stdio server 的 command/args/env/header 编辑仍很薄。
- 未做真实 remote MCP packaged app smoke。

## 2026-07-02 MCP Server Config CRUD Slice

Product alignment:

- 把 MCP 管理从右侧 inspector 的只读展示推进到第一段可写产品闭环：用户可以在 Desktop 里新增 remote MCP server、启用/禁用、删除；CLI/TUI/Desktop 共用的 Rust App Bridge 暴露同一套 `/api/mcp` 配置管理语义。
- 本轮不做 MCP 进程 lifecycle 常驻管理、不做 OAuth/插件市场、不做 local stdio 的完整命令编辑器、不推 GitHub；只补配置 CRUD + Desktop 操作面。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `GET /api/mcp` 新增 `writable`、`config_path`、`readonly_reason`，保留原有脱敏 servers/tools payload。
  - 新增 `POST /api/mcp/servers`、`PATCH /api/mcp/servers/{name}`、`DELETE /api/mcp/servers/{name}`。
  - 无显式 MCP 配置时默认写入 workspace `.openagent/mcp.json`；显式文件路径配置可写；inline JSON 配置保持只读。
  - mutation 支持已有 `mcpServers` / `mcp.servers` / direct object 形态，写入前复用 `load_mcp_config_from_value` 校验，避免落半截无效配置。
  - protocol manifest 的 MCP endpoint 描述同步更新。
- `desktop/src/App.tsx`
  - MCP card 新增 Name / URL / Transport 表单。
  - server row 新增 enable/disable 与 delete 图标按钮。
  - mutation 成功后直接刷新 MCP payload，错误只显示在 MCP card 内。
- `desktop/src/styles.css`
  - 新增 MCP 配置表单、action button、server row actions 样式，保持右侧 inspector 紧凑布局。

Verification:

```bash
cargo fmt --all -- --check
cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture
npm --prefix desktop run build
node --input-type=module <<'NODE'
# inline Playwright fake App Bridge smoke:
# open details -> add remote-tools -> disable -> delete
# observed POST /api/mcp/servers, PATCH /api/mcp/servers/remote-tools, DELETE /api/mcp/servers/remote-tools
NODE
git diff --check -- runtime/http/src/http_runtime.rs desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
rg -n "[ \t]+$" runtime/http/src/http_runtime.rs desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
pgrep -fl "vite|openagent-http-runtime|openagent-desktop|smoke-packaged-app|node --input-type=module" || true
```

Evidence:

- `cargo fmt --all -- --check` 通过。
- `cargo test -p openagent-http-runtime app_bridge_mcp --lib -- --nocapture` 通过 3 个 MCP tests。
- 新增 `app_bridge_mcp_server_config_crud_writes_default_file` 覆盖 default `.openagent/mcp.json` add -> disable -> delete，并断言 `/api/mcp` payload 不泄露 `crud-secret`。
- `npm --prefix desktop run build` 通过。
- inline Node + Playwright fake App Bridge smoke 通过：浏览器打开 Desktop、展开 details、MCP 表单新增 `remote-tools`、禁用、删除；fake bridge 收到 `POST /api/mcp/servers`、`PATCH /api/mcp/servers/remote-tools`、`DELETE /api/mcp/servers/remote-tools`；console/page errors 为 0。
- `git diff --check` 和显式 trailing whitespace 检查通过。
- 无本轮残留 OpenAgent runtime/Desktop/Vite/smoke/node 进程；`pgrep` 仅看到既有 sub2api 前端 Vite。

Residual risk:

- 这是 MCP config CRUD，不是 MCP server lifecycle；还没有 start/stop/reconnect/health 常驻管理。
- local stdio server 的 command/args/env/header 编辑仍需要更完整 UI。
- 未做 OAuth、插件市场安装、真实 remote MCP packaged smoke。

## 2026-07-02 Packaged Tauri Real Sub2API Streaming Smoke Slice

Product alignment:

- 把 packaged `.app` 验证从 fake provider approval/rollback 推进到真实公网 Sub2API streaming：真实 `OpenAgent.app` 通过 LaunchServices 启动，Tauri shell 托管 bundled Rust App Bridge，App Bridge 使用 OpenAI-compatible Responses wire API 调用 `http://47.116.192.3/v1` 的 `gpt-5.4-mini`，并证明 SSE delta、turn completion、message persistence 都打通。
- 本轮不改 provider 配置、不输出 API key、不改 runtime 语义、不做签名/DMG/auto-update、不推 GitHub；只做 packaged app real-provider 验收并记录安全摘要。

Verification:

```bash
node - <<'NODE'
# checked local provider config without printing the key:
# env_file_exists=true, api_key=set(len=67), base_url=http://47.116.192.3/v1, local default model=gpt-5.5
NODE
npm run tauri -- build --bundles app
npm run smoke:packaged-app:real-streaming
rm -rf desktop/src-tauri/gen/schemas && rmdir desktop/src-tauri/gen 2>/dev/null || true
pgrep -fl 'openagent-http-runtime|openagent-desktop|OpenAgent.app|smoke-packaged-app|node.*smoke-packaged' || true
git diff --check -- desktop/scripts/smoke-packaged-app.mjs desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' desktop/scripts/smoke-packaged-app.mjs desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
```

Evidence:

- Provider config preflight did not print the key; it only reported `api_key: set(len=67)`. The smoke command explicitly overrode local default `gpt-5.5` to `gpt-5.4-mini`.
- `npm run tauri -- build --bundles app` 通过，刷新 packaged `/Users/william/coding/harness/openharness/desktop/src-tauri/target/release/bundle/macos/OpenAgent.app`。
- `npm run smoke:packaged-app:real-streaming` 通过：
  - LaunchServices 启动 packaged `OpenAgent.app`。
  - bundled bridge `/Contents/Resources/openagent-http-runtime` 健康检查通过，`auth_required: true`、`service: openagent-http-runtime`、`ui_enabled: false`。
  - provider health：`healthy: true`、`model: gpt-5.4-mini`、`model_count: 18`、`configured_model_available: true`、`base_url: http://47.116.192.3/v1`、`api_key: set`。
  - provider config summary 只显示 `api_key: set(len=67)`，没有泄露 key。
  - real streaming workflow 生成 session `session_1782982181196_39013_0` 和 turn `turn_1782982181200_39013_1`，elapsed `3450ms`。
  - persisted messages count `2`，assistant text 为 `OA_PACKAGED_REAL_STREAM_BEGIN\nOA_PACKAGED_REAL_STREAM_END`，begin/end markers 均为 true。
  - events summary：`event_count: 15`、methods `turn/started` / `item/agentMessage/delta` / `turn/completed`、`delta_count: 13`、`completed_count: 1`、`failed_count: 0`、`turn_model: gpt-5.4-mini`。
- Screenshot artifact 仍为 `null`，原因同上一轮：当前自动化环境里 OpenAgent 窗口截图失败且前台仍为 Codex；主 workflow/API/session/events 验收不受影响。
- `desktop/src-tauri/gen/schemas/` 构建产物已清理；无残留 OpenAgent runtime/Desktop/smoke/node 进程；`git diff --check` 和 trailing whitespace 检查通过。

Residual risk:

- 这是 real provider streaming smoke，不覆盖 approval/diff/checkpoint rollback；rollback 已由上一轮 fake provider packaged workflow 覆盖。
- 截图 artifact 仍受当前 macOS 前台/截图权限限制；真实视觉 QA 需要手动前台或额外系统权限。
- macOS 签名、DMG、auto-update、Windows packaged app 仍未覆盖。

## 2026-07-02 Packaged Tauri Approval/Rollback Workflow Smoke Slice

Product alignment:

- 把 Desktop 验证从 Vite/web preview 推进到真实 packaged `.app`：通过 LaunchServices 启动 `OpenAgent.app`，由 Tauri shell 托管 bundled Rust App Bridge，再走 provider streaming -> tool approval -> file write -> diff -> checkpoint restore -> rollback 的主闭环。
- 本轮不改 Rust runtime/agent loop 语义、不接真实 Sub2API、不做签名/DMG/auto-update、不推 GitHub；只验证 packaged app 主链路，并修 smoke 脚本里 macOS 截图 API 的脆弱点。

Implemented:

- `desktop/scripts/smoke-packaged-app.mjs`
  - `captureScreenshot(...)` 对 macOS `screencapture -l<window>` 失败增加降级处理。
  - 如果窗口级截图失败且 OpenAgent 不能被自动置前，脚本现在输出 warning 并返回 `screenshot: null`，不再让截图权限/前台窗口限制掩盖已经通过的 packaged workflow 断言。
  - 如果窗口级截图失败但 OpenAgent 仍是前台，则继续尝试全屏截图作为 artifact。

Verification:

```bash
npm run build
npm run tauri -- build --no-bundle
npm run tauri -- build --bundles app
npm run smoke:packaged-app:workflow
git diff --check -- desktop/scripts/smoke-packaged-app.mjs desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' desktop/scripts/smoke-packaged-app.mjs desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|OpenAgent.app|smoke-packaged-app|node.*smoke-packaged' || true
```

Evidence:

- `npm run build` 通过。
- `npm run tauri -- build --no-bundle` 通过，重新构建 release `openagent-desktop`。
- `npm run tauri -- build --bundles app` 通过，刷新 `/Users/william/coding/harness/openharness/desktop/src-tauri/target/release/bundle/macos/OpenAgent.app`。
- `npm run smoke:packaged-app:workflow` 通过：
  - LaunchServices 启动 packaged `OpenAgent.app`。
  - bundled bridge `/Contents/Resources/openagent-http-runtime` 通过健康检查，`auth_required: true`、`service: openagent-http-runtime`。
  - fake OpenAI-compatible provider 收到 2 次 `/v1/responses` 和 2 次 `/v1/models` 请求。
  - workflow 生成 session/turn，触发 `approval_call_packaged_workflow_write`。
  - allow 后写入 `workflow.txt`，内容为 `approved packaged workflow\n`。
  - diff 包含 `workflow.txt`，checkpoint restore 使用 `ckpt_1782981912502_1`，restore 后文件不存在。
  - events 包含 `turn/started`、`item/agentMessage/delta`、`item/toolCall/started`、`turn/approval_requested`、`turn/approval_resolved`、`item/toolCall/completed`、`patch/detected`、`turn/completed`、`checkpoint/restored`。
- smoke 输出 `screenshot: null`，原因是当前自动化环境中窗口截图失败且前台仍为 Codex；这个 warning 已被记录，不影响主 workflow 验收。
- `git diff --check`、显式 trailing whitespace 检查通过；无残留 OpenAgent runtime/Desktop/smoke/node 进程；Tauri 生成的 `desktop/src-tauri/gen/schemas/` 已清理。

Residual risk:

- 这是 fake provider 下的 packaged approval/rollback workflow，不是公网 Sub2API real streaming packaged smoke。
- 当前环境无法稳定把 OpenAgent 置为前台截图，视觉 screenshot artifact 为空；主链路由 App Bridge API、session/message/diff/checkpoint/events 证明。
- macOS 签名、DMG、auto-update、Windows packaged app 仍未覆盖。

## 2026-07-02 Desktop Scheduler Lifecycle UI Slice

Product alignment:

- 把 Rust App Bridge 已有的 scheduler hardening 状态接到 Desktop 产品面：queued timeout、expired queued turns、recovered durable queue、stale lease takeover 不再只是 API 字段，而是在 Jobs inspector 中可见、可解释。
- 本轮不改后端调度语义、不做 queue priority / running timeout / distributed queue、不做 packaged Tauri smoke、不推 GitHub；只补 Desktop 对现有 `/api/turns` payload 的产品承接。

Implemented:

- `desktop/src/App.tsx`
  - `TurnSchedulerSummary` 增加 `turn_queue_timeout_ms`、`expired_queued_turns`。
  - `isTurnJobTerminal(...)` 把 `expired` 纳入 terminal status，避免过期任务被误算成 active/running。
  - `statusClass(...)`、`turnJobStatusLabel(...)`、`turnJobLifecycleMessage(...)` 增加 expired / interrupted / recovered / cancel_requested 语义。
  - Jobs inspector 指标从 Workers/Queued/Durable/Lease 升级为 Workers/Queued/Durable/Timeout/Expired；scheduler strip 展示 `recovery`、`stale lease takeover`、job index persisted。
  - selected job detail 增加 Timeout 行，expired job 的 payload 显示 `removed`，并显示 “Queued turn expired after <timeout> without a worker...” 解释。
  - 左侧任务列表对 terminal job 展示 `expired/stopped + turn id`，减少只看 turn id 的不透明感。
- `desktop/src/styles.css`
  - Job metrics 改为 5 栏；新增 recovered/bad/warn lifecycle note 样式，保持 Codex-like 轻量信息面板风格。

Verification:

```bash
npm run build
# inline Node + Playwright rendered smoke:
# - fake App Bridge returns running + recovered queued + expired turn jobs
# - /api/turns scheduler includes turn_queue_timeout_ms=1800000, turn_queue_lease_stale_ms=45000, expired_queued_turns=2
# - open inspector and assert Jobs card shows 1/1 workers, Timeout 30m, Expired 2 pruned,
#   1 recovered from disk, stale lease takeover 45s, job index persisted
# - click Expired scheduler session and assert selected detail shows expired, Payload removed,
#   and Queued turn expired after 30m without a worker
# - assert browser console warnings/errors = 0 and page errors = 0
git diff --check -- desktop/src/App.tsx desktop/src/styles.css
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' desktop/src/App.tsx desktop/src/styles.css
grep -n $'\t' desktop/src/App.tsx desktop/src/styles.css || true
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|vite.*openharness|node.*playwright|http_runtime-6ca' || true
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- Rendered smoke 通过：Jobs inspector 能展示 timeout/expired/recovery/stale lease takeover，expired job detail 能解释 payload removed 与 timeout；console/page errors 均为 0。
- `git diff --check`、显式 trailing whitespace 检查、tab 检查通过。
- 未发现本轮残留 OpenAgent runtime/Desktop/Vite/Playwright/test binary 进程。

Residual risk:

- 这是 Desktop scheduler lifecycle UI，不是新的 scheduler policy；running turn timeout/cancel policy、queue priority、SQLite/DB-backed distributed queue 仍是后续任务。
- Smoke 使用 fake App Bridge，没有跑真实 packaged Tauri GUI。

## 2026-07-02 Desktop Typed Elicitation Types Slice

Product alignment:

- 继续推进 MCP typed elicitation/forms 与 question queue 的 Desktop 产品面：pending question 表单从纯 select/textarea 升级为能保留 option label/value、识别 select/multiselect/boolean/number/integer/text，并在提交前做本地校验。
- 本轮不改 Rust App Bridge question 协议、不做 MCP server 配置编辑、不做 packaged Tauri smoke、不推 GitHub；只把 Desktop 对现有 `answers: string[][]` 的承接做完整。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `ElicitationFieldKind`、`ElicitationOption`，把 question draft 从一维字符串改为 `Record<string, string[][]>`，与后端 `answers` 形态对齐。
  - `questionOptions(...)` 保留 option `label/value`，支持 `options`、`choices`、`enum`、schema enum 和 array items enum。
  - 新增类型识别与默认值归一：select、multiselect、boolean、number、integer、text；boolean 的 `false` 视为有效回答，multiselect 去重并提交多个 answer 值。
  - 新增 `questionValidationErrors(...)`，提交前校验 required、无效 option、number/integer、minimum/maximum；底部 dock 的快速 Reply 在表单未满足要求时禁用并提示打开详情。
  - `QuestionElicitationForm` 改为按字段类型渲染 select、多选 checkbox group、boolean checkbox、number input、textarea，并显示字段类型和错误。
- `desktop/src/styles.css`
  - 新增 typed elicitation 的 checkbox group、inline error、number input 等轻量样式，保持右侧 Trust inspector 的紧凑 Codex-like 观感。

Verification:

```bash
npm run build
# inline Node + Playwright rendered smoke:
# - fake App Bridge returns one pending question with select + multiselect + boolean + integer fields
# - open inspector via Toggle details
# - select Apply patch, check src/tests, uncheck Dry run, set Retry count to 4
# - click Reply and assert POST /api/questions/question_call_elicit/reply body is {"answers":[["apply"],["src","tests"],["false"],["4"]]}
# - assert browser console warnings/errors = 0 and page errors = 0
git diff --check -- desktop/src/App.tsx desktop/src/styles.css
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' desktop/src/App.tsx desktop/src/styles.css
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- Rendered smoke 通过：fake App Bridge 收到精确 `answers` payload `{"answers":[["apply"],["src","tests"],["false"],["4"]]}`；console/page errors 均为 0。
- `git diff --check` 和显式 trailing whitespace 检查通过。

Residual risk:

- 这是 Desktop typed elicitation 表单完整化，不是后端 MCP elicitation protocol 扩展。
- 还没有 schema-level pattern/minLength/maxLength/custom validation，也没有真实 packaged Tauri app smoke。
- MCP server 配置编辑、启停、删除仍是后续任务。

## 2026-07-02 Desktop Typed Elicitation Form Slice

Product alignment:

- 推进 MCP typed elicitation/forms 与 question queue 的 Desktop 产品面：pending question 不再只能一键默认回答，右侧 Trust 面板能把结构化 `questions[]` 渲染成表单字段，并把用户输入序列化成 Rust App Bridge 已支持的 `answers: string[][]`。
- 本轮不改后端 question 协议、不做 MCP server 配置编辑、不做 packaged app smoke、不推 GitHub；只补 Desktop 对现有 `/api/questions` payload 的结构化表单承接。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `ElicitationField` 与 `questionDrafts` state，按 `request_id` 保存 pending question draft。
  - 新增 `questionItems(...)`、`questionOptions(...)`、`questionDefaultAnswer(...)`、`questionElicitationFields(...)`，支持 `questions[]`、`tool_input.questions[]`、option label/value、默认值和 required 标记。
  - `respondQuestion(...)` 从一键默认 `questionAnswers(question.question)` 改为读取当前 draft，并提交 `answers: string[][]`；成功或 dismiss 后清理对应 draft。
  - 新增 `QuestionElicitationForm`，右侧 Trust 面板 pending question 渲染为 select/textarea 表单；带 options 的字段用 select，无 options 的字段用 textarea。
- `desktop/src/styles.css`
  - 新增 `.trust-question-form`、`.elicitation-form`、`.elicitation-field` 样式，保持右侧 inspector 的紧凑表单体验。

Verification:

```bash
npm run build
# inline Node + Playwright rendered smoke:
# - fake App Bridge returns one pending question with two structured fields
# - open inspector via Toggle details
# - assert pending-question-form contains Execution mode and Target file
# - select Apply patch, fill src/mcp/src/mcp_bridge.rs, click Reply
# - assert POST /api/questions/question_call_elicit/reply body is {"answers":[["Apply patch"],["src/mcp/src/mcp_bridge.rs"]]}
# - assert browser console warnings/errors = 0 and page errors = 0
git diff --check -- desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|vite.*openharness|node.*playwright|http_runtime-6ca' || true
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- Rendered smoke 通过：结构化 pending question 可见、可编辑、可提交，fake App Bridge 收到精确 `answers` payload；console/page errors 均为 0。
- `git diff --check` 和显式 trailing whitespace 检查通过。
- 未发现本轮残留 OpenAgent runtime/Desktop/Vite/Playwright/test binary 进程。

Residual risk:

- 这是 Desktop 对现有 question queue 的 typed form 承接，不是完整 MCP elicitation protocol 后端实现。
- 目前字段类型按 options/select 与 textarea 推断；尚未支持 number/boolean/checkbox/multiselect/schema validation。
- Smoke 使用 fake App Bridge，没有跑真实 packaged Tauri app。

## 2026-07-02 Desktop MCP Server Management Slice

Product alignment:

- 把右侧 MCP inspector 从“只看总数”推进到 Codex/Zcode 式可操作管理面板雏形：用户可以看到每个 MCP server 的 endpoint、transport、状态、错误、工具列表，并能手动刷新 `/api/mcp`。
- 本轮不做 MCP 配置写入、不做 typed elicitation/forms、不做 packaged app smoke、不推 GitHub；只做 Desktop 对现有 Rust App Bridge `/api/mcp` payload 的只读管理承接。

Implemented:

- `desktop/src/App.tsx`
  - `McpServerSummary` 增加 `last_refreshed_at` 字段。
  - 新增 `refreshMcp()`，独立调用 `/api/mcp?refresh=true`，更新 MCP payload、刷新状态和错误。
  - 新增 `mcpEndpointLabel(...)`、`mcpTransportLabel(...)`、`mcpToolLabel(...)` helper。
  - MCP inspector 标题栏增加 `Refresh MCP` 图标按钮，刷新期间显示旋转状态。
  - MCP server 列表从单行摘要升级为 `mcp-server-list`：展示 server type、enabled/disabled、configured/selected transport、endpoint、status、tool count、timeout、headers/env redacted count、last_error。
  - 每个 server 展示最多 6 个 tools，包含 title/original/name、dynamic tool id 和 description 摘要。
- `desktop/src/styles.css`
  - 新增 `.icon-button.mini`、`.spin` animation。
  - 新增 `.mcp-server-list`、`.mcp-server-row`、`.mcp-server-heading`、`.mcp-server-meta`、`.mcp-tool-list`、`.mcp-tool-row` 样式，保持右侧 inspector 的紧凑浅色信息密度。

Verification:

```bash
npm run build
# inline Node + Playwright rendered smoke:
# - serve desktop/dist
# - fake App Bridge returns two MCP servers: remote-tools(ok, two tools) and local-tools(error, no tools)
# - open inspector via Toggle details
# - assert MCP card contains remote-tools, auto -> http, Echo, Search remote docs, local-tools, stdio server unavailable
# - click Refresh MCP and assert /api/mcp request count increases
# - assert browser console warnings/errors = 0 and page errors = 0
git diff --check -- desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|vite.*openharness|node.*playwright|http_runtime-6ca' || true
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- Rendered smoke 通过：fake App Bridge 下 MCP card 展示两个 server、工具列表、错误状态，Refresh MCP 按钮触发额外 `/api/mcp` 请求；console/page errors 均为 0。
- `git diff --check` 和显式 trailing whitespace 检查通过。
- 未发现本轮残留 OpenAgent runtime/Desktop/Vite/Playwright/test binary 进程。

Residual risk:

- 这是只读 MCP management panel，不支持新增/编辑/删除 MCP server 配置。
- 还没做 MCP typed elicitation/forms，也没有做 real packaged Tauri MCP smoke。
- `desktop/` 当前仍是 untracked 目录，`git diff` 无法直接展示其中内容；本轮用 build/render smoke 与 `rg` 标识核验。

## 2026-07-02 Desktop MCP Trace Card Slice

Product alignment:

- 把上一轮后端 MCP approval/resume 闭环接到 Codex/Zcode 式 Desktop 可视层：MCP tool result 不再只是普通 tool 输出，现在 timeline 有专门 MCP tool card，右侧 MCP inspector 会显示最近一次 MCP call。
- 本轮不做 MCP typed elicitation/forms、不做完整 MCP 管理面板、不做 packaged app smoke、不推 GitHub；只做 Desktop 对现有 message part metadata 的产品承接。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `McpToolTrace`、`mcpToolTraceFromPart(...)`、`mcpToolTracesFromMessages(...)`。
  - 从 tool message part 的 `content.metadata` 识别 `backend:"mcp"`、`mcp_server`、`mcp_original_tool_name`、`mcp_transport`、`mcp_tool_name` 和 `mcp_non_text_blocks`。
  - MCP tool part 的标题从 `Tool: mcp_tool_...` 升级为 `MCP: <original tool>`，summary 展示 server / transport / output。
  - MCP tool card 增加 `data-testid="mcp-tool-card"`、MCP trace strip、Server/Tool/Transport/Call/Dynamic/Blocks rows。
  - 右侧 MCP inspector 新增 `Latest call` 小卡，展示最近一次 MCP tool、server、transport、call id、status 和输出摘要，使用 `data-testid="mcp-latest-call"` 便于 smoke。
- `desktop/src/styles.css`
  - 新增 `.part-mcp-tool`、`.mcp-trace-strip`、`.mcp-latest-call` 样式；保持浅色、紧凑、低装饰的 Codex-like inspector 视觉。

Verification:

```bash
npm run build
# inline Node + Playwright rendered smoke:
# - serve desktop/dist
# - fake App Bridge returns /api/mcp and a persisted messages_v2 assistant message with MCP tool metadata
# - assert [data-testid="mcp-tool-card"] contains MCP: echo, remote-tools, mcp echo: approved-mcp
# - assert [data-testid="mcp-latest-call"] contains Latest call, echo, http
# - assert browser console warnings/errors = 0 and page errors = 0
git diff --check -- desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|vite.*openharness|node.*playwright|http_runtime-6ca' || true
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- Rendered smoke 通过：timeline 出现 MCP card，右侧 MCP inspector 出现 Latest call，且页面无 console warning/error、无 page error。
- `git diff --check` 和显式 trailing whitespace 检查通过。
- 未发现本轮残留 OpenAgent runtime/Desktop/Vite/Playwright/test binary 进程。

Residual risk:

- `desktop/` 当前仍是 untracked 目录，`git diff` 无法直接展示其中内容；本轮用 `rg` 和 build/render smoke 验证新增标识。
- 这是 persisted message part 的可视化承接；还不是完整 MCP server 管理面板、typed elicitation/forms 或 MCP approval dock 专门交互。
- Smoke 使用 fake App Bridge，没有跑真实 packaged Tauri app。

## 2026-07-02 MCP Approval / Resume E2E Slice

Product alignment:

- 补齐 Rust App Bridge / Agent Runtime 的 MCP 信任边界验收锁：provider 触发动态 MCP tool call 后，`PLAN_ONLY` 会暂停到 approval queue；用户 allow 后 runtime 会真正调用 MCP `tools/call`，再把 MCP 输出作为 `function_call_output` 喂回 provider 并完成最终回答。
- 本轮不做 Desktop MCP trace card、不改 MCP UI、不推 GitHub；先把后端闭环用 E2E regression 锁住。

Implemented:

- `runtime/http/tests/http_runtime.rs`
  - 新增 `remote_runtime_client_resumes_provider_after_mcp_approval_allow`。
  - 覆盖 fake OpenAI Responses provider 发出 `mcp_tool_remote_tools_echo` function_call。
  - 覆盖 Rust runtime 在 `permission: PLAN_ONLY` 下返回 `waiting_approval`，approval payload 指向动态 MCP tool。
  - 覆盖 global approval allow 后清空 `pending_approval` / `pending_provider_turn`，事件流包含 `item/toolCall/completed` 和 `turn/completed`。
  - 覆盖 provider 第二次请求包含 `function_call_output` 与 `mcp echo: approved-mcp`。
  - 覆盖 fake MCP server 收到 `tools/list -> tools/list -> tools/call`，证明 allow 前只 discovery，allow 后才执行远端 MCP tool。
  - 将 fake MCP server helper 抽为 `spawn_fake_mcp_server_with_limit(max_requests)`，普通 MCP 测试仍使用 2 请求上限，approval/resume 测试使用 3 请求上限。

Verification:

```bash
cargo test -p openagent-http-runtime remote_runtime_client_resumes_provider_after_mcp_approval_allow --test http_runtime -- --nocapture
cargo fmt --all
cargo test -p openagent-http-runtime mcp --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_resumes_provider_after_approval_allow --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
git diff --check -- runtime/http/tests/http_runtime.rs progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|vite.*|node.*playwright|http_runtime-6ca' || true
```

Evidence:

- 新增 MCP approval/resume E2E 单测通过。
- `cargo test -p openagent-http-runtime mcp --test http_runtime -- --nocapture` 通过 2 个 MCP tests：普通 MCP tool loop、MCP approval/resume。
- 普通 provider approval resume regression 通过。
- `cargo check -p openagent-http-runtime`、`cargo fmt --all -- --check`、`git diff --check` 通过。
- 未发现本轮残留 OpenAgent runtime/Desktop/smoke/test binary 进程；`pgrep` 只看到既有 `/Users/william/coding/vibe/sub2api/frontend` Vite 进程。

Residual risk:

- 这是后端 E2E regression，不是 Desktop MCP 工具卡片或 MCP 面板。
- approval allow 路径会重新做一次 MCP `tools/list` discovery 再执行 `tools/call`；可用但后续可以加短期缓存，减少 approval resume 延迟。
- MCP typed elicitation/forms 和 Desktop MCP trace card 仍未完成。

## 2026-07-02 Queue Timeout / Expiry Slice

Product alignment:

- 推进 Codex/Zcode 式 App Bridge scheduler hardening：queued async turn 现在不会无限等待；超过配置 timeout 后会进入 terminal `expired` 状态，并清理 durable queue payload 与 owner lease。
- 本轮不做 Desktop 新 UI、不做 queue priority、不做 distributed scheduler、不推 GitHub；只补 Rust App Bridge queue expiry 语义和回归测试。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `HttpRuntimeConfig` 新增 `turn_queue_timeout_ms`，默认 `30 * 60 * 1000` ms，支持 `OPENAGENT_TURN_QUEUE_TIMEOUT_MS` 和 CLI `--turn-queue-timeout-ms` / `--queue-timeout-ms` 覆盖，最小值为 `1`。
  - 新增 `queued_turn_expired(...)`、`expire_queued_turns(...)`、`expire_queued_turns_locked(...)`，根据 queued payload 的 `queued_at_ms` 判断过期。
  - `pop_next_schedulable_queued_turn(...)` 在调度扫描前清理过期项；被 pop 出来但已过期的 queued turn 也会标记 `expired`，不会 promote 为 running。
  - `recover_persisted_queued_turns(...)` 启动恢复时会跳过并标记已过期的 durable queued payload，避免重启后执行陈旧任务。
  - `list_turn_jobs_payload(...)` 和 `turn_status_payload(...)` 触发 expiry，用户刷新 `/api/turns` 或 `/api/turns/{turn_id}` 即可看到 `expired`。
  - `expired` 被纳入 terminal status；terminal TTL prune 继续复用已有规则。
  - async turn accept / queue-full / list scheduler metadata 增加 `turn_queue_timeout_ms`；list scheduler 增加本次刷新触发的 `expired_queued_turns`。
- `runtime/http/tests/http_runtime.rs`
  - 测试 helper 清理 `OPENAGENT_TURN_QUEUE_TIMEOUT_MS`，避免真实环境污染。
  - 新增 `async_turn_expires_queued_turn_after_timeout`：第一条 async turn 占用 session worker，第二条 queued；timeout=150ms 后刷新 `/api/turns`，第二条变为 `expired`，queued payload 文件被删除，`GET /api/turns/{id}` 返回 `expired`，provider 只收到第一条请求。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn_expires_queued_turn_after_timeout --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime async_turn --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|vite.*5187|node.*18831|node.*playwright|http_runtime-6ca' || true
```

Evidence:

- 新增 expiry regression 单测通过。
- async turn targeted suite 通过 8 个测试：accepted-before-completion streaming、same-session queue promote、cross-session global quota queue、queue timeout expiry、queue-full 429 rejection、durable queued payload restart recovery、live owner lease guard、cooperative interrupt cancel。
- `cargo check -p openagent-http-runtime`、`cargo fmt --all -- --check`、`git diff --check` 通过。
- 无残留 OpenAgent runtime/Desktop/Vite/fake App Bridge/test binary 进程。

Residual risk:

- expiry 目前只对 queued-not-yet-running turn 生效；running turn 超时/cancel 仍是后续 worker/runtime policy。
- `expired` 还没有 dedicated Desktop wording；当前 Desktop 会按 terminal job 展示状态，后续可以做 recovered/stale/expired 的细分可视化。
- 这是单 runtime/file-based scheduler hardening，不是 SQLite/DB-backed distributed queue。

## 2026-07-02 Desktop Scheduler Status UI Slice

Product alignment:

- 把最近几轮 App Bridge scheduler 能力接到 Codex/Zcode 式 Desktop 任务可视化：Desktop 现在能展示 global worker quota、queued reason、durable queued payload、lease stale、job index persisted 等状态；queue-full 拒绝也不再只显示原始 HTTP 429。
- 本轮不改 Rust 后端调度语义、不做 queue timeout/expiry、不做 packaged Tauri smoke、不推 GitHub；只做 Desktop 对现有 App Bridge scheduler contract 的产品承接。

Implemented:

- `desktop/src/App.tsx`
  - `TurnJobSummary` 增加 `queue_reason`、`payload_persisted`；`TurnJobsPayload` 增加 `active_count`、`scheduler`；新增 `TurnSchedulerSummary`。
  - `normalizeTurnJobs(...)` 修正 fallback 计数：running 不再把 queued 算进去，queued/active/terminal 也会在 API 缺字段时本地推导。
  - App Bridge `api(...)` 非 2xx 错误现在会保留 JSON body 并抛出 `ApiError`，让 UI 能读取 `error_code` 和 scheduler metadata。
  - `submitPrompt(...)` 处理 `turn_queue_full`：显示中文用户提示 `排队已满：当前会话已有 N/M 个等待任务。`，并刷新 `/api/turns`。
  - optimistic job upsert 现在保留 `queue_reason`；左侧任务树 queued 行显示 `Queue #N · worker quota/session active`，不再只显示 turn id。
  - Jobs inspector 指标从 Running/Queued/Recent/Index 改为 Workers/Queued/Durable/Lease，展示 `running_turn_workers/max_running_turn_workers`、durable queued payload 数、lease stale 秒数。
  - Jobs inspector 新增 scheduler strip：`session queue max N`、`N waiting for worker quota`、`job index persisted`。
  - selected queued job 详情新增 Reason/Payload，并按 `global_worker_quota` / `session_active` 展示不同 waiting note。
- `desktop/src/styles.css`
  - 新增 scheduler strip 和 quota queue note 的轻量样式，保持当前 Codex-like 浅色桌面视觉。

Verification:

```bash
npm run build
# In-app Browser rendered smoke with fake App Bridge:
# - Vite: http://127.0.0.1:5187/
# - Fake App Bridge: http://127.0.0.1:18831
# - jobs payload: 1 running, 1 queued(global_worker_quota, payload_persisted), scheduler max workers=1, max queue=1, lease=30000ms
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|vite.*5187|node.*18831|node.*playwright' || true
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- In-app Browser smoke 通过：
  - Page title 为 OpenAgent Desktop，console warn/error 为空。
  - Jobs inspector DOM 包含 `Workers 1/1`、`Queued 1`、`Durable 1 saved`、`Lease 30s`。
  - scheduler strip 包含 `session queue max 1`、`1 waiting for worker quota`、`job index persisted`。
  - queued job detail 包含 `Reason worker quota`、`Payload persisted`、`Waiting for a runtime worker to free up.`。
  - 提交 prompt 后 fake App Bridge 返回 `turn_queue_full`，composer 下方显示 `排队已满：当前会话已有 1/1 个等待任务。`。
- 临时 fake App Bridge、Vite dev server 已停止；未发现残留 OpenAgent/Vite/Playwright 进程。

Residual risk:

- `desktop/` 目录在当前 worktree 仍是 untracked，`git diff --check` 不覆盖其内容；本轮额外用 `awk` 做了 trailing whitespace 检查。
- 本轮 smoke 用 fake App Bridge 验证 UI contract，没有跑 packaged Tauri GUI，也没有用真实 App Bridge 复现 queue-full/global quota。
- 还缺 queue timeout/expiry、queue priority、Desktop 对 recovered/stale lease takeover 的更完整状态，以及 MCP approval/resume E2E。

## 2026-07-02 Global Turn Worker Quota Slice

Product alignment:

- 推进 Codex/Zcode 式 App Bridge scheduler 底座：async turn 不再只按单 session 排队，也会受 runtime 全局 worker quota 约束；多个 session 同时提交时，超出全局并发预算的 turn 会进入 durable queue，等任一 worker 释放后自动启动。
- 本轮不做 queue timeout/expiry、不改 Desktop UI、不做 distributed scheduler、不推 GitHub；只补 Rust App Bridge 的跨 session 并发控制和 API 可观测字段。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `HttpRuntimeConfig` 新增 `max_running_turn_workers`，默认 `4`，支持 `OPENAGENT_MAX_RUNNING_TURN_WORKERS` 和 CLI `--max-running-turn-workers` / `--max-turn-workers` 覆盖，最小有效值为 `1`。
  - `register_turn_job(...)` 现在同时检查同 session running turn 和全局 running worker 数；同 session active 时 `queue_reason:"session_active"`，全局 quota 满时 `queue_reason:"global_worker_quota"`。
  - queued turn payload 增加内存 `queued_at_ms`，跨 session 调度时从各 session 队列 front 里选择最早入队项，保留每个 session 的 FIFO。
  - worker terminal 后调用全局 `start_next_queued_turns(...)`，在 quota 允许时持续启动可运行 queued turn，不再只扫描刚结束的 session。
  - startup recovery 复用全局调度入口，恢复后的 queued payload 会按全局 quota 启动。
  - async turn accept/list/queue-full 的 scheduler metadata 增加 `max_running_turn_workers`，`GET /api/turns` 增加全局 `running_turn_workers`，供 CLI/TUI/Desktop 展示调度状态。
- `runtime/http/tests/http_runtime.rs`
  - 测试 helper 清理 `OPENAGENT_MAX_RUNNING_TURN_WORKERS`，避免真实环境污染回归。
  - 新增 `async_turn_queues_second_session_when_global_worker_quota_full`：quota=1 时，第一个 session 的 async turn 占用 worker，第二个 session 的 async turn 返回 `queued` / `global_worker_quota`，provider 在第一条完成前只收到 1 次请求，第一条完成后第二条自动执行且只执行一次。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn_queues_second_session_when_global_worker_quota_full --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime async_turn --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md .goal/state.md
```

Evidence:

- 新增 global quota regression 单测通过。
- async turn targeted suite 通过 7 个测试：accepted-before-completion streaming、same-session queue promote、cross-session global quota queue、queue-full 429 rejection、durable queued payload restart recovery、live owner lease guard、cooperative interrupt cancel。
- `cargo check -p openagent-http-runtime`、`cargo fmt --all -- --check`、`git diff --check` 通过。

Residual risk:

- 这是单 runtime 进程内全局 quota，不是跨机器/多 runtime 的强一致调度；已有 file lease 只保护 queued payload recovery，不是完整 distributed scheduler。
- 还没有 queue timeout/expiry、queue priority、Desktop queue-full/recovered/leased/global-quota 状态提示，或 packaged app restart smoke。
- 下一段建议继续做 queue timeout/expiry，或转向 Desktop 任务状态可视化，把 global quota / lease / recovered 原因展示给用户。

## 2026-07-02 Queued Turn Owner Lease Slice

Product alignment:

- 推进 Codex/Zcode 式 durable scheduler 的跨进程安全边界：queued async turn payload 已能落盘恢复后，本轮给 queued turn 增加 owner lease，避免两个 runtime 指向同一个 session root 时同时恢复并执行同一条 queued turn。
- 本轮不做 running turn replay、不做分布式锁服务、不做全局 worker quota、不改 Desktop UI、不推 GitHub；只补 queued payload recovery 的 owner claim / stale takeover 语义。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 新增 `.openagent-runtime/turn_queue_leases/{turn_id}.lease.json` lease schema，包含 `owner_id`、`turn_id`、`claimed_at_ms`、`updated_at_ms`。
  - runtime 进程生成稳定 `owner_id`；queued turn 注册时先原子 `create_new` claim lease，再写 queued payload / job index / memory queue。
  - `claim_queued_turn_lease(...)` 支持 owner 重入、corrupt lease 清理和 stale takeover；默认 stale timeout 为 `30_000ms`，支持 `OPENAGENT_TURN_QUEUE_LEASE_STALE_MS` 和 CLI `--turn-queue-lease-stale-ms` 覆盖。
  - queued payload 被 promote/cancel/terminal 清理时同时释放 lease。
  - startup recovery 只有成功 claim lease 后才会恢复 queued payload；抢不到 live lease 的 runtime 会跳过，不会重复启动。
  - startup recovery 只有在实际 claim 到 queued payload 的 session 上才把旧 non-queued active job 标记为 `interrupted`，避免旁路 runtime 抢不到 lease 也污染 active job index。
  - `/api/turns` 和 async turn accept/queue-full response 的 scheduler block 增加 `turn_queue_lease_stale_ms`。
- `runtime/http/tests/http_runtime.rs`
  - 更新 restart recovery regression，用 `OPENAGENT_TURN_QUEUE_LEASE_STALE_MS=1` 模拟 owner crash 后 stale takeover。
  - 新增 `async_turn_recovery_respects_live_queue_lease_owner`：第二个 runtime 在 live lease 未过期时启动，不会恢复/执行 queued turn；原 owner runtime 完成第一条后按 FIFO 执行第二条，provider 只收到两次请求。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn_recover --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime async_turn --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
```

Evidence:

- `async_turn_recover` targeted tests 通过 2 个：crash/stale takeover recovery、live owner lease non-duplication。
- async turn targeted suite 通过 6 个测试：accepted-before-completion streaming、same-session queue promote、queue-full 429 rejection、durable queued payload restart recovery、live owner lease guard、cooperative interrupt cancel。
- `cargo check -p openagent-http-runtime` 和 `cargo fmt --all -- --check` 通过。

Residual risk:

- 这是 file-based owner lease，不是数据库/etcd/SQLite style distributed scheduler；时钟漂移和强一致性没有彻底解决。
- running turn 仍不会跨进程 replay；重启后只恢复 queued-not-yet-running payload。
- 还没有全局 worker quota、queue timeout/expiry、Desktop queue-full/recovered/leased 状态提示或 packaged app restart smoke。

## 2026-07-02 Durable Queued Turn Payload Recovery Slice

Product alignment:

- 推进 Codex/Zcode 式 durable scheduler 边界：App Bridge queued async turn 不再只存在于进程内队列，queued payload 会落盘；runtime 重启后可以恢复尚未执行的 queued turn，并继续从队列启动执行。
- 本轮不做跨进程强 lease、不做全局 worker quota、不做 running turn payload replay、不改 Desktop UI、不推 GitHub；只覆盖“已经 queued 但尚未 promote 为 running”的可恢复边界。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 新增 `.openagent-runtime/turn_queue/{turn_id}.json` queued payload schema，记录 `session_id`、`turn_id`、`queued_at_ms` 和原始 async turn payload。
  - queued turn 注册时先写 durable payload，再写内存 job/queue 和 job index；payload 写失败时返回 `turn_queue_persist_failed`，避免接受一个重启后必丢的 queued turn。
  - queued turn promote 为 running、queued cancel/interrupted、terminal status 写入时会清理 durable payload 和内存 queue entry。
  - runtime `serve_blocking(...)` 启动后会扫描 durable queued payloads，把仍处于 `queued` 的 job 恢复进内存 FIFO queue，并对每个 session 启动下一条 queued turn。
  - 启动恢复时会把旧进程遗留的 non-queued active job 标记为 `interrupted`，防止重启后 stale running 阻塞 queued recovery。
  - `GET /api/turns` 和 `GET /api/turns/{turn_id}` 对 payload 仍存在的 queued job 保持 `queued`，并返回 `payload_persisted:true`，不再误标为 `interrupted`。
- `runtime/http/tests/http_runtime.rs`
  - 新增 `async_turn_recovers_persisted_queued_turn_after_runtime_restart`：第一条 async turn 正在运行、第二条 queued 已落盘时 kill runtime；用相同 session root 重启后，第二条 queued turn 自动恢复并完成；旧 running turn 被标记为 `interrupted`；provider 只收到 first + recovered second 两次请求。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn_recovers_persisted_queued_turn_after_runtime_restart --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime async_turn --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
```

Evidence:

- 单独 restart recovery regression 通过。
- async turn targeted suite 通过 5 个测试：accepted-before-completion streaming、same-session queue promote、queue-full 429 rejection、durable queued payload restart recovery、cooperative interrupt cancel。
- `cargo check -p openagent-http-runtime` 和 `cargo fmt --all -- --check` 通过。

Residual risk:

- 这只恢复 queued-not-yet-running payload；running turn 仍不会跨进程 replay，重启后会标记为 `interrupted`。
- 还没有跨进程强 lease/owner token；如果两个 runtime 指向同一个 session root 并同时运行，仍可能发生重复 recovery。
- 还没有 queue timeout/expiry、全局 worker quota、Desktop queue-full/recovered toast，或 packaged app restart smoke 覆盖。

## 2026-07-02 Async Turn Queue Guardrails Slice

Product alignment:

- 推进 Codex/Zcode 式任务调度底座：App Bridge async turn queue 不再是无限入队，同一 session 的等待队列有明确上限，超限时返回可机器处理的 429 / `turn_queue_full`，CLI/TUI/Desktop 可以共用这个语义做用户提示。
- 本轮不做 durable payload recovery、不做跨进程 worker lease、不做跨 session 并发 quota、不改 Desktop UI、不推 GitHub；只补 session-level queue guardrail 和列表 API 计数语义。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `HttpRuntimeConfig` 新增 `max_queued_turns_per_session`，默认 `8`，支持 `OPENAGENT_MAX_QUEUED_TURNS_PER_SESSION` 和 CLI `--max-queued-turns-per-session` / `--max-queued-turns` 覆盖。
  - `register_turn_job(...)` 在同 session 有 running turn 时先检查 queued turn 数量；达到上限时不写入 job registry、不调用 provider，直接返回 queue-full。
  - async `/api/sessions/{session_id}/turns` 超限时返回 HTTP `429`，payload 包含 `error_code:"turn_queue_full"`、`accepted:false`、`queued:false`、当前 `queued_count` 和 scheduler policy。
  - queued accept response 增加 `queue_position` 和 `scheduler.max_queued_turns_per_session`。
  - `GET /api/turns` 现在区分 `running_count`、`queued_count`、`active_count`、`terminal_count`，queued jobs 会带 `queue_position`，避免入口层把 queued 误当 running。
- `runtime/http/tests/http_runtime.rs`
  - 更新 async queue regression，断言 queued response/list 带 queue position，且 running/queued/active 计数正确。
  - 新增 `async_turn_rejects_when_session_queue_is_full`：设置 queue limit 为 1，第一条 running、第二条 queued、第三条返回 429，并确认 provider 只收到前两条请求。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app|vite.*5192|node.*18812' || true
```

Evidence:

- async turn targeted suite 通过 4 个测试：accepted-before-completion streaming、same-session queue promote、queue-full 429 rejection、cooperative interrupt cancel。
- queue-full regression 证明第三条 async turn 没有进入 registry/provider，provider request count 保持 2。
- `cargo check -p openagent-http-runtime`、`cargo fmt --all -- --check`、`git diff --check` 通过。
- 无残留 OpenAgent runtime、Desktop、packaged smoke、queued UI smoke 进程。

Residual risk:

- 这是 session-level queue guardrail，不是 durable scheduler；runtime 重启后 queued payload 仍不会恢复执行。
- 还没有跨 session 全局 worker quota、持久 worker lease、queue payload 落盘/恢复、queue timeout/expiry 或 Desktop queue-full toast。
- 下一步可以继续做 durable queue lease/payload recovery 边界，也可以转向 MCP approval/resume E2E + Desktop MCP tool trace card。

## 2026-07-02 Desktop Queued Turn UI Slice

Product alignment:

- 推进 Codex/Zcode 式任务队列产品面：Desktop 现在能识别 App Bridge `queued` turn 语义，运行中的 session 可以继续提交下一条 prompt 进入队列，并能在任务侧栏/Jobs inspector 看到队列顺序和等待状态。
- 本轮不做 durable queue、不做跨进程 payload 恢复、不做全局 worker quota、不做完整任务 dashboard、不推 GitHub；只收口 App Bridge in-memory queue 在 Desktop 里的可见与可操作体验。

Implemented:

- `desktop/src/App.tsx`
  - `TurnJobSummary` / `TurnJobsPayload` 增加 `queue_position` 与 `queued_count`，新增 queued job helpers 和 `turnSubmitState(...)`，避免 queued turn 被误显示成 idle/running。
  - `submitPrompt(...)` 现在消费 async turn 返回的 `queued/status/queue_position`，乐观写入 queued job，并把 composer 状态更新为 `排队中`。
  - Desktop 将 active jobs 拆成 running 与 queued 两组，左侧任务树显示 `Queue #N`，Jobs inspector 显示 Running/Queued/Recent/Index metrics、`#N waiting` 和 queued wait note。
  - 当 active turn 可中断时，原主按钮继续作为 Stop，同时新增独立 `Queue prompt` 提交按钮，允许用户在运行中把下一条 prompt 排队。
  - queued job 详情里的 Stop 复用 `POST /api/turns/{turn_id}/interrupt`，能取消尚未执行的 queued turn。
- `desktop/src/styles.css`
  - 新增 queued wait note、4 列 job metrics 和 queue submit button 样式，保持当前 Codex-like 桌面布局。

Verification:

```bash
npm run build
node <fake-app-bridge-queued-ui-playwright-smoke>
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- fake App Bridge + Playwright queued smoke 通过：初始 session 有 `turn_running`，composer 没有 `Run prompt` 主提交而显示 Stop + `Queue prompt`；提交后 fake bridge 返回 `turn_queued_new` / `status:"queued"` / `queue_position:1`，Desktop 显示 `排队中`、侧栏 `Queue #1`、Jobs inspector `QUEUED`、`#1 waiting`、`queued #1`；点击详情 Stop 后命中 interrupt route 恰好 1 次，queued job 更新为 `interrupted` 并显示 `已中断`。
- smoke 无 console warning/error；临时 fake bridge 与 Vite 已停止。

Residual risk:

- 这是 Desktop UI 对 in-memory queue 的产品化承接，不是 durable scheduler；runtime 重启后 queued payload 仍不会恢复执行。
- 还没有最大队列长度、跨 session worker quota、持久 worker lease 或完整任务 dashboard。
- 下一步更适合做 scheduler hardening，或者转向 MCP approval/resume E2E + Desktop MCP tool trace card。

## 2026-07-02 Async Turn In-Memory Queue Slice

Product alignment:

- 推进 Codex/Zcode 式任务队列语义：同一个 session 的第二个 async turn 不再只返回冲突，而是进入进程内 FIFO 队列，当前 turn 结束后自动 promote 为 running 并执行。
- 本轮不做跨进程恢复执行、不做持久 payload queue、不做多 worker 并发策略、不做 Desktop 队列 UI 精修、不推 GitHub；只补 App Bridge 的基础排队和自动启动语义。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 新增 `QueuedTurnJob`、`queued_turns()` 和 `turn_scheduler_lock()`，把同 session active 检查、job 插入和 queue push 放在同一调度锁下，避免 active turn 结束与第二个请求入队之间的竞态。
  - `register_turn_job(...)` 现在在同 session 有 running/interrupting job 时创建 `status:"queued"` 的 job，写入 job index，并把 payload 放入进程内 FIFO queue；没有 active runner 时仍直接创建 `running` job。
  - `start_turn_async_payload(...)` 对 queued turn 返回 HTTP `202`、`accepted:true`、`queued:true`、`status:"queued"`，入口层可直接展示排队状态；running turn 仍按原语义后台执行。
  - 抽出 `spawn_async_turn_worker(...)`，running job 和 queued promote 后的 job 复用同一 worker 路径；worker terminal 后调用 `start_next_queued_turn(...)` 自动启动同 session 的下一项。
  - queued job 被 interrupt 时会标记 `interrupted`，promote 时会跳过已取消的 queued job。
  - `new_id(...)` 增加进程内原子序列号，修复快速双请求在同一毫秒生成相同 turn id 的真实碰撞问题。
- `runtime/http/tests/http_runtime.rs`
  - 将上轮 conflict test 升级为 `async_turn_queues_second_active_turn_for_same_session`：覆盖第一条 async turn running、第二条 async turn queued、`GET /api/turns` 同时展示 running + queued、第一条完成后第二条自动 completed、provider 收到两次请求且顺序正确。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app' || true
```

Evidence:

- `async_turn_queues_second_active_turn_for_same_session` 通过：第二条 async turn 返回 `queued:true`，列表中有第一条 `running` 和第二条 `queued`，第一条 terminal 后第二条自动执行到 `completed`，provider requests 长度为 2。
- `async_turn_returns_accepted_before_provider_completion_and_streams` 通过。
- `async_turn_interrupt_cancels_provider_stream_before_completion` 通过。
- `cargo check -p openagent-http-runtime`、`cargo fmt --all -- --check`、`git diff --check` 通过。
- 无残留 OpenAgent runtime、Desktop、packaged smoke 进程；进程扫描中出现的 Vite 是本机 `/Users/william/coding/vibe/sub2api/frontend`，与本次 OpenAgent 验证无关。

Residual risk:

- 这是 in-memory FIFO queue，不是 durable queue：runtime 重启后 queued payload 不会恢复执行，旧 non-terminal index 会按既有规则标记 interrupted。
- 还没有 Desktop 队列 UI 精修；现有 `/api/turns` 会看到 `queued` 状态，但 composer/Jobs inspector 还没专门处理队列顺序和等待原因。
- 没有全局并发策略、最大队列长度、跨 session worker quota 或持久 worker lease；这些是下一步 scheduler hardening。

## 2026-07-02 Async Turn Session Concurrency Guard Slice

Product alignment:

- 推进 Codex/Zcode 式任务运行语义：同一个 session 现在不会同时接受多个 async Agent turn，避免 Desktop/CLI/TUI 连点、重连或重复提交时把一个会话跑成多个并发 provider/tool loop。
- 本轮不做完整 worker queue、不做排队调度、不做跨进程继续执行、不推 GitHub；只补 App Bridge 的 active turn guard 和冲突响应。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `register_turn_job(...)` 改为在同一把 runtime job registry mutex 中完成“检查同 session active job + 插入新 job”，避免两个 HTTP 线程同时接受同一 session 的 async turn。
  - 当同一 session 已有非 terminal job 时，`POST /api/sessions/{session_id}/turns` 的 async 请求返回 HTTP `409`，payload 包含 `error_code:"active_turn_exists"`、`accepted:false`、`existing_turn` 和已有 `turn_id/status`，入口层可以直接展示现有任务。
  - 成功路径仍返回原来的 HTTP `202` + `accepted:true`，不改变同步 turn 路径。
- `runtime/http/tests/http_runtime.rs`
  - 新增 `async_turn_rejects_second_active_turn_for_same_session`，覆盖第一条 async turn accepted、第二条同 session async turn 409、`GET /api/turns` 仍只有 1 个 running job、provider 只收到第一条请求。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|vite|openagent-desktop|smoke-packaged-app' || true
```

Evidence:

- `async_turn_returns_accepted_before_provider_completion_and_streams` 通过。
- `async_turn_rejects_second_active_turn_for_same_session` 通过：第二条同 session async turn 返回 409，`existing_turn.turn_id` 指向第一条，provider requests 长度保持 1。
- `async_turn_interrupt_cancels_provider_stream_before_completion` 通过。
- `cargo check -p openagent-http-runtime`、`cargo fmt --all -- --check`、`git diff --check` 通过。
- 无残留 OpenAgent runtime、Vite、Desktop、packaged smoke 进程。

Residual risk:

- 这是 reject-style concurrency guard，不是完整 queue；后续需要真正 worker queue、排队状态、Desktop 队列 UI 和可配置并发策略。
- Guard 只阻止当前 runtime 进程内 active worker；重启后遗留 index 会按上一 slice 标记 interrupted，不会恢复继续执行。
- 目前只约束 async turn；同步 turn 路径仍按原有阻塞语义执行。

## 2026-07-02 Desktop Jobs Inspector Slice

Product alignment:

- 推进 Codex/Zcode 式任务可观察体验：Desktop 不再只在左侧 sidebar 显示一行 active job，右侧 inspector 现在有 Jobs 面板，可以查看 running/recent job 详情、durable index 状态、时间、cancel 状态和该 turn 的 live trace，并能从详情里中断 job。
- 本轮不做完整 job dashboard、不做 durable worker queue、不做跨进程继续执行、不推 GitHub；只把上轮持久化 job index 接到用户可见产品面。

Implemented:

- `desktop/src/App.tsx`
  - `TurnJobsPayload` 增加 `index_persisted` 字段，Desktop 可以展示 runtime job index 是否持久化。
  - 新增 `selectedTurnJobId` 和 selected job 派生状态，左侧任务点击会切换 active session、聚焦 job、打开右侧 inspector。
  - 右侧 inspector 新增 `data-testid="jobs-inspector-card"` Jobs 面板：展示 running/recent/index metrics，选中 job 的 status、turn/session、started/updated/duration、cancel 状态、Stop/Refresh 操作，以及最近 turn trace event。
  - Jobs 面板复用已有 `interruptTurn(...)`，详情页 Stop 与 composer/sidebar Stop 共享同一路径。
- `desktop/src/styles.css`
  - 新增 jobs inspector 的 metrics、detail、trace、recent list 样式，保持 Codex-like 浅色密集信息面，不做厚重 dashboard。

Verification:

```bash
npm run build
node <fake-app-bridge-jobs-inspector-playwright-smoke>
git diff --check -- desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|vite|openagent-desktop|smoke-packaged-app' || true
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- fake App Bridge + Playwright smoke 通过：Desktop 拉取 `/api/turns`，左侧 `turn_running` 点击后打开右侧 Jobs inspector；Jobs 面板显示 `Index persisted` 和选中 turn；点击详情 Stop 后命中 `POST /api/turns/turn_running/interrupt` 恰好 1 次，UI 从 running 更新为 interrupted。
- smoke 无 console warning/error；无残留 OpenAgent runtime、Vite、Desktop、packaged smoke 进程。

Residual risk:

- 这是轻量 Jobs inspector，不是完整任务 dashboard；还没有多任务队列、并发 quota、可恢复 worker 执行或 job search/filter。
- Trace 只展示当前前端已加载的 live events；重启恢复的历史 job 若没有事件回放，详情会显示状态但 trace 可能为空。
- 下一步更适合补并发 turn 约束/worker queue，或继续做 MCP approval/resume 的 Desktop trace 产品面。

## 2026-07-02 Turn Job Persistent Index Slice

Product alignment:

- 推进 Codex/Zcode 式任务可恢复体验：App Bridge turn job registry 不再只存在于 runtime 进程内，async turn 状态会落到 `.openagent-runtime/turn_jobs.json`，runtime 重启后 `GET /api/turns/{turn_id}` 和 `GET /api/turns` 可以从索引恢复最近 job 状态。
- 本轮不做完整 durable worker queue、不做跨进程继续执行、不做 job dashboard、不推 GitHub；只补恢复状态和旧 terminal job prune 这条底座。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 新增 `TurnJobSnapshot` 和 `.openagent-runtime/turn_jobs.json` schema，记录 `session_id`、`turn_id`、`status`、started/updated time、cancel requested 状态。
  - `register_turn_job`、`mark_turn_job_status`、`request_turn_job_cancel` 现在会同步更新内存 registry 和持久化索引。
  - `GET /api/turns` 会合并持久化 index 与进程内 jobs；如果发现 index 里有非 terminal job 但当前 runtime 内存中没有对应 worker，会标记为 `interrupted`，避免重启后显示仍在 running。
  - `GET /api/turns/{turn_id}` 优先读内存 registry，缺失时读持久化 index，再回退 session store。
  - terminal jobs 支持 7 天 TTL prune，并限制 index 最多 200 条，避免长期运行索引无限增长。
- `runtime/http/tests/http_runtime.rs`
  - 扩展 async interrupt regression：验证 job index 落盘；手动插入旧 terminal job 后重启 runtime；断言 turn status 从 `runtime_job_index` 恢复为 `interrupted`，旧 terminal job 被 prune，事件回放仍包含 delta/interrupted 且没有 completed。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn_interrupt_cancels_provider_stream_before_completion --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|vite|openagent-desktop|smoke-packaged-app' || true
```

Evidence:

- 目标测试通过：`async_turn_interrupt_cancels_provider_stream_before_completion ... ok`。
- `cargo check -p openagent-http-runtime` 通过。
- `cargo fmt --all -- --check` 和 `git diff --check` 通过。
- 无残留 OpenAgent runtime、Vite、Desktop、packaged smoke 进程。

Residual risk:

- 这是 durable job index，不是完整 durable scheduler：runtime 重启后可以恢复状态和展示 interrupted，但不会恢复/继续执行旧 worker。
- provider HTTP request 仍是 cooperative cancel，长时间没有 SSE frame 时后台 reader 仍可能等到下一帧或 timeout 才真正退出。
- 下一步更适合做 Desktop job detail/active inspector，或者补并发 turn 约束与 worker queue。

## 2026-07-02 Desktop Active Jobs Sidebar Slice

Product alignment:

- 推进 Codex/Zcode 式任务树体验：Desktop 现在消费 `GET /api/turns`，左侧 sidebar 新增“任务”小节，可以展示 active/recent turn job，并能直接从任务行中断可运行 job。
- 本轮不做 durable scheduler、不做完整任务管理 dashboard、不改 Rust runtime、不推 GitHub；只把上一轮 App Bridge job list API 接到 Desktop 产品面。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `TurnJobSummary` / `TurnJobsPayload` 类型和 job helper，用于判断 terminal/interruptible、标准化 payload、乐观 upsert job。
  - 新增 `turnJobs` state 和 `refreshTurnJobs()`，主 `refresh()`、SSE `refreshFromEvents()`、prompt submit 后 scheduled refresh 都会同步 `/api/turns`。
  - prompt submit 收到 async `turn_id` 后乐观插入 running job，让 sidebar 立即出现任务。
  - `interruptActiveTurn` 拆为通用 `interruptTurn(turnId)`，composer stop 按钮和 sidebar job stop 按钮复用同一 interrupt 路径。
  - 左侧 rail 新增 `data-testid="turn-jobs-section"` 的“任务”小节：running job 显示 session label、turn id 和 stop icon；terminal job 显示 completed/failed/interrupted 状态。
- `desktop/src/styles.css`
  - 新增 job row/job stop button/job status 样式，保持浅色 Codex-like sidebar 密度，不做厚重 dashboard 卡片。

Verification:

```bash
npm run build
node <fake-app-bridge-jobs-playwright-smoke>
git diff --check -- desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' \
  desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
pgrep -fl 'vite|openagent-http-runtime|openagent-desktop|smoke-packaged-app' || true
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- fake App Bridge + Playwright smoke 通过：Desktop 初始拉取 `GET /api/turns`，sidebar “任务”小节显示 `Jobs Smoke` running job 和 stop button；点击 stop 后命中 `POST /api/turns/turn_job_running/interrupt` 恰好 1 次，UI 更新为 `interrupted` 且 stop button 消失。
- smoke 中 `/api/turns` 被拉取 5 次，无 console warning/error。
- 无残留 Vite/App Bridge/Desktop/smoke 进程。

Residual risk:

- Desktop 现在能消费进程内 job list，但 runtime job registry 仍未持久化；重启后 sidebar 不会恢复历史 active jobs。
- 侧栏任务小节是轻量产品入口，还不是完整 job dashboard；后续仍需要 durable job index、TTL/prune、job detail panel、并发 turn 约束。

## 2026-07-02 App Bridge Turn Job List API Slice

Product alignment:

- 推进 Codex/Zcode 式运行中任务可观测性：App Bridge 不再只能按 `turn_id` 查单个 turn 状态，现在有统一 `GET /api/turns` 列表 API，CLI/TUI/Desktop 可以共用它来展示 active/interrupted/completed job。
- 本轮不做跨进程 durable worker queue、不做 Desktop job 面板、不做并发 quota、不推 GitHub；这是 durable scheduler 和产品 UI 之前的共享状态面。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 新增 `GET /api/turns`，返回当前 runtime job registry 的 `turns`、`count`、`running_count`、`terminal_count`、`filters` 和 `source`。
  - 支持 `session_id`/`session`、`status`、`active=true` 查询过滤，便于 Desktop/CLI/TUI 各自按当前 session 或 active jobs 拉取。
  - protocol manifest 的 `turns` endpoint 补充 `GET /api/turns`。
- `runtime/app-server-client/src/app_bridge_client.rs`
  - 新增 `RemoteRuntimeClient::turns()` 与 `turns_for_session(...)`，让共享 client 不需要各入口层手写 HTTP path。
  - fixture request shapes 增加 `/api/turns?session_id=session_existing`。
- `runtime/http/tests/http_runtime.rs`
  - 在 async interrupt regression 中新增列表 API 断言：accepted 后 `running_count=1` 且 turn 出现在列表；interrupt 后 `running_count=0`、`terminal_count=1`、status 为 `interrupted` 且 `cancel_requested=true`。
- `tests/golden/rust_rewrite/app_bridge_tui.json`
  - 同步新增 shared client request shape golden。

Verification:

```bash
cargo test -p openagent-http-runtime async_turn_interrupt_cancels_provider_stream_before_completion --test http_runtime -- --nocapture
cargo test -p openagent-app-server-client --lib -- --nocapture
cargo test -p openagent-app-server-client --test remote_runtime -- --nocapture
cargo check -p openagent-http-runtime -p openagent-app-server-client
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs runtime/app-server-client/src/app_bridge_client.rs tests/golden/rust_rewrite/app_bridge_tui.json progress.md .goal/state.md
```

Evidence:

- async interrupt targeted test 通过：`GET /api/turns?session_id=<session>` 能看到 running job，interrupt 后同一列表显示 terminal interrupted job，并保留 `cancel_requested=true`。
- app-server-client lib tests 通过；remote runtime golden test 在同步 request shape 后通过。
- `cargo check`、`cargo fmt --check` 和 `git diff --check` 均通过。

Residual risk:

- 这是 runtime 进程内 job registry 的列表 API，不是完整 durable scheduler；runtime 重启后仍只能通过单个 turn fallback 查 session store，列表不会恢复历史 jobs。
- terminal jobs 目前留在内存中，后续需要 TTL/prune 或持久化索引，避免长期运行后列表膨胀。
- Desktop 还没有消费 `/api/turns` 做 active job 面板；下一步可以把它接到右侧 inspector 或 sidebar task section。

## 2026-07-02 Desktop Active Turn Interrupt UI Slice

Product alignment:

- 推进 Codex/Zcode 式 Desktop 交互：composer 在 turn 运行时从发送按钮切换为停止按钮，用户可以从 UI 直接中断当前 active turn，而不是只能等后台 provider/tool loop 自然结束。
- 本轮不做 durable queue、不做复杂 job dashboard、不改 CLI/TUI、不推 GitHub；只补 Desktop 到 App Bridge interrupt route 的可验证点击闭环。

Implemented:

- `desktop/src/App.tsx`
  - `turn/interrupted` 现在映射为独立 `interrupted` stream state，timeline/composer 会显示“已中断”，不再混同为 failed。
  - 新增 `activeTurnIdFromEvents(...)`，从 `turn/started`、stream delta、tool/approval/question 事件中识别当前 active turn，并在 completed/failed/interrupted 后清空。
  - composer submit 在 async `/turns` 返回 `turn_id` 后记录 `activeTurnId`。
  - 新增 `interruptActiveTurn(...)`，调用 `POST /api/turns/{turn_id}/interrupt`，合并返回事件，刷新 session/trust/message 状态，并清除停止态。
  - composer 发送按钮在 running/streaming/waiting approval/waiting question 且存在 active turn 时切为停止按钮，使用 `Square` icon、`button` type 和明确 aria/title。
- `desktop/src/styles.css`
  - 新增 `.send-button.stop-button` 和 disabled 样式，让停止态与发送态清晰区分，并避免 interrupt 请求进行中重复点击。

Verification:

```bash
npm run build
node <fake-app-bridge-playwright-smoke>
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' \
  desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
```

Evidence:

- `npm run build` 通过：`tsc && vite build` 成功。
- fake App Bridge + Playwright smoke 通过：Desktop 打开 Vite 页面后，输入 prompt 点击发送，按钮切换为 `.send-button.stop-button`；点击停止后命中 `POST /api/turns/turn_ui_interrupt/interrupt` 恰好 1 次，UI 显示“已中断”，按钮回到 `aria-label="Run prompt"`。
- smoke 期间无 console warning/error，fake bridge request count 为 52，最终 fake turn state 为 `interrupted`。

Residual risk:

- UI 已能中断进程内 active turn，但 backend cancel 仍是 cooperative/in-memory，不是完整 durable job scheduler；runtime 重启后 job registry 仍不可恢复。
- Desktop 仍缺更完整的 active job list、历史 job 状态页和多 turn 并发控制；这属于后续产品化层。

## 2026-07-02 Async Turn Cooperative Interrupt Slice

Product alignment:

- 推进 Rust Agent Runtime / App Bridge 的 interrupt/cancel 语义：async turn 不再只是后台线程跑到底，`POST /api/turns/{turn_id}/interrupt` 会设置进程内 cancel token，provider streaming loop 会协作退出，避免出现“用户已 interrupt 但 turn 继续 completed”的产品错觉。
- 本轮不做跨进程重启恢复、不做完整 durable queue、不改 Desktop UI、不推 GitHub；这是 durable job scheduler 之前的可验证取消语义底座。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 新增进程内 turn job registry：记录 `session_id`、`turn_id`、`status`、started/updated/cancel timestamp 和 `Arc<AtomicBool>` cancel token。
  - `start_turn_async_payload` 接受 async turn 时注册 job；后台线程结束时把 job 标记为 completed/failed/interrupted。
  - 新增 `GET /api/turns/{turn_id}`，优先返回 runtime job registry 状态；job 不在内存时回退 session store 的 `run.json` / `summary.json`。
  - `POST /api/turns/{turn_id}/interrupt` 先设置 cancel token；即便 run 目录尚未创建，也能通过 job registry 找到 session。
  - provider SSE stream reader 在每个 SSE JSON frame 前检查 cancel token；命中后返回 `turn interrupted`，provider loop 转为 `turn/interrupted`，不再追加 `turn/completed`。
  - interrupted finalizer 去重 `turn/interrupted` terminal event，减少 interrupt endpoint 和 provider thread 的竞态重复。
  - App Bridge protocol manifest 增加 `turn: GET /api/turns/{turn_id}`。
- `runtime/app-server-client/src/app_bridge_client.rs`
  - 新增 `RemoteRuntimeClient::turn_status(...)`，让 CLI/TUI/Desktop 共享新状态 API。
- `runtime/http/tests/http_runtime.rs`
  - 新增 `async_turn_interrupt_cancels_provider_stream_before_completion`：本地 fake provider 先发 delta 后延迟 completed；测试在 delta 出现后 interrupt，断言 status 为 interrupted，SSE 含 `item/agentMessage/delta` 和 `turn/interrupted`，且不含 `turn/completed`。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime async_turn_interrupt_cancels_provider_stream_before_completion --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime async_turn_returns_accepted_before_provider_completion_and_streams --test http_runtime -- --nocapture
cargo test -p openagent-app-server-client --lib -- --nocapture
cargo check -p openagent-http-runtime -p openagent-app-server-client
cargo fmt --all -- --check
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs runtime/app-server-client/src/app_bridge_client.rs progress.md .goal/state.md
```

Evidence:

- 新 interrupt test 通过：slow streaming provider 的 first delta 已进入 App Bridge；interrupt 后 turn status 为 `interrupted`，event methods 含 `turn/interrupted`，不含 `turn/completed`。
- async streaming 回归通过：未 interrupt 的 async turn 仍返回 202，并最终 streaming delta + completed。
- app-server-client lib tests 通过，新增 `turn_status` 不破坏 live SSE client。
- `cargo check` 与 `cargo fmt --check` 通过。

Residual risk:

- 这是进程内 cooperative cancel，不是完整 durable job scheduler：runtime 进程重启后无法恢复 job registry，也没有并发 turn quota、持久 worker queue 或 provider HTTP request 主动 abort handle。
- 取消检查发生在 provider SSE frame 到达时；如果 provider 长时间不发 frame，interrupt event 已写入，但后台 reader 仍可能等到 HTTP read timeout 或下一个 frame 才真正退出。
- Desktop UI 还没有专门展示 job status/cancel progress；CLI/TUI 已能通过现有 interrupt route 和新增 shared client status API 接入。

## 2026-07-02 Packaged App Real Sub2API Streaming Smoke Slice

Product alignment:

- 推进 Release/Desktop 产品形态战线：验证真实 macOS `.app` 经 LaunchServices 启动后，能通过 bundle 内 Rust App Bridge 调用公网 Sub2API `gpt-5.4-mini`，并在 Desktop 中展示真实 provider streaming + persisted assistant message。
- 本轮不做签名、公证、Windows、auto-update，不推 GitHub；只补真实模型 packaged smoke、脱敏输出和可靠截图证据。

Implemented:

- `desktop/scripts/smoke-packaged-app.mjs`
  - 新增 `--workflow=real-streaming`，默认模型锁到 `gpt-5.4-mini`，默认 Base URL 为 `http://47.116.192.3/v1`，API key 从环境或 `.openagent/openagent.env` 读取但只输出 `set(len=...)`。
  - real workflow 会先检查 `/api/models?check=true`，再创建真实 session，异步启动 `/turns`，要求 runtime events 包含 `item/agentMessage/delta` 和 `turn/completed`，并要求 `/messages` 有持久化 assistant text。
  - marker 只作为 evidence 字段 `marker_begin_seen` / `marker_end_seen`，不再作为真实模型 smoke 的唯一完成条件；避免模型轻微偏离格式导致误判产品链路失败。
  - `bridgeEvents` 支持 `live_timeout_ms`，便于真实 turn 期间短轮询事件。
  - 截图改为通过 Swift/CoreGraphics 找 OpenAgent 窗口 ID，再用 `screencapture -l<window_id>` 截 OpenAgent 窗口，避免被其他 macOS 前台窗口遮挡；找不到窗口时才退回 frontmost 校验。
- `desktop/package.json`
  - 新增 `npm run smoke:packaged-app:real-streaming`。

Verification:

```bash
node --check scripts/smoke-packaged-app.mjs
npm run smoke:packaged-app:real-streaming
npm run build
awk '/[ \t]$/{print FILENAME ":" FNR ": trailing whitespace"; bad=1} END{exit bad}' \
  desktop/scripts/smoke-packaged-app.mjs desktop/package.json
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app' || true
launchctl getenv OPENAGENT_WORKSPACE
launchctl getenv OPENAGENT_SESSION_ROOT
launchctl getenv OPENAGENT_DESKTOP_AUTH_TOKEN_PATH
launchctl getenv OPENAGENT_BRIDGE_PORT
```

Evidence:

- `npm run smoke:packaged-app:real-streaming` 通过：LaunchServices 启动真实 `OpenAgent.app`，health `ok:true/auth_required:true/service:"openagent-http-runtime"`，runtime 来自 `OpenAgent.app/Contents/Resources/openagent-http-runtime`。
- Sub2API provider health 真实通过：model `gpt-5.4-mini`，model_count `18`，configured_model_available `true`，base_url `http://47.116.192.3/v1`，key 仅输出 `set(len=67)`。
- Real workflow 通过：session `Packaged real streaming smoke`，turn elapsed 约 `2385ms`，persisted assistant text length `57`，preview 为 `OA_PACKAGED_REAL_STREAM_BEGIN` / `OA_PACKAGED_REAL_STREAM_END`。
- Runtime events 通过：event_count `15`，delta_count `13`，completed_count `1`，failed_count `0`，turn_model `gpt-5.4-mini`。
- 截图 `/tmp/openagent-packaged-real-streaming-smoke.png` 已检查：窗口级截图只包含 OpenAgent `.app`，左侧 model 为 `gpt-5.4-mini`，timeline 显示真实 user prompt 和 assistant marker 输出。
- `npm run build` 通过；无残留 `openagent-http-runtime` / `openagent-desktop` / `smoke-packaged-app` 进程；LaunchServices 注入 env 已清理。

Residual risk:

- 这条 smoke 覆盖真实 provider streaming + persistence，但不覆盖 tool approval/diff/checkpoint；后者由 fake provider packaged workflow smoke 覆盖。
- 仍缺 durable job scheduler、cancel token、并发 turn 管理、PTY terminal、MCP panel、正式项目选择器、签名/公证/auto-update/Windows packaging。

## 2026-07-02 Packaged App Workflow Smoke Slice

Product alignment:

- 推进 Rust Desktop 产品闭环：真实 macOS `.app` 经 LaunchServices 启动后，能完成 `async run -> streaming delta -> tool approval -> tool write -> diff/checkpoint -> rollback` 的端到端 workflow。
- 本轮不推 GitHub，不扩大到签名、公证、Windows、auto-update；重点是把 Codex-like Desktop 的核心工作流从 Vite preview 提升到 packaged `.app` 可重复验收。

Implemented:

- `desktop/scripts/smoke-packaged-app.mjs`
  - 新增 `--workflow=approval-rollback`，内置 OpenAI Responses-compatible fake streaming provider。
  - workflow 会创建真实 session，异步启动 turn，等待 pending approval，调用 `/api/approvals/{request_id}` allow，验证 assistant final text、workspace 文件内容、diff、checkpoint restore 和 `/api/events`。
  - 文件校验改为检查 session workspace 本地文件，避免 `/api/files` root 与 session cwd 不一致导致误判。
- `desktop/package.json`
  - 新增 `npm run smoke:packaged-app:workflow`。
- `desktop/src-tauri/src/lib.rs`
  - diagnostics 增加 `workspace_default_source`，packaged smoke 通过 `OPENAGENT_WORKSPACE` 启动时 Desktop 默认 project 与 managed bridge workspace 对齐。
- `desktop/src/App.tsx`
  - 收到 sessionChanged 事件且当前没有 active session 时，自动选择被触达 session 并刷新 messages/trust。
  - timeline/trust history 折叠同一 approval/question request 的过期 pending part，只显示最新 resolved 状态，避免完整流程跑完后 UI 还显示 `PENDING`。

Verification:

```bash
node --check scripts/smoke-packaged-app.mjs
npm run build
npm run tauri -- build --bundles app
npm run smoke:packaged-app:workflow
git diff --check -- desktop/src/App.tsx progress.md .goal/state.md
pgrep -fl 'openagent-http-runtime|openagent-desktop|smoke-packaged-app' || true
launchctl getenv OPENAGENT_WORKSPACE
launchctl getenv OPENAGENT_SESSION_ROOT
launchctl getenv OPENAGENT_DESKTOP_AUTH_TOKEN_PATH
launchctl getenv OPENAGENT_BRIDGE_PORT
```

Evidence:

- packaged workflow smoke 通过，LaunchServices 启动 `/desktop/src-tauri/target/release/bundle/macos/OpenAgent.app`，health `ok:true/auth_required:true/service:"openagent-http-runtime"`，runtime 来自 bundle resource `Contents/Resources/openagent-http-runtime`。
- workflow event methods 覆盖 `turn/started`、`item/agentMessage/delta`、`item/toolCall/started`、`turn/approval_requested`、`turn/approval_resolved`、`item/toolCall/completed`、`patch/detected`、`checkpoint/restored`。
- fake provider 收到 2 次 `/v1/responses` 请求，说明 approval allow 后 provider loop 被恢复并生成 final assistant text。
- workflow 文件先写入 `approved packaged workflow\n`，restore 到 `step_start` checkpoint 后本地文件不存在，rollback 验证通过。
- 截图 `/tmp/openagent-packaged-workflow-smoke.png` 已检查：真实 `.app` 窗口展示 `Approval · write` 为 `ALLOWED`，`workflow.txt applied` patch card 与 `Restored ckpt_...` checkpoint card 可见；旧 pending approval card 已被隐藏。
- 无残留 `openagent-http-runtime` / `openagent-desktop` / `smoke-packaged-app` 进程；LaunchServices 注入 env 已清理。

Residual risk:

- 这是 deterministic fake provider 的 packaged workflow smoke；还需要后续用真实 Sub2API 做 packaged app 长会话 smoke。
- 后台 async turn 仍不是 durable job scheduler，缺进程重启恢复、cancel token 和并发 turn 管理。
- Desktop 还缺正式项目选择器、PTY terminal、MCP panel、签名/公证/auto-update/Windows packaging。

## 2026-07-02 Async Turn + Long-Lived SSE Desktop Slice

Product alignment:

- 推进 Rust App Bridge / Desktop 第一条闭环中的真实 Agent run 语义：`POST /api/sessions/{id}/turns` 不再只能 blocking 等完整 provider/tool loop 完成，Desktop 提交任务后可以快速返回 UI，由 `/api/events` SSE 持续驱动 streaming、tool、approval、diff/checkpoint 后续刷新。
- 本轮不改 Python、不推 GitHub、不做 interrupt/cancel 深化、不重构 CLI/TUI；保持旧同步 `/turns` 兼容，只新增显式 async 模式。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `POST /api/sessions/{session_id}/turns` 新增 async route 语义：body/query 中 `async=true` / `background=true` / `run_async=true` 时返回 HTTP `202`。
  - async response 立即返回 `status:"running"`、`accepted:true`、`turn_id`，后台线程复用同一 `start_turn_payload_inner` 执行 Rust provider/tool loop，事件仍写入同一 App Bridge JSONL/SSE channel。
  - 保留默认同步 `200` 行为，兼容现有 CLI/TUI/client 调用。
  - protocol manifest 的 `turns` endpoint 描述补充 async 语义。
- `runtime/app-server-client/src/app_bridge_client.rs`
  - 新增 `RemoteRuntimeClient::start_turn_async(...)`，给 CLI/TUI/Desktop 统一 client 暴露同一能力。
- `desktop/src/App.tsx`
  - composer submit 改为发送 `async:true`，只等待 accepted response；不再 await 完整 provider completion。
  - submit 后由 SSE + scheduled refresh 收敛 messages/trust/diff/checkpoint 状态。
  - `/api/events` live timeout 从 5s 提升到 300s，更接近 long-lived subscription。
- `runtime/http/tests/http_runtime.rs`
  - 新增 `async_turn_returns_accepted_before_provider_completion_and_streams`：fake streaming provider 延迟完成，断言 `/turns` 先返回 HTTP 202，随后 turn SSE 收到 `turn/started`、`item/agentMessage/delta`、`turn/completed` 和 final answer。

Verification:

```bash
cargo fmt --all -- --check
cargo test -p openagent-http-runtime async_turn_returns_accepted_before_provider_completion_and_streams --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime app_bridge_protocol_contract_and_client_live_subscription --test http_runtime -- --nocapture
cargo test -p openagent-app-server-client --lib -- --nocapture
cargo test -p openagent-http-runtime global_sse_live_tails_provider_stream_delta_before_completion --test http_runtime -- --nocapture
cargo check -p openagent-http-runtime -p openagent-app-server-client
npm run build
npm run smoke:streaming
```

Evidence:

- 新 async runtime test 通过：HTTP response 为 `202`，`status:"running"`，且在 fake provider 1.5s 延迟完成前返回；随后 turn SSE 收到 delta 和 completed，final answer 为 `streamed answer`。
- 旧同步 live-stream 回归仍通过，证明默认 `/turns` blocking 兼容行为没有被破坏。
- Desktop fake provider smoke `npm run smoke:streaming` 通过：provider request body 仍有 `stream=true`，首个 delta 比 completion 早约 `2500ms`，page final state `rawDeltaRows=0`、final assistant text rendered，persisted assistant text 为 `streamed answer`。
- `openagent_process_count=0`，无残留 runtime/desktop/vite/smoke 进程。

Residual risk:

- 后台 async thread 还不是完整 job scheduler：没有 durable worker queue、进程重启恢复、并发 turn 限制或 cancel token 深化。
- Desktop 仍是 fetch+SSE loop，不是专门的 turn-scoped subscription UI；packaged `.app` 长会话 async smoke 还没单独覆盖。

## 2026-07-02 LaunchServices Packaged GUI Smoke Slice

Product alignment:

- 推进 Release/Desktop 产品形态战线：验证 OpenAgent 不只是能直接执行 `.app/Contents/MacOS/openagent-desktop`，而是能通过 macOS LaunchServices/Finder 风格启动真实 `.app` 窗口，并由 Tauri command 拉起 bundle 内 Rust App Bridge。
- 本轮不做签名、公证、Windows、auto-update，不推 GitHub；只补 GUI 启动 smoke、截图证据和启动期错误收口。

Implemented:

- `desktop/scripts/smoke-packaged-app.mjs`
  - 新增 `--launch=launchservices`，通过 `open -n OpenAgent.app` 启动真实 app bundle。
  - LaunchServices 路径使用 `launchctl setenv` 短暂注入临时 workspace、session root、token path、bridge port、provider env 和 strict bundle runtime 开关，并在 finally 恢复/清理，不泄露 key。
  - 新增 `--screenshot=<path>`，启动后激活 OpenAgent 并截取稳定态窗口证据。
  - 新增按 port 清理 `openagent-http-runtime` 的兜底，修复首轮 smoke 发现的 LaunchServices app 退出后 bridge 子进程残留问题。
- `desktop/package.json`
  - 新增 `npm run smoke:packaged-app:launchservices`。
- `desktop/src/App.tsx`
  - Tauri runtime 下只有 managed bridge running 后才启动 App Bridge API refresh/SSE/session trust refresh，避免旧 localStorage bridge/token 在启动早期产生 `Load failed`。
  - WebKit `Load failed` 归入启动期可忽略 fetch 错误；provider catalog 探测失败降级到 provider 状态，不再污染全局 composer 错误。

Verification:

```bash
npm run tauri -- build --bundles app
npm run smoke:packaged-app:launchservices
npm run smoke:packaged-app
node --check scripts/smoke-packaged-app.mjs
git diff --check -- desktop/src/App.tsx desktop/scripts/smoke-packaged-app.mjs desktop/package.json progress.md .goal/state.md
```

Evidence:

- `npm run smoke:packaged-app:launchservices` 通过，launch mode 为 `launchservices`，health `ok:true/auth_required:true/service:"openagent-http-runtime"`，runtime 来自 `OpenAgent.app/Contents/Resources/openagent-http-runtime`，token 仅输出 `set(len=75)`。
- GUI 截图 `/tmp/openagent-launchservices-smoke.png` 已检查：macOS 菜单栏显示 OpenAgent，独立 `.app` 窗口展示 Codex-like shell，sidebar project online，composer 无 `Load failed`。
- direct packaged smoke `npm run smoke:packaged-app` 仍通过。
- `openagent_process_count=0`，无残留 `openagent-desktop`、`openagent-http-runtime`、`smoke-packaged-app`；LaunchServices 注入的 `OPENAGENT_*` / `OPENAI_*` env 无残留。

Residual risk:

- 还没做 code signing、notarization、auto-update、crash logs、Windows packaging。
- 截图是空会话启动态；长会话 timeline / tool cards / approval dock 的 packaged app 截图还需要后续单独覆盖。

## 2026-07-02 Packaged Runtime Resource Bundle Slice

Product alignment:

- 推进 Release/Desktop 产品形态战线：packaged `.app` 不再依赖 repo `target/debug` 或 `OPENAGENT_HTTP_RUNTIME` 环境变量才能拉起 Rust App Bridge。
- 本轮不做签名/notarization、不做 Windows、不推 GitHub；只把 Rust runtime binary 纳入 macOS app bundle 并做 strict smoke。

Implemented:

- `desktop/src-tauri/tauri.conf.json`
  - `beforeBuildCommand` 改为 `npm run build && cargo build -p openagent-http-runtime --release`。
  - `bundle.resources` 新增 `../../target/release/openagent-http-runtime -> openagent-http-runtime`，打包进 `.app/Contents/Resources/openagent-http-runtime`。
- `desktop/src-tauri/src/lib.rs`
  - `bridge_binary_candidates()` 保持 env + bundle lookup 优先。
  - 新增 `OPENAGENT_DESKTOP_DISABLE_DEV_RUNTIME_FALLBACK`：开启后不再回退到 repo `target/{debug,release}` 或 PATH。
- `desktop/scripts/smoke-packaged-app.mjs`
  - strict smoke 不再设置 `OPENAGENT_HTTP_RUNTIME`。
  - 设置 `OPENAGENT_DESKTOP_DISABLE_DEV_RUNTIME_FALLBACK=1`，因此 packaged app 只能从 env 或 bundle 找 runtime；脚本同时清掉 env runtime，实际只能用 bundle resource。
  - 断言 `Contents/Resources/openagent-http-runtime` 存在，并在输出里记录 bundled bridge path。

Verification:

```bash
npm run tauri -- build --bundles app
npm run smoke:packaged-app
cargo test --manifest-path desktop/src-tauri/Cargo.toml managed_bridge_starts_restarts_and_stops_runtime --lib -- --nocapture
npm run build
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check
git diff --check -- progress.md .goal/state.md
git diff --no-index --check /dev/null desktop/src-tauri/src/lib.rs
git diff --no-index --check /dev/null desktop/src-tauri/tauri.conf.json
git diff --no-index --check /dev/null desktop/scripts/smoke-packaged-app.mjs
git diff --no-index --check /dev/null desktop/package.json
```

Evidence:

- Tauri build 成功，日志显示 release 版 `openagent-http-runtime` 被构建后再 bundle。
- Bundle 内存在可执行资源：`desktop/src-tauri/target/release/bundle/macos/OpenAgent.app/Contents/Resources/openagent-http-runtime`，权限 `-rwxr-xr-x`，大小约 `8.98MB`。
- `npm run smoke:packaged-app` 通过，且 strict 条件为：
  - no `OPENAGENT_HTTP_RUNTIME`
  - `OPENAGENT_DESKTOP_DISABLE_DEV_RUNTIME_FALLBACK=1`
  - health `ok:true`
  - `auth_required:true`
  - `service:"openagent-http-runtime"`
  - token redacted `set(len=75)`
  - output includes bundled bridge resource path。
- 没有残留 `openagent-desktop`、`openagent-http-runtime`、`smoke-packaged-app` 进程。

Residual risk:

- 仍是直接启动 `.app/Contents/MacOS/*`，不是 Finder/LaunchServices 双击 smoke，也没有窗口截图。
- 还没做 code signing、notarization、auto-update、crash logs、Windows packaging。

## 2026-07-02 Packaged Tauri App Bridge Smoke Slice

Product alignment:

- 推进 Release / Desktop 产品形态战线：验证 OpenAgent 不只是 Vite/web preview 能跑，而是 macOS packaged `.app` 能启动，并通过 Tauri command 拉起 Rust App Bridge。
- 本轮不做签名/notarization、不做 Windows、不推 GitHub；只补 packaged app smoke 和一个默认 bridge port 配置点。

Implemented:

- `desktop/src-tauri/src/lib.rs`
  - 新增 `OPENAGENT_BRIDGE_PORT` 支持；`default_bridge_url()`、`stopped_bridge_status()`、managed bridge start 默认端口都走同一个 `default_bridge_port()`。
  - 默认行为仍是 `8787`，但 smoke/测试可以用随机端口，避免撞本机已有 Bridge。
- `desktop/scripts/smoke-packaged-app.mjs`
  - 启动真实 bundle `/desktop/src-tauri/target/release/bundle/macos/OpenAgent.app/Contents/MacOS/*`。
  - 使用临时 HOME/workspace/session root/token path 和随机 `OPENAGENT_BRIDGE_PORT`。
  - 等待 Tauri Desktop 生成 `oa_desktop_*` token，再验证 Rust App Bridge `/api/health` 返回 `ok:true` 且 auth enabled。
  - 输出 token 只显示 `set(len=...)`，不泄露内容。
- `desktop/package.json`
  - 新增 `npm run smoke:packaged-app`。

Verification:

```bash
npm run tauri -- build --bundles app
npm run smoke:packaged-app
cargo test --manifest-path desktop/src-tauri/Cargo.toml managed_bridge_starts_restarts_and_stops_runtime --lib -- --nocapture
npm run build
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml -- --check
git diff --check -- progress.md .goal/state.md desktop/src-tauri/src/lib.rs
git diff --no-index --check /dev/null desktop/src-tauri/src/lib.rs
git diff --no-index --check /dev/null desktop/scripts/smoke-packaged-app.mjs
git diff --no-index --check /dev/null desktop/package.json
```

Evidence:

- Tauri build 成功产物：`desktop/src-tauri/target/release/bundle/macos/OpenAgent.app`。
- `npm run smoke:packaged-app` 通过：
  - app process started
  - random bridge URL example `http://127.0.0.1:55054`
  - `/api/health` returned `ok:true`
  - `auth_required:true`
  - `service:"openagent-http-runtime"`
  - `ui_enabled:false`
  - token redacted as `set(len=75)`
- Managed bridge Rust test 通过，覆盖 start/restart/stop + auth health。
- 没有残留 `openagent-desktop`、`openagent-http-runtime`、`smoke-packaged-app` 进程。

Residual risk:

- 这是直接启动 `.app/Contents/MacOS/*` 的 packaged smoke，不是 Finder/LaunchServices 双击路径，也没有做窗口截图。
- Bridge binary 仍依赖 repo `target/debug/openagent-http-runtime` 或显式 `OPENAGENT_HTTP_RUNTIME`，还没有把 runtime binary 作为 app bundle resource 打进去。
- 尚未做 code signing、notarization、auto-update、crash logs、Windows packaging。

## 2026-07-01 Desktop Real Sub2API Streaming Smoke Slice

Product alignment:

- 推进第一条产品闭环里的真实模型流式输出：验证 `gpt-5.4-mini` 通过 Rust App Bridge 到 Desktop UI，而不是只走 fake provider。
- 本轮不改 Python、不提交、不推远端；只修 Desktop streaming 呈现与 smoke 诊断。

Implemented:

- `desktop/scripts/smoke-real-streaming.mjs`
  - `smoke:streaming:sub2api` 真实模式强制在 Desktop model picker 选择 `gpt-5.4-mini`，避免本地 env 默认 `gpt-5.5` 污染验证。
  - 真实 prompt 调整为 12 行 marker 输出，保证 Sub2API 有足够 delta 可观测。
  - 失败诊断新增脱敏 runtime event summary、persisted assistant summary、page streaming summary；API key 只输出 `set(len=67)`。
  - 成功路径也断言 runtime `turn_model == gpt-5.4-mini`、`delta_count > 0`、`completed_count > 0`。
- `desktop/src/App.tsx`
  - SSE reader 对 `item/agentMessage/delta` 逐帧让步，让批量到达的真实 SSE frame 也能被浏览器绘制成 streaming draft。
  - 同步 `/turns` 返回中包含 delta 时，先绘制短暂 draft，再刷新持久化 transcript，避免真实 provider 过快完成导致 UI 看不到流式态。

Verification:

```bash
npm run smoke:streaming:sub2api
npm run smoke:streaming
cargo test -p openagent-http-runtime provider_sse_stream_stops_on_responses_completed_without_done --lib -- --nocapture
npm run build
git diff --check -- progress.md .goal/state.md runtime/http/src/http_runtime.rs
git diff --no-index --check /dev/null desktop/src/App.tsx
git diff --no-index --check /dev/null desktop/scripts/smoke-real-streaming.mjs
```

Evidence:

- `npm run smoke:streaming:sub2api` 通过，真实 provider config 为 `http://47.116.192.3/v1` + `gpt-5.4-mini` + `responses`，key 已脱敏。
- Runtime events：`event_count=43`，`delta_count=41`，`completed_count=1`，`failed_count=0`，`turn_model=gpt-5.4-mini`。
- Desktop page final state：raw delta rows 未泄漏，Vite overlay false，final assistant text rendered，`OA_REAL_STREAM_END` 可见。
- Persisted messages：assistant text length `186`，包含 `OA_REAL_STREAM_BEGIN` 到 `OA_REAL_STREAM_END`。
- Fake provider regression 仍通过，provider request body `stream=true`，首个 delta 比 completion 早约 `2504ms`。

Residual risk:

- 这仍是 Vite/web preview Desktop smoke，不是 packaged Tauri `.app` 真窗口 smoke。
- 当前 `/turns` endpoint 仍是同步返回；Desktop 通过 SSE + returned events 都能显示 draft，但更理想的产品语义是后续把 turn execution 做成真正 async run + long-lived event subscription。

## 2026-07-01 Desktop Codex Reference UI Polish Slice

Product alignment:

- 按 William 截图反馈“UI 太丑，模仿 Codex”，继续把 Desktop 收敛到 Codex/Zcode 类产品形态。
- 本轮只改 Desktop React/CSS 视觉和一个 streaming draft 收口，不改 Rust runtime/CLI/TUI 语义、不提交、不推远端。

Implemented:

- `desktop/src/App.tsx`
  - 顶部项目 icon 从 database 换成更接近 Codex 的 panel/icon；项目列表 icon 换成 folder。
  - 顶部右侧状态从调试感较强的 provider/plug 文案状态，改成轻量 icon buttons，状态保留在 tooltip。
  - `activeStreamingDraft` 增加 persisted assistant message 收口：完成后的 draft 在持久化 assistant message 出现后不再重复占位。
- `desktop/src/styles.css`
  - 重调 Codex-like token：macOS 灰色 sidebar、白色阅读区、轻 topbar、居中 timeline、底部悬浮 composer。
  - 收敛 sidebar 行高、选中态、图标线宽、顶部按钮、composer 阴影和空状态比例。
  - 消息流保持 assistant 文档流、user 右侧深色气泡、tool/live event 弱化展示，降低 dashboard/card 感。

Verification:

```bash
npm run build
git diff --check -- desktop/src/App.tsx desktop/src/styles.css desktop/package.json runtime/http/src/http_runtime.rs progress.md .goal/state.md
```

Visual QA:

- 参考图 `/var/folders/6h/_xdqdq9177lcf_s0lf4kt8440000gn/T/codex-clipboard-22c6f3ce-3a42-4cbc-a832-fa64fdc81535.png` 已用 `view_image` 检查。
- Vite dev `http://127.0.0.1:5651/`，桌面 `2048x1240` 截图 `/tmp/openagent-ui-after.png` 已用 `view_image` 检查：sidebar/topbar/composer 更接近 Codex 壳，右上状态区不再像调试面板。
- 移动 `390x844` 截图 `/tmp/openagent-ui-mobile.png` 已用 `view_image` 检查，`document.documentElement.scrollWidth > clientWidth` 为 `false`。

Residual risk:

- 本轮是 Vite/web preview 视觉验证，未做 packaged Tauri `.app` 真窗口截图。
- 当前截图为空项目/空会话态；真实长会话内容态还需要后续用 persisted messages 再按 Codex 截图细调 typography 和事件行节奏。

## 2026-07-01 Desktop Real App Bridge Streaming Smoke Slice

Product alignment:

- 推进第一条产品闭环里的“Agent 流式输出”：把上一轮 Desktop delta draft 从前端 mock SSE 验证升级为真实 Rust App Bridge + Desktop 的可重复垂直 smoke。
- 本轮不改 Rust runtime 语义、不碰 Python、不提交、不推远端；新增的是产品级回归验证入口。

Implemented:

- `desktop/scripts/smoke-real-streaming.mjs`
  - 启动一个本地 OpenAI-compatible fake streaming provider，支持 `/v1/models` 和 `/v1/responses` SSE。
  - 启动当前 Rust `openagent-http-runtime`，使用 fake provider、`OPENAI_WIRE_API=responses`、`OPENAGENT_PROVIDER_STREAM=1`。
  - 启动 Vite Desktop，预置真实 App Bridge URL、Bearer token 和临时 project。
  - 通过 Playwright 提交 prompt，并断言 provider completion 前 Desktop 已出现 `[data-testid="streaming-assistant-draft"]`。
  - completion 后断言 draft 消失、最终 `streamed answer` 渲染、raw `agentMessage delta` protocol rows 没泄漏到 timeline、Vite overlay/console/page errors 为空。
- `desktop/package.json`
  - 新增 `npm run smoke:streaming`。

Verification:

```bash
npm run smoke:streaming
npm run build
git diff --no-index --check /dev/null desktop/package.json
git diff --no-index --check /dev/null desktop/scripts/smoke-real-streaming.mjs
git diff --no-index --check /dev/null desktop/src/App.tsx
git diff --no-index --check /dev/null desktop/src/styles.css
```

Evidence:

- `smoke:streaming` 通过；fake provider 收到 `/v1/responses`，request body 中 `stream=true`。
- 首个 provider delta 比 completion 早约 `2501ms`，脚本在 completion 前观测到 Desktop streaming draft。
- completion 后页面状态：`rawDeltaRows=0`、`draftVisible=false`、`overlayVisible=false`、`bodyHasFinal=true`。
- 截图证据：`/var/folders/6h/_xdqdq9177lcf_s0lf4kt8440000gn/T/openagent-desktop-real-stream-1782913472406.png`。

Residual risk:

- 这是 fake provider + 真实 App Bridge/Desktop 的 deterministic smoke，不消耗真实 Sub2API；真实 Sub2API streaming 仍建议后续单独跑一次带真实网络的 smoke。
- 截图是最终态；脚本用时间断言证明 draft 出现在 completion 前，但没有保存中间 draft 截图。

## 2026-07-01 Desktop Codex UI Simplification Follow-up

Product alignment:

- 按 William 最新截图反馈继续收敛 Desktop 到 Codex 产品形态：更像 macOS 原生侧栏 + 白色阅读流 + 底部悬浮 composer。
- 本轮只改 Desktop React/CSS 视觉和空状态行为，不改 Rust runtime/CLI/TUI 语义、不提交、不推远端。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `showComposerContext` 派生状态：只有在有项目/会话/运行态/streaming draft/approval-dock 时才显示 composer 上方状态条。
  - 空状态不再强行显示“新任务”step pill 和 context bar，避免底部像控制台面板。
  - composer textarea 起始行数从 3 行降到 2 行，让输入器高度更接近 Codex。
- `desktop/src/styles.css`
  - 收窄和轻量化 rail/topbar/timeline/composer 的 token：侧栏更浅、边线更弱、正文列更克制、composer 阴影和高度更接近参考。
  - 增加 bare composer 状态，空项目/空会话时只保留干净的底部输入框。
  - 保持移动端单栏，侧栏隐藏，composer 在 390px 宽度下不溢出。

Verification:

```bash
npm run build
# desktop/
git diff --no-index --check /dev/null desktop/src/App.tsx
git diff --no-index --check /dev/null desktop/src/styles.css
```

Visual QA:

- Browser/IAB 打开 `http://127.0.0.1:5174/`，桌面 1280x720 截图检查：rail 宽 306px，topbar 高 52px，bare composer 宽 850px/高 124px，空状态 context bar count 为 0，console warnings/errors 为空。
- 临时切到 390x740 移动视口：rail `display:none`，composer 宽 358px/高 122px，`document.body.scrollWidth == 390`，console warnings/errors 为空；随后恢复默认桌面视口。

Residual risk:

- 本轮仍是 Vite/web preview 视觉 QA，未做 packaged Tauri `.app` 截图验证。
- 真实会话/长 goal 内容态仍需继续对照 Codex 截图做 typography 和 timeline 微调。

## 2026-07-01 Desktop Streaming Delta Draft UI Slice

Product alignment:

- 推进第一条产品闭环里的“Agent 流式输出”：Desktop 不再把 `item/agentMessage/delta` 当协议日志逐条显示，而是聚合为 Codex-like assistant streaming draft。
- 本轮只做 Desktop 前端流式展示体验，不改 Rust runtime/CLI/TUI、不提交、不推远端。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `StreamingDraft` 与 `activeStreamingDraftFromEvents`，按 active session 的 SSE events 聚合最新未完成 turn 的 assistant delta。
  - timeline 渲染时过滤 raw `item/agentMessage/delta` live event，改为一条 `role-assistant streaming-draft` 消息行。
  - 保留非 delta 的 turn/tool/checkpoint live logs，完成后依旧由 `/messages` 持久化 transcript 接管。
- `desktop/src/styles.css`
  - 给 streaming draft 增加轻量 caret，呈现“assistant 正在生成”的视觉状态。

Verification:

```bash
npm run build
# desktop/
```

Visual/runtime QA:

- Playwright + system Chrome mock App Bridge `/api/events`：先返回 `turn/started` + 第一段 `item/agentMessage/delta`，断言 `[data-testid="streaming-assistant-draft"]` 显示 `streamed `，且 `.live-event-row` 中没有 `agentMessage delta` raw 行。
- mock completed 状态和 persisted `/messages` 后，断言 final assistant message 显示 `streamed answer`，streaming draft 消失。
- `consoleIssues=[]`，`pageErrors=[]`，临时 QA 截图已清理。
- `git diff --check -- desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md` 通过。

Residual risk:

- 本轮是 Vite/web preview 的 mock SSE 烟测；还未做真实 Sub2API streaming Desktop E2E。
- CLI/TUI streaming UI 表现未改；turn 完成后的最终 transcript 仍依赖 `/messages` refresh。

## 2026-07-01 App Bridge Provider Loop MCP Tool Execution Slice

Product alignment:

- 推进长期 goal 的 MCP/runtime 战线：MCP 不再只是在 Desktop inspector 中显示 discovery 结果，而是进入 Rust HTTP runtime provider loop，成为 Desktop/App Bridge 发起 turn 时模型可见且可执行的 tool。
- 本轮只做 App Bridge/Rust HTTP runtime 的 MCP tool execution 闭环，不做 MCP installer/auth UI、不做 Desktop 新 UI、不提交、不推远端。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 修复半截 MCP runtime 改动导致的 `config` 参数缺失编译错误。
  - provider loop 初始化时用 `register_runtime_mcp_tools` 读取 `--mcp-config` / `OPENAGENT_MCP_CONFIG`，对 enabled MCP server 执行 `tools/list` discovery，并把 MCP tool definition 注册进 `Toolkit`。
  - provider 返回 MCP function_call 时，runtime 通过同一 permission gate 后执行 `mcp_json_rpc(..., "tools/call", ...)`，把结果转换成 `ToolResult`，再作为 provider `function_call_output` 继续下一步。
- `runtime/http/tests/http_runtime.rs`
  - 新增 `remote_runtime_client_provider_loop_executes_mcp_tool` integration test，覆盖 fake provider -> MCP tool call -> mock MCP `tools/call` -> provider final answer。
  - `spawn_runtime_with_env` 清理 `OPENAGENT_MCP_CONFIG`，避免本机 MCP 配置污染测试。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime remote_runtime_client_provider_loop_executes_mcp_tool --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime app_bridge_mcp_refresh_discovers_tools_without_leaking_endpoint_secret --lib -- --nocapture
cargo check -p openagent-http-runtime -p openagent-cli -p openagent-mcp
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md .goal/state.md
```

Evidence:

- New integration test passed: first provider request contains `mcp_tool_remote_tools_echo`; mock MCP server receives `tools/list` and `tools/call`; second provider request contains `function_call_output` with `mcp echo: from-provider`; final turn answer is `mcp final answer`.
- Existing MCP refresh/sanitization path still passes.

Residual risk:

- MCP installer/auth UI、Desktop MCP trace/card polish、MCP approval resume dedicated E2E、真实 Arbor MCP smoke 仍未完成。

## 2026-07-01 Desktop Codex-like UI Polish Slice

Product alignment:

- 按 William 的 Codex 参考截图，把 OpenAgent Desktop 继续收敛成 Codex/Zcode 这种浅色桌面 agent workspace：浅灰左侧 rail、白底消息阅读流、轻量工具行、底部悬浮 composer。
- 本轮只做 Desktop 前端视觉和消息阅读体验，不改 Rust runtime/CLI，不提交、不推远端。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `TextContent` / inline renderer，消息文本支持单反引号 inline code 和 fenced code block，不再把所有内容当纯文本。
  - `EventContent` 和 session message 渲染统一走新文本组件。
  - 工具 part 只在 error 时展开 pre，正常 output 留在轻量 summary 行，避免消息流出现突兀的大黑块。
- `desktop/src/styles.css`
  - 调整 Codex-like tokens：更宽正文列、更接近参考的浅灰 rail、白底阅读流、轻量选中态和阴影。
  - 增加消息流左侧细虚线进度 rail、inline code pill、fenced code block 样式。
  - 收敛 composer / context bar / step pill 的圆角、阴影和密度，并保留移动端折叠适配。

Verification:

```bash
npm run build
# desktop/
```

Visual QA:

- 用 Playwright + system Chrome 截取空态桌面、mock chat 桌面、390px 移动宽度。
- 用 `view_image` 对比 William 提供的 Codex 参考图和 mock chat 最终截图。
- 检查点：左侧 rail 灰阶/宽度、白底正文流、右侧用户气泡、inline code/code block、轻量工具行、底部 composer、移动端无横向溢出。

Residual risk:

- 当前视觉已接近 Codex 骨架，但没有直接复刻 Codex 的完整 Markdown/streaming transcript 组件；后续如果要 1:1，需要继续做 Markdown 列表、表格、引用、图片和工具折叠交互。
- `desktop/` 当前仍是未跟踪目录，和仓库里其它长期 goal 改动混在同一工作区；本轮没有提交。

## 2026-07-01 App Bridge MCP Refresh Discovery + Shared Runtime Slice

Product alignment:

- 推进长期 goal 的 MCP 战线：App Bridge 不再只显示 MCP 配置摘要，显式 refresh 时可以真实执行 MCP `tools/list` discovery，并把 discovered tool count 暴露给 Desktop。
- 本轮不做 App Bridge provider loop 的 MCP tool execution，也不做 MCP installer/auth UI，不提交、不推远端。

Implemented:

- `src/mcp/Cargo.toml`
  - 为共享 MCP crate 增加 blocking `reqwest`，让 discovery/JSON-RPC 不再只存在于 CLI 内部。
- `src/mcp/src/mcp_bridge.rs`
  - 新增共享 `discover_mcp_server_tools` 和 `mcp_json_rpc`，支持 HTTP/SSE body parse 与 stdio MCP JSON-RPC framing。
  - `RemoteMcpManager` 新增 `set_server_error`，refresh 失败时可以记录 server 状态和脱敏错误。
- `cli/src/prompt/mcp_runtime.rs`
  - 删除重复的 MCP discovery/stdin/http JSON-RPC 实现，改用 `openagent-mcp::{discover_mcp_server_tools,mcp_json_rpc}`。
  - 保留并恢复 `execute_mcp_tool`，Agent Loop 里的 MCP tool call 执行入口继续可用。
- `runtime/http/src/http_runtime.rs`
  - `/api/mcp` 默认仍返回低副作用配置/status 摘要。
  - `/api/mcp?refresh=true` / `?check=true` 会对 enabled server 执行 `tools/list`，更新 `tool_count`、`selected_transport`、server `connected/error` 状态和 tools descriptors。
  - refresh 错误会对 authorization/api_key/password/secret/token 相关信息做脱敏。
  - 新增 targeted test，mock MCP server 验证 `tools/list` 请求、`tool_count=1`、`selected_transport=http` 和 endpoint token 不泄露。
- `desktop/src/App.tsx`
  - MCP card refresh 改为调用 `/api/mcp?refresh=true`，Desktop 可显示真实 discovered tool_count。
- `cli/src/prompt/provider.rs` / `cli/src/prompt.rs` / `cli/src/cli.rs`
  - 清理被 MCP 共享化后遗留的旧 provider SSE dead parser 和 unused MCP imports。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_mcp_refresh_discovers_tools_without_leaking_endpoint_secret --lib -- --nocapture
cargo test -p openagent-http-runtime app_bridge_mcp_status_sanitizes_config --lib -- --nocapture
cargo test -p openagent-mcp --lib --tests -- --nocapture
cargo check -p openagent-http-runtime -p openagent-cli -p openagent-mcp
npm run build
# desktop/
cargo fmt --all -- --check
git diff --check
git diff --no-index --check /dev/null desktop/src/App.tsx
git diff --no-index --check /dev/null desktop/src/styles.css
```

Evidence:

- HTTP runtime refresh test 通过：mock MCP server 收到 `POST /mcp?token=refresh-secret` + JSON-RPC `tools/list`，App Bridge payload 返回 `status=connected`、`tool_count=1`、server `selected_transport=http`、tool `mcp_tool_remote_tools_echo`。
- Sanitization test 继续通过：默认 `/api/mcp` 不泄露 command args/env/header/url query secret。
- `cargo check` 覆盖 HTTP runtime、CLI、MCP crate，且本轮相关 warning 已清理。
- Desktop build 通过，MCP card 会走 refresh endpoint。

Residual risk:

- App Bridge HTTP provider loop 还没有把 MCP discovered tools 并入 Rust `Toolkit`，所以 Desktop 发起的 provider turn 仍不能自动调用 MCP tools。
- Desktop 现在展示 discovery 结果，不提供 MCP tool invocation UI。
- 本轮没有真实 Arbor MCP server smoke，只用 mock MCP server 证明协议路径。

## 2026-07-01 App Bridge MCP Status + Desktop Inspector Slice

Product alignment:

- 补齐长期目标中 App Bridge `/api/mcp` 和 Desktop MCP 信息面，让 MCP 不再只是 CLI 内部能力，而能进入桌面 Agent Client 的右侧 inspector。
- 本轮只做 MCP 配置/status 可视化，不做 MCP discovery/tools/list、不把 MCP tool execution 并入 Agent Loop，不提交、不推远端。

Implemented:

- `Cargo.toml`
  - 新增 workspace dependency `openagent-mcp`。
- `cli/Cargo.toml`
  - `openagent-mcp` 改为 workspace dependency，避免 CLI/HTTP 两套 path 写法漂移。
- `runtime/http/Cargo.toml`
  - HTTP runtime 接入 `openagent-mcp`。
- `runtime/http/src/http_runtime.rs`
  - `HttpRuntimeConfig` 新增 `mcp_config`。
  - CLI 新增 `--mcp-config <json-or-path>`。
  - Protocol manifest 新增 `mcp: GET /api/mcp`。
  - Router 新增 `GET /api/mcp`。
  - 新增 `mcp_payload`，复用 `openagent-mcp` 的 config parser 和 `RemoteMcpManager`，返回安全 MCP 摘要。
  - MCP payload 只显示 command 首项、args/env/header 数量、remote_url_configured、server/tool count，不泄露 env/header/url query/command args 里的 secret。
- `desktop/src/App.tsx`
  - 新增 MCP payload/server 类型与 state。
  - `refresh()` 拉取 `/api/mcp`，失败时降级为 unavailable，不阻断 sessions/provider 刷新。
  - Overview inspector 新增 MCP card，显示 source/configured/server_count/tool_count/refresh TTL 和 server rows。
- `desktop/src/styles.css`
  - 调整 `.file-row` 布局，避免 MCP server 行 title/subtitle 挤在一起。

Verification:

```bash
cargo fmt --all -- --check
cargo test -p openagent-http-runtime app_bridge_mcp_status_sanitizes_config --lib -- --nocapture
cargo check -p openagent-http-runtime -p openagent-cli
npm run build
# desktop/
git diff --check -- Cargo.toml cli/Cargo.toml runtime/http/Cargo.toml runtime/http/src/http_runtime.rs .goal/state.md progress.md
git diff --no-index --check /dev/null desktop/src/App.tsx
git diff --no-index --check /dev/null desktop/src/styles.css
```

Evidence:

- Rust targeted test 通过，覆盖 protocol manifest、router payload 和 secret marker 不泄露。
- 临时 App Bridge `127.0.0.1:18800` + MCP config smoke 返回 `configured=true`、`server_count=2`、`source=config`。
- `/api/mcp` payload grep 未出现 `smoke-secret`、`ARBOR_TOKEN`、`token=`。
- Desktop Playwright smoke 打开 inspector，`[data-testid="mcp-card"]` 显示 `arbor/docs`、`Servers 2`、`Tools 0`，card bbox 在 viewport 内，无横向 overflow，card text 未泄露 secret。
- 临时 App Bridge、MCP config 和截图已清理。

Residual risk:

- 这是配置/status 面，未执行 MCP discovery/tools/list，所以未 refresh 前 `tool_count` 仍为 0。
- MCP tools 还没有真正并入 Rust Agent Loop 的 Toolkit。

## 2026-07-01 Desktop Activity-Backed Goal Strip Slice

Product alignment:

- 推进 Codex/Zcode 桌面 Agent Client 的“底部 goal/context strip”：从静态项目状态升级为真实 session activity，让用户能看到当前/最近任务来自持久化 message，而不是纯前端占位。
- 本轮只改 Desktop 前端数据派生和展示，不改 Rust runtime/CLI/TUI 语义，不提交、不推远端。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `compactText` 和 `formatElapsed` helper。
  - 新增 30s 前端 elapsed tick，用于刷新底部 strip 的运行/最近时长。
  - 从 `messages_v2` 派生 `latestUserActivity`，从 App Bridge events 派生 `latestTurnStartedAtMs`。
  - composer context bar 现在显示“进行中的目标 / 等待处理 / 最近任务 / 当前会话 / 新任务”、最近 user prompt 摘要、elapsed + phase。
  - 给 context bar 和 activity detail 增加 `data-testid`，方便后续第一闭环 smoke 稳定断言。

Verification:

```bash
npm run build
# desktop/
# 临时 App Bridge: 127.0.0.1:18799
# Vite dev: 127.0.0.1:5173
git diff --check -- .goal/state.md progress.md
git diff --no-index --check /dev/null desktop/src/App.tsx
git diff --no-index --check /dev/null desktop/src/styles.css
```

Evidence:

- `npm run build` 通过。
- 临时 Rust App Bridge 创建真实 session/turn，`GET /messages` 返回 persisted user text：`Activity strip should show this real persisted prompt`。
- Desktop smoke 断言 `[data-testid="composer-activity-detail"]` 包含该真实 prompt，context bar 文本为 `最近任务 / Activity strip should show this real persisted prompt / 1m · 可以继续发这段`，无横向 overflow。
- Mobile smoke 断言 hidden `textContent` 包含该真实 prompt，visible `innerText` 显示 `最近任务 / 2m · 可以继续发这段`，无横向 overflow。
- 临时 App Bridge 已停止。

Residual risk:

- 移动端为节省空间隐藏长 prompt，只显示 title + elapsed/phase。
- 真实 goal step count、计划阶段和多 step 进度还没从 runtime/goal metadata 接入。
- 本轮没有做 packaged Tauri GUI click smoke。

## 2026-07-01 Desktop Codex-Like Composer / Context Polish Slice

Product alignment:

- 继续把 OpenAgent Desktop 往 Codex/Zcode 产品形态收敛：左侧项目/会话 rail、中央阅读流、底部浮动 composer 是主骨架。
- 本轮只做 Desktop 前端视觉和轻量 shell 结构，不改 Rust runtime/CLI/TUI 语义，不提交、不推远端。

Implemented:

- `desktop/src/App.tsx`
  - 在 composer 上方新增 Codex-like 当前上下文状态条，展示当前 session/project/path/stream state。
  - 状态条保留打开详情入口，让顶部右侧不承担全部 runtime 状态展示。
- `desktop/src/styles.css`
  - 调整 sidebar 灰度、hover/selected、topbar、timeline、user/assistant 消息、floating composer、状态胶囊和阴影。
  - 新增 `.composer-context-bar`，并补移动端压缩规则，避免窄屏挤爆。
  - 进一步降低 dashboard/card 感，让默认空态和输入区更接近 Codex 截图的阅读型界面。

Verification:

```bash
npm run build
# desktop/
# Vite dev: http://127.0.0.1:5173/
git diff --check -- .goal/state.md progress.md
git diff --no-index --check /dev/null desktop/src/App.tsx
git diff --no-index --check /dev/null desktop/src/styles.css
```

Evidence:

- `npm run build` 通过。
- system Chrome Playwright 截图验证桌面 `2048x1255` 与移动 `390x844`。
- 桌面：sidebar 可见、主画布白色、composer/context bar 居中浮底且在 viewport 内。
- 移动：sidebar 隐藏，topbar/composer/context bar 无横向 overflow。
- 参考 Codex 截图和改后渲染截图已用 `view_image` 人工对比。
- 临时 QA 截图和 npm 临时 lockfile 已清理。

Residual risk:

- 本轮是 Vite/web preview 视觉验证，未做 packaged Tauri GUI click smoke。
- 真实会话内容、goal 文案和消息密度还需要接 runtime 数据后继续按 Codex 截图精修。

## 2026-07-01 Desktop Startup / Restored Project Managed Bridge Auto-Sync Slice

Product alignment:

- 补齐 Desktop 第一闭环入口的启动恢复场景：打开 app 时如果已保存 active project，managed App Bridge 会自动对齐到该 workspace，而不是等用户再次点击项目。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `managedBridgeAutoSyncKey`，防止启动恢复 effect 重复触发 start/restart。
  - 新增 Tauri-only auto-sync effect：auth ready、项目/default workspace 明确且 bridge 不忙时，若 bridge stopped 则 start，若 workspace mismatch 则 restart。
  - bridge 已经对齐时同步 `bridgeUrl`，并记录 ready key。
  - managed bridge command 失败时清 auto-sync key，允许后续重试。
  - `createSession` 增加 `bridgeSwitchInProgress` guard，防止绕过 UI disabled 创建旧 workspace session。

Verification:

```bash
npm run build
# desktop/
# Browser smoke: http://127.0.0.1:5173/
git diff --check -- desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
```

Evidence:

- `npm run build` 通过。
- Browser smoke 通过：web preview 下 Tauri-only effect 未误触发，`.app-shell` / `.composer` 存在，send/new-session 未误禁用，无横向 overflow，console warnings/errors 为空。
- `git diff --check` 通过。

Residual risk:

- 本轮仍未做 packaged Tauri GUI click smoke。
- 真实 start/restart 子进程行为依赖前序 Tauri command 测试覆盖，本轮只验证 Desktop TS/build 与 web preview 非回归。

## 2026-07-01 Desktop Project Selection / Managed Bridge Workspace Sync Slice

Product alignment:

- 推进第一条产品闭环的入口：`打开 Desktop -> 选择项目` 现在会驱动 Rust managed App Bridge 切到对应 workspace，而不只是前端本地状态变化。
- 保持 Rust-first 路径：前端仍只通过 Tauri command / App Bridge 与 runtime 交互，不新增 Python 路径，不推远端。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `managedBridgeWorkspaceMismatch`、`managedBridgeBusyAny`、`bridgeSwitchInProgress` 和 `projectBridgeStatusLabel`。
  - `managedBridgeStartOptions` 支持 `workspaceOverride`，避免项目点击后 state 尚未刷新时 start/restart 仍使用旧 workspace。
  - 抽出 `runManagedBridgeCommand("start" | "restart", workspaceOverride)`，复用 `app_bridge_start` / `app_bridge_restart` Tauri command。
  - `selectProject` / `registerProject` 在更新项目状态后自动同步 managed bridge 到目标 workspace，并清空旧 session/messages/diff/checkpoint/file/git UI 状态。
  - bridge 切换期间禁用“新对话”、项目选择、发送按钮；`submitPrompt` 也会阻止任务发往旧 workspace。

Verification:

```bash
npm run build
# desktop/
# Browser smoke: http://127.0.0.1:5173/
git diff --check -- desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
```

Evidence:

- `npm run build` 通过。
- Browser smoke 通过：`OpenAgent Desktop` 页面可加载，`.app-shell` / `.composer` 存在，composer/new-session/send 各 1 个，web preview 下未误禁用，页面无横向 overflow，console warnings/errors 为空。
- `git diff --check` 通过。

Residual risk:

- 本轮是 Vite/web preview + TS build 验证，没有做 packaged Tauri GUI click smoke。
- 自动 workspace sync 依赖 Tauri command 层；managed bridge start/restart 的真实子进程行为已由前序 command 层测试覆盖，但本轮没有再跑 Tauri Rust 测试。

## 2026-07-01 App Bridge Client Incremental Live SSE Stream Slice

Product alignment:

- 目标是让 CLI/TUI/Desktop 共用 Rust App Bridge client 消费 live SSE，而不是各自手写“等 body 结束再解析”的逻辑。
- 审计发现 `runtime/http` provider streaming 主路径已存在，并已有测试证明 `item/agentMessage/delta` 早于 `turn/completed` 被 App Bridge live SSE 看到；因此本轮补消费层，不重复实现 provider streaming。

Implemented:

- `runtime/app-server-client/src/app_bridge_client.rs`
  - 新增 `RemoteRuntimeClient::global_events_live_stream(...)`。
  - 新增 `RemoteRuntimeClient::turn_events_live_stream(...)`。
  - 新增 `RemoteRuntimeClient::sse_events_live_stream(...)`。
  - 保留旧 `global_events_live` / `turn_events_live` 批量返回接口。
  - 抽出 `send_with_options`，streaming API 直接读取 `reqwest::blocking::Response` body。
  - 新增 `read_sse_response_stream`，按 SSE frame 增量解析并即时 callback。
- tests
  - 新增 `live_sse_stream_callback_receives_event_before_response_finishes`。
  - mock SSE server 先 flush `item/agentMessage/delta`，延迟 700ms 后再发 `turn/completed`。
  - client callback 必须在 400ms 内收到第一条事件，证明不是等 response 结束才返回。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-app-server-client live_sse_stream_callback_receives_event_before_response_finishes --lib -- --nocapture
cargo test -p openagent-app-server-client app_bridge_client_matches_legacy_oracle --test remote_runtime -- --nocapture
cargo test -p openagent-http-runtime global_sse_live_tails_provider_stream_delta_before_completion --test http_runtime -- --nocapture
cargo check -p openagent-app-server-client
cargo fmt --all -- --check
git diff --check -- runtime/app-server-client/src/app_bridge_client.rs .goal/state.md progress.md
```

Evidence:

- Targeted client stream callback test passed.
- Existing app-server-client golden test passed.
- Existing HTTP runtime provider-stream live SSE test passed.
- `cargo check` and formatting checks passed.

Residual risk:

- CLI/TUI have not yet switched from batch polling to the callback live stream API.
- Desktop currently still uses TypeScript fetch/SSE reader directly.
- The stream reader targets App Bridge `/api/events`; it does not special-case provider `[DONE]`, which App Bridge does not emit.

## 2026-07-01 Desktop Codex Reference UI Polish Follow-up

Product alignment:

- 对齐 William 最新给的 Codex 截图，继续把 Desktop 形态收敛成 Codex/Zcode 风格的桌面 Agentic Coding Workspace。
- 本轮只改前端视觉壳和交互状态呈现，不改 Rust runtime、CLI、TUI 语义，不提交、不推远端。

Implemented:

- `desktop/src/App.tsx`
  - composer 顶部新增 Codex-like 状态胶囊。
  - attach 改成 `+`，发送改成圆形上箭头。
  - 空 timeline 增加 `empty` class，用于隐藏非必要进度虚线。
  - 后台初始化轮询遇到 App Bridge 未启动的 `Failed to fetch` 时不再污染主输入区；用户主动操作错误仍保留。
- `desktop/src/styles.css`
  - 收敛为浅灰 sidebar、白色主画布、克制 topbar、居中 conversation、底部浮动 composer。
  - 降低 dashboard/card 感：assistant 正文更像阅读流，user 消息右侧气泡，tool/part 状态降噪。
  - 空态不显示进度虚线；移动端单栏无横向 overflow。

Verification:

```bash
npm run build
# desktop/
# Vite dev http://127.0.0.1:5173/
# system Chrome + Playwright screenshot QA: 1600x1000 and 390x844
```

Evidence:

- `npm run build` 通过。
- 桌面和移动视口 `scrollWidth == innerWidth`。
- composer 在 viewport 内，桌面 sidebar 可见，移动端 sidebar 隐藏。
- 用 `view_image` 对照 William 的 Codex 参考图和渲染截图；临时 QA 截图已清理。

Residual risk:

- 这是 Vite/web preview 视觉验证，不是 packaged Tauri GUI click smoke。
- App Bridge 未启动时浏览器 console 仍会有预期 `ERR_CONNECTION_REFUSED` 网络日志，但主 UI 不再显示 raw `Failed to fetch`。

## 2026-07-01 TUI Remote Terminal Command Slice

Product alignment:

- 上一轮已经把 terminal runner 接进 `runtime/app-server-client` 和 CLI。
- 本轮把同一条能力继续接到 remote attach / TUI command backend，推进 CLI/TUI/Desktop 共用 Rust App Bridge。
- 不做 PTY、不做持续 streaming terminal、不改 Desktop UI。

Implemented:

- `cli/src/remote.rs`
  - 新增共享 helper：
    - `remote_terminal_run_text`
    - `terminal_payload_text`
    - `slash_terminal_command`
  - interactive remote attach 命令列表新增 `/terminal <command>`。
  - remote attach loop 支持 `/terminal <command>` 和 `/term <command>`。
  - `RemoteTerminalHandler::handle_command` 支持 `/terminal <command>`，返回 `terminal` timeline lines。
  - unknown command help 新增 `/terminal <command>`。
  - terminal 请求继续复用 `openagent-app-server-client::RemoteRuntimeClient::terminal_run`。
- `cli/src/remote.rs` tests
  - 新增 `remote_terminal_command_runs_through_app_bridge_client`。
  - 测试启动极小 HTTP mock server，直接调用 TUI backend `handle_command("/terminal printf tui-terminal-ok")`。
  - 断言请求命中 `POST /api/terminal/run`、带 Bearer auth、body 包含 command，并验证 timeline output 含 `tui-terminal-ok`。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-cli remote_terminal_command_runs_through_app_bridge_client --lib -- --nocapture
cargo test -p openagent-cli binary_terminal_runs_remote_bridge_command --test cli_commands -- --nocapture
cargo fmt --all -- --check
cargo check -p openagent-cli -p openagent-app-server-client
lsof -nP -iTCP -sTCP:LISTEN | rg 'openagent|5202|8787|openagent-http-runtime' || true
```

Evidence:

- TUI backend unit path passed and proved the command goes through `/api/terminal/run`.
- Existing CLI real App Bridge terminal integration still passes.
- `cargo check` passed.
- No leftover local listener.

Residual risk:

- Still not a PTY: no stdin, resize, kill, live streaming, shell profile/env policy, or terminal approval queue.
- This verifies backend command handling, not a rendered curses screenshot.
- Older remote attach/session/task paths still partly use local HTTP helpers; follow-up migration to `RemoteRuntimeClient` remains useful.

## 2026-07-01 App Bridge Terminal Client + CLI Entry Slice

Product alignment:

- 把 Desktop 已有的 `POST /api/terminal/run` 从单一 UI 面板能力推进成 CLI 可复用能力。
- 这是朝 “CLI/TUI/Desktop 共用 Rust App Bridge client” 的方向走，不绕 Python，也不新增另一套 HTTP 协议。
- 本轮不做 PTY、交互式 shell、terminal streaming 或 Desktop UI 改动。

Implemented:

- `runtime/app-server-client/src/app_bridge_client.rs`
  - 新增 `RemoteRuntimeClient::terminal_run(command, cwd, timeout_ms)`。
  - 请求封装为 `POST /api/terminal/run`，body 包含 `command`、可选 `cwd`、可选 `timeout_ms`。
  - client fixture 增加 `terminal_run` request shape。
- `cli/Cargo.toml`
  - 新增 `openagent-app-server-client` workspace 依赖。
- `cli/src/cli.rs`
  - 注册 `openagent terminal`。
- `cli/src/help.rs`
  - root help 增加 `terminal`。
  - 新增 `terminal_help()`。
- `cli/src/remote.rs`
  - 新增 app-server-client auth 转换。
  - 新增 `terminal_command`：
    - `--server-url` / `--attach`
    - `--server-token` / `--server-token-env`
    - basic auth `-u/-p`
    - `--cwd` / `--workspace` / `--dir`
    - `--timeout-ms`
    - `--format text|json`
    - `--command <text>` 或 `-- <command...>`
  - text 模式原样输出远端 stdout/stderr。
  - json 模式输出完整 terminal payload。
  - CLI exit code 映射远端命令 exit code。
- `cli/tests/cli_commands.rs`
  - help smoke 覆盖 `terminal`。
  - 新增 `binary_terminal_runs_remote_bridge_command`，启动真实 `openagent serve`，通过 CLI 跑 terminal command。
- `tests/golden/rust_rewrite/app_bridge_tui.json`
  - 同步 `terminal_run` request shape。
  - 同步当前 `AppEvent::to_value` 输出 `event_id` 的 fixture 字段。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-cli binary_terminal_runs_remote_bridge_command --test cli_commands -- --nocapture
cargo test -p openagent-app-server-client app_bridge_client_matches_legacy_oracle --test remote_runtime -- --nocapture
cargo test -p openagent-cli binary_help_smoke_covers_legacy_command_surface --test cli_commands -- --nocapture
cargo fmt --all -- --check
cargo check -p openagent-cli -p openagent-app-server-client
lsof -nP -iTCP -sTCP:LISTEN | rg 'openagent|5202|8787|openagent-http-runtime' || true
```

Evidence:

- CLI terminal integration passed with real Rust App Bridge:
  - text command: `printf cli-terminal-ok`
  - stdout: `cli-terminal-ok`
  - json command: `pwd` with `--cwd <workspace>/nested`
  - payload: `success=true`, `cwd_relative=nested`, `exit_code=0`
- app-server-client golden passed with new terminal request shape.
- root help smoke passed with `terminal --help`.
- `cargo check` passed for `openagent-cli` and `openagent-app-server-client`.
- No leftover local App Bridge/Vite listener from this slice.

Residual risk:

- Still not a PTY: no stdin, resize, kill, background attach, long-running job streaming, shell profile/env policy, or approval for terminal commands.
- TUI has not exposed this terminal command path yet.
- Several older CLI remote paths still use local HTTP helpers; next cleanup should migrate attach/session/task paths into `RemoteRuntimeClient` as well.

## 2026-07-01 Desktop Latest Codex Reference Visual Polish Slice

Desktop product alignment:

- 按 William 最新截图继续向 Codex 桌面 app 的产品形态靠近。
- 本轮只做视觉/信息密度收敛，不改 Rust runtime、App Bridge、CLI/TUI 语义。
- 目标是把界面从“dashboard/调试面板感”压成“左侧会话树 + 白色阅读区 + 底部浮动 composer”的安静 client。

Implemented:

- `desktop/src/styles.css`
  - 重新整理设计 tokens：`--rail-width`、`--content-max`、`--composer-width`、亮色文本/边框/状态色。
  - 左侧 sidebar 改成更接近 Codex 截图的浅灰渐变、紧凑导航、轻选中态和贴底 profile。
  - 主工作区改为真实白色阅读画布，topbar 降低边框和状态色，timeline 居中且更疏朗。
  - message/tool/diff/checkpoint cards 降低卡片感，保留必要左侧状态线。
  - bottom composer 调整为更接近 Codex 的浮动输入框：轻边框、柔和阴影、圆形 send、橙色权限选择。
  - `Failed to fetch` 等预览错误被压成轻量状态行，不再抢主视觉。
  - 移动/窄屏下隐藏 sidebar，topbar、composer、error 状态保持不溢出。

Verification:

```bash
npm --prefix desktop run build
npm --prefix desktop run dev -- --port 5202
Playwright(system Chrome): screenshot 2048x1272 -> /tmp/openagent-codex-like-ui-v2.png
Playwright(system Chrome): screenshot 390x844 -> /tmp/openagent-codex-like-ui-mobile.png
view_image(reference screenshot)
view_image(/tmp/openagent-codex-like-ui-v2.png)
view_image(/tmp/openagent-codex-like-ui-mobile.png)
```

Evidence:

- Desktop build passed.
- 2048x1272 desktop screenshot: sidebar width/gray tone matches Codex reference more closely, main canvas is white, composer is centered and floating, topbar status icons are neutral gray.
- 390x844 mobile screenshot: no horizontal overflow, composer and topbar fit.
- Browser plugin was not exposed with direct navigate/screenshot tools this turn, so verification used project Playwright with system Chrome.

Residual risk:

- Vite preview did not start App Bridge, so the screenshot intentionally shows a light `Failed to fetch` state.
- This is visual polish only; real goal/session strip and populated conversation state still depend on runtime data wiring.
- Packaged Tauri GUI click smoke is still pending.

## 2026-07-01 Desktop Terminal Panel MVP Slice

Desktop product alignment:

- 右侧信息面板现在有可工作的 Terminal panel，补上目标里明确列出的 `terminal panel` 能力。
- 这是一版受控 command runner：适合短命令和诊断，不是完整 PTY。
- 后端能力落在 Rust App Bridge，不绕过 runtime，也不接 Python。

Implemented:

- `runtime/http/src/http_runtime.rs`
  - 新增 `POST /api/terminal/run`。
  - `/api/protocol` manifest 新增 `terminal_run` endpoint。
  - 命令执行限制在 App Bridge workspace 内，`cwd` 通过 `resolve_path_in_root` 校验。
  - 默认 timeout 10s，最大 30s；command 最大 4096 chars；stdout/stderr 分别最多 20000 chars 并返回 truncated 标记。
  - 返回 command、workspace、cwd、cwd_relative、success、exit_code、timed_out、duration、stdout、stderr 等字段。
  - 新增 `app_bridge_terminal_run_is_workspace_scoped` 单测。
- `desktop/src/App.tsx`
  - 新增 `TerminalRunResult` state 和 `runTerminalCommand`。
  - 右侧 Overview 增加 Terminal card，支持输入命令、Run、展示 CWD/Exit/Time/stdout/stderr。
- `desktop/src/styles.css`
  - 新增 terminal form/output 样式，stdout/stderr 使用紧凑 dark output block。

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_terminal_run_is_workspace_scoped -- --nocapture
npm --prefix desktop run build
target/debug/openagent-http-runtime --host 127.0.0.1 --port 8803 --workspace /tmp/openagent-terminal-smoke/workspace --session-root /tmp/openagent-terminal-smoke/sessions --headless
npm --prefix desktop run dev -- --port 5201
Browser rendered smoke: open Desktop -> Details -> Bridge URL 8803 -> Terminal command -> Run
```

Evidence:

- Rust test passed: `printf terminal-ok` succeeds in `nested` cwd, `cwd: ".."` returns 400.
- Desktop build passed.
- Rendered smoke passed with real Rust App Bridge and real Desktop Vite:
  - command: `printf terminal-ui-ok`
  - output: `terminal-ui-ok`
  - exit code: `0`
  - browser console logs: `[]`
  - screenshot: `/tmp/openagent-terminal-panel-smoke.png`

Residual risk:

- No PTY yet: no interactive stdin, live stream, attach, shell profile, long-running jobs, terminal resize, or kill button.
- Endpoint is Desktop-facing today; `runtime/app-server-client`/CLI/TUI wrappers are still pending.
- Terminal command execution currently relies on local shell `sh -lc` or Windows `cmd /C`; platform-specific shell policy still needs a proper adapter before release.

## 2026-07-01 Desktop Codex UI Polish Slice

Desktop product alignment:

- 主界面继续向 Codex/Zcode 产品形态靠近：左侧是会话/项目树，中间是清爽 timeline，底部是浮动 composer，右侧详情才放连接/诊断。
- 把 raw Bridge URL/Token 从侧栏移走，避免主界面看起来像调试控制台。
- 保留现有 App Bridge/Session/Review 能力，不改 runtime 语义，不推远端。

Implemented:

- `desktop/src/App.tsx`
  - 左上增加 Codex-like traffic lights、sidebar/back/forward chrome。
  - Bridge URL/Token 移入右侧 Overview 的 `Connection` card。
  - 侧栏底部只保留 profile，连接配置不再占据主导航。
- `desktop/src/styles.css`
  - 新增响应式 `--rail-width` 和 `--content-max`，让 1280/1440 宽度下侧栏不再过宽。
  - 调轻 sidebar 灰度、选中态、导航行高、topbar icon-only status、timeline 宽度和 composer 阴影。
  - 隐藏 raw project path add form 与 sidebar bridge settings，底部 profile 贴底。
  - 增加 Connection card form 样式，并验证窄屏单栏无横向溢出。

Verification:

```bash
npm --prefix desktop run build
Browser path: open http://127.0.0.1:5197/
Browser viewport: 1440x920 desktop visual QA
Browser viewport: 800x900 narrow visual QA
```

Evidence:

- Build passed.
- Default preview page logs were empty.
- 1440x920: rail `320px`, composer `850px`, horizontal overflow `false`.
- 800x900: rail hidden, composer `768px`, horizontal overflow `false`.
- Inspector opens from topbar and shows the moved Connection card with Bridge URL/Token inputs.
- Screenshots inspected:
  - `/tmp/openagent-codex-ui-polish-desktop.png`
  - `/tmp/openagent-codex-ui-polish-narrow.png`

Residual risk:

- 这轮是视觉/信息架构 polish，不是新的 Agent runtime 能力。
- 当前 QA 是 Vite/web preview，不是 packaged Tauri 窗口。
- 空态还没有 Codex 那种真实 goal strip / running-step strip，因为需要把 goal/session 数据源接进 Desktop。
- `Failed to fetch` 是预览时 App Bridge 未启动导致，不代表构建失败。

## 2026-07-01 Structured Diff / Split Review Slice

Desktop product alignment:

- Review mode now has a real split diff surface instead of only dumping unified diff text.
- App Bridge keeps the old `diff` string for compatibility while adding structured rows that Desktop/TUI/CLI clients can consume.
- This pushes the first product loop closer to a Codex/Zcode-style review experience: changed file -> structured diff -> checkpoint restore.

Implemented:

- `runtime/http/src/http_runtime.rs`
  - `public_file_change` now adds `side_by_side` when original before/after content is available.
  - Added line-level side-by-side generation with existing LCS thresholds.
  - Structured rows expose `kind`, `old_line`, `new_line`, `old`, and `new`, with row truncation metadata.
- `runtime/http/tests/http_runtime.rs`
  - Extended the file diff/undo/redo integration test to assert side-by-side rows for created and edited files.
- `desktop/src/App.tsx`
  - Review `Change Review` now prefers `latest.side_by_side.rows`.
  - Falls back to unified diff only when structured rows are unavailable.
- `desktop/src/styles.css`
  - Added split diff table styling, line numbers, added/removed row states, and a direct `.inspector.open` visibility rule.
  - The `.inspector.open` rule fixes a strict smoke failure where the Review DOM existed but the drawer bbox stayed offscreen.

Verification:

```bash
cargo fmt --all
npm --prefix desktop run build
cargo test -p openagent-http-runtime remote_runtime_client_tracks_file_diff_undo_and_redo --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
cargo build -p openagent-http-runtime
node <inline Playwright strict split-diff rendered smoke>
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
```

Evidence:

- `/api/sessions/{id}/diff` returned `latest.side_by_side.rows` with added `alpha`.
- Desktop rendered `.review-split-diff`, not `.review-diff-code`.
- Strict rendered smoke required the drawer to be actually visible:
  - inspector bbox `x=1202 width=380 right=1582`
  - split diff bbox `x=1228 width=328 right=1556`
  - body horizontal overflow `0`
- Split diff rendered 2 added rows for `alpha` and `beta`.
- Browser QA was clean: console issues `0`, page errors `0`, failed responses `0`.
- Screenshot inspected: `/tmp/openagent-desktop-split-diff-visible-smoke.png`.
- Temporary App Bridge/Vite processes were stopped.

Residual risk:

- This is line-level LCS diff, not AST/semantic diff.
- The Review panel still focuses on the latest patch rather than multi-file review navigation.
- The smoke is still Vite Desktop with a real Rust App Bridge, not packaged Tauri GUI automation.

## 2026-07-01 Desktop Managed Bridge Command Readiness Slice

Desktop product alignment:

- The Tauri-managed Rust App Bridge path is now verified at the command/process boundary, not just as registered UI buttons.
- Desktop `Start`/`Restart` is safer because the Tauri command waits for the local runtime port to become reachable before reporting success.
- This moves the packaged-app path closer to the desired product shape: Desktop shell owns a local Rust App Bridge process, passes auth, and can restart it on the selected workspace.

Implemented:

- `desktop/src-tauri/src/lib.rs`
  - Replaced the fixed startup sleep with `wait_for_bridge_port`.
  - Startup now monitors child process exit and waits up to 4 seconds for `127.0.0.1:<port>` to accept TCP connections.
  - Startup failure kills and waits for the child before returning an explicit error.
  - Added `managed_bridge_starts_restarts_and_stops_runtime`, which starts a real `openagent-http-runtime`, verifies bearer auth, checks status, restarts on a second workspace, and stops cleanly.

Verification:

```bash
cargo build -p openagent-http-runtime
npm --prefix desktop run build
cargo test --manifest-path desktop/src-tauri/Cargo.toml managed_bridge_starts_restarts_and_stops_runtime -- --nocapture
cargo test --manifest-path desktop/src-tauri/Cargo.toml -- --nocapture
npm --prefix desktop run tauri -- build --no-bundle
git diff --check -- desktop/src-tauri/src/lib.rs desktop/index.html desktop/public/favicon.svg desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
```

Evidence:

- Tauri crate tests passed: `desktop_auth_token_persists_with_override` and `managed_bridge_starts_restarts_and_stops_runtime`.
- The managed bridge test used a real temporary workspace/session root/port and real `target/debug/openagent-http-runtime`.
- No-token `/api/health` returned `401 Unauthorized`; Bearer token `/api/health` returned `200 OK` and an `ok` payload.
- `restart` moved the managed runtime from `workspace-a` to `workspace-b` and health still passed.
- `stop` closed the runtime port and final status reported stopped.
- `tauri build --no-bundle` produced `/Users/william/coding/harness/openharness/desktop/src-tauri/target/release/openagent-desktop`.
- Attempted GUI smoke by launching both the release binary and `.app/Contents/MacOS/openagent-desktop`; Computer Use could not capture a window and returned `cgWindowNotFound`. Processes were cleaned up.

Residual risk:

- Real packaged GUI clicking of `Start`/`Restart` is still unverified in this environment.
- The core managed bridge behavior is covered at Tauri command level, but not yet through a window-level automation path.
- Next UI-facing trust slice should cover question reply/dismiss or approval deny in rendered Desktop, since allow/restore is already covered.

## 2026-07-01 Desktop Real App Bridge Review/Restore Smoke Slice

Desktop product alignment:

- The Codex-like desktop shell, workspace dock, and Review panel are now verified against a real Rust App Bridge data path, not only mocked frontend data.
- The visible trust loop works end to end: pending approval -> user allows -> file changes -> dock shows diff/checkpoint -> Review opens -> checkpoint restore rolls the file back.
- The verification keeps the product direction unchanged: Desktop is the app client, Rust App Bridge is the product API, and Python is not part of this path.

Implemented:

- `desktop/index.html`
  - Added an explicit SVG favicon link so Chrome/Vite no longer produces a favicon 404 during rendered smoke tests.
- `desktop/public/favicon.svg`
  - Added a small OpenAgent app icon asset.

Verification:

```bash
cargo build -p openagent-http-runtime
npm --prefix desktop run build
git diff --check -- desktop/index.html desktop/public/favicon.svg desktop/src/App.tsx desktop/src/styles.css
node <inline Playwright real App Bridge review/restore smoke>
```

Evidence:

- Playwright launched a temporary real `openagent-http-runtime` and a temporary Vite Desktop preview.
- The smoke created a real App Bridge session and a `PLAN_ONLY` write approval for `notes.txt`.
- Desktop clicked `Allow`; the workspace file became `codex-real-ui\n`.
- Desktop dock then exposed diff/checkpoint rows; clicking the dock opened `.review-panel`.
- Review showed `+codex-real-ui` in the diff and rendered the Checkpoint Browser.
- Clicking the real `step_start` checkpoint restore button removed `notes.txt` from the workspace.
- UI showed `Restored ckpt_...` and `.review-checkpoint-row.restored`.
- Global `/api/events?last_event_id=0` replay contained `checkpoint/restored`.
- Browser QA was clean: horizontal overflow `0`, console issues `0`, page errors `0`, failed responses `0`.
- Screenshot inspected: `/tmp/openagent-desktop-real-review-bridge-smoke-v2.png`.

Residual risk:

- This is still Vite/web preview plus external Rust runtime, not a packaged Tauri managed-bridge click test.
- Review still consumes the existing unified diff string; side-by-side or AST-aware diff is not implemented yet.
- The smoke pre-seeds the approval through the App Bridge API for determinism; a provider-generated approval path should be covered in a later packaged app test.

## 2026-07-01 Desktop Review Panel / Diff-Checkpoint Browser Slice

Desktop product alignment:

- The workspace dock now opens a focused Review mode, so the user can move from current agent activity to a fuller diff/checkpoint inspection surface.
- Review mode keeps the Codex-like main conversation intact while using the right drawer as a change-review panel.
- This moves the first required product loop closer to: file modified -> UI shows diff -> checkpoint generated -> user can inspect and restore.

Implemented:

- `desktop/src/App.tsx`
  - Added `inspectorMode` with `overview` and `review`.
  - Added Overview/Review tabs to the inspector header.
  - Added `openReviewPanel` and `openOverviewPanel`.
  - Dock diff/checkpoint rows are keyboard/click accessible and open Review mode.
  - Review panel shows Pending/Undo/Checkpoints summary, Change Review diff preview, Undo/Redo, Checkpoint Browser restore rows, and Context file/git/workspace preview.
- `desktop/src/styles.css`
  - Added inspector tab styling, review summary cards, diff preview, checkpoint browser rows, clickable dock focus states, and mobile inspector width constraints.

Verification:

```bash
npm --prefix desktop run build
git diff --check -- desktop/src/App.tsx desktop/src/styles.css
```

Rendered QA:

- Started Desktop Vite on `127.0.0.1:5177`.
- Used system Chrome through Playwright fallback with mocked App Bridge responses.
- Clicked the dock diff row and verified `.review-panel` appears with active `Review` tab.
- Desktop QA: inspector size `380x914`, horizontal overflow `0`, diff code visible, context file preview visible, 2 checkpoint restore rows visible.
- Mobile QA: inspector x `10`, width `370`, horizontal overflow `0`, Review tab active.
- Screenshots inspected:
  - `/tmp/openagent-review-panel-click.png`
  - `/tmp/openagent-review-panel-mobile.png`

Residual risk:

- This is still frontend review-panel polish; it reuses the existing diff string and checkpoint list APIs.
- No structured/side-by-side diff API was added.
- Real packaged Tauri click smoke and real App Bridge end-to-end run/approval/restore smoke remain future slices.

## 2026-07-01 Desktop Workspace Dock / Trust-Diff-Checkpoint UI Slice

Desktop product alignment:

- Continued the Codex-like product direction by moving approval/question/diff/checkpoint actions into the main conversation surface.
- The composer now has a lightweight workspace dock above it, so the user can approve, answer, undo/redo, or restore without opening the details drawer.
- Git/file diagnostics remain in the right details drawer, keeping the first screen focused on the current agent decision.

Implemented:

- `desktop/src/App.tsx`
  - Added derived state for latest patch, latest checkpoint, and whether the workspace dock should render.
  - Added `workspace-dock` above the composer.
  - Dock renders pending approvals with Allow/Deny, pending questions with Reply/Dismiss, latest diff with Undo/Redo, and latest checkpoint with Restore.
  - Workspace gets a `has-dock` class so the timeline can reserve bottom space when the dock is present.
- `desktop/src/styles.css`
  - Added dock item styling, attention state, compact action buttons, stronger bottom white overlay, and mobile scroll behavior.
  - Tuned message part cards to look more like inline execution records, with left status rails for tool/patch/context/trust parts.

Verification:

```bash
npm --prefix desktop run build
git diff --check -- desktop/src/App.tsx desktop/src/styles.css
```

Rendered QA:

- Started Desktop Vite on `127.0.0.1:5177`.
- Used system Chrome through Playwright fallback with mocked App Bridge responses, avoiding external model/provider calls.
- Mocked session included user/tool/assistant messages, a tool card, pending approval, pending question, latest diff, latest checkpoint, and git changes.
- Final desktop QA: dock rows `4`, dock height `188`, dock is above composer, horizontal overflow `0`, approval/question/diff/checkpoint/tool card all visible, git summary is not in the dock.
- Final mobile QA: horizontal overflow `0`, dock height `178`, composer width `358`, dock remains above composer and scrolls when needed.
- Screenshots inspected:
  - `/tmp/openagent-workspace-dock-final.png`
  - `/tmp/openagent-workspace-dock-mobile-final.png`

Residual risk:

- This slice is UI composition only; it does not change Rust runtime/API semantics.
- The dock is a compact action surface; full diff viewer, checkpoint browser polish, terminal panel, MCP panel, and real packaged Tauri click smoke remain future slices.

## 2026-07-01 Desktop Codex-like UI Shell Slice

Desktop product alignment:

- Reworked the Desktop client from a dark diagnostics dashboard into a Codex-like agent workspace.
- The primary shape is now a light translucent left rail, a white conversation canvas, a minimal top bar, a centered timeline, a floating composer, and an on-demand right details drawer.
- Existing runtime surfaces remain available: protocol, Desktop bridge, provider, stream, trust, diff, checkpoints, files, and git moved into the details drawer instead of dominating the first screen.

Implemented:

- `desktop/src/App.tsx`
  - Added `inspectorOpen` drawer state.
  - Rebuilt the app shell into Codex-style navigation, pinned context, project/session lists, topbar status controls, timeline, floating composer, and toggleable details drawer.
  - Kept the existing App Bridge/session/provider/approval/checkpoint/file/git logic intact.
- `desktop/src/styles.css`
  - Replaced the dark three-column dashboard styling with a light Codex-like visual system.
  - Added sidebar, conversation canvas, composer dock, compact status pills, drawer/scrim, responsive single-column mobile behavior, and softer error styling.

Verification:

```bash
npm --prefix desktop run build
git diff --check -- desktop/src/App.tsx desktop/src/styles.css
```

Rendered QA:

- Started Desktop Vite on `127.0.0.1:5177`.
- Browser plugin controls were unavailable, so QA used system Chrome through Playwright fallback without installing bundled Playwright browsers.
- Desktop screenshot `1600x1000`: no horizontal overflow; left rail `424px`; composer `850px` and centered; first screen visually matches the Codex-like sidebar/canvas/composer shape.
- Details drawer screenshot: topbar toggle opens the right drawer; scrim and drawer render at expected size.
- Mobile screenshot `390x844`: no horizontal overflow; sidebar collapses and composer remains usable.
- Screenshots inspected:
  - `/tmp/openagent-codex-like-desktop-v2.png`
  - `/tmp/openagent-codex-like-inspector.png`
  - `/tmp/openagent-codex-like-mobile.png`

Residual risk:

- Vite preview did not run a local App Bridge, so expected `ERR_CONNECTION_REFUSED` logs and a lightweight `Failed to fetch` status appear in screenshots.
- This slice is visual/information architecture polish only; it does not deepen chat message states, tool-card affordances, diff viewer fidelity, or native Tauri click coverage.

## 2026-07-01 Desktop First-run Local Auth Token Slice

Desktop product alignment:

- Tauri Desktop now creates and persists a local App Bridge auth token on first run instead of relying on an empty optional token for the managed Rust bridge.
- The same token feeds the existing Bearer auth path and managed bridge `--auth-token`, so Desktop-owned bridge traffic has a local trust boundary.
- The UI shows auth state and token file location without rendering the token value.

Implemented:

- `desktop/src-tauri/Cargo.toml`
  - Added `getrandom` for OS-backed token generation.
- `desktop/src-tauri/src/lib.rs`
  - Added `DesktopAuthToken` payload and `desktop_auth_token` command.
  - Added local token path handling: `OPENAGENT_DESKTOP_AUTH_TOKEN_PATH` override or `~/.openagent/desktop/bridge-auth-token`.
  - Added `oa_desktop_` token generation, secret file write, and Unix `0600` mode.
  - Added `desktop_auth_token_persists_with_override` unit test.
- `desktop/src/App.tsx`
  - Loads the Tauri local token on startup and feeds it into the existing request auth path.
  - Waits for Tauri token readiness before initial refresh and SSE polling.
  - Keeps managed bridge Start/Restart using the same `authToken` option.
  - Adds Auth status to the Desktop inspector and a non-secret local token path hint in Bridge settings.

Verification:

```bash
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run build
cargo test --manifest-path desktop/src-tauri/Cargo.toml desktop_auth_token_persists_with_override -- --nocapture
cargo check --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run tauri -- build --no-bundle
git diff --check -- desktop/src-tauri/Cargo.toml desktop/src-tauri/src/lib.rs desktop/src/App.tsx desktop/src/styles.css .goal/state.md progress.md
```

Auth smoke:

- Started `openagent-http-runtime` on `127.0.0.1:8802` with `--auth-token oa_desktop_test_token`.
- `GET /api/health` without auth returned HTTP `401`.
- `GET /api/health` with `Authorization: Bearer oa_desktop_test_token` returned HTTP `200` and `{"ok":true,"service":"openagent-http-runtime"}`.

Rendered/API smoke:

- Started temporary App Bridge on `127.0.0.1:8801` with `/tmp/openagent-desktop-auth-sessions`.
- Started Desktop Vite on `127.0.0.1:5200`.
- Browser path succeeded: page identity `OpenAgent Desktop`, DOM snapshot included Desktop content, Vite overlay false, page console warnings/errors empty.
- Desktop inspector showed `Auth none` in web preview, as expected because Tauri commands are unavailable there.
- UI flow: fill Bridge URL `http://127.0.0.1:8801` -> clear Token -> fill Project path `/Users/william/coding/harness/openharness` -> Add project -> New session.
- API check `GET /api/sessions` confirmed the created session workspace is `/Users/william/coding/harness/openharness`.
- Temporary processes were stopped; `5200`, `8801`, and `8802` had no listeners at cleanup.
- Tauri-generated `desktop/src-tauri/gen/schemas` cache was removed after build.

Residual risk:

- Native Tauri GUI has not yet clicked Start/Restart/Stop to prove the generated token is passed into the managed child process at runtime.
- Token persistence is a local file, not system keychain integration.
- Web preview proves UI integrity only; native token load is covered by Rust unit test and Tauri build.

## 2026-07-01 Desktop Managed Bridge Restart Policy Slice

Desktop product alignment:

- Desktop now has an explicit restart path for the managed Rust App Bridge, so a selected project can become the active bridge workspace instead of only showing a mismatch warning.
- Start and Restart share one `managedBridgeStartOptions` path, keeping selected project workspace, session root, port, and auth token behavior aligned.
- Bridge actions now disable as a group while any managed bridge operation is busy, reducing racey Start/Stop/Restart clicks.

Implemented:

- `desktop/src-tauri/src/lib.rs`
  - Added `AppBridgeProcess::restart`.
  - Added and registered `app_bridge_restart` Tauri command.
- `desktop/src/App.tsx`
  - Added `managedBridgeStartOptions`.
  - Added `restartManagedBridge`.
  - Added a Restart button to the workspace mismatch warning.
  - Added Restart to the Desktop inspector managed bridge controls.
  - Unified busy disabling for Start/Restart/Stop/Status.
- `desktop/src/styles.css`
  - Added narrow-rail layout and button styling for project warning actions.

Verification:

```bash
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run build
cargo check --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run tauri -- build --no-bundle
git diff --check -- desktop/src-tauri/src/lib.rs desktop/src/App.tsx desktop/src/styles.css
```

Rendered/API smoke:

- Started temporary App Bridge on `127.0.0.1:8800` with `/tmp/openagent-desktop-restart-sessions`.
- Started Desktop Vite on `127.0.0.1:5199`.
- Browser path succeeded: page identity `OpenAgent Desktop`, DOM snapshot included Desktop content, Vite overlay false, page console warnings/errors empty.
- Desktop inspector rendered exactly one `Restart managed App Bridge on selected project` control; in web preview it was disabled as expected.
- UI flow: fill Bridge URL `http://127.0.0.1:8800` -> fill Project path `/Users/william/coding/harness/openharness` -> Add project -> New session.
- UI showed selected project and topbar workspace `/Users/william/coding/harness/openharness`.
- API check `GET /api/sessions` confirmed the created session workspace is `/Users/william/coding/harness/openharness`.
- Temporary Vite and App Bridge processes were stopped; `5199` and `8800` had no listeners at cleanup.
- Tauri-generated `desktop/src-tauri/gen/schemas` cache was removed after build.

Residual risk:

- `app_bridge_restart` is compiled and registered, but Start/Restart/Stop were not clicked inside a live Tauri GUI session.
- Web preview can only prove disabled web behavior and UI integrity, not native process restart behavior.
- First-run auth token generation and persistence are still future slices.

## 2026-07-01 Desktop Native Folder Picker / Bridge Workspace Hint Slice

Desktop product alignment:

- Desktop now has a native folder picker entry point in real Tauri runtime while keeping manual path entry for web-preview smoke/dev.
- Project registration still canonicalizes and validates paths through Rust, and the new picker reuses the same `ProjectPathInfo` shape as manual Add.
- The Bridge card now warns when a managed App Bridge is running against a different workspace than the selected project, making project/bridge mismatch visible before restart policy is added.

Implemented:

- `desktop/src-tauri/Cargo.toml`
  - Added `rfd` for native folder selection.
- `desktop/src-tauri/src/lib.rs`
  - Added `choose_project_folder` Tauri command.
  - Refactored project path validation through `project_path_info_for_input`.
- `desktop/src/App.tsx`
  - Added a folder picker icon button beside the project path input.
  - Added `registerProject` helper shared by manual Add and native picker.
  - Added managed bridge workspace mismatch warning.
- `desktop/src/styles.css`
  - Updated project Add layout for input + picker + add controls.
  - Added compact project warning styling.

Verification:

```bash
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml
cargo check --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run build
npm --prefix desktop run tauri -- build --no-bundle
```

Rendered/API smoke:

- Started temporary App Bridge on `127.0.0.1:8799` with `/tmp/openagent-desktop-picker-sessions`.
- Started Desktop Vite on `127.0.0.1:5198`.
- Browser path succeeded: page identity `OpenAgent Desktop`, target controls unique, Vite overlay false, console warnings/errors empty.
- In web preview the native `Choose project folder` button was present and disabled, while manual project entry remained usable.
- UI flow: fill Bridge URL `http://127.0.0.1:8799` -> fill Project path `/Users/william/coding/harness/openharness` -> Add project -> New session.
- UI showed selected project and topbar workspace `/Users/william/coding/harness/openharness`.
- API check `GET /api/sessions` confirmed the created session workspace is `/Users/william/coding/harness/openharness`.
- Temporary Vite and App Bridge processes were stopped; `5198` and `8799` had no listeners at cleanup.
- Tauri-generated `desktop/src-tauri/gen/schemas` cache was removed after build.

Residual risk:

- Native folder dialog is compiled and registered, but not yet clicked in a live packaged Tauri GUI session.
- Workspace mismatch is surfaced as a warning only; it does not yet auto-restart or offer a one-click restart action.
- First-run auth token generation and auto-start policy remain future slices.

## 2026-07-01 Desktop Project Registry / Workspace Selection Slice

Desktop product alignment:

- Desktop no longer has a hard-coded `openharness` project row. The left rail now has a persisted project registry with path input, Add action, selectable project rows, and project-scoped session focus.
- `New session` now sends the selected project path as `cwd` to `POST /api/sessions`, so created sessions carry the chosen workspace.
- Managed App Bridge start now uses the selected project path as its `--workspace`, so the Desktop-owned bridge process aligns with the active project context.
- Tauri exposes a `project_path_info` command for path validation/canonicalization in real desktop runtime; Vite/web preview still allows manual project entry for smoke/dev.

Implemented:

- `desktop/src-tauri/src/lib.rs`
  - Added `project_path_info` command.
  - Supports `~` expansion, exists/dir checks, canonical path, display name, and error reporting.
- `desktop/src/App.tsx`
  - Added `DesktopProject` registry persisted in localStorage.
  - Added selected project state and project path form.
  - Added project-scoped session list and selected workspace display.
  - `createSession` now sends `cwd: selectedProjectPath`.
  - managed bridge start now prefers `selectedProjectPath`.
- `desktop/src/styles.css`
  - Added compact project input/list/error styles that fit the left rail.

Verification:

```bash
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml
cargo check --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run build
npm --prefix desktop run tauri -- build --no-bundle
git diff --check -- desktop/src-tauri/src/lib.rs desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
```

Rendered/API smoke:

- Started temporary App Bridge on `127.0.0.1:8798` with a separate `/tmp/openagent-desktop-project-sessions` session root.
- Started Desktop Vite on `127.0.0.1:5197`.
- Browser path succeeded: page identity `OpenAgent Desktop`, app shell nonblank, Vite overlay false, console warnings/errors empty.
- UI flow: fill Bridge URL `http://127.0.0.1:8798` -> fill Project path `/Users/william/coding/harness/openharness` -> Add project -> project row selected -> New session.
- UI showed selected project `openharness`, topbar workspace `/Users/william/coding/harness/openharness`, and a single `openharness session`.
- API check `GET /api/sessions` confirmed the created session workspace is `/Users/william/coding/harness/openharness`.
- Temporary Vite and App Bridge processes were stopped; `5197` and `8798` had no listeners at cleanup.

Residual risk:

- This is still manual path entry, not native folder picker/onboarding.
- Tauri path validation compiles but the Add Project command was not clicked inside a live packaged Tauri GUI session.
- App Bridge process workspace changes still require starting/restarting the managed bridge; no auto-start or bridge migration policy yet.

## 2026-07-01 Desktop-managed App Bridge Process Slice

Desktop product alignment:

- Tauri Desktop shell can now own the local Rust App Bridge process instead of only pointing at an external URL.
- Added `app_bridge_status`, `app_bridge_start`, and `app_bridge_stop` commands that discover `openagent-http-runtime`, launch it headless on `127.0.0.1`, pass workspace/session-root/auth options, report pid/url/workspace/session root, and kill the child process when stopped or when the app exits.
- Desktop inspector now shows managed bridge state (`running` / `stopped` / `web preview`), pid, workspace, and Start/Stop/Status controls. In Vite/web preview the controls are disabled and the card degrades without Tauri invoke errors.
- Start success writes the managed bridge URL back into the active Bridge URL so the existing Desktop API/SSE client uses the local process.

Implemented:

- `desktop/src-tauri/src/lib.rs`
  - Added managed child process state with cleanup on drop.
  - Added bridge binary discovery for repo `target/release` in addition to env/bundle/repo debug/PATH candidates.
  - Added default workspace/session root helpers and Tauri commands for bridge lifecycle.
- `desktop/src/App.tsx`
  - Added `ManagedBridgeStatus`, managed bridge state, Start/Stop/Status handlers, and Desktop inspector controls.

Verification:

```bash
cargo fmt --manifest-path desktop/src-tauri/Cargo.toml
cargo check --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run build
npm --prefix desktop run tauri -- build --no-bundle
git diff --check -- desktop/src-tauri/src/lib.rs desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
```

Runtime smoke:

- `target/debug/openagent-http-runtime` exists in the default Tauri discovery path.
- Started runtime with the same headless arguments Desktop uses on `127.0.0.1:8797`.
- `curl -i http://127.0.0.1:8797/api/health` returned `HTTP/1.1 200 OK` with `{"ok": true, "service": "openagent-http-runtime", "ui_enabled": false}`.
- Smoke process was stopped.

Rendered smoke:

- Started Desktop Vite on `127.0.0.1:5196`.
- Browser path succeeded: page identity `OpenAgent Desktop`, app shell nonblank, Vite overlay false, console warnings/errors empty.
- Desktop card rendered `Runtime web preview`, `Managed web preview`, and disabled Start/Stop/Status buttons.
- Vite preview server was stopped.

Residual risk:

- The Tauri command was compile-verified and its runtime arguments were smoke-tested manually, but Start/Stop were not clicked inside a live packaged Tauri GUI session.
- This does not yet implement project picker/onboarding, auto-start policy, auth token generation, signed bundles, or auto-update.

## 2026-07-01 Desktop First-run Diagnostics / Tauri Shell Slice

Desktop product alignment:

- Desktop shell is no longer just a Vite web surface: `desktop/src-tauri` now has a valid Tauri v2 crate structure that can `cargo check` and `tauri build --no-bundle`.
- Tauri exposes a `desktop_diagnostics` command for first-run diagnostics: runtime, app version, OS/arch, default bridge URL, session root, and local `openagent-http-runtime` binary discovery.
- The React Desktop inspector now shows a Desktop diagnostics card. In real Tauri it will call the command; in Vite/web preview it degrades to `web preview` / `external` without throwing.
- Added a minimal temporary PNG icon so `tauri::generate_context!()` can compile. This is a placeholder until a final brand icon is designed.

Implemented:

- `desktop/src-tauri/Cargo.toml`
  - Added `serde`.
  - Added an empty `[workspace]` so the Tauri crate can build independently under the larger repo.
- `desktop/src-tauri/build.rs`
  - Added standard `tauri_build::build()`.
- `desktop/src-tauri/src/lib.rs`
  - Added Tauri v2 `run()` entry and `desktop_diagnostics` command.
  - Added bridge binary discovery through `OPENAGENT_HTTP_RUNTIME`, bundle-adjacent paths, repo `target/debug`, and `PATH`.
- `desktop/src-tauri/src/main.rs`
  - Reduced to `openagent_desktop_lib::run()`.
- `desktop/src-tauri/icons/icon.png`
  - Added temporary 256x256 app icon.
- `desktop/src/App.tsx`
  - Added Tauri runtime detection, diagnostics loading, and Desktop inspector card.
- `desktop/src/styles.css`
  - Added diagnostics warning style.

Verification:

```bash
cargo check --manifest-path desktop/src-tauri/Cargo.toml
npm --prefix desktop run build
npm --prefix desktop run tauri -- info
npm --prefix desktop run tauri -- build --no-bundle
git diff --check -- desktop/src-tauri/Cargo.toml desktop/src-tauri/build.rs desktop/src-tauri/src/lib.rs desktop/src-tauri/src/main.rs desktop/src-tauri/icons/icon.png desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
```

Rendered smoke:

- Started Desktop Vite on `127.0.0.1:5195`.
- Browser path succeeded: page identity `OpenAgent Desktop`, app shell nonblank, Vite overlay false, console warnings/errors empty.
- Desktop diagnostics card rendered with `Runtime web preview`, `Bridge external`, `Default URL http://127.0.0.1:8787`, and no Tauri invocation error in browser mode.
- `tauri build --no-bundle` produced `/Users/william/coding/harness/openharness/desktop/src-tauri/target/release/openagent-desktop`.

Residual risk:

- Full macOS `.app`/DMG signing/bundling is not proven; `tauri info` still reports Xcode app missing and rustup not installed, though Command Line Tools and no-bundle build work.
- The app does not yet auto-launch/manage the Rust App Bridge process; diagnostics only discovers likely binaries and reports readiness.
- The PNG icon is a temporary placeholder, not final branding.

## 2026-07-01 App Bridge Runtime Event ID Slice

App Bridge / Desktop / TUI alignment:

- App Bridge events now carry a runtime-issued `event_id` generated from `session_id + turn_id + sequence`.
- `/api/protocol` documents `event_id` as the preferred event identity field, while keeping `global_sequence` / `sequence` as SSE resume cursors.
- Blocking `POST /turns` response events and live `/api/events` SSE replay now expose the same `event_id` for the same event.
- Rust remote client, TUI, Desktop, and the built-in static App UI now prefer `event_id` for dedupe and fall back to older sequence/semantic keys for old sessions.

Implemented:

- `runtime/app-server/src/app_bridge.rs`
  - Added optional `event_id` to `AppEvent`.
- `runtime/http/src/http_runtime.rs`
  - Added `event_id` to protocol manifest compatibility.
  - Added `normalize_app_event` and `app_event_id`.
  - `append_app_events` / `append_unpersisted_app_events` now normalize events in place before persisting, so returned payloads and SSE replay share identity.
- `runtime/app-server-client/src/app_bridge_client.rs`
  - Parses `event_id` and prefers it in `RemoteEventKey`.
- `runtime/tui/src/events.rs`
  - `event_identity_key` prefers `event_id`.
- `desktop/src/App.tsx`
  - `AppEvent` includes `event_id`; `eventKey` prefers it.
- `runtime/static/app-server/static/app.js`
  - Added lightweight event dedupe using `event_id` first.

Verification:

```bash
cargo fmt --all
npm --prefix desktop run build
cargo test -p openagent-http-runtime app_bridge_protocol_contract_and_client_live_subscription --test http_runtime -- --nocapture
cargo test -p openagent-app-server-client --lib -- --nocapture
cargo build -p openagent-http-runtime
cargo test -p openagent-tui --lib -- --nocapture
cargo test -p openagent-http-runtime global_sse_live_tails_provider_stream_delta_before_completion --test http_runtime -- --nocapture
cargo test -p openagent-app-server --lib -- --nocapture
git diff --check -- runtime/app-server/src/app_bridge.rs runtime/app-server-client/src/app_bridge_client.rs runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs runtime/tui/src/events.rs runtime/static/app-server/static/app.js desktop/src/App.tsx progress.md .goal/state.md
```

Evidence:

- `app_bridge_protocol_contract_and_client_live_subscription` now asserts `/api/protocol` requires `event_id`, `POST /turns` returned `turn/completed` has an `event_id`, live `/api/events` returns the same `event_id`, and turn-scoped SSE includes the same identity.
- Provider streaming live SSE test still passes after in-place normalization.
- TUI lib test suite passed: 51 tests.

Residual risk:

- `event_id` is deterministic from per-turn sequence, so future event insertion before already-persisted events would still be a compatibility concern; current append-only runtime behavior is safe.
- Existing old session event JSONL without `event_id` still relies on fallback keys.
- This is protocol/runtime/client hardening, not packaged Tauri validation.

## 2026-07-01 Desktop Event Identity Normalization Slice

Desktop Agent Workspace alignment:

- Desktop timeline/live event list 现在用 semantic event identity 去重，避免同一个 turn 的事件先通过 live `/api/events` 到达、再通过 blocking `POST /turns` response 返回时重复渲染。
- 事件身份优先使用 `session_id/thread_id + turn_id/run_id + method + stable params`，没有足够语义字段时才回退到 `global_sequence` / `sequence`。
- 这个 slice 没改后端协议，只收紧 Desktop 接入层的事件合并行为。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `eventTurnId`，兼容 `turn_id` 与 `run_id`。
  - 新增 `stableJson`，对 object keys 做稳定排序，避免同语义 params 因 key 顺序产生不同 event key。
  - 新增 `eventSemanticKey`，在 event 有 session + turn 身份时生成跨 live SSE 和 turn payload 一致的去重 key。
  - `eventKey` 改为 semantic key 优先，再回退 sequence key，保留旧事件兼容性。

Verification:

```bash
npm --prefix desktop run build
cargo build -p openagent-http-runtime
```

Rendered smoke:

- Started fake OpenAI-compatible Responses streaming provider on `127.0.0.1:19094`; it emitted two delayed `response.output_text.delta` SSE chunks and then `response.completed`.
- Started Rust App Bridge on `127.0.0.1:18814` with workspace `/tmp/openagent-event-identity-smoke/workspace`.
- Started Desktop Vite on `127.0.0.1:5194`.
- Browser path succeeded: page identity `OpenAgent Desktop`, URL `http://127.0.0.1:5194/`, app shell nonblank, Vite overlay false, console warnings/errors empty.
- Flow: `New session -> event identity smoke -> live streaming -> completion`.
- Provider request count was `1`; `/api/sessions` showed one idle session with `message_count: 2`.
- Desktop event rows were exactly 4: `started #1`, `agentMessage delta #2 identity`, `agentMessage delta #3 complete`, `completed #4 identity complete`; no duplicate returned-payload rows appeared.
- Stream inspector showed `Events 4`, `Messages 2`, `Cursor 4`, `Resume 4`.
- Screenshot saved at `/tmp/openagent-desktop-event-identity-smoke.jpg`.

Residual risk:

- This is a Desktop-side normalization. Long-term protocol should include a stable runtime-issued `event_id` so every client can dedupe without recomputing semantic keys.
- Semantic keys can collapse two deliberately identical same-method/same-params events in one turn, although that is rare for current app events.
- This is Vite Desktop shell smoke, not packaged Tauri/macOS/Windows validation.

## 2026-07-01 Desktop Stream Status Semantics Slice

Desktop Agent Workspace alignment:

- Desktop 现在把 Agent run state 和 App Bridge event transport state 分开显示，不再在空闲长轮询时误显示 `connecting`。
- `/api/events` SSE response 不再等整段 long-poll body 结束才进入 UI；浏览器端会在每个 SSE frame 到达时立刻 append event、更新 run state，并刷新相关 session/trust 数据。
- 顶部状态 pill 现在按 `idle` / `running` / `streaming` / `waiting_approval` / `waiting_question` / `failed` 着色；右侧 Stream inspector 专门显示 `polling` / `receiving` / `listening` / `reconnecting`。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `SseEventHandler` 和 `streamStateAfterEvents`。
  - `readSse` 支持 frame-level callback，事件到达即处理。
  - Desktop live event loop 将空闲请求标为 `polling`，收到帧标为 `receiving`，成功空闲标为 `listening`/`resumed`，断线才标为 `reconnecting`。
  - `submitPrompt` 使用返回事件计算 run state，不再在 finally 中无条件改回 idle。
  - 顶部 stream pill 使用 `statusClass(streamState)`。

Verification:

```bash
npm --prefix desktop run build
cargo build -p openagent-http-runtime
git diff --check -- desktop/src/App.tsx desktop/src/styles.css progress.md .goal/state.md
```

Rendered smoke:

- Started fake OpenAI-compatible Responses streaming provider on `127.0.0.1:19093`; it emitted three delayed `response.output_text.delta` SSE chunks and then `response.completed`.
- Started Rust App Bridge on `127.0.0.1:18813` with workspace `/tmp/openagent-stream-status-smoke/workspace`.
- Started Desktop Vite on `127.0.0.1:5193`.
- Browser path succeeded: page identity `OpenAgent Desktop`, app shell nonblank, Vite overlay false, console warnings/errors empty.
- Idle state: after Bridge URL set to `http://127.0.0.1:18813`, top status showed `idle`, Stream inspector recovered to `polling`/`listening` style state instead of `connecting`.
- Live state: while the fake provider was still streaming, Desktop rendered `streaming` in the top status and `receiving` in the Stream inspector with partial delta text `stream`.
- Final state: after completion, Desktop returned to top status `idle`, Stream inspector `polling`, final assistant text `stream status complete`, session `idle`, provider request count `1`.
- Screenshot saved at `/tmp/openagent-desktop-stream-status-smoke.jpg`.

Residual risk:

- This fixes Desktop streaming consumption and product semantics, but App Bridge still uses bounded live SSE/long-poll windows rather than one permanent EventSource/WebSocket transport.
- The rendered timeline can still show duplicate live/payload events when the blocking `POST /turns` response returns events already seen through `/api/events`; event identity normalization is a separate follow-up.
- This is Vite Desktop shell smoke, not packaged Tauri/macOS/Windows validation.

## 2026-07-01 Desktop Trust History Cards Slice

Desktop Agent Workspace alignment:

- Approval/question 不再只是 generic JSON/message-part trace；Desktop timeline 和 Trust inspector 都会渲染专用 trust history card。
- Approval card 展示 pending -> allowed 的信任边界轨迹，包括工具名、request/call id、权限动作和目标命令。
- Question card 展示 pending -> answered 的信任边界轨迹，包括问题标题、用户回答状态和 request/call id。
- Trust inspector 新增 Recent history 区块，从持久化 message parts 恢复最近 approval/question 记录，刷新/重进 session 后仍可见。

Implemented:

- `desktop/src/App.tsx`
  - 新增 `TrustHistoryItem` 数据模型和 approval/question message part 解析。
  - `MessagePartCard` 对 approval/question 改为专用 `TrustHistoryCard`。
  - Trust inspector 增加 Recent history dock，显示最近 8 条 interaction history。
  - 状态分类补齐 `allowed`、`answered`、`pending`、`denied`、`dismissed`。
- `desktop/src/styles.css`
  - 新增 trust history card、dock/timeline 变体和 ok/warn/bad tone 样式。
  - approval/question cards 接入现有 inspector/timeline 视觉体系。

Verification:

```bash
npm --prefix desktop run build
cargo build -p openagent-http-runtime
```

Rendered smoke:

- Started fake OpenAI-compatible Responses provider on `127.0.0.1:19092`.
- Started Rust App Bridge on `127.0.0.1:18812` with workspace `/tmp/openagent-trust-history-smoke/workspace`.
- Started Desktop Vite on `127.0.0.1:5192`.
- Browser path succeeded: Desktop page loaded from Vite, Vite overlay false, console warnings/errors empty.
- Approval Allow flow: `New session -> approval history desktop smoke -> pending bash approval -> Allow`; session returned `idle`, `/api/approvals` count `0`, `trust-history.txt` was created, message v2 contained `approval pending` and `approval completed/allowed`, timeline and Recent history both rendered `data-part-kind="approval"` / `data-interaction-status="allowed"`.
- Question Reply flow: `New session -> question history desktop smoke v2 -> pending question -> Reply`; session returned `idle`, `/api/questions` count `0`, message v2 contained `question pending` and `question completed/answered`, timeline and Recent history both rendered `data-part-kind="question"` / `data-interaction-status="answered"`.
- Screenshots saved at `/tmp/openagent-desktop-trust-history-approval.jpg` and `/tmp/openagent-desktop-trust-history-question.jpg`.

Residual risk:

- This is Vite Desktop shell smoke, not packaged Tauri/macOS/Windows validation.
- Trust history is now product-shaped, but still lives in the right inspector and timeline; a bottom/floating approval dock can be a later UX pass.
- Stream inspector still shows long-poll `connecting`; transport/product wording remains a separate slice.
- Smoke provider must use the full question schema (`questions[]` with options). A thin `{question: ...}` payload produces empty answers and a 400 on reply, so future question tests should mirror the real tool schema.

## 2026-07-01 Desktop Deny / Dismiss Rendered Smoke Slice

Desktop Agent Workspace alignment:

- Negative trust-boundary decisions are now proven through the rendered Desktop, not only runtime tests.
- Desktop can show a pending approval, let the user click Deny, clear Trust, keep the session idle, and persist the denied approval trace.
- Desktop can show a pending question, let the user click Dismiss, clear Trust, keep the session idle, and persist the dismissed question trace.

Verification:

```bash
npm --prefix desktop run build
cargo build -p openagent-http-runtime
```

Rendered smoke:

- Started a local fake OpenAI-compatible provider on `127.0.0.1:19091`.
- Started current-build Rust App Bridge on `127.0.0.1:18811` with workspace `/tmp/openagent-deny-dismiss-smoke-current/workspace`.
- Started Desktop Vite on `127.0.0.1:5191`.
- Browser path succeeded: page identity `OpenAgent Desktop`, URL `http://127.0.0.1:5191/`, nonblank snapshot, Vite overlay false, console warnings/errors empty.
- Approval Deny flow: `New session -> approval deny desktop smoke current -> pending bash approval -> Deny`; Desktop ended with Trust `CLEAR` / `No pending interaction`, session `idle`, no Deny button, and timeline cards `tool`, `approval`, `approval` with denied trace visible.
- Question Dismiss flow: `New session -> question dismiss desktop smoke current -> pending question -> Dismiss`; Desktop ended with Trust `CLEAR` / `No pending interaction`, session `idle`, no Dismiss button, and timeline cards `tool`, `question`, `question` with dismissed trace visible.
- API confirmation: `/api/approvals` count `0`, `/api/questions` count `0`, both smoke sessions `idle` with `message_count: 3`, `/tmp/openagent-deny-dismiss-smoke-current/workspace/denied-ui.txt` absent.
- Message API confirmation:
  - deny session has assistant parts `tool pending`, `approval pending`, `approval error` with `content_status: denied`;
  - dismiss session has assistant parts `tool pending`, `question pending`, `question error` with `content_status: dismissed`.
- Screenshots saved at `/tmp/openagent-desktop-approval-deny-smoke.png` and `/tmp/openagent-desktop-question-dismiss-smoke.png`.

Residual risk:

- The UI still renders approval/question history as generic append-only message part cards; a polished trust-history dock remains the next product slice.
- The stream inspector still shows long-poll `connecting` semantics after the terminal state; this is accurate for the current transport but needs product wording/transport work.
- This smoke covered desktop web shell through Vite, not packaged Tauri/macOS/Windows builds.

## 2026-07-01 Approval Deny / Question Dismiss Runtime Slice

Desktop Agent Workspace alignment:

- The trust boundary now has dedicated runtime evidence for both negative decisions: approval deny and question dismiss.
- Deny/dismiss stop the paused provider loop instead of resuming it, clear pending interaction state, and leave durable error-status resolution parts in the session timeline.
- App Bridge responses for these terminal interaction paths now include a top-level `status: "failed"` so CLI/TUI/Desktop clients can consume them consistently.

Implemented:

- Added `status` to non-resume interaction response payloads:
  - approval deny returns `status: "failed"`;
  - question dismiss returns `status: "failed"`;
  - non-provider question answer returns `status: "completed"`.
- Added `remote_runtime_client_stops_provider_after_approval_deny`.
- Added `remote_runtime_client_stops_provider_after_question_dismiss`.
- The new tests assert provider calls stop after the first paused request, pending metadata is cleared, sessions return to `idle`, and message v2 contains both the original pending part and the terminal error resolution part.

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime remote_runtime_client_stops_provider_after_approval_deny --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_stops_provider_after_question_dismiss --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_resumes_provider_after_approval_allow --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_resumes_provider_after_question_reply --test http_runtime -- --nocapture
```

Evidence:

- Approval deny test proves `turn/approval_resolved` has `status: denied`, `turn/failed` has `approval denied`, the requested bash command does not create `denied.txt`, provider request count stays at 1, `pending_approval` and `pending_provider_turn` are cleared, and the approval message parts include `pending` plus `error/denied` resolution with the note.
- Question dismiss test proves `item/question/resolved` has `status: dismissed`, `turn/failed` uses the dismiss note, provider request count stays at 1, `pending_question` and `pending_provider_turn` are cleared, and the question message parts include `pending` plus `error/dismissed` resolution.
- Existing allow/reply provider resume tests still pass after the response-shape change.

Residual risk:

- This slice is runtime/API evidence only; Desktop rendered smoke for Deny and Dismiss still needs a later UI pass.
- Approval/question timeline is still append-only trace cards, not a polished trust-history dock.
- Full transport semantics are still recoverable long-poll/SSE style, not final true streaming.

## 2026-07-01 Desktop Question Flow Rendered Smoke Slice

Desktop Agent Workspace alignment:

- Question interactions now have rendered Desktop evidence, not only HTTP runtime/unit evidence.
- The Desktop Trust panel shows the question queue clears after reply, while the timeline keeps the pending question request and answered resolution as durable message parts.
- Session replay no longer marks a tool call as `interrupted` just because the agent was waiting for a pending question response.

Implemented:

- Updated session replay finalization so pending/running `tool` parts tied to pending/running `approval` or `question` parts stay pending instead of being marked interrupted.
- Added a focused session-store regression test for the pending-question tool state.
- Verified the existing question resume path still persists pending and answered question parts through the HTTP runtime.

Verification:

```bash
cargo fmt --all
cargo test -p openagent-session file_session_store_keeps_tool_call_pending_while_waiting_for_question --test session_trace -- --nocapture
cargo test -p openagent-session file_session_store_marks_unfinished_tool_calls_interrupted_on_replay --test session_trace -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_resumes_provider_after_question_reply --test http_runtime -- --nocapture
```

Rendered smoke:

- Reused local fake provider `127.0.0.1:19081`, Rust App Bridge `127.0.0.1:18801`, and Vite Desktop `127.0.0.1:5186`.
- Browser path succeeded this time: page identity `OpenAgent Desktop`, URL `http://127.0.0.1:5186/`, nonblank snapshot, Vite overlay false, console warnings/errors empty, screenshot captured.
- Desktop state contained final answer `question-smoke-completed`, Trust showed `CLEAR` and `No pending interaction`, and no rendered `interrupted` text was present.
- `/api/questions` returned `{"count":0}`; `/api/sessions` showed the question-smoke session `idle`; `/api/sessions/{id}/messages` showed `message_v2_count: 3` with assistant parts `tool pending`, `question pending`, `tool completed`, `question completed/answered`.
- Screenshot saved at `/tmp/openagent-desktop-question-final.png`.

Residual risk:

- Pending and completed question parts are append-only, so the timeline still shows the original pending request alongside the answered resolution; this is trace-correct but needs a polished history/dock presentation.
- Approval deny and question dismiss still need dedicated runtime/UI tests.
- The stream indicator can still show `connecting` during long-poll waits; this needs a later product wording/transport pass.

## 2026-07-01 Interaction Resolved Parts Slice

Desktop Agent Workspace alignment:

- Approval/question interactions now have durable resolved trace parts, not only pending queue state and App Bridge events.
- Restored sessions can show both sides of the trust-boundary interaction: what the agent asked for and how the user responded.
- Provider-loop approval/question resume tests now assert the message timeline, not just final provider completion.

Implemented:

- Added `assistant_message_id` to pending question payloads so question replies attach to the same assistant message as the original tool call.
- Added `append_interaction_resolution_part` for approval/question resolution records.
- Approval allow now appends a completed `approval` message part with `status: allowed` and the response payload.
- Question reply now appends a completed `question` message part with `status: answered` and answer payload.
- Approval deny and question dismiss use the same helper with error status when an assistant message exists.
- Extended HTTP runtime tests to verify standalone approval, provider approval resume, and provider question resume expose pending plus resolved interaction parts through `/api/sessions/{id}/messages`.

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_resumes_provider_after_approval_allow --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_resumes_provider_after_question_reply --test http_runtime -- --nocapture
```

Evidence:

- Standalone Desktop-style approval path now persists a completed `approval` part on the assistant message.
- Provider approval resume now proves both pending and completed approval parts are recoverable from messages API.
- Provider question reply now proves both pending and completed question parts are recoverable from messages API.
- No frontend files changed; Desktop already renders non-text message parts generically, so this slice did not rerun `npm --prefix desktop run build`.

Residual risk:

- Deny/dismiss branches share the same helper but still need dedicated tests.
- Question flow still needs a rendered Desktop smoke.
- Timeline cards for approval/question are still generic part cards, not a polished approval dock history view.

## 2026-07-01 Approval-Resume Runtime Consistency Slice

Desktop Agent Workspace alignment:

- Approval allow now leaves the same durable trace shape as a direct tool turn.
- The first product loop no longer has a checkpoint/message gap after the user approves a write: the approved tool execution gets a completion assistant message, a step-end checkpoint, and assistant context/patch parts.
- Provider-loop approval resume also finalizes the approved provider step before continuing to the next provider call, which keeps the Rust Agent Loop trace coherent.

Implemented:

- Added runtime approval metadata for `assistant_message_id` and `snapshot_start` before pausing.
- Added `append_runtime_completion_assistant` so direct tool turns and standalone approval completions share the same assistant-message/checkpoint finalization path.
- Finalized provider-loop approval steps before resuming provider execution.
- Extended the trust-boundary runtime test so approval allow must produce user/tool/assistant messages, unique assistant part ids, `context(checkpoint)` part, `patch` part, and a `step_end` checkpoint.

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_round_trips_tui_approval --test http_runtime -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_resumes_provider_after_approval_allow --test http_runtime -- --nocapture
```

Evidence:

- Targeted trust-boundary test passed and now proves approval allow persists the same assistant context/patch shape that Desktop renders for direct tool turns.
- TUI/client approval round-trip passed.
- Provider approval resume passed, covering the Agent Loop resume path after an approval.
- No frontend files changed, so Desktop build was not rerun in this slice.

Residual risk:

- Pending approval/question message parts are still append-only and do not yet get marked resolved in place.
- Question flow still needs its own rendered smoke.
- The broader Desktop diff viewer/checkpoint browser is still a later product slice.

## 2026-07-01 Desktop Checkpoint Restore E2E Rollback Slice

Desktop Agent Workspace alignment:

- The first visible product loop now reaches rollback from the Desktop UI: approval appears, the user approves, a file is changed, diff/checkpoint state appears, and the user restores a checkpoint to remove the change.
- Checkpoint restore now has explicit UI state rather than a silent icon click.
- Restoring to a checkpoint that deletes the focused file is treated as a normal state, not a failed file API request.

Implemented:

- Added Desktop `restoringCheckpointId` and `restoredCheckpointId` state.
- Added checkpoint restore event recognition from App Bridge events.
- Changed checkpoint restore button handling to reuse `refreshFromEvents`, matching the SSE-driven refresh path.
- Added `restored` badge, `Restored <checkpoint>` line, disabled restore buttons while a restore is in flight, and highlighted restored checkpoint rows.
- Changed `/api/files` to return HTTP 200 with `exists:false` for valid in-workspace paths that no longer exist.
- Added Files panel handling for missing focused files, showing `<path> no longer exists`.
- Extended the HTTP runtime trust-boundary test to assert global App Bridge event replay includes `checkpoint/restored` after restore.

Verification:

```bash
cargo fmt --all
npm --prefix desktop run build
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
BRIDGE=http://127.0.0.1:18798 APP=http://127.0.0.1:5182 node <inline Playwright approval-to-restore smoke>
```

Evidence:

- Desktop build passed with `tsc && vite build`.
- Targeted Rust test passed with the new checkpoint-restored event replay assertion.
- Rendered smoke created a temporary `PLAN_ONLY` write approval for `rollback.txt`.
- Before approval, Desktop showed pending approval and the diff preview.
- After Allow, `rollback.txt` existed with content `restore-me-2\n`, Trust was clear, and the selected session was idle.
- Desktop exposed a `step_start` checkpoint; clicking its Restore button removed `rollback.txt` from the workspace.
- After restore, Desktop showed `Restored ckpt_...`, one highlighted `.checkpoint-row.restored`, and Files showed `rollback.txt no longer exists`.
- Vite overlay false, consoleIssues 0, pageErrors 0.
- Screenshot inspected at `/tmp/openagent-desktop-restore-e2e-smoke.png`.
- Temporary listeners on 18798 and 5182 were stopped after verification.

Residual risk:

- Approval-resume currently creates the rollback-capable `step_start` checkpoint, but does not yet mirror direct tool turns with a `step_end` checkpoint and assistant checkpoint/patch message parts.
- Question flow still needs its own rendered smoke.
- Restore is visible in the side inspector; a fuller diff/checkpoint browser remains future work.

## 2026-07-01 Desktop Approval Live Interaction Slice

Desktop Agent Workspace alignment:

- Approval handling now behaves more like an agent client trust boundary: the right-side Trust panel reacts to App Bridge events and the session list updates after a user decision.
- This moves the first product loop forward: a pending write approval is visible, the user can approve it, the file is written, diff/checkpoint state refreshes, and the session leaves `paused`.
- The same event-driven sync path is wired for question requested/resolved events, though this slice only rendered-smoked approval.

Implemented:

- Added interaction event classifiers for approval/question requested/resolved and session-changing events.
- Added `refreshInteractions` for lightweight `/api/approvals` + `/api/questions` sync without re-running provider health checks.
- Added `refreshFromEvents` so SSE event batches refresh pending interactions, sessions, current session messages, diff/checkpoints, files, and git context.
- Reused `refreshFromEvents` after Allow/Deny/Reply/Dismiss button responses so manual actions and SSE actions share the same state sync path.
- Added Trust pending/clear badge, last sync source/time line, and per-request busy/disabled button state.
- Extended the HTTP runtime trust-boundary test to prove approval queue clears after allow and global SSE includes approval requested/resolved plus tool completion events.

Verification:

```bash
cargo fmt --all
npm --prefix desktop run build
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
BRIDGE=http://127.0.0.1:18797 APP=http://127.0.0.1:5181 node <inline Playwright approval live smoke>
```

Evidence:

- Desktop build passed with `tsc && vite build`.
- Targeted Rust test passed with the new queue-clear and event-stream assertions.
- Rendered smoke created a temporary `PLAN_ONLY` write approval for `approval2.txt`.
- Before clicking Allow, Desktop showed `1 pending`, the approval preview path, and the diff preview.
- After clicking Allow, Desktop showed `clear` and `No pending interaction`; selected session row changed from `paused` to `idle`.
- File `/tmp/openagent-desktop-approval-smoke/workspace/approval2.txt` contained `approved-live-2\n`.
- Diff/checkpoint panels were visible for the applied file change.
- Vite overlay false, consoleIssues 0, pageErrors 0.
- Screenshot inspected at `/tmp/openagent-desktop-approval-live-smoke.png`.
- Temporary listeners on 18797 and 5181 were stopped after verification.

Residual risk:

- Question response uses the same sync path but still needs its own rendered smoke.
- Trust is still a right-side inspector card, not a full dock/floating approval modal.
- Stream transport remains recoverable long polling, so the Stream status can show `connecting` while waiting for the next event batch.

## 2026-07-01 Desktop Tool/Patch/Checkpoint Cards Slice

Desktop Agent Workspace alignment:

- Persisted message v2 parts are now visible as product UI, not just recoverable transcript data.
- Tool execution, checkpoint creation, and patch detection now appear in the main timeline in the same object-oriented shape expected from Codex/Zcode/OpenCode-style agent clients.
- Session part IDs are safer for long-lived trace/checkpoint UIs because default generated part IDs no longer collide when multiple parts are appended in the same millisecond.

Implemented:

- Changed Desktop `messageContent` to render only text parts as normal message body.
- Added structured message part cards for `tool`, `patch`, and `context` checkpoint parts.
- Tool cards show tool name, call id, status, output, and error text when present.
- Patch cards show added/modified/deleted counts, before/after checkpoint ids, and changed file paths.
- Checkpoint cards show snapshot start/end checkpoint ids.
- Added CSS for dense, scan-friendly cards inside the existing timeline.
- Fixed `FileSessionStore::append_part` default part ID generation to use owner id + seq + kind instead of millisecond-only IDs.
- Added frontend key fallback using part id + kind + index for old sessions that may already contain duplicate part ids.
- Extended the App Bridge targeted test to assert tool/context/patch message parts and unique assistant part ids.

Verification:

```bash
cargo fmt --all
npm --prefix desktop run build
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
BRIDGE=http://127.0.0.1:18796 APP=http://127.0.0.1:5180 node <inline Playwright tool-card smoke>
```

Evidence:

- Desktop build passed with `tsc && vite build`.
- Targeted Rust test passed and now proves direct tool flow persists a `tool` part plus assistant `context(checkpoint)` and `patch` parts with unique ids.
- Rendered smoke created a temporary App Bridge session, ran a FULL bash tool call that wrote `tool-card.txt`, and loaded Desktop through Vite.
- Desktop rendered 3 `.message-part-card` elements with kinds `tool`, `context`, and `patch`.
- The page contained `tool-card-output-unique`, `Checkpoint`, `Patch`, and `tool-card.txt`.
- Vite overlay false, consoleIssues 0, pageErrors 0.
- Screenshot inspected at `/tmp/openagent-desktop-tool-cards-smoke.png`.
- Temporary listeners on 18796 and 5180 were stopped after verification.

Residual risk:

- Cards are still lightweight timeline inspectors, not a full expandable trace panel.
- Patch cards list changed files but do not yet open a full file diff viewer.
- The stream inspector can still show `connecting` during long-poll waits, which is accurate but needs clearer product wording later.

## 2026-07-01 Desktop SSE Reconnect Resume Slice

Desktop Agent Workspace alignment:

- Desktop now treats App Bridge event streaming as an explicit recoverable connection, not an invisible best-effort poll.
- The Stream inspector exposes cursor, resume position, reconnect attempts, recovered count, and batch size, which makes the live execution trace easier to trust and debug.
- This builds on message persistence: if live events drop temporarily, persisted messages remain visible and the event stream can resume from the last cursor.

Implemented:

- Added `StreamHealth` state in Desktop with `status`, `resume_cursor`, `reconnect_attempts`, `recovered_count`, `last_batch_count`, `last_error`, and retry metadata.
- Updated the SSE loop to request `/api/events?last_event_id=<cursor>&live_timeout_ms=5000` using the current cursor at the start of each attempt.
- Transient stream failures now set stream health to `reconnecting` and no longer write noisy SSE errors into the global error banner.
- Successful recovery resets reconnect attempts, increments recovered count, clears stream error, and records the latest batch size.
- Expanded the Stream inspector with Status, Resume, Attempts, Recovered, Batch, and last stream error.
- Added targeted Rust assertion proving `/api/events?last_event_id=` returns only events after the requested cursor.

Verification:

```bash
cargo fmt --all
npm --prefix desktop run build
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
BRIDGE=http://127.0.0.1:18795 APP=http://127.0.0.1:5179 node <inline Playwright SSE resume smoke>
```

Evidence:

- Desktop build passed.
- Targeted Rust test passed with the new SSE resume assertion.
- Rendered smoke pre-created a completed tool session, forced the first two `/api/events` requests to return 503, then allowed the third request to reach App Bridge.
- Desktop recovered and displayed persisted message rows plus live event rows; Stream inspector showed `Events4 Messages3 Cursor4 Resume4 Attempts0 Recovered1 Batch4`.
- Vite overlay 0, page errors 0; the only ignored console issue was the intentionally forced 503 resource log.
- Screenshot inspected at `/tmp/openagent-desktop-sse-resume-smoke.png`.
- Temporary listeners on 18795 and 5179 were stopped after verification.

Residual risk:

- This is still client-side recoverable long polling, not a full always-open streaming transport manager.
- Turn-scoped stream state is not shown separately from global stream state.
- Formal protocol docs still need to capture retry and resume behavior.

## 2026-07-01 Desktop Message Persistence Recovery Slice

Desktop Agent Workspace alignment:

- Desktop timeline no longer depends on in-memory SSE events as the only conversation surface.
- Rust session message v2 transcript is now the primary recoverable chat timeline, with live App Bridge events rendered as execution trace underneath.
- Direct tool turns now persist tool result messages, so tool output can survive reload and become the basis for richer tool cards later.

Implemented:

- Added Desktop message v2 types and `SessionMessagesPayload`.
- Added `refreshSessionMessages`, wired to active session changes, prompt submit, approval/question response, undo/redo, checkpoint restore, and Stream inspector counts.
- Changed timeline rendering to show persisted message rows first, then live event rows.
- Added message row styling for user, assistant, and tool roles.
- Fixed `run_http_tool_turn` so direct tool calls reuse `append_completed_tool_result` and persist tool result messages.
- Moved direct tool assistant message id/index allocation after tool messages so user/tool/assistant transcript order is stable.
- Extended the HTTP runtime targeted test to assert `/api/sessions/{id}/messages` returns v2 messages for approval flow and direct tool flow.

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
npm --prefix desktop run build
BRIDGE=http://127.0.0.1:18794 APP=http://127.0.0.1:5178 node <inline Playwright message-persistence smoke>
```

Evidence:

- Targeted Rust test passed and now covers `/messages` returning user/tool after approval and user/tool/assistant after direct tool execution.
- Desktop build passed with `tsc && vite build`.
- Rendered smoke pre-created a completed tool session, intentionally blocked `/api/events` with 503, loaded a fresh Desktop page, selected the session, and still rendered 3 `.message-row` cards from `/messages`: user prompt, tool output `persisted-message`, assistant `tool execution completed`.
- Stream inspector showed `Events0 Messages3`, proving the rendered transcript did not depend on live event memory.
- Vite overlay 0, page errors 0; the only console issue was the expected intercepted `/api/events` 503 resource log.
- Screenshot inspected at `/tmp/openagent-desktop-message-persistence-smoke.png`.
- Temporary listeners on 18794 and 5178 were stopped after verification.

Residual risk:

- Message UI is still a simple transcript, not full OpenCode/Codex-style tool cards with expandable structured parts.
- Pagination using `before` is supported by API but not yet exposed in Desktop.
- SSE reconnect/resume still needs a dedicated UX/protocol tightening pass.

## 2026-07-01 Desktop Files/Git Rollback Slice

Desktop Agent Workspace alignment:

- Desktop trust boundary now closes the visible rollback loop: approval -> file mutation -> diff/checkpoint -> file preview -> checkpoint restore -> file preview rollback.
- App Bridge file/git context is visible in the Desktop inspector instead of being hidden behind raw event JSON.

Implemented:

- Added Desktop Files inspector card backed by `/api/files`, showing workspace, entry count, root entries, focus path, and text preview.
- Added Desktop Git inspector card backed by `/api/git`, showing branch, ahead/behind, change count, and changed paths.
- Wired session trust refresh so approval, question response, undo/redo, checkpoint restore, active session changes, and refresh paths update diff/checkpoints/files/git together.
- Added stable `data-checkpoint-id` on checkpoint restore buttons for deterministic UI smoke coverage.
- Tightened `/api/files` text detection to check file metadata size before reading content.

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
npm --prefix desktop run build
BRIDGE=http://127.0.0.1:18793 APP=http://127.0.0.1:5177 node <inline Playwright files/git restore smoke>
npm --prefix desktop run build
```

Evidence:

- Rust targeted test passed and covers pending approval listing, global approval response, diff, checkpoint list, restore, `/api/git`, and `/api/files?path=notes.txt&content=true`.
- Desktop build passed after the Files/Git UI changes.
- Rendered smoke used a temporary git workspace, created a pending write approval, clicked Allow in Desktop, verified Files preview `alpha`, used App Bridge to create a direct `beta` tool update, reloaded Desktop, clicked the specific checkpoint restore button, and verified Files preview returned to `alpha` while Git showed the target file.
- Smoke result: Vite overlay 0, restore buttons 3, console issues 0, page errors 0.
- Screenshot inspected at `/tmp/openagent-desktop-files-git-restore-smoke.png`.
- In-app Browser path was attempted first but timed out during navigation/snapshot and reset; local Playwright fallback was used for the same flow.
- Temporary listeners on 18793 and 5177 were stopped after verification.

Residual risk:

- `/api/files` remains a lightweight read-only tree/preview API, not a full editor or watcher.
- `/api/git` remains status-only; stage/commit/diff/apply workflows are not implemented.
- Desktop still needs PTY terminal, project picker, MCP/tool catalog panel, stronger SSE reconnect/resume UX, and native Tauri packaging.

## 2026-07-01 Desktop Trust Boundary Slice

Desktop Agent Workspace alignment:

- App Bridge now exposes Desktop-friendly trust boundary APIs instead of requiring the UI to infer everything from raw timeline events.
- Desktop can show and act on pending approvals/questions, inspect patch state, and browse checkpoints from the Rust runtime.
- This moves the first product loop closer to: tool approval -> file mutation -> diff visible -> checkpoint visible -> rollback route available.

Implemented:

- Extended the App Bridge protocol manifest with global approval/question endpoints and checkpoint endpoints.
- Added `GET /api/approvals` and `GET /api/questions` to list pending interactions across sessions, with optional `session_id` filtering.
- Added `POST /api/approvals/{request_id}` and `POST /api/questions/{request_id}/reply` as Desktop-friendly response routes, while keeping the existing turn-scoped routes.
- Added `GET /api/sessions/{session_id}/checkpoints` and `POST /api/sessions/{session_id}/checkpoints/{checkpoint_id}/restore`.
- `checkpoint/restored` is now emitted as an App Bridge event when a checkpoint restore route succeeds.
- Added Desktop inspector cards for Trust, Diff, and Checkpoints.
- Trust panel supports Allow/Deny and question Reply/Dismiss.
- Diff panel shows undo/redo counts, latest patch path/status/diff, and Undo/Redo actions.
- Checkpoints panel shows latest checkpoint metadata and restore buttons.

Verification:

```bash
cargo fmt --all
cargo test -p openagent-http-runtime app_bridge_trust_boundary_routes_list_approve_diff_and_restore_checkpoint -- --nocapture
npm --prefix desktop run build
cargo run -q -p openagent-http-runtime --bin openagent-http-runtime -- --host 127.0.0.1 --port 18792 --workspace /tmp/openagent-trust-smoke.*/workspace --session-root /tmp/openagent-trust-smoke.*/sessions --headless
npm --prefix desktop run dev -- --host 127.0.0.1 --port 5176
node <inline Playwright trust-boundary smoke>
```

Evidence:

- Rust targeted test passed and covered pending approval listing, global approval response, file patch diff, checkpoint listing, and checkpoint restore.
- Desktop build passed with `tsc && vite build`.
- Rendered smoke used a temporary `/tmp` workspace, created a pending write approval, approved it from Desktop, observed patch events and `+alpha` in the Diff panel, then created a direct tool change so Checkpoints displayed 3 records and 3 restore buttons.
- Screenshot inspected at `/tmp/openagent-trust-current.png`.
- Temporary listeners on 18792 and 5176 were stopped after verification.

Residual risk:

- Rendered smoke did not click the checkpoint restore button; restore behavior is covered by the Rust test.
- Question Reply/Dismiss is implemented in API and UI but was not exercised in the rendered smoke.
- This still needs `/api/files` and `/api/git` plus file tree/git status panels to fully close the Desktop trust loop visually.

## 2026-07-01 Desktop Minimal Vertical Shell Slice

Desktop Agent Workspace alignment:

- Added the first Rust-first desktop product surface under `desktop/`: a Tauri v2 shell with React/TypeScript/Vite UI.
- The Desktop UI now talks to Rust App Bridge instead of Python or a separate runtime.
- The visible workflow covers the first product path segment: open Desktop shell -> connect App Bridge -> create session -> submit prompt -> stream events into timeline -> inspect protocol/provider/session state.

Implemented:

- Created `desktop/package.json`, Vite/TypeScript config, Tauri config, React entrypoint, and the main `App.tsx`/CSS shell.
- Added left rail for project/session/Bridge config, central timeline + composer, and right inspector for protocol/provider/stream status.
- Wired App Bridge endpoints: `GET /api/protocol`, `GET /api/models?check=true`, `GET /api/sessions`, `POST /api/sessions`, `POST /api/sessions/{session}/turns`, and live `/api/events` SSE polling.
- Fixed Desktop event dedupe for React StrictMode by removing side effects from the state updater; timeline now renders streamed App Bridge events.
- Added `node_modules/` to `.gitignore` so desktop dependencies do not pollute source control.

Verification:

```bash
npm --prefix desktop run build
set -a; . .openagent/openagent.env; set +a; cargo run -q -p openagent-http-runtime --bin openagent-http-runtime -- --host 127.0.0.1 --port 18791 --workspace /Users/william/coding/harness/openharness --session-root /tmp/openagent-desktop-bridge.* --headless
npm --prefix desktop run dev -- --host 127.0.0.1 --port 5175
node <inline Playwright smoke>
```

Evidence:

- Build passed with `tsc && vite build`.
- Playwright smoke loaded `http://127.0.0.1:5175/`, preconfigured Bridge URL `http://127.0.0.1:18791`, created a new session, submitted `Reply with exactly: desktop-smoke-3`, and observed `desktop-smoke-3` in the timeline.
- Smoke state: title `OpenAgent Desktop`, 7 `.event-row` cards, protocol `openagent.app_bridge`, provider healthy, no error line, no Vite overlay, console error count 0.
- Screenshot inspected at `/tmp/openagent-desktop-smoke.png`.
- Temporary App Bridge and Vite listeners on 18791/5175 were stopped after verification.

Residual risk:

- This is not yet the full required Desktop product loop: approval/question dock, tool approval execution, diff viewer, checkpoint browser, rollback, file tree, terminal, settings, packaging, and cross-platform release are still pending.
- Browser plugin verification timed out during reload, so the final clean rendered QA used local Playwright fallback.
- Tauri native packaging/build was not run in this slice; only the web shell build and browser smoke were verified.

## 2026-07-01 App Bridge Protocol Contract And Live Client Slice

Desktop Agent Workspace alignment:

- App Bridge now exposes a machine-readable protocol contract for Desktop/TUI/CLI clients.
- Persisted SSE events carry explicit event schema and protocol versions.
- The shared app-server client now has a live SSE subscription helper instead of forcing each UI surface to hand-roll HTTP requests.

Implemented:

- Added `GET /api/protocol`, returning `openagent.app_bridge` protocol v1, event schema `openagent.app_event.v1`, required envelope fields, SSE resume/live parameters, endpoint map, event method list, and terminal methods.
- Added `schema_version` and `protocol_version` to persisted App Bridge event envelopes in `append_app_events`.
- Added `RemoteRuntimeClient::protocol()`.
- Added `RemoteRuntimeClient::global_events_live()` and `turn_events_live()`, which send `Accept: text/event-stream` and parse live SSE frames.
- Added integration coverage proving a client can read the protocol manifest, subscribe globally before a turn starts, receive versioned tool/terminal events, and fetch turn-scoped live events.

Verification:

```bash
cargo test -p openagent-http-runtime app_bridge_protocol_contract_and_client_live_subscription -- --nocapture
cargo test -p openagent-http-runtime global_sse_live_tails_events_after_connection -- --nocapture
cargo test -p openagent-http-runtime app_bridge_provider_health_uses_runtime_provider_config_without_leaking_key -- --nocapture
cargo check -p openagent-http-runtime -p openagent-app-server-client
cargo test -p openagent-app-server-client --lib -- --nocapture
cargo fmt --all -- --check
cargo run -q -p openagent-http-runtime --bin openagent-http-runtime -- --headless ...; curl /api/protocol
```

Evidence:

- `/api/protocol` smoke returned `protocol: openagent.app_bridge`, `protocol_version: 1`, `event_schema_version: openagent.app_event.v1`, and `global_events: GET /api/events`.
- `app_bridge_protocol_contract_and_client_live_subscription` proved live client subscription receives `schema_version`, `protocol_version`, tool completion, `global_sequence`, and terminal turn completion events.

Residual risk:

- The protocol manifest is machine-readable but not yet mirrored into the requested long-form `docs/architecture/APP_BRIDGE_PROTOCOL.md`.
- Client live helper returns one live response window, not an infinite reconnecting subscription manager. Desktop can build reconnect/backoff on top next.
- Existing event `params` payloads are listed by method but not yet fully JSON-schema-typed.

## 2026-07-01 App Bridge Provider Diagnostics Slice

Desktop Agent Workspace alignment:

- Rust App Bridge now exposes provider/model diagnostics instead of a static placeholder model list.
- The HTTP runtime's real turn execution and `/api/models?check=true` use the same runtime provider config resolver.
- Desktop/TUI/CLI clients can ask the App Bridge for provider health without seeing API secrets.

Implemented:

- Upgraded `GET /api/models` to return provider, base URL, model, wire API, config sources, key status, variants, thinking modes, and model records.
- Added live provider probe mode through `GET /api/models?check=true` / `refresh=true`, which performs a real OpenAI-compatible `GET {base_url}/models`.
- Added `RemoteRuntimeClient::provider_health()` for shared CLI/TUI/Desktop access.
- Rewired HTTP runtime `provider_turn_result` to use the same provider resolver for payload, session metadata, provider env, generic `OPENAGENT_*`, auth file, and defaults.
- Isolated HTTP runtime tests from the developer machine's real provider env/auth unless a test explicitly provides fake provider config.

Verification:

```bash
cargo test -p openagent-http-runtime app_bridge_provider_health_uses_runtime_provider_config_without_leaking_key -- --nocapture
cargo test -p openagent-http-runtime remote_runtime_client_controls_model_agent_variant_and_thinking -- --nocapture
cargo check -p openagent-http-runtime -p openagent-app-server-client
cargo fmt --all -- --check
set -a; . .openagent/openagent.env; set +a; openagent-http-runtime --headless ...; curl /api/models?check=true
```

Evidence:

- Fake provider App Bridge test returned `healthy: true`, `model_endpoint_ok: true`, `model_count: 2`, `configured_model_available: true`, and no secret in response JSON.
- Real Sub2API App Bridge smoke returned `healthy: true`, model `gpt-5.4-mini`, `model_count: 17`, `model_endpoint_ok: true`, `configured_model_available: true`, and `api_key: set`.

Residual risk:

- `/api/models` only live-probes when `check=true` or `refresh=true`; normal calls remain cheap and do not touch the network.
- Provider config resolver is now aligned inside HTTP runtime, but the shared implementation is still duplicated between CLI and App Bridge. A later crate-level extraction would reduce drift.
- Next product slice should formalize App Bridge protocol events for Desktop/TUI continuous subscription.

## 2026-07-01 Rust Doctor And Provider Config Slice

Desktop Agent Workspace alignment:

- Rust CLI/provider is now the trusted product path for Sub2API connectivity checks.
- `openagent doctor` performs a real OpenAI-compatible `/models` probe instead of trusting `OPENAGENT_DOCTOR_MODEL_ENDPOINT_OK`.
- `openagent run` and `doctor` now share provider config resolution, reducing the old gap where run could work while doctor still reported unhealthy.

Implemented:

- Added unified provider config resolution for CLI flags, environment variables, explicit env files, auth records, and provider defaults.
- Wired Rust doctor to real `GET {base_url}/models`, with safe JSON output for endpoint, model count, and configured-model availability.
- Wired provider calls to the same config resolver, including auth-file support for run.
- Updated `config show` to display provider value/key status sources without printing secrets.
- Added CLI regression coverage for real doctor probes and auth-file-backed run without `--skip-doctor`.
- Aligned the ignored local `.openagent/openagent.env` default model to `gpt-5.4-mini`.

Verification:

```bash
cargo test -p openagent-cli binary_doctor_json -- --nocapture
cargo test -p openagent-cli binary_run_uses_auth_file_provider_config_without_skip_doctor -- --nocapture
cargo fmt --all -- --check
set -a; . .openagent/openagent.env; set +a; cargo run -q -p openagent-cli --bin openagent -- doctor --format json
set -a; . .openagent/openagent.env; set +a; cargo run -q -p openagent-cli --bin openagent -- run --format json --model gpt-5.4-mini --max-steps 1 --timeout-s 60 'Reply with exactly: pong'
```

Evidence:

- Sub2API doctor returned `healthy: true`, `model_endpoint_ok: true`, HTTP 200 from `/v1/models`, 17 models, and default model `gpt-5.4-mini` listed.
- Sub2API run returned `pong` without `--skip-doctor`.

Residual risk:

- The explicit env-file reader is opt-in through `--env-file` / `OPENAGENT_ENV_FILE` to avoid tests accidentally consuming private workspace config.
- App Bridge still needs to reuse this resolver and expose provider diagnostics through product APIs.

## 2026-06-30 Message Persistence And Checkpoint Slice

OpenCode/Claude Code alignment:

- Promoted transcript v2 replay ahead of `state.latest.json` legacy message cache when loading sessions.
- Made stale-state recovery work when a user message has been appended to transcript but the latest state cache has not yet been rewritten.
- Marked unfinished replayed tool calls as `interrupted`, so a killed turn does not resume with a broken pending tool result.
- Added file checkpoints with diff/restore support, patch parts, compaction boundaries, fork/truncate APIs, and CLI restore/list entry points.
- Moved runtime checkpoints to the tool-mutation boundary so upstream provider streaming still emits first deltas immediately.
- Added first-class `parent_message_id`, `seq`, `updated_at_ms`, and `completed_at_ms` fields to `MessageInfo`, with replay normalization for legacy records.

Implemented:

- `FileSessionStore::save_state` now writes a `messages_v2` projection cache while treating transcript replay as authoritative.
- `FileSessionStore::load_session` now materializes messages from v2 transcript first, falling back to legacy state only when transcript projection is empty.
- Added checkpoint records under `checkpoints/`, workspace snapshot/restore, checkpoint diff records, `patch` message parts, and `checkpoint.created` / `checkpoint.restored` / `patch.detected` events.
- Added message/part tombstone replay, compaction-boundary replay trimming, and v2 fork remapping.
- CLI `--fork` and HTTP `fork_from` now use the v2 fork path rather than copying legacy `Session.messages`.
- CLI and HTTP provider/tool loops now create checkpoint context around tool mutations and append patch parts when workspace files change.
- Added `openagent session checkpoints` and `openagent session restore` commands.

Verification:

```bash
cargo fmt
cargo test -p openagent-protocol --test message_v2
cargo test -p openagent-session --test session_trace
cargo test -p openagent-cli --test cli_commands
cargo test -p openagent-http-runtime --test http_runtime
cargo check --workspace
git diff --check
```

Residual risk:

- Checkpoints currently use a lightweight file-copy snapshot and `DefaultHasher` fingerprint, not a content-addressed store or git worktree snapshot.
- Snapshot coverage intentionally skips `.openagent`, `.git`, `target`, and `node_modules`.

---

## 2026-06-30 Step 10 Subagent Workspace Isolation

OpenCode/Claude Code alignment:

- Write-capable subagents can now run in an isolated workspace copy instead of sharing the parent workspace directly.
- Isolation is opt-in per Task call (`isolate_workspace` / `workspace_isolation`) or per agent profile (`workspace_isolation: true`), matching the idea that risky workers should be explicitly isolated.
- Isolated child sessions still use the normal Task tool path, independent context, profile prompt, tool filtering, permissions, lineage metadata, and lifecycle APIs.

Implemented:

- Added `prepare_isolated_workspace` in `openagent-tools`, which creates a deterministic per-task directory copy under the session store and skips heavy/recursive directories such as `.git`, `target`, `node_modules`, virtualenvs, and the isolation target itself.
- Extended the Task tool schema with `isolate_workspace`.
- Added `workspace_isolation` / `isolate_workspace` profile parsing and public profile metadata in CLI and HTTP runtimes.
- CLI Task execution now switches the child session workspace to the isolated copy before running the child loop, so child file tools operate inside the copy.
- HTTP Task execution now does the same for foreground and queued background tasks, and `GET /api/sessions/{session_id}/tasks` surfaces `workspace` and `workspace_isolation` directly.
- Child session state and Task tool result metadata record isolation method, source workspace, and isolated workspace path.

Verification:

```bash
cargo fmt
cargo test -p openagent-tools prepare_isolated_workspace_copies_workspace_without_heavy_dirs --test tool_runtime
cargo test -p openagent-cli binary_run_executes_subagent_in_isolated_workspace --test cli_commands
cargo test -p openagent-http-runtime task_subagent_runs_in_isolated_workspace --test http_runtime
cargo test -p openagent-tools
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-cli
cargo test -p openagent-protocol
cargo test -p openagent-app-server-client
```

Residual risk:

- This slice implements isolated directory copies, not git worktree branch creation or merge-back review.
- Isolation skips symlinks and heavy directories by design; specialized repos may need future profile-level include/exclude controls.
- Merge-back conflict review and cleanup policy are still product decisions outside this 4-10 subagent completion pass.

## 2026-06-30 Step 9 Description-Based Automatic Subagent Routing

OpenCode/Claude Code alignment:

- Subagents are no longer only explicit `@subagent` or direct Task-tool targets; the harness can now route a user prompt to a matching subagent based on the subagent description.
- Automatic routing uses the same filtered descriptor list that powers the Task tool, so hidden agents, denied `permission.task` routes, and nested-governance-rejected agents are not eligible candidates.
- The route remains inspectable: parent task events expose `auto=true`, route metadata, deterministic `auto_task_<subagent>` call ids, and child sessions persist the normal task lineage/profile metadata.

Implemented:

- Added `select_task_subagent_for_prompt` in `openagent-tools`, with conservative lexical scoring over subagent id, name, and description. It routes only when there is a unique high-confidence match.
- CLI `run` now attempts automatic subagent routing after pending resumes and manual `@subagent` handling, but before calling the parent provider. Manual invocation still takes precedence.
- HTTP `start_turn` now performs the same automatic routing when there are no explicit `tool_call(s)` and no manual `@subagent` invocation.
- Auto-routed turns create normal Task tool calls with `command=auto_route`, run the selected child subagent in its independent context, and return the task result to the parent.

Verification:

```bash
cargo fmt
cargo test -p openagent-tools task_subagent_router_selects_unique_description_match --test tool_runtime
cargo test -p openagent-cli binary_run_auto_routes_prompt_to_matching_subagent_description --test cli_commands
cargo test -p openagent-http-runtime start_turn_auto_routes_prompt_to_matching_subagent_description --test http_runtime
cargo test -p openagent-tools
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-cli
cargo test -p openagent-protocol
cargo test -p openagent-app-server-client
```

Residual risk:

- The router is intentionally conservative and lexical; prompts that require semantic judgement may still be left for the provider to handle through the normal Task tool.
- Automatic routing currently runs foreground tasks. Background auto-routing remains a later product choice.
- Route scoring is shared by CLI and HTTP, but future profile fields such as explicit route keywords could make selection more precise.

## 2026-06-30 Step 8 Scout External Docs And Dependency Research Subagent

OpenCode alignment:

- Added a dedicated Scout subagent class for external documentation and dependency research, matching OpenCode's task-oriented "specialized worker" concept instead of forcing all research through a general agent.
- Scout is a real Task-routeable subagent profile in both CLI and HTTP runtimes, with its own prompt, READONLY permission, and constrained tool access.
- External research is backed by an actual read-only network tool instead of a fake profile label, so Scout can fetch source material inside its isolated child context and return only the summary/result to the parent task.

Implemented:

- Added `web_fetch` as a built-in tool in `openagent-tools` with HTTP(S)-only URL validation, timeout, redirect limit, max-byte cap, content metadata, truncation handling, and localhost proxy bypass for deterministic local tests.
- Added `web_fetch` to READONLY and PLAN_ONLY permission rules so read-only research agents can execute it without opening write/shell permissions.
- Added a shared `skill/prompts/scout.txt` system prompt focused on source-backed external docs/dependency research.
- Added built-in `scout` profiles in CLI and HTTP runtime registries with READONLY permission and a tight read-only tool allowlist: `web_fetch`, workspace search/read tools, `skill`, and `todoread`.
- HTTP Task subagent execution now proves Scout can fetch external docs in the child provider loop, pass the fetched content back through `function_call_output`, and complete as a normal task session with lifecycle metadata.

Verification:

```bash
cargo fmt
cargo test -p openagent-tools web_fetch_reads_http_sources_under_readonly_permissions --test tool_runtime
cargo test -p openagent-cli binary_agent_registry_exposes_builtin_subagents --test cli_commands
cargo test -p openagent-http-runtime task_subagent_scout_fetches_external_docs --test http_runtime
cargo test -p openagent-protocol
cargo test -p openagent-tools
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-cli
cargo test -p openagent-app-server-client
```

Residual risk:

- Scout currently has direct URL fetch, not full search/ranking. A future slice can add a search tool or provider-backed search integration if product scope requires it.
- `web_fetch` intentionally caps body size and returns text via lossy UTF-8 conversion; binary/PDF/document extraction remains outside this slice.
- CLI Task execution can route to `scout`, but this slice's live fetch-through-provider proof is covered on the HTTP runtime path where fake provider sequencing already exercises real tool loops.

## 2026-06-30 Step 7 Task Navigation And Background Visualization

OpenCode alignment:

- Task/subagent sessions are now visible and navigable from user-facing surfaces instead of only existing as metadata in the session store.
- Background tasks show queued/running/completed/canceled state in the UI, with explicit run/cancel controls for queued background work.
- CLI/TUI remote attach gains a lightweight task navigation surface similar to OpenCode's task-oriented workflow: inspect task trees and jump into a task session.

Implemented:

- Added `RemoteRuntimeClient::tasks_payload(session_id)` so app/TUI/CLI integrations can read the full task response, including `tree` and `flat_tasks`.
- Added remote attach commands `/tasks` and `/task <task_session_id>`:
  - `/tasks` renders the current session's recursive task tree with status, subagent, depth, background marker, and title.
  - `/task <id>` switches the active remote session to the chosen subagent task session.
- Added the same `/tasks` and `/task <id>` support to the remote TUI terminal handler.
- Web app sidebar now renders the active session's task tree, including nested tasks and queued background tasks.
- Web app task rows can open a task session; queued background tasks expose Run and Cancel buttons wired to the existing task lifecycle APIs.
- Web app refreshes the task tree on session selection, turn completion/interruption, and Task tool completion/failure events.

Verification:

```bash
cargo fmt
node --check runtime/static/app-server/static/app.js
cargo test -p openagent-cli remote_
cargo test -p openagent-http-runtime app_bridge_http_routes_cover_static_sse_auth_and_tui_control --test http_runtime
cargo test -p openagent-http-runtime task_subagent_nested_tree_and_governance_guards --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-cli
cargo test -p openagent-app-server-client
```

Residual risk:

- Remote `/tasks` is text-first rather than a full interactive picker; a richer keyboard task picker can be layered later.
- Web app styling is intentionally compact and utilitarian; no dedicated app-side visual regression/browser screenshot test was added in this slice.
- Task color from Step 6 is not yet used for per-agent row styling.

## 2026-06-30 Step 6 OpenCode Agent Config Compatibility

OpenCode alignment:

- Project agents can now be authored as Markdown files with frontmatter, matching the OpenCode-style "frontmatter config + Markdown body prompt" workflow.
- The runtime understands OpenCode-style agent fields including `steps`, `hidden`, `disable`, `color`, `temperature`, and `top_p`.
- Extra model option fields in agent config are preserved and propagated to provider payloads instead of being discarded.

Implemented:

- CLI and HTTP agent discovery now reads JSON and Markdown profiles from `.openagent/agents`, `.opencode/agents`, and `.opencode/agent`.
- Markdown profile parsing uses YAML frontmatter and treats the Markdown body as the agent prompt when `prompt` is not explicitly set.
- Added `disabled`, `color`, `temperature`, `top_p`, and `model_options` fields to CLI and HTTP profile metadata/public payloads.
- `steps`, `max_steps`, and deprecated `maxSteps` now resolve to the same execution step limit.
- Disabled agents are not loaded; hidden agents remain loadable but are omitted from user/model visible subagent listings.
- HTTP provider requests now merge profile/session model options into the OpenAI payload; CLI provider requests do the same for active agent profiles.
- Child subagent sessions persist model options, color, temperature, and top_p for resume/audit evidence.
- Added CLI and HTTP integration tests for `.opencode/agents/*.md`, hidden/disabled handling, Markdown body prompts, step limits, and provider option propagation.

Verification:

```bash
cargo fmt
cargo test -p openagent-cli binary_agent_registry_loads_opencode_markdown_agents --test cli_commands
cargo test -p openagent-http-runtime task_subagent_loads_opencode_markdown_agent_options --test http_runtime
cargo test -p openagent-http-runtime remote_runtime_client_executes_task_subagent_tool --test http_runtime
cargo test -p openagent-http-runtime task_subagent_nested_tree_and_governance_guards --test http_runtime
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-cli --test cli_commands
cargo test -p openagent-app-server-client
```

Residual risk:

- Markdown parsing relies on normal YAML frontmatter through `serde_yaml`; exotic OpenCode config syntaxes beyond YAML frontmatter are not covered.
- Global OpenCode config directories are not loaded yet; this slice covers project-local compatibility.
- `color` is parsed and surfaced but not yet used by app/TUI rendering; that belongs with Step 7 UI visualization work.

## 2026-06-30 Step 5 Nested Subagent Governance And Task Tree

OpenCode alignment:

- Subagents can now spawn nested subagents while preserving a clear task lineage instead of flattening every child under the original parent.
- The runtime exposes recursive task structure, making nested subagent execution inspectable as a tree.
- Governance now prevents obvious runaway recursion: self-calls, repeated agents in the lineage, and depth beyond a configurable maximum are rejected before a child session is created.

Implemented:

- HTTP and CLI Task tool registration now filters available subagents by the current session's depth/lineage, so child agents do not see unavailable recursive routes.
- HTTP and CLI Task execution now enforce the same governance even for forged `task` tool calls.
- Child subagent sessions now persist `task_depth`, `task_root_session_id`, `task_parent_session_id`, and `task_lineage_subagents`.
- `GET /api/sessions/{session_id}/tasks` remains backward-compatible with direct `tasks`, and now also returns recursive `tree` plus preorder `flat_tasks`.
- Added `OPENAGENT_MAX_SUBAGENT_DEPTH` with default depth `3`.
- Added an HTTP integration test where `outer -> inner` succeeds, while `inner -> inner` self-call and `inner -> third` over-depth calls fail without creating child sessions.

Verification:

```bash
cargo fmt
cargo test -p openagent-http-runtime task_subagent_nested_tree_and_governance_guards --test http_runtime
cargo test -p openagent-http-runtime task_subagent_task_id_resumes_existing_child_session --test http_runtime
cargo test -p openagent-http-runtime remote_runtime_client_executes_task_subagent_tool --test http_runtime
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-cli binary_run_emits_provider_sse_delta_before_stream_closes --test cli_commands
cargo test -p openagent-cli --test cli_commands
```

Note:

- The first full CLI run hit a transient SSE timing assertion in `binary_run_emits_provider_sse_delta_before_stream_closes`; the failed test passed immediately when rerun directly, and the full CLI suite then passed.

Residual risk:

- The recursive task tree is API-visible but not yet rendered in TUI/app navigation; that remains Step 7.
- Governance is session/metadata based, not worktree-isolation based; isolated worktrees remain a later parity item.
- `OPENAGENT_MAX_SUBAGENT_DEPTH` is process-wide for now rather than per-agent config/frontmatter.

## 2026-06-30 Step 4 Task `task_id` Resume/Continue Semantics

OpenCode alignment:

- Task/subagent sessions now behave like stable task handles: passing `task_id` continues the existing child session instead of silently creating an unrelated subagent context.
- Resume is scoped to the original parent session and original subagent profile, closing off cross-parent task hijacks and accidental agent switching.
- Continuing a task preserves the child transcript and appends a new user prompt, while keeping the subagent system prompt bound exactly once.

Implemented:

- HTTP Task tool validates `task_id` before reuse: the target must be a subagent, belong to the current parent session, match the requested subagent profile, and not be queued/running/canceled/paused/compacting.
- CLI Task tool now applies the same parent/profile/status resume validation and records resume metadata.
- HTTP subagent system prompt binding is idempotent, so resumed child sessions do not accumulate duplicate system prompts.
- Resume metadata now records `task_resume_count` and `task_resumed_at_ms` on child sessions for debugging and lifecycle evidence.
- Added an HTTP integration test that starts a subagent, rejects wrong-agent and wrong-parent resumes, then continues the same `task_id` and verifies one child session, one system prompt, both user prompts, and preserved provider context.

Verification:

```bash
cargo fmt
cargo test -p openagent-http-runtime task_subagent_task_id_resumes_existing_child_session --test http_runtime
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
cargo test -p openagent-http-runtime remote_runtime_client_executes_task_subagent_tool --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-cli --test cli_commands
cargo test -p openagent-app-server-client
```

Residual risk:

- CLI has the same resume validation and execution path, but the existing CLI mock provider only emits mock tool calls on fresh tool-free transcripts, so the full same-parent CLI continuation scenario is covered indirectly by HTTP integration plus CLI Task regression.
- Background completed-task continuation is supported through the same `task_id` path, but explicit retry/backoff/heartbeat/index semantics remain planned for the high-availability step.

## 2026-06-30 Step 3 HTTP Background Task Worker

OpenCode alignment:

- Background subagent tasks now run automatically after being queued, instead of requiring a user/client to call `run_task`.
- The worker reuses the same Task runtime path as explicit execution, including child context, tool filtering, model/provider selection, `max_steps`, task lifecycle state, and run lock protection.
- Existing explicit `run_task`, cancel, duplicate-run lock, and stale-lock recovery paths remain testable by disabling the worker with `OPENAGENT_BACKGROUND_WORKER=0`.

Implemented:

- HTTP runtime starts a lightweight background task worker when the server binds successfully.
- Worker scans the session store for `subagent=true`, `background=true`, `task_status=queued` child sessions and consumes them in deterministic order.
- Worker execution calls the existing `run_session_task_payload`, so atomic `task.run.lock`, stale-lock recovery, status transitions, provider loop behavior, and failure handling stay shared.
- Added `OPENAGENT_BACKGROUND_WORKER=0` to disable scheduling and `OPENAGENT_BACKGROUND_WORKER_POLL_MS` to tune the scan interval.
- Task metadata records `run_started_by=background_worker` or `run_task` for debugging.
- Added a worker integration test proving a queued background task is automatically run to completion without explicit `run_task`.

Verification:

```bash
cargo test -p openagent-http-runtime task_subagent_background_worker_auto_runs_queued_task --test http_runtime
cargo test -p openagent-http-runtime task_subagent_background_true_queues_queryable_task --test http_runtime
cargo test -p openagent-http-runtime task_subagent_run_rejects_duplicate_consumer --test http_runtime
cargo test -p openagent-http-runtime task_subagent_run_recovers_stale_lock --test http_runtime
cargo test -p openagent-http-runtime task_subagent_cancel_rejects_later_run --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
cargo test -p openagent-cli --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md
```

Residual risk:

- The worker is single-process and scan-based; multi-process coordination relies on the existing lock files but does not yet have a persistent distributed queue.
- No heartbeat/lease renewal is implemented yet; stale-lock recovery remains time-threshold based.
- Running-task cancel and retry/backoff policy remain later high-availability steps.

## 2026-06-29 Step 2 OpenCode-Style `@subagent` Manual Invocation

OpenCode alignment:

- Users can manually route work to a subagent with `@subagent prompt` instead of waiting for the primary model to choose the Task tool.
- Manual invocation reuses the same Task tool execution path, child session metadata, permission checks, and result envelope as model-initiated task calls.
- Step 1 `permission.task` rules still apply: allowed `@subagent` calls run, denied `@subagent` calls fail before creating a child task.

Implemented:

- CLI `openagent run "@allowed-worker ..."` now detects a leading `@subagent` mention, synthesizes a Task tool call, and executes the subagent directly without a parent provider call.
- HTTP `start_turn` now detects leading `@subagent` input when no explicit `tool_call` payload is supplied, then runs the same direct task-turn path used by app integrations.
- Manual Task tool events are marked with `manual=true`, and CLI manual runs report `source=manual_subagent`.
- Added CLI and HTTP integration tests covering allowed manual subagent execution and denied manual subagent enforcement.

Verification:

```bash
cargo test -p openagent-cli binary_run_invokes_subagent_with_at_mention --test cli_commands
cargo test -p openagent-http-runtime start_turn_invokes_subagent_with_at_mention --test http_runtime
cargo test -p openagent-cli --test cli_commands
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-tools
cargo test -p openagent-app-server-client
git diff --check -- src/tools/src/toolkit.rs cli/src/cli.rs cli/src/prompt.rs cli/src/prompt/profile.rs cli/src/prompt/agent_loop.rs cli/tests/cli_commands.rs runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md
```

Residual risk:

- This slice implements leading prompt mentions (`@agent prompt`); interactive autocomplete, inline mentions mid-message, and TUI visual affordances remain future UI work.
- Manual invocation currently returns the raw task envelope as the final answer in CLI; richer parent summarization can be layered later.
- HTTP/app clients need UI affordances to discover the syntax; the runtime behavior is available now.

## 2026-06-29 Step 1 OpenCode-Style `permission.task`

OpenCode alignment:

- Agent profile `permission.task` now controls which subagents the Task tool exposes and can execute.
- Denied subagents are removed from the provider-visible Task tool description, matching the "model cannot see denied subagents" behavior.
- Execution is re-checked through the normal permission gate, so a model-forged `subagent_type` is denied even if it bypasses the schema.
- Rules support glob-like patterns and last matching rule wins via ordered permission manager evaluation.

Implemented:

- Added shared `TaskPermissionRule` helpers in `openagent-tools`.
- CLI agent profiles now parse OpenCode-style config such as:

```json
{
  "permission": {
    "ruleset": "FULL",
    "task": {
      "*": "deny",
      "allowed-worker": "allow"
    }
  }
}
```

- CLI `run_agent_loop` now registers the Task tool with only task-permitted subagents and injects profile task rules into `ToolContext`.
- HTTP runtime now parses primary/subagent/all workspace profiles, not only subagent profiles, so a primary runtime agent can govern Task tool routing.
- HTTP provider loops and direct tool-turns now build Task tool schemas from the current agent profile and enforce the same task rules at execution.
- Agent/task metadata now preserves `task_permissions` for debugging and lifecycle inspection.

Verification:

```bash
cargo test -p openagent-cli binary_run_enforces_agent_task_permissions --test cli_commands
cargo test -p openagent-http-runtime task_tool_respects_agent_task_permissions --test http_runtime
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-tools
cargo test -p openagent-cli --test cli_commands
git diff --check -- src/tools/src/toolkit.rs cli/src/cli.rs cli/src/prompt.rs cli/src/prompt/profile.rs cli/src/prompt/agent_loop.rs cli/tests/cli_commands.rs runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md
```

Residual risk:

- `permission.task` currently supports JSON profiles; OpenCode-style Markdown/frontmatter agent files remain a later compatibility step.
- `ask` task rules pass through the existing approval path, but richer UX around subagent-specific approvals is not polished yet.
- Automatic description-based subagent routing is still future work; this slice makes the routing surface governable, not autonomous.

## 2026-06-29 HTTP Runtime Stale Task Lock Recovery

- `task.run.lock` now records a claim timestamp and `run_task` can reclaim stale lock files left by a crashed or killed runtime process.
- `cancel_task` now removes stale queued-task run locks before canceling, so abandoned locks no longer permanently trap queued background subagent tasks.
- Added an HTTP integration test proving a queued background task with an abandoned `task.run.lock` (`claimed_at_ms=0`) can still be run to completion.
- Extended the cancel integration test to prove stale locks are cleared before queued task cancellation.

Verification:

```bash
cargo test -p openagent-http-runtime task_subagent_run_recovers_stale_lock --test http_runtime
cargo test -p openagent-http-runtime task_subagent_cancel_rejects_later_run --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs runtime/app-server-client/src/app_bridge_client.rs progress.md
```

Residual risk:

- Stale recovery is time-threshold based (`OPENAGENT_TASK_RUN_LOCK_STALE_MS`, default 15 minutes), not a full worker heartbeat or lease renewal system yet.
- Background task execution is still explicit through `run_task`; automatic worker scheduling, retries, and running-task interrupt remain future high-availability slices.
- CLI `task background=true` still rejects background tasks; HTTP/app runtime remains ahead of CLI here.

## 2026-06-29 HTTP Runtime Task Run Lock

- Added an atomic `task.run.lock` claim for `POST /api/sessions/{session_id}/tasks/{task_id}/run` so only one consumer can start a queued background subagent task.
- `run_task` now re-loads and re-validates the child session after claiming the lock, records `run_claimed_at_ms`, and then starts the provider loop.
- `cancel_task` now rejects queued task cancellation while a run lock exists, avoiding the race between cancellation and just-started execution.
- Added a delayed-provider concurrent run integration test proving one request completes, the duplicate request is rejected, and only one provider request is made.

Verification:

```bash
cargo test -p openagent-http-runtime task_subagent_run_rejects_duplicate_consumer --test http_runtime
cargo test -p openagent-http-runtime task_subagent_background_true_queues_queryable_task --test http_runtime
cargo test -p openagent-http-runtime task_subagent_cancel_rejects_later_run --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs runtime/app-server-client/src/app_bridge_client.rs progress.md
```

Residual risk:

- Lock files are process-local durability guards; stale-lock recovery after a crash is not implemented yet.
- This is still explicit/synchronous background execution, not an automatic worker queue.
- CLI `task background=true` still rejects background tasks; HTTP/app runtime remains ahead of CLI here.

## 2026-06-29 HTTP Runtime Queued Task Cancel API

- Added `POST /api/sessions/{session_id}/tasks/{task_id}/cancel` for queued background subagent tasks.
- Added `RemoteRuntimeClient::cancel_task(session_id, task_id)` for app/TUI/CLI integrations.
- Cancel validates parent/task ownership, subagent identity, and `task_status=queued`, then updates the child task metadata to `task_status=canceled` with `canceled_at_ms`.
- Canceled tasks remain visible through `GET /api/sessions/{session_id}/tasks`, and subsequent `run_task` calls are rejected before any provider loop starts.
- Added an HTTP integration test proving queued task cancel changes lifecycle state and prevents later execution.

Verification:

```bash
cargo test -p openagent-http-runtime task_subagent_cancel_rejects_later_run --test http_runtime
cargo test -p openagent-http-runtime task_subagent_background_true_queues_queryable_task --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs runtime/app-server-client/src/app_bridge_client.rs progress.md
```

Residual risk:

- Cancel currently applies only to queued tasks; interrupting a running provider loop is still handled separately at turn level and is not wired as task cancel.
- Retry and duplicate-run locking remain future high-availability slices.
- CLI `task background=true` still rejects background tasks; HTTP/app runtime remains ahead of CLI here.

## 2026-06-29 HTTP Runtime Background Task Run API

- Added `POST /api/sessions/{session_id}/tasks/{task_id}/run` to explicitly consume a queued background subagent task.
- Added `RemoteRuntimeClient::run_task(session_id, task_id, extra)` for app/TUI/CLI integrations.
- The run endpoint validates parent/task ownership, subagent identity, and `task_status=queued`, then starts the saved child session with the original system/user messages and existing profile metadata.
- Queued tasks now transition through `running` to `completed` or `failed`, update `task_status`, write run summaries, and appear updated through `GET /api/sessions/{session_id}/tasks`.
- The background task integration test now proves queueing does not call the provider, explicit run does call the provider, and the lifecycle view changes to completed.

Verification:

```bash
cargo test -p openagent-http-runtime task_subagent_background_true_queues_queryable_task --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs runtime/app-server-client/src/app_bridge_client.rs progress.md
```

Residual risk:

- Background execution is explicit and synchronous through the run endpoint; there is still no automatic worker loop.
- Cancel, retry policy, concurrent run locking, and queued task index persistence remain future slices.
- CLI `task background=true` still rejects background tasks; this slice is HTTP/app runtime only.

## 2026-06-29 HTTP Runtime Background Task Queue Foundation

- HTTP `task` tool now accepts `background: true` for subagents instead of rejecting it.
- Background task requests create an independent subagent child session with system/user messages, agent profile, parent linkage, model/provider, permission, max_steps, `background=true`, and `task_status=queued` metadata.
- Parent task results now return a queued `<task ...>` result with task/session/run ids, so callers can immediately track the background task through `GET /api/sessions/{session_id}/tasks`.
- Task lifecycle summaries now prioritize explicit `task_status` metadata and expose `background` plus `run_status`, allowing queued tasks to be represented before a provider run exists.
- Added an HTTP integration test proving `background: true` queues a queryable subtask without invoking a provider loop.

Verification:

```bash
cargo test -p openagent-http-runtime task_subagent_background_true_queues_queryable_task --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md
```

Residual risk:

- Queued background tasks do not execute yet; this slice creates the durable task object and lifecycle surface only.
- CLI `task background=true` still rejects background tasks; HTTP/app runtime is now ahead of CLI on this path.
- Cancel/resume/worker scheduling and explicit task index persistence remain future high-availability slices.

## 2026-06-29 HTTP Runtime Subtask Lifecycle API

- Added `GET /api/sessions/{session_id}/tasks` as a task-specific lifecycle view over subagent child sessions.
- Added `RemoteRuntimeClient::tasks(session_id)` so app/TUI/CLI integrations can query subtask state without scraping generic child sessions or turn events.
- Task summaries now include task/session/run ids, run status from child `summary.json`, session status, description/title, subagent type, agent profile, provider/model, permission, max_steps, parent session/run/tool-call ids, finish reason, error, and original metadata.
- Upgraded HTTP task subagent integration coverage so both completed and failed/max-step subagents are visible through the task lifecycle API.

Verification:

```bash
cargo test -p openagent-http-runtime remote_runtime_client_executes_task_subagent_tool --test http_runtime
cargo test -p openagent-http-runtime task_subagent_profile_max_steps_failure_propagates_to_parent --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-app-server-client
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs runtime/app-server-client/src/app_bridge_client.rs progress.md
```

Residual risk:

- The lifecycle API is read-only and foreground-only for now; background start/cancel/resume control remains future work.
- Task status is reconstructed from session-store run files, which is good enough for current runs but should become an explicit task index when background tasks arrive.
- Nested subagent trees are naturally representable through parent_session_id, but no recursive tree endpoint is implemented yet.

## 2026-06-29 HTTP Runtime Subagent Max Steps

- HTTP task subagents now pass profile `max_steps` into the child provider loop instead of only recording it on the child run metadata.
- Child subagent sessions now persist `max_steps` in session metadata for debugging and resume evidence.
- Parent `task` results now treat child run statuses other than `completed` as failed tool results, so max-step exhaustion and other child failures are no longer reported as successful task completions.
- Added an HTTP runtime integration test where a workspace `one-step` subagent performs a tool call, exhausts `max_steps=1`, and propagates the failed child status back to the parent task event.

Verification:

```bash
cargo test -p openagent-http-runtime task_subagent_profile_max_steps_failure_propagates_to_parent --test http_runtime
cargo test -p openagent-http-runtime remote_runtime_client_executes_task_subagent_tool --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs progress.md
```

Residual risk:

- Background task execution is still intentionally rejected; task execution remains foreground only.
- Worktree isolation, nested/background task orchestration, and richer task lifecycle APIs remain future high-availability slices.
- Parent direct tool-turn responses still return an overall completed turn with a failed tool event; this matches existing HTTP behavior, but richer task lifecycle APIs should expose failed subtask status more directly.

## 2026-06-29 HTTP Runtime Workspace Subagent Tools

- HTTP/app runtime now merges built-in runtime subagents with workspace `.openagent/agents/*.json` profiles for `/api/agents`, task tool descriptions, and task execution, resolving the previous static-profile gap.
- Workspace subagent profiles now carry `tools` metadata through `/api/agents`, child session state, and provider-visible tool schemas.
- Runtime provider loops now reconstruct subagent profile metadata from child sessions and filter visible tools by profile wildcard patterns; disallowed tool calls are rejected with a failed tool result instead of executing.
- The HTTP task subagent integration test now proves a workspace `deep-research` profile can restrict tools to `read`, hide `write` from the provider payload, reject a forged `write` call, and still return the subagent final result.

Verification:

```bash
cargo test -p openagent-http-runtime remote_runtime_client_executes_task_subagent_tool --test http_runtime
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-cli binary_run_executes_task_subagent_tool --test cli_commands
git diff --check -- runtime/http/src/http_runtime.rs runtime/http/tests/http_runtime.rs
```

Residual risk:

- Background task execution is still intentionally rejected; task execution remains foreground only.
- Worktree isolation, nested/background task orchestration, and richer task lifecycle APIs remain future high-availability slices.

## 2026-06-29 Subagent Task Tool Routing

- Added first-class `task` tool schema in `openagent-tools`, with OpenCode-style parameters (`description`, `prompt`, `subagent_type`, optional `task_id`/`command`/`background`) and dynamic subagent descriptions.
- Added built-in CLI agent profiles (`build`, `general`, `explore`, `plan`) and merged them with `.openagent/agents/*.json`, with user profiles overriding built-ins.
- Implemented CLI `task` execution as an independent subagent session/run: parent invokes `task`, child receives its own system prompt, tool filtering, model/provider selection, permission ruleset, and session metadata; parent only receives the final task result.
- Implemented the same runtime concept for HTTP/app mode with runtime subagent profiles and child session metadata.
- Updated permission pattern extraction to use `subagent_type`/agent fields so task approvals are readable and reusable.

Verification:

```bash
cargo test -p openagent-cli --test cli_commands
cargo test -p openagent-http-runtime --test http_runtime
cargo test -p openagent-tools
cargo test -p openagent-core
```

Residual risk:

- Background task execution is intentionally rejected for now; task execution is foreground only.
- HTTP runtime subagent profiles are currently built-in/static and not yet merged with workspace `.openagent/agents` registry.

## 2026-06-23 TUI Color Scheme Slice

- Added OpenCode-style color scheme command coverage for `system`, `light`, and `dark`.
- Extended `TuiConfig` with `color_scheme`, including `.openagent/tui.jsonc` loading via `color_scheme` or `scheme` and `/config` visibility.
- Added `/theme-scheme`, `/color-scheme`, and `/scheme` command aliases:
  - no argument opens/lists available schemes depending on event-loop path
  - `system|light|dark` sets the scheme
  - `cycle`/`next` cycles through `system -> light -> dark`
- Added a shared picker surface for color schemes with current marker, keyboard filter/select flow, and terminal render snapshot coverage.
- Added App Bridge/TUI control aliases for `open-theme-schemes`, `select-theme-scheme`, `cycle-theme-scheme`, plus `color-scheme` aliases and OpenCode-style `theme.scheme.light` publish topics.
- Updated the App Bridge TUI golden fixture for the expanded command/action contract.

Verification:

```bash
cargo test -q -p openagent-tui color_scheme
cargo test -q -p openagent-tui control_requests_open_model_theme_and_palette_surfaces
cargo test -q -p openagent-tui tui_config_loads_jsonc_and_theme_command_updates_state
cargo test -q -p openagent-tui --test tui_control
cargo test -q -p openagent-tui
cargo check -q -p openagent-tui -p openagent-app-server-client
git diff --check
```

Residual risk:

- Color scheme is now selectable and persisted in TUI config state, but it does not yet preview or recolor every theme token the way OpenCode's browser UI does.
- Theme preview-on-highlight remains a separate future slice.

## 2026-06-23 TUI Theme Picker Slice

- Upgraded `/themes` and `/theme` with no argument from passive theme listing into an OpenCode-style keyboard theme picker.
- Reused the shared choice picker dock for themes, including query filtering, Up/Down/Tab selection, Enter-to-select, Esc-to-close, and a `current` marker for the active TUI theme.
- Kept direct theme setting compatible: `/themes <name>`, `/theme <name>`, and `/tui/select-theme` still update `TuiConfig.theme` immediately.
- Updated `/tui/open-themes` remote control to open the same picker path with direct theme payload support.
- Added keyflow, remote control, and terminal render snapshot coverage for the theme picker.

Verification:

```bash
cargo test -q -p openagent-tui theme_picker
cargo test -q -p openagent-tui control_requests_open_model_theme_and_palette_surfaces
cargo test -q -p openagent-tui tui_config_loads_jsonc_and_theme_command_updates_state
cargo test -q -p openagent-tui
cargo check -q -p openagent-tui -p openagent-app-server-client
git diff --check
```

Residual risk:

- This implements theme selection as a TUI picker; OpenCode-style theme preview-on-highlight and color-scheme cycle/set remain future slices.
- Theme choices are still the local OpenAgent TUI theme list unless a remote control payload supplies a custom list.

## 2026-06-22 TUI Variant Thinking Picker Slice

- Upgraded `/variant` and `/thinking` from passive command/help paths into OpenCode-style keyboard pickers.
- Added a shared choice picker dock with query filtering, Up/Down/Tab selection, Enter-to-select, and Esc-to-close.
- Wired both pickers through `TerminalEventHandler::list_models`; `AppBridgeTerminalHandler` backs them with `RemoteRuntimeClient::models`, so opening either picker calls real `GET /api/models`.
- Selection reuses the existing `/variant <name>` and `/thinking <level>` commands and writes the current session setting through `PATCH /api/sessions/{session_id}`.
- Updated `/tui/open-variants` and `/tui/open-thinking` remote control to use the same handler-backed picker path when no direct payload is supplied, while preserving direct payload support for embedded control requests.
- Added keyflow, remote control, terminal render snapshot, and App Bridge handler smoke coverage.

Verification:

```bash
cargo test -q -p openagent-tui variant_and_thinking
cargo test -q -p openagent-tui terminal_render_snapshot_contains_choice_picker_overlay
cargo test -q -p openagent-tui
cargo check -q -p openagent-tui -p openagent-app-server-client
git diff --check
```

Residual risk:

- This completes the variant/thinking picker path only; theme picker and richer agent/variant profile semantics remain future slices.
- The App Bridge smoke uses a deterministic in-test bridge server, not a full provider-backed runtime.

## 2026-06-22 TUI Agent Picker Slice

- Upgraded `/agents` from a passive list command into an OpenCode-style keyboard agent/profile picker.
- Added a TUI agent picker dock with query filtering, Up/Down/Tab selection, Enter-to-select, and Esc-to-close.
- Wired the picker through `TerminalEventHandler::list_agents`; `AppBridgeTerminalHandler` now backs it with `RemoteRuntimeClient::agents`, so opening the picker calls real `GET /api/agents`.
- Selection reuses the existing `/agent <id>` path and writes the current session profile through `PATCH /api/sessions/{session_id}`.
- Updated `/tui/open-agents` remote control to open the same picker path, with direct agent payload support and handler-backed fetch support.
- Added keyflow, remote control, terminal render snapshot, and App Bridge handler smoke coverage.

Verification:

```bash
cargo test -q -p openagent-tui key_event_flow_opens_agent_picker_filters_and_selects
cargo test -q -p openagent-tui remote_control_open_agents_dispatches_picker_fetch
cargo test -q -p openagent-tui terminal_render_snapshot_contains_agent_picker_overlay
cargo test -q -p openagent-tui app_bridge_terminal_agent_picker_fetches_and_sets_agent
cargo test -q -p openagent-tui
cargo check -q -p openagent-tui -p openagent-app-server-client
```

Residual risk:

- This completes the agent/profile picker path only; variant and thinking pickers still have command/control coverage but are not yet full keyboard docks.
- The App Bridge smoke uses a deterministic in-test bridge server, not a full provider-backed runtime.

## 2026-06-22 TUI Model Picker Slice

- Upgraded `/models` from a passive list command into an OpenCode-style keyboard model picker.
- Added a TUI model picker dock with query filtering, Up/Down/Tab selection, Enter-to-select, and Esc-to-close.
- Wired the picker through `TerminalEventHandler::list_models`; `AppBridgeTerminalHandler` now backs it with `RemoteRuntimeClient::models`, so opening the picker calls real `GET /api/models`.
- Selection reuses the existing `/models <id>` path and writes the current session model through `PATCH /api/sessions/{session_id}`.
- Updated `/tui/open-models` remote control to open the same picker path, with direct model payload support and handler-backed fetch support.
- Added keyflow, remote control, terminal render snapshot, and App Bridge handler smoke coverage.

Verification:

```bash
cargo test -q -p openagent-tui key_event_flow_opens_model_picker_filters_and_selects
cargo test -q -p openagent-tui remote_control_open_models_dispatches_picker_fetch
cargo test -q -p openagent-tui terminal_render_snapshot_contains_model_picker_overlay
cargo test -q -p openagent-tui app_bridge_terminal_model_picker_fetches_and_sets_model
cargo test -q -p openagent-tui
cargo check -q -p openagent-tui -p openagent-app-server-client
```

Residual risk:

- This completes the model picker path only; agent, variant, and thinking pickers still have command/control coverage but are not yet full keyboard docks.
- The App Bridge smoke uses a deterministic in-test bridge server, not a full provider-backed runtime.

## 2026-06-22 TUI Session Picker Slice

- Upgraded `/sessions [query]` from a passive timeline listing into an OpenCode-style keyboard session picker.
- Added a TUI session picker dock with query text, remote session candidates, Up/Down/Tab selection, Enter-to-resume, and Esc-to-close.
- Wired the picker through the real `TerminalEventHandler::search_sessions` boundary; `AppBridgeTerminalHandler` now backs it with `RemoteRuntimeClient::search_sessions`, so `/sessions smoke` calls `GET /api/sessions?query=smoke`.
- Updated remote control `/tui/open-sessions` to open the same picker path instead of only appending a hint line.
- Added coverage for key event flow, remote control dispatch, terminal render snapshot, and App Bridge handler search/resume smoke.

Verification:

```bash
cargo test -q -p openagent-tui key_event_flow_opens_session_picker_filters_and_resumes
cargo test -q -p openagent-tui remote_control_open_sessions_dispatches_picker_search
cargo test -q -p openagent-tui terminal_render_snapshot_contains_session_picker_overlay
cargo test -q -p openagent-tui app_bridge_terminal_session_picker_searches_and_resumes
cargo test -q -p openagent-tui
cargo check -q -p openagent-tui -p openagent-app-server-client
```

Residual risk:

- Picker selection resumes by session id; richer OpenCode-style session detail panes and inline rename/delete/archive actions remain future slices.
- The smoke uses the deterministic in-test App Bridge server, not a full provider-backed runtime session.

## 2026-06-22 TUI Session Transcript Slice

- Added a real App Bridge transcript path for session management parity:
  - `GET /api/sessions/{session_id}/messages?limit=N` in `openagent-http-runtime`
  - `RemoteRuntimeClient::session_messages`
  - TUI `/transcript [limit]` command backed by the remote session store
- The endpoint returns structured persisted messages with role, content, metadata, index, total message count, and a bounded limit.
- The TUI renders a compact chronological transcript summary so a resumed session can be inspected without leaving the terminal.
- Added a runtime client round trip against the real `openagent-http-runtime` binary and a TUI App Bridge handler smoke proving `/transcript 2` sends the expected HTTP request and renders remote messages.

Verification:

```bash
cargo test -q -p openagent-http-runtime --test http_runtime remote_runtime_client_reads_session_transcript
cargo test -q -p openagent-tui app_bridge_terminal_transcript_reads_real_session_messages
cargo test -q -p openagent-app-server-client
cargo check -q -p openagent-http-runtime -p openagent-tui
```

Residual risk:

- Transcript is read-only and compact text rendering only; full interactive session picker/detail navigation remains a later session-management slice.
- The TUI transcript command verifies handler/client/HTTP integration, not full raw-mode terminal drawing.

## 2026-06-22 App Bridge Interaction Keyflow Smoke Slice

- Added a real-handler TUI smoke for permission/question interaction flows.
- The smoke drives approval and question dock key events through `handle_key_event`, `AppBridgeTerminalHandler`, and `RemoteRuntimeClient`, with the deterministic in-test App Bridge server receiving actual HTTP response routes:
  - `POST /api/turns/{turn_id}/approvals/{request_id}`
  - `POST /api/turns/{turn_id}/questions/{request_id}/reply`
- Verified approval quick-pick posts `allow`/`once`, clears the active approval, and applies the returned resolved/completed events into the TUI timeline.
- Verified question option selection posts structured `answers`, clears the active question, and applies the returned resolved/completed events into the TUI timeline.
- This strengthens the permission/question parity evidence beyond state-only dock tests and client-only runtime tests.

Verification:

```bash
cargo test -q -p openagent-tui app_bridge_terminal_interaction_keyflow_posts_real_responses
cargo test -q -p openagent-tui
```

Residual risk:

- The smoke still uses a deterministic in-test App Bridge server rather than the full `openagent-http-runtime` binary.
- It verifies keyflow-to-HTTP response integration, not a full raw-mode PTY loop.

## 2026-06-22 App Bridge Terminal Keyflow Smoke Slice

- Added an end-to-end TUI smoke that drives `handle_key_event` through the real `AppBridgeTerminalHandler` and `RemoteRuntimeClient` over HTTP.
- The smoke uses a deterministic in-test App Bridge server implementing the session and event routes needed by the terminal handler:
  - `GET /api/health`
  - `GET /api/sessions`
  - `POST /api/sessions`
  - `POST /api/sessions/{id}/turns`
  - `GET /api/events?last_event_id=...`
- Verified the TUI keyflow can create a remote session with `/new`, submit a prompt through the App Bridge, apply returned turn events into the timeline, merge usage totals, and poll a runtime warning from global App Bridge SSE.
- This closes the earlier evidence gap where TUI coverage was mostly state-level and did not prove the real terminal handler/client path.

Verification:

```bash
cargo test -q -p openagent-tui app_bridge_terminal_keyflow_smoke_uses_real_remote_handler
cargo test -q -p openagent-tui
```

Residual risk:

- The smoke uses a deterministic in-test App Bridge server, not the full `openagent-http-runtime` binary or a real provider.
- It proves keyflow plus handler/client integration, but not a full PTY raw-mode terminal loop with crossterm polling.

## 2026-06-22 Composer At-Trigger File Picker Slice

- Added OpenCode-style `@` composer trigger: pressing `@` in a normal prompt opens the file picker dock instead of inserting a literal character.
- Preserved slash-command behavior: `@` remains literal inside commands such as `/rename @title`, so command arguments are not hijacked by the composer picker.
- The existing picker path is reused, so after `@` users can type to filter, use Up/Down/Tab, press Enter to insert `@path`, or Esc to close without exiting the TUI.
- Added key event coverage for the `@` trigger and command-literal behavior.

Verification:

```bash
cargo test -q -p openagent-tui key_event_flow_at_opens_file_picker_without_touching_commands
cargo test -q -p openagent-tui key_event_flow_opens_file_picker_filters_and_attaches
cargo test -q -p openagent-tui
```

Residual risk:

- `@` only opens the local workspace file picker; remote URL/resource attachment flows remain future composer work.
- Attachment tokens with whitespace in paths remain unsupported by the submit-time parser.

## 2026-06-22 Composer Modal File Picker Slice

- Upgraded `/files [query]` from a timeline-only listing into a keyboard-driven composer file picker dock.
- Added `TerminalEventHandler::search_files` so the TUI state owns modal/key behavior while the App Bridge handler searches the real active workspace.
- File picker now supports incremental query filtering, Up/Down or Tab selection, Enter-to-attach, and Esc-to-close without triggering global exit or prompt history.
- App Bridge `file.open` / `/tui/open-files` now opens the same picker instead of dispatching a plain `/files` timeline command; `file.select` closes the picker and inserts the selected `@path[:range]` reference.
- Added terminal render snapshot coverage proving the file picker appears in the frame, plus key event flow coverage proving filter/select/attach works end to end inside the TUI event loop.

Verification:

```bash
cargo test -q -p openagent-tui key_event_flow_opens_file_picker_filters_and_attaches
cargo test -q -p openagent-tui terminal_render_snapshot_contains_file_picker_overlay
cargo test -q -p openagent-tui remote_control_file_picker_dispatches_and_selects_into_composer
cargo test -q -p openagent-tui
```

Residual risk:

- Superseded by the Composer At-Trigger File Picker Slice: `@` typed inside a normal draft opens the file picker.
- Attachment tokens with whitespace in paths remain unsupported by the submit-time parser.
- Remote URL/resource/image upload attachment flows remain future composer work.

## 2026-06-22 Composer File Picker Slice

- Added OpenCode-style composer file discovery commands: `/files [query]` searches the active workspace and renders ranked `@path` attachment candidates; `/attach <path[:range]>` inserts a normalized file/image reference back into the prompt composer.
- Added App Bridge TUI controls for file attachment workflows:
  - `/tui/open-files` and `file.open` queue a real `/files <query>` command through the terminal handler.
  - `/tui/select-file`, `file.select`, and publish topics `tui.file.select` / `tui.file.attach` insert `@path`, `@path:line`, or `@path:start-end` into the composer.
- Reused the same fuzzy file matcher for both picker listing and submit-time `@file` expansion so selected refs and direct typed refs resolve consistently.
- Updated App Bridge TUI golden action mapping and added coverage for local `/attach`, fuzzy file listing, image/file refs, and remote control dispatch.

Verification:

```bash
cargo test -q -p openagent-tui composer_file_picker_and_attach_controls_insert_references
cargo test -q -p openagent-tui remote_control_file_picker_dispatches_and_selects_into_composer
cargo test -q -p openagent-tui
```

Residual risk:

- Superseded by the Composer Modal File Picker Slice: `/files` now opens a keyboard-driven TUI dock.
- Attachment tokens with whitespace in paths are rejected because the current submit-time parser is whitespace-token based.
- `/files` is workspace-local; remote resource/URL attachments still remain future composer work.

## 2026-06-22 Agent Variant Thinking Control Slice

- Added App Bridge TUI control actions for `agent.open`, `agent.select`, `variant.open`, `variant.select`, `thinking.open`, and `thinking.select`.
- Wired `/tui/open-agents`, `/tui/select-agent`, `/tui/open-variants`, `/tui/select-variant`, `/tui/open-thinking`, and `/tui/select-thinking` into the same command-dispatch path as model selection.
- Added `tui.agent.*`, `tui.variant.*`, and `tui.thinking.*` publish topic routing so external App Bridge publishers can drive these controls.
- Added tests for picker surfaces and real handler command dispatch, and updated the App Bridge TUI golden action map.

Verification:

```bash
cargo test -q -p openagent-tui remote_control_agent_variant_and_thinking_dispatch_handler_commands
cargo test -q -p openagent-tui control_requests_open_model_theme_and_palette_surfaces
cargo test -q -p openagent-tui
cargo test -q -p openagent-app-server -p openagent-app-server-client -p openagent-tui
cargo test -q -p openagent-http-runtime
cargo check -q -p openagent-cli
```

Residual risk:

- The controls now work through App Bridge, but the visible picker is still timeline/list based rather than a full fuzzy modal.
- Variant/thinking validation is command-level only; the TUI does not yet constrain arbitrary values against runtime-provided capabilities.
- Agent/profile switching is per-session metadata today; richer profile inheritance and per-turn overrides still need product polish.

## 2026-06-22 Interaction Live SSE Resume Slice

- Added complete App Bridge metadata to interaction-resolved events:
  - `turn/approval_resolved` now includes `thread_id` and top-level `request_id`.
  - `item/question/resolved` now includes `session_id`, `thread_id`, resolved `turn_id`, and `status` (`answered`/`dismissed`).
- Added an end-to-end live SSE smoke that runs both question and approval resume flows. The fake provider delays the final model response, and `/api/events` must receive the resolved interaction event before `turn/completed`.
- Confirmed approval/question resume still continues the provider loop and records tool outputs into the next provider request.

Verification:

```bash
cargo test -q -p openagent-http-runtime live_sse_tails_interaction_resolved_events_before_provider_final
cargo test -q -p openagent-http-runtime
cargo test -q -p openagent-app-server-client -p openagent-http-runtime -p openagent-tui
cargo check -q -p openagent-cli
cargo check -q -p openagent-http-runtime
```

Residual risk:

- The interaction dock has keyboard and control-response coverage, but there is still no full terminal live-session smoke that drives the dock through real key input against a running HTTP runtime.
- Approval/question responses are now observable before final answer, but long-running tool stdout/stderr still does not stream incrementally.
- Remaining parity work still includes richer composer UX, fuzzy pickers, message-level undo/revert, and broader terminal render snapshots.

## 2026-06-22 TUI Rendered Diff Slice

- Upgraded TUI patch rendering from generic `status` lines to structured timeline kinds: `patch`, `diff-meta`, `diff-hunk`, `diff-add`, and `diff-del`.
- Added theme-aware colors for rendered diff lines so additions, deletions, hunk headers, and patch markers are visually distinct in the terminal.
- Added undo/redo action hints in `/details` and patch result lines, making reversible file changes discoverable from the TUI.
- Added coverage for `patch/detected` rendering and `/details` undo/redo stack markers.

Verification:

```bash
cargo test -q -p openagent-tui patch_events_render_structured_diff_and_undo_redo_markers
cargo test -q -p openagent-tui
cargo test -q -p openagent-app-server -p openagent-app-server-client -p openagent-tui
cargo test -q -p openagent-http-runtime remote_runtime_client_tracks_file_diff_undo_and_redo
cargo check -q -p openagent-cli
```

Residual risk:

- Diff UX is now visibly structured, but still line-oriented; it is not yet a full split-pane or file-tree diff viewer.
- Undo/redo remains tied to file-change snapshots, not a full OpenCode-style message-level revert/unrevert with prompt restoration.
- The `/details` command exposes the latest patch and stack counts; a richer interactive patch picker remains future work.

## 2026-06-22 App Bridge Session Control Slice

- Fixed `/tui/select-session` so the control response dispatches `/resume <session_id>` to the real terminal handler, keeping App Bridge state and the active remote session in sync.
- Added App Bridge session control aliases for rename/archive/unarchive/delete/fork/children/parent/share/unshare/compact/details/undo/redo.
- Added `tui.session.*` publish-topic routing so external UI publishers can invoke session management actions without falling back to raw command strings.
- Added a remote-control test proving these session controls are dispatched as real handler commands, and updated the App Bridge TUI golden action map.

Verification:

```bash
cargo test -q -p openagent-tui remote_control_session_actions_dispatch_handler_commands
cargo test -q -p openagent-tui remote_control_select_model_dispatches_handler_command
cargo test -q -p openagent-tui
cargo test -q -p openagent-app-server -p openagent-app-server-client -p openagent-tui
cargo test -q -p openagent-http-runtime
cargo check -q -p openagent-cli
```

Residual risk:

- Session controls now reach the real handler command path, but the visible picker UI is still text/list based rather than a full OpenCode-style fuzzy session dialog.
- Delete/archive/share controls still rely on slash-command semantics and do not yet have confirmation modals.
- Child/subagent navigation exists via `/children` and `/parent`, but nested navigation UI polish remains.

## 2026-06-22 Provider Tool Event Live SSE Slice

- Provider-loop tool events now flush to the App Bridge event log as soon as each tool starts and completes, instead of waiting for the final `turn/completed` append.
- Live `/api/events` clients can now see `item/toolCall/started`, `item/toolCall/completed`, rendered output metadata, and diff/patch events during a real provider turn while the next provider call is still in flight.
- Added an end-to-end smoke where the fake Responses provider streams a function call, OpenAgent executes `read`, the second provider call deliberately delays the final answer, and live SSE proves tool events are visible before `turn/completed`.

Verification:

```bash
cargo test -q -p openagent-http-runtime global_sse_live_tails_provider_tool_events_before_final_answer
cargo test -q -p openagent-http-runtime
cargo test -q -p openagent-app-server-client -p openagent-http-runtime -p openagent-tui
cargo check -q -p openagent-cli
cargo check -q -p openagent-http-runtime
```

Residual risk:

- Approval/question request events already flush on pause, but their response/resume phases still deserve a dedicated live-SSE smoke.
- Tool progress is event-level live now; long-running tool stdout/stderr incremental streaming is not yet implemented.
- Remaining OpenCode parity work still includes richer composer, session navigation, rendered diff UX, model/theme picker polish, config/keybinds, and broader terminal E2E snapshots.

## 2026-06-22 Runtime Provider Streaming App Bridge Slice

- Added OpenAI Responses SSE normalization to `openagent-provider` so runtime code can materialize text deltas, tool calls, finish reason, and usage from provider stream chunks.
- Changed the HTTP runtime provider path to request native provider streaming by default (`stream: true`, `Accept: text/event-stream`) while preserving JSON fallback for providers/tests that return non-SSE responses.
- Moved provider calls into the runtime provider loop so first turns plus approval/question resumes share the same streaming path.
- App Bridge live SSE now receives provider text deltas while the upstream provider response is still in flight; already-persisted live events are not appended again at turn completion.
- Added an end-to-end smoke where a fake Responses provider sends one delta, delays completion, and `/api/events` receives the delta before `turn/completed`.

Verification:

```bash
cargo check -q -p openagent-provider -p openagent-http-runtime
cargo test -q -p openagent-http-runtime global_sse_live_tails_provider_stream_delta_before_completion
cargo test -q -p openagent-http-runtime
cargo test -q -p openagent-provider
cargo test -q -p openagent-app-server-client -p openagent-http-runtime -p openagent-tui
cargo check -q -p openagent-cli
```

Residual risk:

- Provider streaming is now real for OpenAI-compatible chat/responses SSE, but Anthropic/native non-OpenAI runtime streaming is not wired in this runtime path yet.
- Tool started/completed events inside the provider loop are still mostly flushed on pause/final completion; this slice focused on model token deltas into live App Bridge SSE.
- The full TUI parity goal still has remaining product surfaces: richer pickers, composer extmarks/attachments, rendered diff UX, and broader terminal render/keyflow E2E.

## 2026-06-22 TUI/App Bridge Parity Push

- Replaced the HTTP runtime plain-turn mock path with a real OpenAI-compatible provider call path.
- Added a bounded runtime provider tool loop: provider tool calls are appended as assistant tool-call messages, executed through the built-in toolkit, recorded as `Role::Tool`, and sent back to the provider for the final answer.
- Wired approval/question pause and resume into the provider loop. `/allow` and `/answer` now resume the pending provider turn instead of only updating local state.
- Added live SSE tail support for EventSource-style clients by handling connections concurrently and streaming new app events until terminal turn events or timeout.
- Added TUI Interaction Dock v1 for approval/question: pending requests render in a focused dock and support keyboard selection, numeric quick-pick, Enter, Esc, and custom question answers.
- Completed App Bridge TUI control paths for model/theme/palette open/select/execute so they no longer hard-return unsupported for those namespaces.

Verification:

```bash
cargo test -q -p openagent-http-runtime
cargo test -q -p openagent-app-server-client -p openagent-http-runtime -p openagent-tui
cargo check -q -p openagent-cli
rg -n "OPENAGENT_MOCK|hello from server|echo:|TUI control unsupported|unsupported.*model|unsupported.*theme|unsupported.*palette" \
  runtime/http/src/http_runtime.rs \
  runtime/http/tests/http_runtime.rs \
  runtime/tui/src/terminal_ui.rs \
  tests/golden/rust_rewrite/app_bridge_tui.json \
  tests/golden/rust_rewrite/http_runtime.json
```

Residual risk:

- Provider HTTP calls in `openagent-http-runtime` are still non-streaming `.send().text()` calls; live SSE now tails runtime events, but token deltas are not emitted while the upstream model response is still in flight.
- Explicit payload-driven `tool_call(s)` turns remain a bridge/test execution path and do not ask the provider for a final response.
- TUI model/theme/palette now have working control paths, but full OpenCode-style fuzzy picker UI is still basic compared with `DialogModel`/`DialogThemeList`.

## 2026-06-12 Swarm Kernel Proposal (decoupled, agent-agnostic)

- Rewrote `doc/multi-agent.md` around a **standalone, agent-agnostic swarm
  kernel**. The kernel (working package `swarm/`) has **zero openagent
  dependency**; openagent is the *reference adapter* (`OpenAgentRunner`), one
  citizen of the swarm. Any CLI / HTTP / A2A agent can join via the same
  `AgentRunner` protocol. Analogy: MCP standardized tool access; this kernel
  standardizes agent-to-agent orchestration.
- Dependency direction is strict: `openagent → swarm`, never the reverse. A CI
  guard asserts no openagent import under `src/swarm/`.
- Protocol named `AgentRunner` (the existing `AgentAdapter` name is taken by the
  model+config reply-stream adapter). Kernel injected into openagent via
  `ToolContext.extra["swarm"]`.
- Restructured `tasks.json` into kernel tasks `SW-001..SW-008` and openagent
  adapter tasks `OA-018..OA-021`. Phased order:
  `OA-002 → SW-001/002 → OA-018/019 (P0) → SW-003 + OA-020 + SW-007 (P1) →
  SW-004/005 (P2) → SW-006 (P3) → OA-003 → SW-008 + OA-021 (P4)`.
- Decisions locked in `doc/multi-agent.md` §9: workers same model as lead (v0),
  compact-JSON result, failures never raise, Supervisor topology first.
- Open question (§9): out-of-process transport ordering (Subprocess first vs
  A2A first); current lean is subprocess first. Packaging recommendation (§8):
  in-repo `src/swarm/` with enforced zero-import boundary, extract to its own
  repo once the protocol stabilizes.

Verification for this proposal:

```bash
python -m json.tool tasks.json >/dev/null
rg -n "import openagent|from openagent" src/swarm || echo "kernel boundary clean (package not created yet)"
```

---

## 2026-06-11 Maintenance State Initialized

- Added local maintenance entry points:
  - `doc/maintenance.md`
  - `tasks.json`
  - `progress.md`
  - `init.sh`
- Captured known trace/eval/cost/runtime-warning maintenance items from recent OpenAgent work.
- Current active local change is `OA-001`: prevent runtime-only options such as `trace` and `runtime_warnings` from leaking to provider-facing model options.
- Current active local documentation change is `OA-017`: capture static step-budget behavior and closeout risk.
- LangSmith / OpenTelemetry integration has been removed and pushed at `4886a8f`; Langfuse export remains.
- Recommended next task: finish, verify, and commit `OA-001`.

Verification for this maintenance setup:

```bash
python -m json.tool tasks.json >/dev/null
bash init.sh
```
