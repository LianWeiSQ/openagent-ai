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
const token = "desktop-checkpoint-restore-smoke-token";

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

async function waitForJson(label, producer, timeoutMs = 15_000) {
  const deadline = Date.now() + timeoutMs;
  let lastError = "";
  while (Date.now() < deadline) {
    try {
      const value = await producer();
      if (value) return value;
    } catch (error) {
      lastError = error instanceof Error ? error.message : String(error);
    }
    await new Promise((resolve) => setTimeout(resolve, 250));
  }
  throw new Error(`Timed out waiting for ${label}${lastError ? `: ${lastError}` : ""}`);
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

function startModelsProvider(port) {
  const server = http.createServer((request, response) => {
    if (request.method === "GET" && request.url === "/v1/models") {
      response.writeHead(200, { "content-type": "application/json" });
      response.end(JSON.stringify({ object: "list", data: [{ id: "fake-checkpoint-model", object: "model" }] }));
      return;
    }
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ id: "unused", output_text: "unused" }));
  });
  return new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(port, "127.0.0.1", () => resolve(server));
  });
}

async function waitForPageState(page, predicate, arg, timeoutMs = 15_000) {
  await page.waitForFunction(predicate, arg, { timeout: timeoutMs });
}

async function selectSmokeSession(page) {
  await page.locator(".composer").waitFor({ state: "visible", timeout: 15_000 });
  const sessionRow = page.locator(".session-row").filter({ hasText: "Desktop checkpoint restore smoke" }).first();
  await sessionRow.waitFor({ state: "visible", timeout: 15_000 });
  await sessionRow.click();
}

