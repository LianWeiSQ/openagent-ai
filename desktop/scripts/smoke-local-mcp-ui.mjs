#!/usr/bin/env node

import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { chromium } from "playwright";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const desktopDir = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(desktopDir, "..");
const chromePath = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const token = "desktop-local-mcp-ui-smoke-token";

function freePort() {
  return new Promise((resolve, reject) => {
    const server = http.createServer();
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      const port = typeof address === "object" && address ? address.port : 0;
      server.close(() => resolve(port));
    });
  });
}

async function waitForHttp(url, { headers = {}, timeoutMs = 60_000 } = {}) {
  const deadline = Date.now() + timeoutMs;
  let lastError = "";
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url, { headers });
      if (response.ok) return response;
      lastError = `HTTP ${response.status}`;
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Timed out waiting for ${url}: ${lastError}`);
}

function spawnLogged(command, args, options = {}) {
  const child = spawn(command, args, {
    stdio: ["ignore", "pipe", "pipe"],
    ...options,
  });
  const stdout = [];
  const stderr = [];
  child.stdout?.on("data", (chunk) => stdout.push(chunk.toString()));
  child.stderr?.on("data", (chunk) => stderr.push(chunk.toString()));
  child.outputText = () => `${stdout.join("")}${stderr.join("")}`.trim();
  return child;
}

async function stopChild(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve) => {
    const killer = setTimeout(() => {
      try {
        child.kill("SIGKILL");
      } catch {
        // Process may have exited after SIGTERM.
      }
      resolve();
    }, 2_000);
    child.once("exit", () => {
      clearTimeout(killer);
      resolve();
    });
    try {
      child.kill("SIGTERM");
    } catch {
      clearTimeout(killer);
      resolve();
    }
  });
}

async function bridgeJson(port, method, urlPath, body) {
  const response = await fetch(`http://127.0.0.1:${port}${urlPath}`, {
    method,
    headers: {
      authorization: `Bearer ${token}`,
      "content-type": "application/json",
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(`${method} ${urlPath} -> HTTP ${response.status}: ${JSON.stringify(payload)}`);
  }
  return payload;
}

function writeLocalMcpServerScript(scriptPath) {
  fs.writeFileSync(
    scriptPath,
    `#!/usr/bin/env node
import fs from "node:fs";

let buffer = Buffer.alloc(0);
const requestLog = process.env.LOCAL_REQUEST_LOG || "";

function logMethod(method) {
  if (!requestLog) return;
  fs.appendFileSync(requestLog, \`\${process.pid} \${method}\\n\`);
}

function send(id, result) {
  if (id === undefined || id === null) return;
  const payload = JSON.stringify({ jsonrpc: "2.0", id, result });
  process.stdout.write(\`Content-Length: \${Buffer.byteLength(payload)}\\r\\n\\r\\n\${payload}\`);
}

function handle(message) {
  logMethod(message.method || "unknown");
  if (message.method === "initialize") {
    send(message.id, {
      protocolVersion: "2024-11-05",
      capabilities: { tools: {} },
      serverInfo: { name: "desktop-local-mcp-ui", version: "1.0.0" },
    });
    return;
  }
  if (message.method === "tools/list") {
    send(message.id, {
      tools: [
        {
          name: "stdio_echo",
          title: "Stdio Echo",
          description: "Echo input through Desktop local MCP UI smoke",
          inputSchema: {
            type: "object",
            properties: { text: { type: "string" } },
            required: ["text"],
          },
        },
        {
          name: "stdio_fail",
          title: "Stdio Failure",
          description: "Return a visible MCP error through Desktop local MCP UI smoke",
          inputSchema: {
            type: "object",
            properties: { text: { type: "string" } },
            required: ["text"],
          },
        },
      ],
    });
    return;
  }
  if (message.method === "tools/call") {
    const name = message.params?.name || "";
    const text = message.params?.arguments?.text || "";
    if (name === "stdio_fail") {
      send(message.id, {
        isError: true,
        content: [{ type: "text", text: \`desktop stdio failure: \${text}\` }],
      });
      return;
    }
    send(message.id, {
      content: [{ type: "text", text: \`desktop stdio echo: \${text}\` }],
    });
    return;
  }
  if (message.method === "shutdown") {
    send(message.id, {});
    return;
  }
  if (message.method === "exit") {
    process.exit(0);
  }
}

function pump() {
  while (buffer.length > 0) {
    const headerEnd = buffer.indexOf("\\r\\n\\r\\n");
    if (headerEnd < 0) return;
    const header = buffer.slice(0, headerEnd).toString("utf8");
    const match = /content-length:\\s*(\\d+)/i.exec(header);
    if (!match) process.exit(2);
    const length = Number.parseInt(match[1], 10);
    const total = headerEnd + 4 + length;
    if (buffer.length < total) return;
    const body = buffer.slice(headerEnd + 4, total).toString("utf8");
    buffer = buffer.slice(total);
    handle(JSON.parse(body));
  }
}

process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  pump();
});
`,
  );
  fs.chmodSync(scriptPath, 0o755);
}

