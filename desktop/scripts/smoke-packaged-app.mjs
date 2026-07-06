#!/usr/bin/env node

import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import fs from "node:fs";
import http from "node:http";
import os from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";

const scriptDir = path.dirname(fileURLToPath(import.meta.url));
const desktopDir = path.resolve(scriptDir, "..");
const repoRoot = path.resolve(desktopDir, "..");
const defaultAppPath = path.join(desktopDir, "src-tauri", "target", "release", "bundle", "macos", "OpenAgent.app");
const execFileAsync = promisify(execFile);
const DEFAULT_REAL_MODEL = "gpt-5.5";

function parseArgs(argv) {
  const options = {
    appPath: defaultAppPath,
    launch: "direct",
    screenshot: "",
    workflow: "",
    envFile: path.join(repoRoot, ".openagent", "openagent.env"),
    model: "",
    baseUrl: "",
  };
  for (const arg of argv) {
    if (arg.startsWith("--app=")) options.appPath = path.resolve(arg.slice("--app=".length));
    if (arg.startsWith("--launch=")) options.launch = arg.slice("--launch=".length);
    if (arg.startsWith("--screenshot=")) options.screenshot = path.resolve(arg.slice("--screenshot=".length));
    if (arg.startsWith("--workflow=")) options.workflow = arg.slice("--workflow=".length);
    if (arg.startsWith("--env-file=")) options.envFile = path.resolve(arg.slice("--env-file=".length));
    if (arg.startsWith("--model=")) options.model = arg.slice("--model=".length);
    if (arg.startsWith("--base-url=")) options.baseUrl = arg.slice("--base-url=".length);
  }
  assert.ok(["direct", "launchservices"].includes(options.launch), "--launch must be direct or launchservices");
  assert.ok(
    ["", "approval-rollback", "approval-dock", "real-streaming", "local-mcp-lifecycle"].includes(options.workflow),
    "--workflow must be approval-rollback, approval-dock, real-streaming, or local-mcp-lifecycle",
  );
  return options;
}

function readDotenvFile(filePath) {
  if (!fs.existsSync(filePath)) return {};
  const parsed = {};
  for (const rawLine of fs.readFileSync(filePath, "utf8").split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const match = /^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/.exec(line);
    if (!match) continue;
    let value = match[2].trim();
    if (
      (value.startsWith('"') && value.endsWith('"')) ||
      (value.startsWith("'") && value.endsWith("'"))
    ) {
      value = value.slice(1, -1);
    }
    parsed[match[1]] = value;
  }
  return parsed;
}

function providerEnvFromLocalConfig(options) {
  const localEnv = readDotenvFile(options.envFile);
  const allowedKeys = [
    "OPENAI_API_KEY",
    "OPENAI_BASE_URL",
    "OPENAI_MODEL",
    "OPENAGENT_MODEL",
    "OPENAI_WIRE_API",
    "OPENAGENT_APP_MAX_STEPS",
    "OPENAGENT_PROVIDER_STREAM",
  ];
  const providerEnv = {};
  for (const key of allowedKeys) {
    const value = process.env[key] || localEnv[key];
    if (value) providerEnv[key] = value;
  }
  if (options.baseUrl) providerEnv.OPENAI_BASE_URL = options.baseUrl;
  if (options.model) {
    providerEnv.OPENAI_MODEL = options.model;
    providerEnv.OPENAGENT_MODEL = options.model;
  }
  if (options.workflow === "real-streaming") {
    providerEnv.OPENAI_MODEL = options.model || DEFAULT_REAL_MODEL;
    providerEnv.OPENAGENT_MODEL = options.model || DEFAULT_REAL_MODEL;
    providerEnv.OPENAI_WIRE_API = providerEnv.OPENAI_WIRE_API || "responses";
    providerEnv.OPENAGENT_PROVIDER_STREAM = "1";
    providerEnv.OPENAGENT_APP_MAX_STEPS = providerEnv.OPENAGENT_APP_MAX_STEPS || "2";
    if (!providerEnv.OPENAI_API_KEY) {
      throw new Error(`OPENAI_API_KEY is missing; checked process env and ${options.envFile}`);
    }
    if (!providerEnv.OPENAI_BASE_URL) {
      throw new Error(`OPENAI_BASE_URL is missing; checked process env and ${options.envFile}`);
    }
  }
  return providerEnv;
}

function providerSummaryFromEnv(env) {
  const apiKey = env.OPENAI_API_KEY || "";
  return {
    base_url: env.OPENAI_BASE_URL || "",
    model: env.OPENAI_MODEL || env.OPENAGENT_MODEL || "",
    wire_api: env.OPENAI_WIRE_API || "",
    api_key: apiKey ? `set(len=${apiKey.length})` : "missing",
  };
}

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

function waitForFile(filePath, timeoutMs = 20_000) {
  const started = Date.now();
  return new Promise((resolve, reject) => {
    const tick = () => {
      if (fs.existsSync(filePath) && fs.readFileSync(filePath, "utf8").trim()) {
        resolve(fs.readFileSync(filePath, "utf8").trim());
        return;
      }
      if (Date.now() - started >= timeoutMs) {
        reject(new Error(`Timed out waiting for ${filePath}`));
        return;
      }
      setTimeout(tick, 150);
    };
    tick();
  });
}

async function waitForHealth(port, token, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError = "";
  while (Date.now() < deadline) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/api/health`, {
        headers: { authorization: `Bearer ${token}` },
      });
      if (response.ok) return response.json();
      lastError = `HTTP ${response.status}`;
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Timed out waiting for packaged App Bridge health on ${port}: ${lastError}`);
}

async function sleep(ms) {
  await new Promise((resolve) => setTimeout(resolve, ms));
}