async function main() {
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "openagent-desktop-checkpoint-restore-"));
  const workspace = path.join(tempRoot, "workspace");
  const sessionRoot = path.join(tempRoot, "sessions");
  fs.mkdirSync(workspace, { recursive: true });
  fs.mkdirSync(sessionRoot, { recursive: true });

  let runtime;
  let vite;
  let browser;
  let page;
  let provider;
  let phase = "launch";

  try {
    const runtimePort = await freePort();
    const vitePort = await freePort();
    const providerPort = await freePort();
    provider = await startModelsProvider(providerPort);

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
          OPENAI_MODEL: "fake-checkpoint-model",
        },
      },
    );

    await waitForHttp(`http://127.0.0.1:${runtimePort}/api/health`, {
      headers: { authorization: `Bearer ${token}` },
      timeoutMs: 90_000,
    });

    const created = await bridgeJson(runtimePort, "POST", "/api/sessions", {
      cwd: workspace,
      title: "Desktop checkpoint restore smoke",
    });
    const sessionId = created.session_id || created.id || created.session?.id;
    assert.ok(sessionId, "session id missing");

    const writeFile = path.join(workspace, "checkpoint-ui.txt");
    const turn = await bridgeJson(runtimePort, "POST", `/api/sessions/${sessionId}/turns`, {
      input: "Create checkpoint-ui.txt so the Desktop review panel can restore it.",
      permission: "FULL",
      tool_call: {
        call_id: "call_checkpoint_restore_ui",
        name: "write",
        input: { file_path: "checkpoint-ui.txt", content: "checkpoint restore ui smoke\n" },
      },
    });
    assert.ok(["completed", "running"].includes(turn.status), `write turn did not run: ${JSON.stringify(turn)}`);

    await waitForJson("checkpoint file write", () => {
      if (!fs.existsSync(writeFile)) return null;
      const content = fs.readFileSync(writeFile, "utf8");
      return content.includes("checkpoint restore ui smoke") ? { content } : null;
    });

    const diff = await bridgeJson(runtimePort, "GET", `/api/sessions/${sessionId}/diff`);
    assert.ok(JSON.stringify(diff).includes("checkpoint-ui.txt"), `diff did not include checkpoint-ui.txt: ${JSON.stringify(diff)}`);
    const checkpoints = await bridgeJson(runtimePort, "GET", `/api/sessions/${sessionId}/checkpoints`);
    const checkpointList = Array.isArray(checkpoints.checkpoints) ? checkpoints.checkpoints : [];
    assert.ok(checkpointList.length > 0, "expected checkpoints");
    const restoreTarget =
      checkpointList.find((checkpoint) => checkpoint.kind === "step_start") || checkpointList[checkpointList.length - 1];
    const checkpointId = restoreTarget?.checkpoint_id;
    assert.ok(checkpointId, "checkpoint id missing");

    vite = spawnLogged("npm", ["run", "dev", "--", "--port", String(vitePort), "--strictPort"], {
      cwd: desktopDir,
      env: process.env,
    });
    await waitForHttp(`http://127.0.0.1:${vitePort}/`, { timeoutMs: 60_000 });

    const consoleIssues = [];
    const pageErrors = [];
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
          name: "checkpoint-restore-smoke",
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
    await selectSmokeSession(page);
    await page.locator('[data-testid="diff-dock-item"]').filter({ hasText: "checkpoint-ui.txt" }).waitFor({
      state: "visible",
      timeout: 15_000,
    });
    await page.locator('[data-testid="checkpoint-dock-item"]').waitFor({ state: "visible", timeout: 15_000 });

    phase = "review-restore";
    await page.locator('[data-testid="diff-dock-item"]').filter({ hasText: "checkpoint-ui.txt" }).click();
    await page.locator('[data-testid="review-panel"]').waitFor({ state: "visible", timeout: 15_000 });
    await page.locator('[data-testid="change-review-card"]').filter({ hasText: "checkpoint-ui.txt" }).waitFor({
      state: "visible",
      timeout: 15_000,
    });
    await page.locator(`[data-testid="review-checkpoint-restore"][data-checkpoint-id="${checkpointId}"]`).click();
    await waitForJson("checkpoint restore file removal", () => (!fs.existsSync(writeFile) ? { exists: false } : null));
    await waitForPageState(
      page,
      (id) =>
        Boolean(document.querySelector(`[data-testid="checkpoint-restore-state"][data-checkpoint-id="${id}"]`)) &&
        Boolean(document.querySelector(`[data-testid="review-checkpoint-row"][data-checkpoint-id="${id}"][data-checkpoint-restored="true"]`)) &&
        Boolean(document.querySelector(`[data-testid="checkpoint-restore-history"][data-checkpoint-id="${id}"]`)),
      checkpointId,
    );

    phase = "reload";
    await page.reload({ waitUntil: "domcontentloaded" });
    await selectSmokeSession(page);
    await page.locator(`[data-testid="checkpoint-dock-item"][data-restored-checkpoint-id="${checkpointId}"]`).waitFor({
      state: "visible",
      timeout: 15_000,
    });
    await page.locator('[data-testid="checkpoint-dock-item"]').click();
    await page.locator(`[data-testid="checkpoint-restore-state"][data-checkpoint-id="${checkpointId}"]`).waitFor({
      state: "visible",
      timeout: 15_000,
    });
    await page.locator(`[data-testid="review-checkpoint-row"][data-checkpoint-id="${checkpointId}"][data-checkpoint-restored="true"]`).waitFor({
      state: "visible",
      timeout: 15_000,
    });
    await page.locator(`[data-testid="checkpoint-restore-history"][data-checkpoint-id="${checkpointId}"]`).filter({
      hasText: "Workspace restored",
    }).first().waitFor({
      state: "visible",
      timeout: 15_000,
    });

    const pageState = await page.evaluate((id) => ({
      overlayVisible: Boolean(document.querySelector("vite-error-overlay")),
      bodyOverflow: Math.max(0, document.body.scrollWidth - window.innerWidth),
      dockText: document.querySelector('[data-testid="checkpoint-dock-item"]')?.textContent || "",
      restoreText: document.querySelector(`[data-testid="checkpoint-restore-state"][data-checkpoint-id="${id}"]`)?.textContent || "",
      restoreHistoryText: [...document.querySelectorAll(`[data-testid="checkpoint-restore-history"][data-checkpoint-id="${id}"]`)]
        .map((item) => item.textContent || "")
        .join("\n"),
      restoredRows: document.querySelectorAll(`[data-testid="review-checkpoint-row"][data-checkpoint-restored="true"]`).length,
    }), checkpointId);
    assert.equal(pageState.overlayVisible, false, "Vite overlay is visible");
    assert.equal(pageState.bodyOverflow, 0, `Horizontal overflow detected: ${pageState.bodyOverflow}`);
    assert.ok(pageState.dockText.includes("Restored"), `dock did not show restored state: ${pageState.dockText}`);
    assert.ok(pageState.restoreText.includes("Restored"), `review panel did not show restored state: ${pageState.restoreText}`);
    assert.ok(
      pageState.restoreHistoryText.includes("Workspace restored") &&
        pageState.restoreHistoryText.includes("Restore run") &&
        pageState.restoreHistoryText.includes("Files"),
      `restore history did not show detail: ${pageState.restoreHistoryText}`,
    );
    assert.ok(pageState.restoredRows >= 1, `expected restored checkpoint row: ${JSON.stringify(pageState)}`);
    assert.deepEqual(pageErrors, [], `page errors: ${pageErrors.join("\n")}`);
    assert.deepEqual(consoleIssues, [], `console issues: ${consoleIssues.join("\n")}`);

    const screenshotPath = path.join(os.tmpdir(), `openagent-desktop-checkpoint-restore-${Date.now()}.png`);
    await page.screenshot({ path: screenshotPath, fullPage: true });

    console.log(
      JSON.stringify(
        {
          ok: true,
          bridge_url: `http://127.0.0.1:${runtimePort}`,
          session_id: sessionId,
          restore_checkpoint_id: checkpointId,
          file_after_restore_exists: fs.existsSync(writeFile),
          screenshot: screenshotPath,
        },
        null,
        2,
      ),
    );
  } catch (error) {
    const diagnostics = {
      phase,
      runtime: runtime?.outputText?.(),
      vite: vite?.outputText?.(),
      page: page
        ? await page.evaluate(() => ({
            body: (document.body.textContent || "").slice(0, 4000),
            restoreState: document.querySelector('[data-testid="checkpoint-restore-state"]')?.outerHTML || "",
            dock: document.querySelector('[data-testid="checkpoint-dock-item"]')?.outerHTML || "",
          })).catch((innerError) => ({ error: innerError instanceof Error ? innerError.message : String(innerError) }))
        : null,
    };
    console.error(JSON.stringify(diagnostics, null, 2));
    throw error;
  } finally {
    if (browser) await browser.close().catch(() => {});
    await stopChild(vite);
    await stopChild(runtime);
    if (provider) await new Promise((resolve) => provider.close(resolve));
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : String(error));
  process.exit(1);
});
