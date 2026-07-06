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
const token = "desktop-approval-dock-smoke-token";

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

function questionCall(callId) {
  return {
    type: "function_call",
    call_id: callId,
    name: "question",
    arguments: JSON.stringify({
      questions: [
        {
          header: "Confirm",
          question: "Proceed with the approval dock smoke?",
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

function startFakeResponsesProvider(port) {
  const requests = [];
  const responses = [
    {
      id: "resp_question_reply",
      output: [questionCall("call_question_reply")],
      usage: { input_tokens: 4, output_tokens: 1 },
    },
    {
      id: "resp_question_reply_final",
      output_text: "continuing after yes",
      usage: { input_tokens: 7, output_tokens: 3 },
    },
    {
      id: "resp_question_dismiss",
      output: [questionCall("call_question_dismiss")],
      usage: { input_tokens: 4, output_tokens: 1 },
    },
  ];
  let responseIndex = 0;
  const server = http.createServer((request, response) => {
    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ object: "list", data: [{ id: "fake-approval-dock-model", object: "model" }] }));
      return;
    }
    if (request.method !== "POST" || request.url !== "/v1/responses") {
      response.writeHead(404, { "content-type": "application/json" });
      response.end(JSON.stringify({ error: { message: "not found" } }));
      return;
    }
    let body = "";
    request.on("data", (chunk) => {
      body += chunk.toString();
    });
    request.on("end", () => {
      requests.push(body);
      const payload = responses[responseIndex++] ?? {
        id: `resp_extra_${responseIndex}`,
        output_text: "extra response",
        usage: { input_tokens: 1, output_tokens: 1 },
      };
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify(payload));
    });
  });
  return new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(port, "127.0.0.1", () => resolve({ server, requests }));
  });
}

async function waitForPageState(page, predicate, timeoutMs = 15_000) {
  await page.waitForFunction(predicate, undefined, { timeout: timeoutMs });
}

async function clickFirstDockButton(page, testId, label) {
  const item = page.locator(`[data-testid="${testId}"]`).first();
  await item.waitFor({ state: "visible", timeout: 15_000 });
  await item.getByRole("button", { name: label, exact: true }).click();
}

async function refreshDesktop(page) {
  await page.reload({ waitUntil: "domcontentloaded" });
  await page.locator(".composer").waitFor({ state: "visible", timeout: 15_000 });
  const sessionRow = page.locator(".session-row").filter({ hasText: "Desktop approval dock smoke" }).first();
  await sessionRow.waitFor({ state: "visible", timeout: 15_000 });
  await sessionRow.click();
}