async function bridgeJson(port, token, method, apiPath, body = undefined) {
  const response = await fetch(`http://127.0.0.1:${port}${apiPath}`, {
    method,
    headers: {
      accept: "application/json",
      authorization: `Bearer ${token}`,
      ...(body === undefined ? {} : { "content-type": "application/json" }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let payload = null;
  if (text.trim()) {
    try {
      payload = JSON.parse(text);
    } catch {
      payload = { raw: text };
    }
  }
  if (!response.ok) {
    throw new Error(`${method} ${apiPath} HTTP ${response.status}: ${text.slice(0, 500)}`);
  }
  return { status: response.status, payload };
}

async function waitForJson(description, fn, timeoutMs = 30_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError = "";
  while (Date.now() < deadline) {
    try {
      const value = await fn();
      if (value) return value;
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await sleep(250);
  }
  throw new Error(`Timed out waiting for ${description}${lastError ? `: ${lastError}` : ""}`);
}

function parseSseEvents(text) {
  return text
    .split(/\n\n/)
    .map((frame) =>
      frame
        .split(/\r?\n/)
        .filter((line) => line.startsWith("data:"))
        .map((line) => line.replace(/^data:\s?/, ""))
        .join("\n")
        .trim(),
    )
    .filter(Boolean)
    .filter((line) => line !== "[DONE]")
    .map((line) => JSON.parse(line));
}

async function bridgeEvents(port, token, lastEventId = 0, liveTimeoutMs = 0) {
  const query = new URLSearchParams({ last_event_id: String(lastEventId) });
  if (liveTimeoutMs > 0) query.set("live_timeout_ms", String(liveTimeoutMs));
  const response = await fetch(`http://127.0.0.1:${port}/api/events?${query.toString()}`, {
    headers: {
      accept: "text/event-stream",
      authorization: `Bearer ${token}`,
    },
  });
  if (!response.ok) throw new Error(`GET /api/events HTTP ${response.status}`);
  return parseSseEvents(await response.text());
}

async function startWorkflowProvider() {
  const requests = [];
  const server = http.createServer(async (request, response) => {
    const chunks = [];
    request.on("data", (chunk) => chunks.push(chunk));
    await new Promise((resolve) => request.on("end", resolve));
    const body = Buffer.concat(chunks).toString("utf8");
    requests.push({ method: request.method, url: request.url, body });

    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ object: "list", data: [{ id: "fake-workflow" }] }));
      return;
    }

    if (request.method === "POST" && request.url === "/v1/responses") {
      const responseCount = requests.filter((item) => item.method === "POST" && item.url === "/v1/responses").length;
      response.writeHead(200, {
        "content-type": "text/event-stream; charset=utf-8",
        "cache-control": "no-cache",
        connection: "close",
      });
      if (responseCount === 1) {
        response.write('data: {"type":"response.output_text.delta","delta":"planning packaged workflow "}\n\n');
        await sleep(200);
        const toolEvent = {
          type: "response.output_item.done",
          item: {
            type: "function_call",
            call_id: "call_packaged_workflow_write",
            name: "write",
            arguments: JSON.stringify({
              file_path: "workflow.txt",
              content: "approved packaged workflow\n",
            }),
          },
        };
        response.write(`data: ${JSON.stringify(toolEvent)}\n\n`);
        response.write('data: {"type":"response.completed","response":{"usage":{"input_tokens":8,"output_tokens":2}}}\n\n');
        response.write("data: [DONE]\n\n");
        response.end();
        return;
      }
      response.write('data: {"type":"response.output_text.delta","delta":"workflow completed"}\n\n');
      response.write('data: {"type":"response.completed","response":{"usage":{"input_tokens":5,"output_tokens":2}}}\n\n');
      response.write("data: [DONE]\n\n");
      response.end();
      return;
    }

    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "not found" }));
  });
  const providerPort = await new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      resolve(typeof address === "object" && address ? address.port : 0);
    });
  });
  return {
    port: providerPort,
    requests,
    env: {
      OPENAI_API_KEY: "test-key",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "fake-workflow",
      OPENAGENT_MODEL: "fake-workflow",
      OPENAI_WIRE_API: "responses",
      OPENAGENT_PROVIDER_STREAM: "1",
      OPENAGENT_APP_MAX_STEPS: "4",
    },
    close: () =>
      new Promise((resolve) => {
        server.close(() => resolve());
      }),
  };
}

function questionCall(callId) {
  return {
    type: "function_call",
    call_id: callId,
    name: "question",
    arguments: JSON.stringify({
      questions: [
        {
          header: "Confirm",
          question: "Proceed with the packaged approval dock smoke?",
          multiple: false,
          options: [
            { label: "yes", description: "Continue" },
            { label: "no", description: "Stop" },
          ],
        },
      ],
    }),
  };
}

async function startApprovalDockWorkflowProvider() {
  const requests = [];
  const responses = [
    {
      id: "resp_packaged_question_reply",
      output: [questionCall("call_packaged_question_reply")],
      usage: { input_tokens: 4, output_tokens: 1 },
    },
    {
      id: "resp_packaged_question_reply_final",
      output_text: "packaged continuing after yes",
      usage: { input_tokens: 7, output_tokens: 3 },
    },
    {
      id: "resp_packaged_question_dismiss",
      output: [questionCall("call_packaged_question_dismiss")],
      usage: { input_tokens: 4, output_tokens: 1 },
    },
  ];
  let responseIndex = 0;
  const server = http.createServer(async (request, response) => {
    const chunks = [];
    request.on("data", (chunk) => chunks.push(chunk));
    await new Promise((resolve) => request.on("end", resolve));
    const body = Buffer.concat(chunks).toString("utf8");
    requests.push({ method: request.method, url: request.url, body });

    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ object: "list", data: [{ id: "fake-approval-dock", object: "model" }] }));
      return;
    }

    if (request.method === "POST" && request.url === "/v1/responses") {
      const payload = responses[responseIndex++] ?? {
        id: `resp_packaged_extra_${responseIndex}`,
        output_text: "extra packaged approval dock response",
        usage: { input_tokens: 1, output_tokens: 1 },
      };
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify(payload));
      return;
    }

    response.writeHead(404, { "content-type": "application/json" });
    response.end(JSON.stringify({ error: "not found" }));
  });
  const providerPort = await new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      resolve(typeof address === "object" && address ? address.port : 0);
    });
  });
  return {
    port: providerPort,
    requests,
    env: {
      OPENAI_API_KEY: "test-key",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "fake-approval-dock",
      OPENAGENT_MODEL: "fake-approval-dock",
      OPENAI_WIRE_API: "responses",
      OPENAGENT_APP_MAX_STEPS: "4",
    },
    close: () =>
      new Promise((resolve) => {
        server.close(() => resolve());
      }),
  };
}

function writeLocalMcpServerScript(scriptPath) {
  fs.writeFileSync(
    scriptPath,
    `#!/usr/bin/env node
import fs from "node:fs";

let buffer = Buffer.alloc(0);

function writeFrame(id, result) {
  const body = JSON.stringify({ jsonrpc: "2.0", id, result });
  process.stdout.write(\`Content-Length: \${Buffer.byteLength(body)}\\r\\n\\r\\n\${body}\`);
}

function logMethod(method) {
  const logPath = process.env.LOCAL_REQUEST_LOG || "";
  if (!logPath) return;
  fs.appendFileSync(logPath, \`\${process.pid} \${method}\\n\`);
}

function handleMessage(raw) {
  const message = JSON.parse(raw);
  const method = message.method || "unknown";
  logMethod(method);
  if (method === "initialize") {
    writeFrame(message.id, {
      protocolVersion: "2024-11-05",
      capabilities: { tools: {} },
      serverInfo: { name: "packaged-local-mcp", version: "1.0.0" },
    });
    return;
  }
  if (method === "notifications/initialized") return;
  if (method === "tools/list") {
    writeFrame(message.id, {
      tools: [
        {
          name: "stdio_echo",
          title: "Stdio Echo",
          description: "Echo input through packaged local stdio MCP",
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
  if (method === "tools/call") {
    const text = message.params?.arguments?.text || "";
    writeFrame(message.id, {
      content: [{ type: "text", text: \`packaged stdio echo: \${text}\` }],
    });
    return;
  }
  if (method === "shutdown") {
    writeFrame(message.id, {});
    return;
  }
  if (method === "exit") {
    process.exit(0);
  }
}

function pump() {
  while (true) {
    const split = buffer.indexOf(Buffer.from("\\r\\n\\r\\n"));
    if (split < 0) return;
    const header = buffer.slice(0, split).toString("utf8");
    const match = /^content-length:\\s*(\\d+)$/im.exec(header);
    if (!match) {
      buffer = buffer.slice(split + 4);
      continue;
    }
    const length = Number.parseInt(match[1], 10);
    const bodyStart = split + 4;
    const bodyEnd = bodyStart + length;
    if (buffer.length < bodyEnd) return;
    const body = buffer.slice(bodyStart, bodyEnd).toString("utf8");
    buffer = buffer.slice(bodyEnd);
    handleMessage(body);
  }
}

process.stdin.on("data", (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  pump();
});
`,
    "utf8",
  );
}