function prepareLocalMcpConfig(workspace, tempRoot) {
  const toolsDir = path.join(workspace, "mcp-tools");
  const openagentDir = path.join(workspace, ".openagent");
  fs.mkdirSync(toolsDir, { recursive: true });
  fs.mkdirSync(openagentDir, { recursive: true });
  const serverScript = path.join(toolsDir, "desktop-local-mcp-ui.mjs");
  const requestLog = path.join(tempRoot, "local-mcp-requests.log");
  writeLocalMcpServerScript(serverScript);
  const mcpConfigPath = path.join(openagentDir, "mcp.json");
  fs.writeFileSync(
    mcpConfigPath,
    JSON.stringify(
      {
        mcp: {
          servers: {},
        },
      },
      null,
      2,
    ),
  );
  return { mcpConfigPath, requestLog, serverScript };
}

function readLocalMcpRequestLog(requestLog) {
  const text = fs.existsSync(requestLog) ? fs.readFileSync(requestLog, "utf8") : "";
  return text
    .split(/\r?\n/)
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const [pid, method] = line.split(/\s+/, 2);
      return { pid, method };
    });
}

async function waitForPageState(page, predicate, timeoutMs = 15_000, arg = undefined) {
  await page.waitForFunction(predicate, arg, { timeout: timeoutMs });
}

function messageDebugSummary(messagesPayload) {
  const messages = Array.isArray(messagesPayload.messages_v2) ? messagesPayload.messages_v2 : [];
  return messages.map((message) => ({
    role: message.info?.role,
    parts: (Array.isArray(message.parts) ? message.parts : []).map((part) => ({
      kind: part.kind,
      status: part.status,
      content: typeof part.content === "string" ? part.content.slice(0, 120) : part.content,
    })),
  }));
}

async function pageDebugSummary(page) {
  return page.evaluate(() => ({
    selectedSession: document.querySelector(".session-row.selected")?.textContent || "",
    mcpCard: document.querySelector('[data-testid="mcp-card"]')?.textContent || "",
    toolCard: document.querySelector('[data-testid="mcp-tool-card"]')?.textContent || "",
    latestCall: document.querySelector('[data-testid="mcp-latest-call"]')?.textContent || "",
    timelineText: document.querySelector(".timeline")?.textContent || "",
  }));
}

async function ensureInspectorOpen(page) {
  const open = await page.evaluate(() => document.querySelector(".inspector")?.classList.contains("open") ?? false);
  if (!open) {
    await page.getByTitle("Toggle details").click();
  }
  await page.locator(".inspector.open").waitFor({ state: "visible", timeout: 15_000 });
}

async function mcpPanelSnapshot(page) {
  return page.evaluate(() => {
    const textOf = (element) => element?.textContent?.trim() || "";
    const card = document.querySelector('[data-testid="mcp-card"]');
    const summary = {};
    const summaryDl = card?.querySelector(":scope > dl");
    for (const dt of summaryDl?.querySelectorAll("dt") || []) {
      const key = textOf(dt);
      const value = textOf(dt.nextElementSibling);
      if (key) summary[key] = value;
    }
    const servers = [...(card?.querySelectorAll(".mcp-server-row") || [])].map((row) => {
      const meta = {};
      for (const item of row.querySelectorAll(".mcp-server-meta div")) {
        const key = textOf(item.querySelector("dt"));
        const value = textOf(item.querySelector("dd"));
        if (key) meta[key] = value;
      }
      return {
        text: textOf(row),
        type: textOf(row.querySelector(".file-badge")),
        name: textOf(row.querySelector(".mcp-server-copy strong")),
        copy: textOf(row.querySelector(".mcp-server-copy span")),
        status: textOf(row.querySelector(".mcp-server-actions .stream-state")),
        lifecycle: textOf(row.querySelector(".mcp-lifecycle-strip")),
        meta,
      };
    });
    return {
      summary,
      servers,
    };
  });
}