async function waitForNoPendingApprovals(port) {
  const deadline = Date.now() + 15_000;
  while (Date.now() < deadline) {
    const payload = await bridgeJson(port, "GET", "/api/approvals");
    if ((payload.count ?? 0) === 0) return;
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error("Timed out waiting for approval queue to clear");
}

async function main() {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "openagent-desktop-approval-dock-"));
  const workspace = path.join(tempRoot, "workspace");
  const sessionRoot = path.join(tempRoot, "sessions");
  fs.mkdirSync(workspace, { recursive: true });
  fs.mkdirSync(sessionRoot, { recursive: true });

  let runtime;
  let vite;
  let browser;
  let page;
  let provider;

  try {
    const runtimePort = await freePort();
    const vitePort = await freePort();
    const providerPort = await freePort();
    provider = await startFakeResponsesProvider(providerPort);

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
      {
        cwd: repoRoot,
        env: {
          ...process.env,
          OPENAI_API_KEY: "test-key",
          OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
          OPENAI_WIRE_API: "responses",
          OPENAI_MODEL: "fake-approval-dock-model",
        },
      },
    );

    await waitForHttp(`http://127.0.0.1:${runtimePort}/api/health`, {
      headers: { authorization: `Bearer ${token}` },
      timeoutMs: 90_000,
    });

    const created = await bridgeJson(runtimePort, "POST", "/api/sessions", {
      cwd: workspace,
      title: "Desktop approval dock smoke",
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
    let phase = "launch";
    const launchOptions = { headless: true, args: ["--disable-dev-shm-usage"] };
    if (fs.existsSync(chromePath)) launchOptions.executablePath = chromePath;
    browser = await chromium.launch(launchOptions);
    page = await browser.newPage({ viewport: { width: 1440, height: 920 } });
    page.on("console", (message) => {
      if (["error", "warning"].includes(message.type())) consoleIssues.push(`[${phase}] ${message.text()}`);
    });
    page.on("pageerror", (error) => pageErrors.push(error.message));
    await page.addInitScript(
      ({ bridgeUrl, tokenValue, workspacePath }) => {
        const project = {
          id: workspacePath,
          name: "approval-dock-smoke",
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
    phase = "initial-load";
    await page.locator(".composer").waitFor({ state: "visible", timeout: 15_000 });
    await page
      .locator(".session-row.selected")
      .filter({ hasText: "Desktop approval dock smoke" })
      .waitFor({ state: "visible", timeout: 15_000 });

    const allowStarted = await bridgeJson(runtimePort, "POST", `/api/sessions/${sessionId}/turns`, {
      input: "Approval dock allow path",
      permission: "PLAN_ONLY",
      tool_call: {
        call_id: "call_dock_allow",
        name: "write",
        input: { file_path: "dock-allow.txt", content: "allowed\n" },
      },
    });
    assert.equal(allowStarted.status, "waiting_approval", `allow approval did not pause: ${JSON.stringify(allowStarted)}`);
    phase = "allow";
    await refreshDesktop(page);
    await waitForPageState(page, () => {
      const item = document.querySelector('[data-testid="approval-dock-approval"]');
      const text = item?.textContent || "";
      return (
        item?.getAttribute("data-approval-risk") === "write" &&
        text.includes("File write") &&
        text.includes("Needs approval") &&
        text.includes("Permission: confirm before running") &&
        text.includes("Risk: can edit files") &&
        text.includes("dock-allow.txt")
      );
    });
    await clickFirstDockButton(page, "approval-dock-approval", "Allow");
    await waitForPageState(page, () => {
      const text = document.body.textContent || "";
      return text.includes("allowed") && text.includes("dock-allow.txt");
    });
    await waitForNoPendingApprovals(runtimePort);
    await waitForPageState(page, () => !document.querySelector('[data-testid="approval-dock-approval"]'));

    const denyStarted = await bridgeJson(runtimePort, "POST", `/api/sessions/${sessionId}/turns`, {
      input: "Approval dock deny path",
      permission: "PLAN_ONLY",
      tool_call: {
        call_id: "call_dock_deny",
        name: "write",
        input: { file_path: "dock-deny.txt", content: "denied\n" },
      },
    });
    assert.equal(denyStarted.status, "waiting_approval", `deny approval did not pause: ${JSON.stringify(denyStarted)}`);
    phase = "deny";
    await refreshDesktop(page);
    try {
      await waitForPageState(page, () => {
        const text = document.querySelector('[data-testid="approval-dock"]')?.textContent || "";
        return text.includes("dock-deny.txt") && text.includes("Permission: confirm before running") && text.includes("Risk: can edit files");
      });
    } catch (error) {
      const pending = await bridgeJson(runtimePort, "GET", "/api/approvals");
      const debug = await page.evaluate(() => ({
        selected: document.querySelector(".session-row.selected")?.textContent || "",
        dock: document.querySelector('[data-testid="approval-dock"]')?.textContent || "",
        body: (document.body.textContent || "").slice(0, 3000),
      }));
      throw new Error(`${error instanceof Error ? error.message : String(error)}; pending=${JSON.stringify(pending)}; page=${JSON.stringify(debug)}`);
    }
    await clickFirstDockButton(page, "approval-dock-approval", "Deny");
    await waitForPageState(page, () => (document.body.textContent || "").includes("denied"));
    await waitForNoPendingApprovals(runtimePort);
    await waitForPageState(page, () => !document.querySelector('[data-testid="approval-dock-approval"]'));

    const questionReplyStarted = await bridgeJson(runtimePort, "POST", `/api/sessions/${sessionId}/turns`, {
      input: "Ask the approval dock question reply path.",
      permission: "FULL",
    });
    assert.equal(questionReplyStarted.status, "waiting_question", `question reply did not pause: ${JSON.stringify(questionReplyStarted)}`);
    phase = "question-reply";
    await refreshDesktop(page);
    await waitForPageState(page, () => {
      const text = document.querySelector('[data-testid="approval-dock-question"]')?.textContent || "";
      return text.includes("Confirm") && text.includes("Needs answer") && text.includes("Question: reply to resume") && text.includes("Reply") && text.includes("Dismiss");
    });
    await clickFirstDockButton(page, "approval-dock-question", "Reply");
    await waitForPageState(page, () => (document.body.textContent || "").includes("continuing after yes"));

    const questionDismissStarted = await bridgeJson(runtimePort, "POST", `/api/sessions/${sessionId}/turns`, {
      input: "Ask the approval dock question dismiss path.",
      permission: "FULL",
    });
    assert.equal(questionDismissStarted.status, "waiting_question", `question dismiss did not pause: ${JSON.stringify(questionDismissStarted)}`);
    phase = "question-dismiss";
    await refreshDesktop(page);
    await waitForPageState(page, () => {
      const text = document.querySelector('[data-testid="approval-dock-question"]')?.textContent || "";
      return text.includes("Confirm") && text.includes("Dismiss");
    });
    await clickFirstDockButton(page, "approval-dock-question", "Dismiss");
    await waitForPageState(page, () => (document.body.textContent || "").includes("dismissed"));

    phase = "final-reload";
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.locator(".composer").waitFor({ state: "visible", timeout: 15_000 });
    try {
      await waitForPageState(page, () => {
        const text = document.body.textContent || "";
        const history = [...document.querySelectorAll('[data-testid="trust-history-item"]')].map((item) => ({
          kind: item.getAttribute("data-part-kind") || "",
          status: item.getAttribute("data-interaction-status") || "",
          flowState: item.getAttribute("data-flow-state") || "",
          flowText: item.querySelector('[data-testid="trust-history-flow"]')?.textContent || "",
          callId: item.getAttribute("data-call-id") || "",
          text: item.textContent || "",
        }));
        const hasAllowed = history.some(
          (item) =>
            item.kind === "approval" &&
            item.status.includes("allow") &&
            item.flowState === "ok" &&
            item.flowText.includes("Requested") &&
            item.flowText.includes("Allowed") &&
            item.callId === "call_dock_allow" &&
            item.text.includes("dock-allow.txt"),
        );
        const hasDenied = history.some(
          (item) =>
            item.kind === "approval" &&
            (item.status === "deny" || item.status === "denied") &&
            item.flowState === "blocked" &&
            item.flowText.includes("Requested") &&
            item.flowText.includes("Denied") &&
            item.callId === "call_dock_deny" &&
            item.text.includes("dock-deny.txt"),
        );
        const hasAnswered = history.some(
          (item) =>
            item.kind === "question" &&
            item.status === "answered" &&
            item.flowState === "ok" &&
            item.flowText.includes("Asked") &&
            item.flowText.includes("Answered") &&
            item.callId === "call_question_reply",
        );
        const hasDismissed = history.some(
          (item) =>
            item.kind === "question" &&
            item.status === "dismissed" &&
            item.flowState === "blocked" &&
            item.flowText.includes("Asked") &&
            item.flowText.includes("Dismissed") &&
            item.callId === "call_question_dismiss",
        );
        return (
          hasAllowed &&
          hasDenied &&
          hasAnswered &&
          hasDismissed &&
          text.includes("allowed") &&
          text.includes("denied") &&
          text.includes("continuing after yes") &&
          text.includes("dismissed") &&
          !document.querySelector('[data-testid="approval-dock-approval"]') &&
          !document.querySelector('[data-testid="approval-dock-question"]')
        );
      });
    } catch (error) {
      const debug = await page.evaluate(() => ({
        body: (document.body.textContent || "").slice(0, 5000),
        trustHistory: [...document.querySelectorAll('[data-testid="trust-history-item"]')].map((item) => ({
          kind: item.getAttribute("data-part-kind") || "",
          status: item.getAttribute("data-interaction-status") || "",
          flowState: item.getAttribute("data-flow-state") || "",
          flowText: item.querySelector('[data-testid="trust-history-flow"]')?.textContent || "",
          requestId: item.getAttribute("data-request-id") || "",
          callId: item.getAttribute("data-call-id") || "",
          text: item.textContent || "",
        })),
        pendingApproval: document.querySelector('[data-testid="approval-dock-approval"]')?.textContent || "",
        pendingQuestion: document.querySelector('[data-testid="approval-dock-question"]')?.textContent || "",
      }));
      throw new Error(`${error instanceof Error ? error.message : String(error)}; reloadDebug=${JSON.stringify(debug)}`);
    }

    const pageState = await page.evaluate(() => ({
      overlayVisible: Boolean(document.querySelector("vite-error-overlay")),
      bodyOverflow: Math.max(0, document.body.scrollWidth - window.innerWidth),
      trustHistory: [...document.querySelectorAll('[data-testid="trust-history-item"]')].map((item) => ({
        kind: item.getAttribute("data-part-kind") || "",
        status: item.getAttribute("data-interaction-status") || "",
        flowState: item.getAttribute("data-flow-state") || "",
        flowText: item.querySelector('[data-testid="trust-history-flow"]')?.textContent || "",
        requestId: item.getAttribute("data-request-id") || "",
        callId: item.getAttribute("data-call-id") || "",
        text: item.textContent || "",
      })),
    }));
    assert.ok(
      pageState.trustHistory.some(
        (item) =>
          item.kind === "approval" &&
          item.status.includes("allow") &&
          item.flowState === "ok" &&
          item.flowText.includes("Requested") &&
          item.flowText.includes("Allowed") &&
          item.callId === "call_dock_allow" &&
          item.text.includes("dock-allow.txt"),
      ),
      `missing persisted allowed approval history: ${JSON.stringify(pageState.trustHistory)}`,
    );
    assert.ok(
      pageState.trustHistory.some(
        (item) =>
          item.kind === "approval" &&
          (item.status === "deny" || item.status === "denied") &&
          item.flowState === "blocked" &&
          item.flowText.includes("Requested") &&
          item.flowText.includes("Denied") &&
          item.callId === "call_dock_deny" &&
          item.text.includes("dock-deny.txt"),
      ),
      `missing persisted denied approval history: ${JSON.stringify(pageState.trustHistory)}`,
    );
    assert.ok(
      pageState.trustHistory.some(
        (item) =>
          item.kind === "question" &&
          item.status === "answered" &&
          item.flowState === "ok" &&
          item.flowText.includes("Asked") &&
          item.flowText.includes("Answered") &&
          item.callId === "call_question_reply",
      ),
      `missing persisted answered question history: ${JSON.stringify(pageState.trustHistory)}`,
    );
    assert.ok(
      pageState.trustHistory.some(
        (item) =>
          item.kind === "question" &&
          item.status === "dismissed" &&
          item.flowState === "blocked" &&
          item.flowText.includes("Asked") &&
          item.flowText.includes("Dismissed") &&
          item.callId === "call_question_dismiss",
      ),
      `missing persisted dismissed question history: ${JSON.stringify(pageState.trustHistory)}`,
    );
    assert.equal(pageState.overlayVisible, false, "Vite error overlay is visible");
    assert.equal(pageState.bodyOverflow, 0, `page has horizontal overflow: ${pageState.bodyOverflow}`);
    assert.equal(pageErrors.length, 0, `page errors: ${pageErrors.join("\n")}`);
    assert.equal(consoleIssues.length, 0, `console issues: ${consoleIssues.join("\n")}`);
    assert.equal(fs.readFileSync(path.join(workspace, "dock-allow.txt"), "utf8"), "allowed\n");
    assert.equal(fs.existsSync(path.join(workspace, "dock-deny.txt")), false, "denied approval wrote a file");
    assert.equal(provider.requests.length, 3, `provider request count mismatch: ${provider.requests.length}`);

    const screenshotPath = path.join(os.tmpdir(), `openagent-desktop-approval-dock-${Date.now()}.png`);
    await page.screenshot({ path: screenshotPath, fullPage: false });
    console.log(
      JSON.stringify(
        {
          ok: true,
          bridge_url: `http://127.0.0.1:${runtimePort}`,
          session_id: sessionId,
          provider_request_count: provider.requests.length,
          screenshot: screenshotPath,
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
    if (provider?.server) {
      await new Promise((resolve) => provider.server.close(resolve)).catch(() => {});
    }
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