function prepareLocalMcpLifecycleWorkflow(workspace, tempRoot) {
  const toolsDir = path.join(workspace, "mcp-tools");
  const openagentDir = path.join(workspace, ".openagent");
  fs.mkdirSync(toolsDir, { recursive: true });
  fs.mkdirSync(openagentDir, { recursive: true });
  const serverScript = path.join(toolsDir, "packaged-local-mcp.mjs");
  const requestLog = path.join(tempRoot, "local-mcp-requests.log");
  writeLocalMcpServerScript(serverScript);
  const mcpConfigPath = path.join(openagentDir, "mcp.json");
  fs.writeFileSync(
    mcpConfigPath,
    JSON.stringify(
      {
        mcp: {
          servers: {
            "local-tools": {
              type: "local",
              command: [process.execPath, serverScript],
              cwd: "mcp-tools",
              env: {
                LOCAL_REQUEST_LOG: requestLog,
                LOCAL_SECRET: "packaged-local-mcp-secret",
              },
              timeout_ms: 5000,
              enabled: true,
            },
          },
        },
      },
      null,
      2,
    ),
  );
  return { mcpConfigPath, requestLog, serverScript };
}

async function runCommand(command, args, options = {}) {
  try {
    return await execFileAsync(command, args, { windowsHide: true, ...options });
  } catch (error) {
    const stderr = error?.stderr ? String(error.stderr).trim() : "";
    const stdout = error?.stdout ? String(error.stdout).trim() : "";
    const detail = [stderr, stdout].filter(Boolean).join("\n");
    throw new Error(`${command} ${args.join(" ")} failed${detail ? `:\n${detail}` : ""}`);
  }
}

async function launchctlGetenv(name) {
  try {
    const { stdout } = await execFileAsync("launchctl", ["getenv", name], { windowsHide: true });
    const value = stdout.trimEnd();
    return value ? { found: true, value } : { found: false, value: "" };
  } catch {
    return { found: false, value: "" };
  }
}

async function launchctlSetenv(name, value) {
  await runCommand("launchctl", ["setenv", name, value]);
}

async function launchctlUnsetenv(name) {
  await runCommand("launchctl", ["unsetenv", name]);
}

async function withLaunchServicesEnv(env, callback) {
  const names = Object.keys(env);
  const previous = new Map();
  for (const name of names) {
    previous.set(name, await launchctlGetenv(name));
  }
  try {
    for (const [name, value] of Object.entries(env)) {
      await launchctlSetenv(name, value);
    }
    return await callback();
  } finally {
    for (const name of names.reverse()) {
      const old = previous.get(name);
      if (old?.found) {
        await launchctlSetenv(name, old.value);
      } else {
        await launchctlUnsetenv(name);
      }
    }
  }
}