function assertLocalMcpServerVisible(snapshot, { enabled, lifecyclePid, lifecycleStatus = "running", tools = "2" } = {}) {
  assert.equal(snapshot.summary.Servers, "1", `MCP server count not visible: ${JSON.stringify(snapshot)}`);
  assert.equal(snapshot.summary.Tools, tools, `MCP total tool_count not visible: ${JSON.stringify(snapshot)}`);
  const server = snapshot.servers.find((item) => item.name === "local-tools");
  assert.ok(server, `local-tools row missing: ${JSON.stringify(snapshot)}`);
  assert.equal(server.type, "local", `server transport/type badge missing: ${JSON.stringify(server)}`);
  assert.match(server.copy, /stdio/, `stdio transport label missing: ${JSON.stringify(server)}`);
  if (enabled !== undefined) {
    assert.match(server.copy, enabled ? /enabled/ : /disabled/, `enabled state missing: ${JSON.stringify(server)}`);
  }
  assert.notEqual(server.status, "-", `server status missing: ${JSON.stringify(server)}`);
  assert.equal(server.meta.Tools, tools, `server tool_count meta missing: ${JSON.stringify(server)}`);
  assert.match(server.lifecycle, new RegExp(lifecycleStatus), `lifecycle status missing: ${JSON.stringify(server)}`);
  const pidPattern = lifecyclePid ?? "\\d+";
  assert.match(server.lifecycle, new RegExp(`pid\\s+${pidPattern}`), `lifecycle PID missing: ${JSON.stringify(server)}`);
  assert.match(server.lifecycle, new RegExp(`runtime tools\\s+${tools}`), `lifecycle tool_count missing: ${JSON.stringify(server)}`);
  return server;
}