async function stopProcess(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((resolve) => {
    const killer = setTimeout(() => {
      try {
        child.kill("SIGKILL");
      } catch {
        // The process may have exited between SIGTERM and SIGKILL.
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

async function stopLaunchServicesApp(appPath, port) {
  if (process.platform !== "darwin") return;
  const executablePath = path.join(appPath, "Contents", "MacOS", "openagent-desktop");
  try {
    await runCommand("pkill", ["-f", executablePath]);
  } catch {
    // The process may already be gone, or pkill may not find a path-backed match.
  }
  if (port) {
    try {
      await runCommand("pkill", ["-f", `openagent-http-runtime.*--port ${port}`]);
    } catch {
      // The managed bridge is normally killed by the app process; this is a smoke-test safety net.
    }
  }
}

async function frontmostProcessName() {
  if (process.platform !== "darwin") return "";
  try {
    const { stdout } = await execFileAsync("osascript", [
      "-e",
      'tell application "System Events" to get name of first application process whose frontmost is true',
    ]);
    return stdout.trim();
  } catch {
    return "";
  }
}

async function activateOpenAgentForScreenshot(appPath) {
  if (process.platform !== "darwin") return;
  try {
    await runCommand("open", ["-b", "ai.openagent.desktop"]);
  } catch {
    await runCommand("open", ["-a", appPath]);
  }
  try {
    await runCommand("osascript", [
      "-e",
      'tell application "System Events" to set frontmost of first application process whose bundle identifier is "ai.openagent.desktop" to true',
    ]);
  } catch {
    // Activation via LaunchServices is usually enough; System Events can fail without accessibility grants.
  }
  await sleep(1_000);
}

async function openAgentWindowId() {
  if (process.platform !== "darwin") return "";
  const script = `
import CoreGraphics
import Foundation

let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID) as? [[String: Any]] ?? []
for window in windows {
  let owner = window[kCGWindowOwnerName as String] as? String ?? ""
  let layer = window[kCGWindowLayer as String] as? Int ?? -1
  guard owner == "OpenAgent", layer == 0 else { continue }
  guard let number = window[kCGWindowNumber as String] as? UInt32 else { continue }
  let bounds = window[kCGWindowBounds as String] as? [String: Any] ?? [:]
  let width = bounds["Width"] as? Double ?? 0
  let height = bounds["Height"] as? Double ?? 0
  guard width > 200, height > 200 else { continue }
  print(number)
  exit(0)
}
exit(1)
`;
  try {
    const { stdout } = await execFileAsync("swift", ["-e", script], {
      timeout: 20_000,
      windowsHide: true,
    });
    return stdout.trim().split(/\s+/)[0] || "";
  } catch {
    return "";
  }
}

async function captureScreenshot(screenshotPath, appPath) {
  if (!screenshotPath || process.platform !== "darwin") return null;
  fs.mkdirSync(path.dirname(screenshotPath), { recursive: true });
  await activateOpenAgentForScreenshot(appPath);
  const windowId = await openAgentWindowId();
  if (windowId) {
    try {
      await runCommand("screencapture", ["-x", `-l${windowId}`, screenshotPath]);
      const stat = fs.statSync(screenshotPath);
      assert.ok(stat.size > 10_000, `Screenshot looks empty: ${screenshotPath}`);
      return screenshotPath;
    } catch (error) {
      try {
        fs.rmSync(screenshotPath, { force: true });
      } catch {
        // The screenshot may not have been created.
      }
      const frontmost = await frontmostProcessName();
      if (frontmost && frontmost !== "OpenAgent") {
        console.warn(
          `warning: OpenAgent window screenshot failed and app is not frontmost; frontmost=${frontmost}; ${
            error instanceof Error ? error.message : String(error)
          }`,
        );
        return null;
      }
      // macOS can transiently report a valid window id before screencapture can
      // materialize it. Fall back to a full-screen capture so the smoke still
      // proves the packaged app reached a visible state.
    }
  }
  const frontmost = await frontmostProcessName();
  if (frontmost && frontmost !== "OpenAgent") {
    console.warn(`warning: OpenAgent is not frontmost before screenshot; frontmost=${frontmost}`);
    return null;
  }
  await sleep(1_500);
  await runCommand("screencapture", ["-x", screenshotPath]);
  const stat = fs.statSync(screenshotPath);
  assert.ok(stat.size > 10_000, `Screenshot looks empty: ${screenshotPath}`);
  return screenshotPath;
}

function siblingScreenshotPath(screenshotPath, suffix) {
  if (!screenshotPath) return "";
  const parsed = path.parse(screenshotPath);
  return path.join(parsed.dir, `${parsed.name}.${suffix}${parsed.ext || ".png"}`);
}

async function runApprovalRollbackWorkflow(port, token, workspace, appPath = "", restoredScreenshotPath = "") {
  const created = await bridgeJson(port, token, "POST", "/api/sessions", {
    cwd: workspace,
    title: "Packaged workflow smoke",
  });
  const sessionId = created.payload.session_id || created.payload.id;
  assert.ok(sessionId, "session id missing");

  const started = await bridgeJson(port, token, "POST", `/api/sessions/${sessionId}/turns`, {
    input: "Use the write tool to create workflow.txt, then report completion.",
    model: "fake-workflow",
    permission: "PLAN_ONLY",
    stream: true,
    async: true,
  });
  assert.equal(started.status, 202);
  assert.equal(started.payload.status, "running");
  const turnId = started.payload.turn_id;
  assert.ok(turnId, "turn id missing");

  const approval = await waitForJson("pending approval", async () => {
    const { payload } = await bridgeJson(port, token, "GET", "/api/approvals");
    const approvals = Array.isArray(payload.approvals) ? payload.approvals : [];
    return approvals.find((item) => item.session_id === sessionId && item.turn_id === turnId);
  });
  const requestId = approval.request_id;
  assert.ok(requestId, "approval request id missing");

  const eventsBeforeApproval = await bridgeEvents(port, token, 0);
  assert.ok(
    eventsBeforeApproval.some((event) => event.method === "item/agentMessage/delta"),
    "expected streaming assistant delta before approval",
  );
  assert.ok(
    eventsBeforeApproval.some((event) => event.method === "turn/approval_requested"),
    "expected approval requested event",
  );

  const allowed = await bridgeJson(port, token, "POST", `/api/approvals/${requestId}`, {
    action: "allow",
    scope: "once",
  });
  assert.ok(["running", "completed"].includes(allowed.payload.status), "approval did not resume run");

  const completed = await waitForJson("completed workflow turn", async () => {
    const { payload } = await bridgeJson(port, token, "GET", `/api/sessions/${sessionId}/messages?limit=100`);
    const messages = Array.isArray(payload.messages_v2) ? payload.messages_v2 : [];
    const assistantText = messages
      .filter((message) => message.info?.role === "assistant")
      .flatMap((message) => (Array.isArray(message.parts) ? message.parts : []))
      .filter((part) => part.kind === "text")
      .map((part) => {
        const content = part.content;
        if (typeof content === "string") return content;
        if (content && typeof content === "object" && typeof content.text === "string") return content.text;
        return "";
      })
      .join("\n");
    return assistantText.includes("workflow completed") ? { messages, assistantText } : null;
  });

  const turnStatus = await waitForJson("workflow turn terminal status", async () => {
    const { payload } = await bridgeJson(port, token, "GET", `/api/turns/${turnId}`);
    return payload.status === "completed" ? payload : null;
  });

  const workflowFile = path.join(workspace, "workflow.txt");
  const fileBeforeRestore = await waitForJson("workflow file write", async () => {
    if (!fs.existsSync(workflowFile)) return null;
    const content = fs.readFileSync(workflowFile, "utf8");
    return content === "approved packaged workflow\n" ? { path: workflowFile, content } : null;
  });

  const diff = await bridgeJson(port, token, "GET", `/api/sessions/${sessionId}/diff`);
  const diffText = JSON.stringify(diff.payload);
  assert.ok(diffText.includes("workflow.txt"), "diff did not include workflow.txt");

  const checkpoints = await bridgeJson(port, token, "GET", `/api/sessions/${sessionId}/checkpoints`);
  const checkpointList = Array.isArray(checkpoints.payload.checkpoints) ? checkpoints.payload.checkpoints : [];
  assert.ok(checkpointList.length > 0, "expected checkpoints");
  const restoreTarget =
    checkpointList.find((checkpoint) => checkpoint.kind === "step_start") || checkpointList[checkpointList.length - 1];
  const checkpointId = restoreTarget?.checkpoint_id;
  assert.ok(checkpointId, "checkpoint id missing");

  const restored = await bridgeJson(
    port,
    token,
    "POST",
    `/api/sessions/${sessionId}/checkpoints/${checkpointId}/restore`,
    {},
  );
  assert.equal(restored.payload.status, "restored");

  const fileAfterRestore = await waitForJson("workflow file restore", async () => {
    return fs.existsSync(workflowFile) ? null : { exists: false, path: workflowFile };
  });

  const eventsAfterRestore = await bridgeEvents(port, token, 0);
  assert.ok(
    eventsAfterRestore.some((event) => event.method === "turn/completed"),
    "expected completed event",
  );
  assert.ok(
    eventsAfterRestore.some((event) => event.method === "checkpoint/restored"),
    "expected checkpoint restored event",
  );

  const restoredSession = await waitForJson("persisted checkpoint restore metadata", async () => {
    const { payload } = await bridgeJson(port, token, "GET", "/api/sessions");
    const sessions = Array.isArray(payload.sessions) ? payload.sessions : [];
    const session = sessions.find((item) => item.session_id === sessionId || item.id === sessionId);
    const restore = session?.metadata?.latest_checkpoint_restore;
    const history = Array.isArray(session?.metadata?.checkpoint_restore_history)
      ? session.metadata.checkpoint_restore_history
      : [];
    const historyRecord = history.find((item) => item?.checkpoint_id === checkpointId);
    return restore?.checkpoint_id === checkpointId && historyRecord
      ? {
          checkpoint_id: restore.checkpoint_id,
          run_id: restore.run_id,
          restored_at_ms: restore.restored_at_ms,
          history_count: history.length,
          history_run_id: historyRecord.run_id,
          history_file_count: historyRecord.file_count,
        }
      : null;
  });

  let restoredScreenshot = "";
  if (restoredScreenshotPath && appPath) {
    await sleep(1_000);
    restoredScreenshot = (await captureScreenshot(restoredScreenshotPath, appPath)) || "";
    assert.ok(restoredScreenshot, "restored checkpoint screenshot was not captured");
  }

  return {
    session_id: sessionId,
    turn_id: turnId,
    approval_request_id: requestId,
    turn_status: turnStatus.status,
    message_count: completed.messages.length,
    assistant_text_preview: completed.assistantText.slice(0, 160),
    file_before_restore: {
      path: fileBeforeRestore.path,
      content: fileBeforeRestore.content,
    },
    restore_checkpoint_id: checkpointId,
    restore_metadata: restoredSession,
    restored_screenshot: restoredScreenshot || null,
    file_after_restore: {
      exists: fileAfterRestore.exists,
      path: fileAfterRestore.path,
    },
    event_methods: [...new Set(eventsAfterRestore.map((event) => event.method))],
  };
}

async function waitForNoPendingInteractions(port, token) {
  await waitForJson("pending approval/question queues to clear", async () => {
    const approvals = await bridgeJson(port, token, "GET", "/api/approvals");
    const questions = await bridgeJson(port, token, "GET", "/api/questions");
    const approvalCount = approvals.payload.count ?? approvals.payload.approvals?.length ?? 0;
    const questionCount = questions.payload.count ?? questions.payload.questions?.length ?? 0;
    return approvalCount === 0 && questionCount === 0 ? { approvalCount, questionCount } : null;
  });
}

async function sessionMessages(port, token, sessionId) {
  const { payload } = await bridgeJson(port, token, "GET", `/api/sessions/${sessionId}/messages?limit=120`);
  return Array.isArray(payload.messages_v2) ? payload.messages_v2 : [];
}

function interactionPartsFromMessages(messages, kind) {
  return messages
    .flatMap((message) => (Array.isArray(message.parts) ? message.parts : []))
    .filter((part) => part.kind === kind);
}

function interactionPartContent(part) {
  return part?.content && typeof part.content === "object" ? part.content : {};
}

function interactionPartMatches(part, { status, callId, text }) {
  const content = interactionPartContent(part);
  const request = content.request && typeof content.request === "object" ? content.request : {};
  const serialized = JSON.stringify(part);
  return (
    (!status || content.status === status || part.status === status) &&
    (!callId || content.call_id === callId || request.call_id === callId || part.attributes?.call_id === callId) &&
    (!text || serialized.includes(text))
  );
}

async function waitForInteractionHistory(port, token, sessionId, kind, criteria) {
  let lastMessages = [];
  return waitForJson(`persisted ${kind} interaction history`, async () => {
    const messages = await sessionMessages(port, token, sessionId);
    lastMessages = messages;
    const parts = interactionPartsFromMessages(messages, kind);
    const matched = parts.find((part) => interactionPartMatches(part, criteria));
    return matched ? { messages, part: matched, parts } : null;
  }).catch((error) => {
    throw new Error(
      `${error.message}; ${kind}_parts=${JSON.stringify(interactionPartsFromMessages(lastMessages, kind))}`,
    );
  });
}

async function pendingApprovalFor(port, token, sessionId, callId) {
  return waitForJson(`pending approval ${callId}`, async () => {
    const { payload } = await bridgeJson(port, token, "GET", "/api/approvals");
    const approvals = Array.isArray(payload.approvals) ? payload.approvals : [];
    return approvals.find((item) => {
      const approval = item.approval || {};
      return item.session_id === sessionId && (approval.call_id === callId || item.request_id === `approval_${callId}`);
    });
  });
}

async function pendingQuestionFor(port, token, sessionId, callId) {
  return waitForJson(`pending question ${callId}`, async () => {
    const { payload } = await bridgeJson(port, token, "GET", "/api/questions");
    const questions = Array.isArray(payload.questions) ? payload.questions : [];
    return questions.find((item) => {
      const question = item.question || {};
      return item.session_id === sessionId && (question.call_id === callId || item.request_id === `question_${callId}`);
    });
  });
}

async function runApprovalDockWorkflow(port, token, workspace, appPath, pendingScreenshotPath = "") {
  const created = await bridgeJson(port, token, "POST", "/api/sessions", {
    cwd: workspace,
    title: "Packaged approval dock smoke",
  });
  const sessionId = created.payload.session_id || created.payload.id;
  assert.ok(sessionId, "session id missing");

  const allowStarted = await bridgeJson(port, token, "POST", `/api/sessions/${sessionId}/turns`, {
    input: "Packaged approval dock allow path",
    permission: "PLAN_ONLY",
    tool_call: {
      call_id: "call_packaged_dock_allow",
      name: "write",
      input: { file_path: "dock-allow-packaged.txt", content: "packaged allowed\n" },
    },
  });
  assert.equal(allowStarted.payload.status, "waiting_approval", "allow approval did not pause");
  const allowApproval = await pendingApprovalFor(port, token, sessionId, "call_packaged_dock_allow");
  assert.equal(allowApproval.approval?.tool_name, "write");
  assert.equal(allowApproval.approval?.permission_action, "ask");
  assert.equal(allowApproval.approval?.permission_pattern, "dock-allow-packaged.txt");
  let pendingScreenshot = "";
  if (pendingScreenshotPath) {
    await sleep(1_000);
    pendingScreenshot = (await captureScreenshot(pendingScreenshotPath, appPath)) || "";
    assert.ok(pendingScreenshot, "pending approval dock screenshot was not captured");
  }
  const allowed = await bridgeJson(port, token, "POST", `/api/approvals/${allowApproval.request_id}`, {
    action: "allow",
    scope: "once",
  });
  assert.ok(["running", "completed"].includes(allowed.payload.status), "allow approval did not resume");
  const allowFile = path.join(workspace, "dock-allow-packaged.txt");
  await waitForJson("packaged allowed file", async () => {
    if (!fs.existsSync(allowFile)) return null;
    const content = fs.readFileSync(allowFile, "utf8");
    return content === "packaged allowed\n" ? { content } : null;
  });
  await waitForInteractionHistory(port, token, sessionId, "approval", {
    status: "allowed",
    callId: "call_packaged_dock_allow",
    text: "dock-allow-packaged.txt",
  });
  await waitForNoPendingInteractions(port, token);

  const denyStarted = await bridgeJson(port, token, "POST", `/api/sessions/${sessionId}/turns`, {
    input: "Packaged approval dock deny path",
    permission: "PLAN_ONLY",
    tool_call: {
      call_id: "call_packaged_dock_deny",
      name: "write",
      input: { file_path: "dock-deny-packaged.txt", content: "packaged denied\n" },
    },
  });
  assert.equal(denyStarted.payload.status, "waiting_approval", "deny approval did not pause");
  const denyApproval = await pendingApprovalFor(port, token, sessionId, "call_packaged_dock_deny");
  const denied = await bridgeJson(port, token, "POST", `/api/approvals/${denyApproval.request_id}`, {
    action: "deny",
    scope: "once",
    note: "denied from packaged approval dock smoke",
  });
  assert.equal(denied.payload.status, "failed", "deny approval should fail the turn");
  assert.equal(fs.existsSync(path.join(workspace, "dock-deny-packaged.txt")), false, "denied approval wrote a file");
  await waitForInteractionHistory(port, token, sessionId, "approval", {
    status: "denied",
    callId: "call_packaged_dock_deny",
    text: "dock-deny-packaged.txt",
  });
  await waitForNoPendingInteractions(port, token);

  const questionReplyStarted = await bridgeJson(port, token, "POST", `/api/sessions/${sessionId}/turns`, {
    input: "Ask the packaged approval dock question reply path.",
    model: "fake-approval-dock",
    permission: "FULL",
  });
  assert.equal(questionReplyStarted.payload.status, "waiting_question", "question reply did not pause");
  const replyQuestion = await pendingQuestionFor(port, token, sessionId, "call_packaged_question_reply");
  assert.ok(
    JSON.stringify(replyQuestion.question || {}).includes("Proceed with the packaged approval dock smoke?"),
    "question prompt missing",
  );
  const replied = await bridgeJson(port, token, "POST", `/api/questions/${replyQuestion.request_id}/reply`, {
    answers: [["yes"]],
  });
  assert.equal(replied.payload.status, "completed", "question reply did not complete");
  await waitForInteractionHistory(port, token, sessionId, "question", {
    status: "answered",
    callId: "call_packaged_question_reply",
    text: "yes",
  });
  await waitForJson("packaged question reply final answer", async () => {
    const text = assistantTextFromMessages(await sessionMessages(port, token, sessionId));
    return text.includes("packaged continuing after yes") ? { text } : null;
  });
  await waitForNoPendingInteractions(port, token);

  const questionDismissStarted = await bridgeJson(port, token, "POST", `/api/sessions/${sessionId}/turns`, {
    input: "Ask the packaged approval dock question dismiss path.",
    model: "fake-approval-dock",
    permission: "FULL",
  });
  assert.equal(questionDismissStarted.payload.status, "waiting_question", "question dismiss did not pause");
  const dismissQuestion = await pendingQuestionFor(port, token, sessionId, "call_packaged_question_dismiss");
  const dismissed = await bridgeJson(port, token, "POST", `/api/questions/${dismissQuestion.request_id}/reply`, {
    dismissed: true,
    note: "dismissed from packaged approval dock smoke",
  });
  assert.equal(dismissed.payload.status, "failed", "question dismiss should fail the turn");
  await waitForInteractionHistory(port, token, sessionId, "question", {
    status: "dismissed",
    callId: "call_packaged_question_dismiss",
    text: "dismissed from packaged approval dock smoke",
  });
  await waitForNoPendingInteractions(port, token);

  const reloadedMessages = await sessionMessages(port, token, sessionId);
  const approvalParts = interactionPartsFromMessages(reloadedMessages, "approval");
  const questionParts = interactionPartsFromMessages(reloadedMessages, "question");
  assert.ok(
    approvalParts.some((part) =>
      interactionPartMatches(part, {
        status: "allowed",
        callId: "call_packaged_dock_allow",
        text: "dock-allow-packaged.txt",
      }),
    ),
    "reloaded messages missing allowed approval",
  );
  assert.ok(
    approvalParts.some((part) =>
      interactionPartMatches(part, {
        status: "denied",
        callId: "call_packaged_dock_deny",
        text: "dock-deny-packaged.txt",
      }),
    ),
    "reloaded messages missing denied approval",
  );
  assert.ok(
    questionParts.some((part) =>
      interactionPartMatches(part, {
        status: "answered",
        callId: "call_packaged_question_reply",
        text: "yes",
      }),
    ),
    "reloaded messages missing answered question",
  );
  assert.ok(
    questionParts.some((part) =>
      interactionPartMatches(part, {
        status: "dismissed",
        callId: "call_packaged_question_dismiss",
        text: "dismissed",
      }),
    ),
    "reloaded messages missing dismissed question",
  );

  return {
    session_id: sessionId,
    allow_request_id: allowApproval.request_id,
    deny_request_id: denyApproval.request_id,
    reply_question_request_id: replyQuestion.request_id,
    dismiss_question_request_id: dismissQuestion.request_id,
    pending_screenshot: pendingScreenshot || null,
    allowed_file: allowFile,
    denied_file_exists: fs.existsSync(path.join(workspace, "dock-deny-packaged.txt")),
    approval_history_count: approvalParts.length,
    question_history_count: questionParts.length,
    message_count: reloadedMessages.length,
  };
}

function assistantTextFromMessages(messages) {
  return messages
    .filter((message) => message.info?.role === "assistant")
    .flatMap((message) => (Array.isArray(message.parts) ? message.parts : []))
    .filter((part) => part.kind === "text")
    .map((part) => {
      const content = part.content;
      if (typeof content === "string") return content;
      if (content && typeof content === "object" && typeof content.text === "string") return content.text;
      return "";
    })
    .join("\n")
    .trim();
}

function realStreamingPrompt() {
  return [
    "For an automated packaged desktop streaming smoke test, write exactly two lines.",
    "The first line must be OA_PACKAGED_REAL_STREAM_BEGIN.",
    "The second line must be OA_PACKAGED_REAL_STREAM_END.",
    "Do not use a code block and do not add any other text.",
  ].join(" ");
}

function summarizeEvents(events, turnId) {
  const methods = events.map((event) => event.method).filter(Boolean);
  const turnStarted = events.find(
    (event) => event.method === "turn/started" && (!turnId || event.params?.turn_id === turnId),
  );
  return {
    event_count: events.length,
    methods: [...new Set(methods)],
    delta_count: methods.filter((method) => method === "item/agentMessage/delta").length,
    completed_count: methods.filter((method) => method === "turn/completed").length,
    failed_count: methods.filter((method) => method === "turn/failed").length,
    turn_model: turnStarted?.params?.model,
    last_method: methods.at(-1),
  };
}

async function runRealStreamingWorkflow(port, token, workspace, providerSummary) {
  const providerHealth = await bridgeJson(port, token, "GET", "/api/models?check=true");
  assert.equal(providerHealth.payload.healthy, true, "provider health check failed");
  assert.equal(providerHealth.payload.model, providerSummary.model, "provider model mismatch");

  const created = await bridgeJson(port, token, "POST", "/api/sessions", {
    cwd: workspace,
    title: "Packaged real streaming smoke",
  });
  const sessionId = created.payload.session_id || created.payload.id;
  assert.ok(sessionId, "session id missing");

  const startedAt = Date.now();
  const started = await bridgeJson(port, token, "POST", `/api/sessions/${sessionId}/turns`, {
    input: realStreamingPrompt(),
    model: providerSummary.model,
    permission: "FULL",
    stream: true,
    async: true,
  });
  assert.equal(started.status, 202);
  assert.equal(started.payload.status, "running");
  const turnId = started.payload.turn_id;
  assert.ok(turnId, "turn id missing");

  const completed = await waitForJson("real packaged streaming completion", async () => {
    const { payload } = await bridgeJson(port, token, "GET", `/api/sessions/${sessionId}/messages?limit=100`);
    const messages = Array.isArray(payload.messages_v2) ? payload.messages_v2 : [];
    const assistantText = assistantTextFromMessages(messages);
    const events = await bridgeEvents(port, token, 0, 1_000);
    const eventSummary = summarizeEvents(events, turnId);
    if (
      assistantText.length > 0 &&
      eventSummary.delta_count > 0 &&
      eventSummary.completed_count > 0 &&
      eventSummary.failed_count === 0
    ) {
      return { messages, assistantText, eventSummary };
    }
    return null;
  }, 180_000);

  const eventSummary = completed.eventSummary;
  assert.equal(eventSummary.turn_model, providerSummary.model, "runtime used the wrong model");

  return {
    session_id: sessionId,
    turn_id: turnId,
    elapsed_ms: Date.now() - startedAt,
    message_count: completed.messages.length,
    assistant_text_length: completed.assistantText.length,
    assistant_text_preview: completed.assistantText.slice(0, 160),
    marker_begin_seen: completed.assistantText.includes("OA_PACKAGED_REAL_STREAM_BEGIN"),
    marker_end_seen: completed.assistantText.includes("OA_PACKAGED_REAL_STREAM_END"),
    provider_health: {
      healthy: providerHealth.payload.healthy,
      model: providerHealth.payload.model,
      model_count: providerHealth.payload.model_count,
      configured_model_available: providerHealth.payload.configured_model_available,
      base_url: providerHealth.payload.base_url,
      api_key: providerHealth.payload.api_key,
    },
    events: eventSummary,
  };
}

function partContent(part) {
  return part?.content && typeof part.content === "object" ? part.content : {};
}

function partAttributes(part) {
  return part?.attributes && typeof part.attributes === "object" ? part.attributes : {};
}

function partMetadata(part) {
  const content = partContent(part);
  const attributes = partAttributes(part);
  if (content.metadata && typeof content.metadata === "object") return content.metadata;
  if (attributes.metadata && typeof attributes.metadata === "object") return attributes.metadata;
  if (part?.metadata && typeof part.metadata === "object") return part.metadata;
  return {};
}

function toolPartCallId(part) {
  return partContent(part).call_id || partAttributes(part).call_id || "";
}

function toolPartName(part) {
  return partContent(part).name || partAttributes(part).name || "";
}

function toolPartOutput(part) {
  return partContent(part).output || "";
}

function mcpToolPartsFromMessages(messages) {
  return messages
    .flatMap((message) => (Array.isArray(message.parts) ? message.parts : []))
    .filter((part) => part.kind === "tool")
    .filter((part) => {
      const metadata = partMetadata(part);
      const name = toolPartName(part);
      return metadata.backend === "mcp" || name.startsWith("mcp_tool_") || Boolean(metadata.mcp_tool_name);
    });
}

function completedToolEventFromTurn(payload, callId) {
  const events = Array.isArray(payload?.events) ? payload.events : [];
  return events.find(
    (event) =>
      event?.method === "item/toolCall/completed" &&
      event?.params?.call_id === callId,
  );
}

function messageDebugSummary(messages) {
  return messages.slice(-5).map((message) => ({
    role: message?.info?.role,
    parts: (Array.isArray(message?.parts) ? message.parts : []).map((part) => ({
      kind: part.kind,
      status: part.status,
      call_id: toolPartCallId(part),
      name: toolPartName(part),
      output: toolPartOutput(part).slice(0, 120),
      metadata: partMetadata(part),
    })),
  }));
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

async function runLocalMcpLifecycleWorkflow(port, token, workspace, localMcp) {
  assert.ok(localMcp?.mcpConfigPath, "local MCP config was not prepared");
  const started = await bridgeJson(port, token, "POST", "/api/mcp/servers/local-tools/start", {});
  const server = started.payload.servers?.find((item) => item.name === "local-tools");
  assert.equal(server?.lifecycle_status, "running", "local MCP lifecycle did not start");
  assert.equal(server?.lifecycle_tool_count, 1, "local MCP lifecycle did not discover tools");
  const lifecyclePid = server.lifecycle_pid;
  assert.ok(lifecyclePid, "local MCP lifecycle pid missing");

  const created = await bridgeJson(port, token, "POST", "/api/sessions", {
    cwd: workspace,
    title: "Packaged local MCP lifecycle smoke",
  });
  const sessionId = created.payload.session_id || created.payload.id;
  assert.ok(sessionId, "session id missing");

  const turn = await bridgeJson(port, token, "POST", `/api/sessions/${sessionId}/turns`, {
    input: "Call the local MCP echo tool through the packaged App Bridge lifecycle session.",
    permission: "FULL",
    dangerously_skip_permissions: true,
    tool_call: {
      call_id: "call_packaged_local_mcp",
      name: "mcp_tool_local_tools_stdio_echo",
      input: { text: "packaged-lifecycle" },
    },
  });
  assert.equal(turn.payload.status, "completed", "local MCP tool turn did not complete");
  const completedEvent = completedToolEventFromTurn(turn.payload, "call_packaged_local_mcp");
  assert.ok(completedEvent, `local MCP tool completion event missing: ${JSON.stringify(turn.payload.events ?? [])}`);
  const eventMetadata = completedEvent.params?.metadata || {};
  const eventOutput = completedEvent.params?.output || "";
  assert.ok(
    eventOutput.includes("packaged stdio echo: packaged-lifecycle"),
    `unexpected local MCP event output: ${eventOutput}`,
  );
  assert.equal(eventMetadata.mcp_lifecycle_reused, true, "local MCP tool did not reuse lifecycle session");
  assert.equal(eventMetadata.mcp_lifecycle_pid, lifecyclePid, "local MCP lifecycle pid missing from event metadata");

  let lastMessages = [];
  const completed = await waitForJson(
    "packaged local MCP lifecycle tool message",
    async () => {
      const { payload } = await bridgeJson(port, token, "GET", `/api/sessions/${sessionId}/messages?limit=100`);
      const messages = Array.isArray(payload.messages_v2) ? payload.messages_v2 : [];
      lastMessages = messages;
      const mcpParts = mcpToolPartsFromMessages(messages);
      const part = mcpParts.find(
        (item) =>
          toolPartCallId(item) === "call_packaged_local_mcp" ||
          toolPartName(item) === "mcp_tool_local_tools_stdio_echo",
      );
      if (!part) return null;
      const metadata = partMetadata(part);
      const output = toolPartOutput(part);
      if (output.includes("packaged stdio echo: packaged-lifecycle")) {
        return {
          messages,
          part,
          metadata: Object.keys(metadata).length > 0 ? metadata : eventMetadata,
          output,
        };
      }
      return null;
    },
  ).catch((error) => {
    throw new Error(
      `${error.message}; messages=${JSON.stringify(messageDebugSummary(lastMessages))}; events=${JSON.stringify(
        (turn.payload.events || []).slice(-4),
      )}`,
    );
  });

  const status = await bridgeJson(port, token, "GET", "/api/mcp");
  const statusServer = status.payload.servers?.find((item) => item.name === "local-tools");
  assert.equal(statusServer?.lifecycle_status, "running", "local MCP lifecycle stopped unexpectedly");
  assert.equal(statusServer?.lifecycle_pid, lifecyclePid, "local MCP lifecycle pid changed unexpectedly");

  const stopped = await bridgeJson(port, token, "POST", "/api/mcp/servers/local-tools/stop", {});
  const stoppedServer = stopped.payload.servers?.find((item) => item.name === "local-tools");
  assert.equal(stoppedServer?.lifecycle_status, "stopped", "local MCP lifecycle did not stop cleanly");

  const requestLog = readLocalMcpRequestLog(localMcp.requestLog);
  const pids = [...new Set(requestLog.map((item) => item.pid))];
  assert.deepEqual(pids, [String(lifecyclePid)], `expected one stdio process, got ${JSON.stringify(requestLog)}`);
  const methods = requestLog.map((item) => item.method);
  assert.equal(
    methods.filter((method) => method === "initialize").length,
    1,
    `expected one initialize, got ${JSON.stringify(requestLog)}`,
  );
  assert.equal(
    methods.filter((method) => method === "tools/call").length,
    1,
    `expected one tools/call, got ${JSON.stringify(requestLog)}`,
  );
  assert.ok(
    methods.filter((method) => method === "tools/list").length >= 2,
    `expected lifecycle start and runtime refresh tools/list, got ${JSON.stringify(requestLog)}`,
  );

  const serialized = JSON.stringify({ started: started.payload, completed, status: status.payload });
  assert.equal(serialized.includes("packaged-local-mcp-secret"), false, "MCP secret leaked into API payload");
  assert.equal(serialized.includes("LOCAL_SECRET"), false, "MCP env key leaked into API payload");

  return {
    session_id: sessionId,
    turn_id: turn.payload.turn_id,
    lifecycle_pid: lifecyclePid,
    lifecycle_reused: completed.metadata.mcp_lifecycle_reused,
    output: completed.output,
    request_methods: methods,
    request_pid_count: pids.length,
    mcp_config_path: localMcp.mcpConfigPath,
    server_script: localMcp.serverScript,
  };
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.launch === "launchservices" && process.platform !== "darwin") {
    throw new Error("LaunchServices smoke is only available on macOS");
  }
  const macosDir = path.join(options.appPath, "Contents", "MacOS");
  const resourcesDir = path.join(options.appPath, "Contents", "Resources");
  const appBinary = fs
    .readdirSync(macosDir)
    .map((entry) => path.join(macosDir, entry))
    .find((candidate) => fs.statSync(candidate).isFile());
  const bundledBridgeBinary = path.join(resourcesDir, "openagent-http-runtime");
  assert.ok(appBinary, `Packaged app binary not found in ${macosDir}`);
  assert.ok(fs.existsSync(bundledBridgeBinary), `Bundled bridge binary not found: ${bundledBridgeBinary}`);

  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "openagent-packaged-app-"));
  const workspace = path.join(tempRoot, "workspace");
  const sessionRoot = path.join(tempRoot, "sessions");
  const home = path.join(tempRoot, "home");
  const tokenPath = path.join(tempRoot, "bridge-auth-token");
  fs.mkdirSync(workspace, { recursive: true });
  fs.mkdirSync(sessionRoot, { recursive: true });
  fs.mkdirSync(home, { recursive: true });
  const port = await freePort();
  let child;
  let screenshotPath = "";
  let workflowProvider = null;
  let workflowResult = null;
  let localMcpWorkflow = null;
  let providerEnv = null;
  let providerSummary = null;

  try {
    if (options.workflow === "approval-rollback") {
      workflowProvider = await startWorkflowProvider();
    } else if (options.workflow === "approval-dock") {
      workflowProvider = await startApprovalDockWorkflowProvider();
    }
    if (options.workflow === "local-mcp-lifecycle") {
      localMcpWorkflow = prepareLocalMcpLifecycleWorkflow(workspace, tempRoot);
    }
    providerEnv = {
      ...(options.workflow === "local-mcp-lifecycle" ? {} : providerEnvFromLocalConfig(options)),
      ...(workflowProvider?.env ?? {}),
    };
    providerSummary = providerSummaryFromEnv(providerEnv);
    const env = { ...process.env };
    delete env.OPENAGENT_HTTP_RUNTIME;
    delete env.OPENAGENT_APP_BRIDGE_URL;
    const appEnv = {
      ...providerEnv,
      OPENAGENT_HOME: path.join(tempRoot, "openagent-home"),
      OPENAGENT_DESKTOP_DISABLE_DEV_RUNTIME_FALLBACK: "1",
      OPENAGENT_WORKSPACE: workspace,
      OPENAGENT_SESSION_ROOT: sessionRoot,
      OPENAGENT_DESKTOP_AUTH_TOKEN_PATH: tokenPath,
      OPENAGENT_BRIDGE_PORT: String(port),
      OPENAGENT_BRIDGE_URL: `http://127.0.0.1:${port}`,
      OPENAGENT_APP_BRIDGE_URL: "",
      OPENAGENT_HTTP_RUNTIME: "",
    };

    if (options.launch === "launchservices") {
      await withLaunchServicesEnv(appEnv, async () => {
        await runCommand("open", ["-n", options.appPath]);
      });
    } else {
      child = spawn(appBinary, [], {
        cwd: workspace,
        stdio: ["ignore", "ignore", "ignore"],
        env: {
          ...env,
          HOME: home,
          ...appEnv,
        },
      });
    }

    const token = await waitForFile(tokenPath);
    assert.match(token, /^oa_desktop_/);
    const health = await waitForHealth(port, token);
    assert.equal(health.ok, true);
    if (options.workflow === "approval-rollback") {
      workflowResult = await runApprovalRollbackWorkflow(
        port,
        token,
        workspace,
        options.appPath,
        siblingScreenshotPath(options.screenshot, "restored"),
      );
    } else if (options.workflow === "approval-dock") {
      workflowResult = await runApprovalDockWorkflow(
        port,
        token,
        workspace,
        options.appPath,
        siblingScreenshotPath(options.screenshot, "pending"),
      );
    } else if (options.workflow === "real-streaming") {
      workflowResult = await runRealStreamingWorkflow(port, token, workspace, providerSummary);
    } else if (options.workflow === "local-mcp-lifecycle") {
      workflowResult = await runLocalMcpLifecycleWorkflow(port, token, workspace, localMcpWorkflow);
    }
    screenshotPath = await captureScreenshot(options.screenshot, options.appPath) || "";

    console.log(
      JSON.stringify(
        {
          ok: true,
          launch: options.launch,
          app: options.appPath,
          app_pid: child?.pid ?? null,
          bridge_url: `http://127.0.0.1:${port}`,
          bridge_health: health,
          bundled_bridge: bundledBridgeBinary,
          screenshot: screenshotPath || null,
          workflow: workflowResult,
          provider_config: options.workflow === "real-streaming" ? providerSummary : null,
          workflow_provider: workflowProvider
            ? {
                base_url: `http://127.0.0.1:${workflowProvider.port}/v1`,
                model: providerSummary.model,
                responses_requests: workflowProvider.requests.filter((item) => item.url === "/v1/responses").length,
                models_requests: workflowProvider.requests.filter((item) => item.url === "/v1/models").length,
              }
            : null,
          workspace,
          session_root: sessionRoot,
          token: `set(len=${token.length})`,
        },
        null,
        2,
      ),
    );
  } finally {
    await stopProcess(child);
    if (options.launch === "launchservices") {
      await stopLaunchServicesApp(options.appPath, port);
    }
    if (workflowProvider) {
      await workflowProvider.close();
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
});