async function main() {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "openagent-desktop-local-mcp-ui-"));
  const workspace = path.join(tempRoot, "workspace");
  const sessionRoot = path.join(tempRoot, "sessions");
  fs.mkdirSync(workspace, { recursive: true });
  fs.mkdirSync(sessionRoot, { recursive: true });
  const localMcp = prepareLocalMcpConfig(workspace, tempRoot);

  let runtime;
  let vite;
  let browser;
  let page;
  let screenshotPath = "";

  try {
    const runtimePort = await freePort();
    const vitePort = await freePort();

    runtime = spawnLogged(
      "cargo",
      [
        "run",
        "-q",
        "-p",
        "openagent-http-runtime",
        "--",
        "--host",
        "127.0.0.1",
        "--port",
        String(runtimePort),
        "--workspace",
        workspace,
        "--session-root",
        sessionRoot,
        "--auth-token",
        token,
        "--cors-origin",
        "*",
      ],
      { cwd: repoRoot, env: process.env },
    );

    await waitForHttp(`http://127.0.0.1:${runtimePort}/api/health`, {
      headers: { authorization: `Bearer ${token}` },
      timeoutMs: 90_000,
    });

    const created = await bridgeJson(runtimePort, "POST", "/api/sessions", {
      cwd: workspace,
      title: "Desktop local MCP UI smoke",
    });
    const sessionId = created.session_id || created.id || created.session?.id;
    assert.ok(sessionId, "session id missing");

    vite = spawnLogged("npm", ["run", "dev", "--", "--port", String(vitePort), "--strictPort"], {
      cwd: desktopDir,
      env: process.env,
    });
    await waitForHttp(`http://127.0.0.1:${vitePort}/`, { timeoutMs: 60_000 });

    const consoleIssues = [];
    const pageErrors = [];
    const launchOptions = {
      headless: true,
      args: ["--disable-dev-shm-usage"],
    };
    if (fs.existsSync(chromePath)) launchOptions.executablePath = chromePath;
    browser = await chromium.launch(launchOptions);
    page = await browser.newPage({ viewport: { width: 1440, height: 920 } });
    page.on("console", (message) => {
      if (["error", "warning"].includes(message.type())) consoleIssues.push(message.text());
    });
    page.on("pageerror", (error) => pageErrors.push(error.message));
    await page.addInitScript(
      ({ bridgeUrl, tokenValue, workspacePath }) => {
        const project = {
          id: workspacePath,
          name: "local-mcp-ui-smoke",
          path: workspacePath,
          last_opened_at_ms: Date.now(),
        };
        window.localStorage.setItem("openagent.desktop.bridgeUrl", bridgeUrl);
        window.localStorage.setItem("openagent.desktop.token", tokenValue);
        window.localStorage.setItem("openagent.desktop.projects", JSON.stringify([project]));
        window.localStorage.setItem("openagent.desktop.activeProject", workspacePath);
      },
      {
        bridgeUrl: `http://127.0.0.1:${runtimePort}`,
        tokenValue: token,
        workspacePath: workspace,
      },
    );

    await page.goto(`http://127.0.0.1:${vitePort}/`, { waitUntil: "domcontentloaded" });
    await page.locator(".composer").waitFor({ state: "visible", timeout: 15_000 });
    await ensureInspectorOpen(page);
    await page.locator('[data-testid="mcp-card"]').waitFor({ state: "visible", timeout: 15_000 });
    await page
      .locator(".session-row.selected")
      .filter({ hasText: "Desktop local MCP UI smoke" })
      .waitFor({ state: "visible", timeout: 15_000 });

    const mcpForm = page.locator('[data-testid="mcp-server-form"]');
    await mcpForm.getByLabel("Mode").selectOption("local");
    await mcpForm.getByLabel("Name").fill("local-tools");
    await mcpForm.getByLabel("Command").fill(process.execPath);
    await mcpForm.getByLabel("Args").fill(localMcp.serverScript);
    await mcpForm.getByLabel("Cwd").fill("mcp-tools");
    await mcpForm.getByLabel("Timeout ms").fill("5000");
    await mcpForm.getByLabel("Env").fill(`LOCAL_REQUEST_LOG=${localMcp.requestLog}\nLOCAL_SECRET=desktop-local-mcp-ui-secret`);
    await mcpForm.getByRole("button", { name: "Add", exact: true }).click();
    await page.getByText("local-tools").waitFor({ state: "visible", timeout: 15_000 });

    const initialLog = readLocalMcpRequestLog(localMcp.requestLog);
    assert.equal(initialLog.length, 0, `add/edit UI should not start local MCP: ${JSON.stringify(initialLog)}`);

    await page.getByRole("button", { name: "Edit MCP server local-tools", exact: true }).click();
    await page.locator('[data-testid="mcp-edit-banner"]').filter({ hasText: "local-tools" }).waitFor({ state: "visible" });
    await mcpForm.getByLabel("Timeout ms").fill("6000");
    await mcpForm.getByRole("button", { name: "Save", exact: true }).click();
    await waitForPageState(page, () => {
      const card = document.querySelector('[data-testid="mcp-card"]');
      const text = card?.textContent || "";
      return text.includes("local-tools") && text.includes("6000ms") && !text.includes("Editing local-tools");
    });

    await page.getByRole("button", { name: "Disable MCP server local-tools", exact: true }).click();
    await waitForPageState(page, () => {
      const card = document.querySelector('[data-testid="mcp-card"]');
      const text = card?.textContent || "";
      return text.includes("local-tools") && text.includes("disabled");
    });

    await page.getByRole("button", { name: "Start MCP server local-tools", exact: true }).click();
    await waitForPageState(page, () => {
      const card = document.querySelector('[data-testid="mcp-card"]');
      const text = card?.textContent || "";
      return text.includes("Stdio Echo") && /pid\s+\d+/.test(text) && text.includes("running");
    });

    await page.getByRole("button", { name: "Enable MCP server local-tools", exact: true }).click();
    await waitForPageState(page, () => {
      const card = document.querySelector('[data-testid="mcp-card"]');
      const text = card?.textContent || "";
      return text.includes("enabled") && text.includes("Stdio Echo") && /pid\s+\d+/.test(text);
    });

    await page.getByRole("button", { name: "Test MCP server local-tools", exact: true }).click();
    await waitForPageState(page, () => {
      const card = document.querySelector('[data-testid="mcp-card"]');
      const text = card?.textContent || "";
      return text.includes("Stdio Echo") && text.includes("Stdio Failure") && /runtime tools\s+2/.test(text);
    });

    const pidBeforeRestart = await page.evaluate(() => {
      const row = [...document.querySelectorAll(".mcp-server-row")].find((item) =>
        item.textContent?.includes("local-tools"),
      );
      return /pid\s+(\d+)/.exec(row?.textContent || "")?.[1] || "";
    });
    assert.ok(pidBeforeRestart, "UI did not expose lifecycle pid before restart");

    await page.getByRole("button", { name: "Restart MCP server local-tools", exact: true }).click();
    await waitForPageState(
      page,
      (previousPid) => {
        const card = document.querySelector('[data-testid="mcp-card"]');
        const text = card?.textContent || "";
        const pid = /pid\s+(\d+)/.exec(text)?.[1] || "";
        return Boolean(pid) && pid !== previousPid && text.includes("running") && /runtime tools\s+2/.test(text);
      },
      15_000,
      pidBeforeRestart,
    );

    const uiLifecycle = await page.evaluate(() => {
      const row = [...document.querySelectorAll(".mcp-server-row")].find((item) =>
        item.textContent?.includes("local-tools"),
      );
      const text = row?.textContent || "";
      const pid = /pid\s+(\d+)/.exec(text)?.[1] || "";
      return { text, pid };
    });
    assert.ok(uiLifecycle.pid, `UI did not expose lifecycle pid: ${uiLifecycle.text}`);
    assert.notEqual(uiLifecycle.pid, pidBeforeRestart, "Restart did not replace the local MCP lifecycle pid");
    const runningMcpSnapshot = await mcpPanelSnapshot(page);
    assertLocalMcpServerVisible(runningMcpSnapshot, {
      enabled: true,
      lifecyclePid: uiLifecycle.pid,
    });

    const turn = await bridgeJson(runtimePort, "POST", `/api/sessions/${sessionId}/turns`, {
      input: "Call the local MCP echo tool from the Desktop UI smoke.",
      permission: "FULL",
      dangerously_skip_permissions: true,
      tool_call: {
        call_id: "call_desktop_local_mcp_ui",
        name: "mcp_tool_local_tools_stdio_echo",
        input: { text: "ui-lifecycle" },
      },
    });
    assert.equal(turn.status, "completed", "local MCP UI smoke turn did not complete");

    try {
      await waitForPageState(page, () => {
        const toolCard = document.querySelector('[data-testid="mcp-tool-card"]');
        const latest = document.querySelector('[data-testid="mcp-latest-call"]');
        const text = `${toolCard?.textContent || ""}\n${latest?.textContent || ""}`;
        return (
          text.includes("MCP: stdio_echo") &&
          text.includes("desktop stdio echo: ui-lifecycle") &&
          text.includes("lifecycle reused") &&
          text.includes("reused") &&
          /pid\s+\d+/.test(text)
        );
      });
    } catch (error) {
      const messages = await bridgeJson(runtimePort, "GET", `/api/sessions/${sessionId}/messages?limit=100`);
      const pageDebug = await pageDebugSummary(page);
      throw new Error(
        `${error instanceof Error ? error.message : String(error)}; page=${JSON.stringify(pageDebug)}; messages=${JSON.stringify(
          messageDebugSummary(messages),
        )}`,
      );
    }

    const errorTurn = await bridgeJson(runtimePort, "POST", `/api/sessions/${sessionId}/turns`, {
      input: "Call the failing local MCP tool from the Desktop UI smoke.",
      permission: "FULL",
      dangerously_skip_permissions: true,
      tool_call: {
        call_id: "call_desktop_local_mcp_error",
        name: "mcp_tool_local_tools_stdio_fail",
        input: { text: "ui-error" },
      },
    });
    assert.equal(errorTurn.status, "completed", "local MCP error smoke turn did not finish");

    await waitForPageState(page, () => {
      const cards = [...document.querySelectorAll('[data-testid="mcp-tool-card"]')];
      const latest = document.querySelector('[data-testid="mcp-latest-call"]');
      const text = `${cards.map((card) => card.textContent || "").join("\n")}\n${latest?.textContent || ""}`;
      return (
        text.includes("MCP: stdio_fail") &&
        text.includes("desktop stdio failure: ui-error") &&
        text.includes("lifecycle reused") &&
        text.includes("reused") &&
        /pid\s+\d+/.test(text)
      );
    });

    await page.reload({ waitUntil: "domcontentloaded" });
    await page.locator(".composer").waitFor({ state: "visible", timeout: 15_000 });
    await ensureInspectorOpen(page);
    await waitForPageState(page, () => {
      const cards = [...document.querySelectorAll('[data-testid="mcp-tool-card"]')];
      const latest = document.querySelector('[data-testid="mcp-latest-call"]');
      const text = `${cards.map((card) => card.textContent || "").join("\n")}\n${latest?.textContent || ""}`;
      return (
        text.includes("MCP: stdio_echo") &&
        text.includes("desktop stdio echo: ui-lifecycle") &&
        text.includes("MCP: stdio_fail") &&
        text.includes("desktop stdio failure: ui-error") &&
        text.includes("reused")
      );
    });
    const restoredMcpSnapshot = await mcpPanelSnapshot(page);
    assertLocalMcpServerVisible(restoredMcpSnapshot, {
      enabled: true,
      lifecyclePid: uiLifecycle.pid,
    });

    const pageState = await page.evaluate(() => {
      const card = document.querySelector('[data-testid="mcp-card"]');
      const toolCards = [...document.querySelectorAll('[data-testid="mcp-tool-card"]')];
      const latest = document.querySelector('[data-testid="mcp-latest-call"]');
      return {
        overlayVisible: Boolean(document.querySelector("vite-error-overlay")),
        mcpCardText: card?.textContent || "",
        toolCardText: toolCards.map((item) => item.textContent || "").join("\n"),
        latestCallText: latest?.textContent || "",
        bodyOverflow: Math.max(0, document.body.scrollWidth - window.innerWidth),
      };
    });

    assert.equal(pageState.overlayVisible, false, "Vite error overlay is visible");
    assert.equal(pageState.bodyOverflow, 0, `page has horizontal overflow: ${pageState.bodyOverflow}`);
    assert.equal(pageErrors.length, 0, `page errors: ${pageErrors.join("\n")}`);
    assert.equal(consoleIssues.length, 0, `console issues: ${consoleIssues.join("\n")}`);

    await page.getByRole("button", { name: "Stop MCP server local-tools", exact: true }).click();
    await waitForPageState(page, () => {
      const card = document.querySelector('[data-testid="mcp-card"]');
      const text = card?.textContent || "";
      return text.includes("local-tools") && text.includes("stopped") && text.includes("pid -");
    });
    await page.getByRole("button", { name: "Delete MCP server local-tools", exact: true }).click();
    await waitForPageState(page, () => {
      const card = document.querySelector('[data-testid="mcp-card"]');
      const text = card?.textContent || "";
      return text.includes("No MCP servers configured");
    });
    const requestLog = readLocalMcpRequestLog(localMcp.requestLog);
    const pids = [...new Set(requestLog.map((item) => item.pid))];
    const methods = requestLog.map((item) => item.method);
    const callPids = [...new Set(requestLog.filter((item) => item.method === "tools/call").map((item) => item.pid))];
    assert.deepEqual(pids, [pidBeforeRestart, uiLifecycle.pid], `expected start + restart stdio pids, got ${JSON.stringify(requestLog)}`);
    assert.deepEqual(callPids, [uiLifecycle.pid], `expected both tool calls on restarted lifecycle pid, got ${JSON.stringify(requestLog)}`);
    assert.equal(methods.filter((method) => method === "initialize").length, 2, `initialize count mismatch: ${JSON.stringify(requestLog)}`);
    assert.ok(methods.filter((method) => method === "tools/list").length >= 2, `tools/list count mismatch: ${JSON.stringify(requestLog)}`);
    assert.equal(methods.filter((method) => method === "tools/call").length, 2, `tools/call count mismatch: ${JSON.stringify(requestLog)}`);

    const serialized = JSON.stringify({ pageState, turn, errorTurn });
    assert.equal(serialized.includes("desktop-local-mcp-ui-secret"), false, "MCP secret leaked into UI/API payload");
    assert.equal(serialized.includes("LOCAL_SECRET"), false, "MCP env key leaked into UI/API payload");

    screenshotPath = path.join(os.tmpdir(), `openagent-desktop-local-mcp-ui-${Date.now()}.png`);
    await page.screenshot({ path: screenshotPath, fullPage: false });

    console.log(
      JSON.stringify(
        {
          ok: true,
          bridge_url: `http://127.0.0.1:${runtimePort}`,
          session_id: sessionId,
          lifecycle_pid: Number(uiLifecycle.pid),
          lifecycle_pid_before_restart: Number(pidBeforeRestart),
          request_methods: methods,
          request_pid_count: pids.length,
          tool_call_pid_count: callPids.length,
          screenshot: screenshotPath,
          mcp_config_path: localMcp.mcpConfigPath,
          server_script: localMcp.serverScript,
        },
        null,
        2,
      ),
    );
  } finally {
    if (page) await page.close().catch(() => {});
    if (browser) await browser.close().catch(() => {});
    await stopChild(vite);
    await stopChild(runtime);
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
