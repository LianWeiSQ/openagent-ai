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
const token = "desktop-stream-smoke-token";
const DEFAULT_REAL_MODEL = "gpt-5.4-mini";

function parseArgs(argv) {
  const options = {
    envFile: path.join(repoRoot, ".openagent", "openagent.env"),
    provider: "fake",
  };
  for (const arg of argv) {
    if (arg === "--real-provider" || arg === "--provider=real") {
      options.provider = "real";
    } else if (arg === "--fake-provider" || arg === "--provider=fake") {
      options.provider = "fake";
    } else if (arg.startsWith("--model=")) {
      options.model = arg.slice("--model=".length);
    } else if (arg.startsWith("--base-url=")) {
      options.baseUrl = arg.slice("--base-url=".length);
    } else if (arg.startsWith("--env-file=")) {
      options.envFile = path.resolve(arg.slice("--env-file=".length));
    }
  }
  return options;
}

function parseEnvFile(filePath) {
  if (!fs.existsSync(filePath)) return {};
  const values = {};
  for (const rawLine of fs.readFileSync(filePath, "utf8").split(/\r?\n/)) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#") || !line.includes("=")) continue;
    const index = line.indexOf("=");
    const key = line.slice(0, index).trim();
    let value = line.slice(index + 1).trim();
    if ((value.startsWith('"') && value.endsWith('"')) || (value.startsWith("'") && value.endsWith("'"))) {
      value = value.slice(1, -1);
    }
    values[key] = value;
  }
  return values;
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

function listen(server) {
  return new Promise((resolve, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      assert.equal(typeof address, "object");
      resolve(address.port);
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
        // The process may have exited between the timeout and kill.
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

function fakeProviderEnv(providerPort) {
  return {
    env: {
      OPENAI_API_KEY: "test-key",
      OPENAI_BASE_URL: `http://127.0.0.1:${providerPort}/v1`,
      OPENAI_MODEL: "fake-stream",
      OPENAI_WIRE_API: "responses",
      OPENAGENT_PROVIDER_STREAM: "1",
    },
    summary: {
      base_url: `http://127.0.0.1:${providerPort}/v1`,
      model: "fake-stream",
      wire_api: "responses",
      api_key: "set(fake)",
    },
  };
}

function realProviderEnv(options) {
  const envFileValues = parseEnvFile(options.envFile);
  const merged = { ...envFileValues, ...process.env };
  const apiKey = merged.OPENAI_API_KEY;
  const baseUrl = options.baseUrl || merged.OPENAI_BASE_URL || "http://47.116.192.3/v1";
  const model = options.model || DEFAULT_REAL_MODEL;
  const wireApi = merged.OPENAI_WIRE_API || "responses";

  if (!apiKey) throw new Error(`OPENAI_API_KEY is missing; checked env and ${options.envFile}`);

  return {
    env: {
      OPENAI_API_KEY: apiKey,
      OPENAI_BASE_URL: baseUrl,
      OPENAI_MODEL: model,
      OPENAI_WIRE_API: wireApi,
      OPENAGENT_PROVIDER_STREAM: "1",
    },
    summary: {
      base_url: baseUrl,
      model,
      wire_api: wireApi,
      api_key: `set(len=${apiKey.length})`,
    },
  };
}

function promptForMode(providerMode) {
  if (providerMode === "fake") return "stream provider response through the real app bridge";
  return [
    "For an automated streaming smoke test, write exactly twelve short lines.",
    "The first line must contain OA_REAL_STREAM_BEGIN.",
    "The last line must contain OA_REAL_STREAM_END.",
    "Do not use a code block.",
  ].join(" ");
}

function finalNeedleForMode(providerMode) {
  return providerMode === "fake" ? "streamed answer" : "OA_REAL_STREAM_END";
}

async function persistedAssistantSummary(runtimePort) {
  const sessionsResponse = await fetch(`http://127.0.0.1:${runtimePort}/api/sessions`, {
    headers: { authorization: `Bearer ${token}` },
  });
  if (!sessionsResponse.ok) throw new Error(`GET /api/sessions ${sessionsResponse.status}`);
  const sessionsPayload = await sessionsResponse.json();
  const sessions = Array.isArray(sessionsPayload.sessions) ? sessionsPayload.sessions : [];
  const session = sessions[0];
  const sessionId = session?.session_id || session?.id;
  if (!sessionId) throw new Error("No session was created");

  const messagesResponse = await fetch(
    `http://127.0.0.1:${runtimePort}/api/sessions/${sessionId}/messages?limit=100`,
    { headers: { authorization: `Bearer ${token}` } },
  );
  if (!messagesResponse.ok) {
    throw new Error(`GET /api/sessions/${sessionId}/messages ${messagesResponse.status}`);
  }

  const messagesPayload = await messagesResponse.json();
  const messages = Array.isArray(messagesPayload.messages_v2) ? messagesPayload.messages_v2 : [];
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
    .join("\n")
    .trim();

  return {
    session_id: sessionId,
    message_count: messages.length,
    assistant_text_length: assistantText.length,
    assistant_text_preview: assistantText.slice(0, 160),
  };
}

async function selectModel(page, model) {
  const selector = page.locator('select[title="Model"]');
  await selector.waitFor({ state: "visible", timeout: 15_000 });
  await page.waitForFunction(
    ({ expectedModel }) => {
      const select = document.querySelector('select[title="Model"]');
      if (!(select instanceof HTMLSelectElement)) return false;
      return [...select.options].some((option) => option.value === expectedModel);
    },
    { expectedModel: model },
    { timeout: 30_000 },
  );
  await selector.selectOption(model);
  const selected = await selector.inputValue();
  assert.equal(selected, model, `Desktop model select did not stay on ${model}`);
}

function parseSseText(text) {
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
    .filter((data) => data && data !== "[DONE]")
    .map((data) => {
      try {
        return JSON.parse(data);
      } catch {
        return null;
      }
    })
    .filter(Boolean);
}

async function runtimeEventSummary(runtimePort) {
  if (!runtimePort) return {};
  try {
    const response = await fetch(`http://127.0.0.1:${runtimePort}/api/events?last_event_id=0`, {
      headers: { authorization: `Bearer ${token}` },
    });
    if (!response.ok) return { events_error: `HTTP ${response.status}` };
    const events = parseSseText(await response.text());
    const methods = events.map((event) => event.method).filter(Boolean);
    const turnStarted = events.find((event) => event.method === "turn/started");
    return {
      event_count: events.length,
      methods,
      delta_count: methods.filter((method) => method === "item/agentMessage/delta").length,
      completed_count: methods.filter((method) => method === "turn/completed").length,
      failed_count: methods.filter((method) => method === "turn/failed").length,
      turn_model: turnStarted?.params?.model,
      last_method: methods.at(-1),
    };
  } catch (error) {
    return { events_error: error instanceof Error ? error.message : String(error) };
  }
}

async function pageStreamingSummary(page) {
  if (!page) return {};
  try {
    return await page.evaluate(() => ({
      draft_text_length: document.querySelector('[data-testid="streaming-assistant-draft"]')?.textContent?.trim().length || 0,
      assistant_text_length: [...document.querySelectorAll(".role-assistant .event-text")]
        .map((node) => node.textContent?.trim() || "")
        .join("\n")
        .trim().length,
      body_has_begin: Boolean(document.body.textContent?.includes("OA_REAL_STREAM_BEGIN")),
      body_has_end: Boolean(document.body.textContent?.includes("OA_REAL_STREAM_END")),
      vite_overlay: Boolean(document.querySelector("vite-error-overlay")),
    }));
  } catch (error) {
    return { page_error: error instanceof Error ? error.message : String(error) };
  }
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const providerMode = options.provider;
  const useFakeProvider = providerMode === "fake";
  const tempRoot = fs.mkdtempSync(path.join(os.tmpdir(), "openagent-desktop-stream-"));
  const workspace = path.join(tempRoot, "workspace");
  const sessionRoot = path.join(tempRoot, "sessions");
  fs.mkdirSync(workspace, { recursive: true });
  fs.mkdirSync(sessionRoot, { recursive: true });

  const providerRequests = [];
  let firstDeltaSentAt = 0;
  let completionSentAt = 0;
  const provider = useFakeProvider
    ? http.createServer((request, response) => {
        const chunks = [];
        request.on("data", (chunk) => chunks.push(chunk));
        request.on("end", () => {
          const body = Buffer.concat(chunks).toString("utf8");
          if (request.url?.startsWith("/v1/models")) {
            providerRequests.push({ method: request.method, url: request.url, body });
            response.writeHead(200, { "content-type": "application/json" });
            response.end(JSON.stringify({ object: "list", data: [{ id: "fake-stream", object: "model" }] }));
            return;
          }

          if (request.url?.startsWith("/v1/responses")) {
            providerRequests.push({ method: request.method, url: request.url, body });
            response.writeHead(200, {
              "cache-control": "no-cache",
              connection: "close",
              "content-type": "text/event-stream; charset=utf-8",
            });
            response.write('data: {"type":"response.output_text.delta","delta":"streamed "}\n\n');
            firstDeltaSentAt = Date.now();
            setTimeout(() => {
              response.write('data: {"type":"response.output_text.delta","delta":"answer"}\n\n');
              response.write(
                'data: {"type":"response.completed","response":{"usage":{"input_tokens":7,"output_tokens":2}}}\n\n',
              );
              response.write("data: [DONE]\n\n");
              completionSentAt = Date.now();
              response.end();
            }, 2_500);
            return;
          }

          response.writeHead(404, { "content-type": "text/plain" });
          response.end("not found");
        });
      })
    : null;

  let runtime;
  let vite;
  let browser;
  let page;
  let runtimePort = 0;
  let providerConfig;
  let screenshotPath = "";

  try {
    const providerPort = provider ? await listen(provider) : 0;
    providerConfig = useFakeProvider ? fakeProviderEnv(providerPort) : realProviderEnv(options);
    runtimePort = await freePort();
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
      {
        cwd: repoRoot,
        env: {
          ...process.env,
          ...providerConfig.env,
        },
      },
    );

    await waitForHttp(`http://127.0.0.1:${runtimePort}/api/health`, {
      headers: { authorization: `Bearer ${token}` },
      timeoutMs: 90_000,
    });

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
    page = await browser.newPage({ viewport: { width: 1280, height: 720 } });
    page.on("console", (message) => {
      if (["error", "warning"].includes(message.type())) consoleIssues.push(message.text());
    });
    page.on("pageerror", (error) => pageErrors.push(error.message));
    await page.addInitScript(
      ({ bridgeUrl, tokenValue, workspacePath }) => {
        const project = {
          id: workspacePath,
          name: "stream-smoke",
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
    await selectModel(page, providerConfig.summary.model);
    const promptText = promptForMode(providerMode);
    const finalNeedle = finalNeedleForMode(providerMode);
    await page.getByPlaceholder("要求后续变更").fill(promptText);
    await page.locator(".send-button").click();

    await page.waitForFunction(
      ({ finalText, mode }) => {
        const draft = document.querySelector('[data-testid="streaming-assistant-draft"]')?.textContent || "";
        if (!draft.trim()) return false;
        if (mode === "fake") return draft.includes("streamed ");
        return draft.includes("OA_REAL_STREAM_BEGIN") || !document.body.textContent?.includes(finalText);
      },
      { finalText: finalNeedle, mode: providerMode },
      { timeout: useFakeProvider ? 2_000 : 45_000 },
    );
    if (useFakeProvider) {
      assert.ok(firstDeltaSentAt > 0, "provider did not send first delta");
      assert.equal(completionSentAt, 0, "Desktop only showed draft after provider completion");
    }

    await page.waitForFunction(
      ({ finalText }) => {
        const draftVisible = Boolean(document.querySelector('[data-testid="streaming-assistant-draft"]'));
        const assistantTexts = [...document.querySelectorAll(".role-assistant .event-text")]
          .map((node) => node.textContent?.trim() || "")
          .filter(Boolean);
        const hasNeedle = Boolean(document.body.textContent?.includes(finalText));
        const hasSubstantialAssistant = assistantTexts.some((text) => text.length >= 80);
        return !draftVisible && (hasNeedle || hasSubstantialAssistant);
      },
      { finalText: finalNeedle },
      { timeout: useFakeProvider ? 10_000 : 90_000 },
    );

    const pageState = await page.evaluate(() => {
      const rawDeltaRows = [...document.querySelectorAll(".live-event-row")].filter((row) =>
        row.textContent?.includes("agentMessage delta"),
      ).length;
      const draftVisible = Boolean(document.querySelector('[data-testid="streaming-assistant-draft"]'));
      const overlayVisible = Boolean(document.querySelector("vite-error-overlay"));
      const finalAssistantTextLength = [...document.querySelectorAll(".role-assistant .event-text")]
        .map((node) => node.textContent?.trim() || "")
        .join("\n")
        .trim().length;
      return {
        rawDeltaRows,
        draftVisible,
        overlayVisible,
        bodyHasFinalNeedle: Boolean(document.body.textContent?.includes("streamed answer")) ||
          Boolean(document.body.textContent?.includes("OA_REAL_STREAM_END")),
        finalAssistantTextLength,
      };
    });

    assert.equal(pageState.rawDeltaRows, 0, "raw delta protocol rows leaked into timeline");
    assert.equal(pageState.draftVisible, false, "streaming draft did not clear after final message");
    assert.equal(pageState.overlayVisible, false, "Vite error overlay is visible");
    assert.equal(
      pageState.finalAssistantTextLength > 0 || pageState.bodyHasFinalNeedle,
      true,
      "final assistant message was not rendered",
    );
    assert.equal(pageErrors.length, 0, `page errors: ${pageErrors.join("\n")}`);
    assert.equal(consoleIssues.length, 0, `console issues: ${consoleIssues.join("\n")}`);

    screenshotPath = path.join(os.tmpdir(), `openagent-desktop-real-stream-${Date.now()}.png`);
    await page.screenshot({ path: screenshotPath, fullPage: false });

    let providerStreamRequested = true;
    if (useFakeProvider) {
      const responsesRequest = providerRequests.find((item) => item.url?.startsWith("/v1/responses"));
      assert.ok(responsesRequest, "runtime did not call /v1/responses");
      const responsesBody = JSON.parse(responsesRequest.body || "{}");
      providerStreamRequested = responsesBody.stream === true;
      assert.equal(providerStreamRequested, true, "runtime did not request provider streaming");
    }
    const persisted = await persistedAssistantSummary(runtimePort);
    assert.equal(persisted.assistant_text_length > 0, true, "assistant message was not persisted");
    const runtimeEvents = useFakeProvider ? undefined : await runtimeEventSummary(runtimePort);
    if (!useFakeProvider) {
      assert.equal(runtimeEvents.turn_model, providerConfig.summary.model, "runtime used the wrong model");
      assert.equal(runtimeEvents.delta_count > 0, true, "runtime did not persist provider delta events");
      assert.equal(runtimeEvents.completed_count > 0, true, "runtime did not persist turn completion");
    }

    console.log(
      JSON.stringify(
        {
          ok: true,
          provider: providerMode,
          provider_config: providerConfig.summary,
          runtime_url: `http://127.0.0.1:${runtimePort}`,
          desktop_url: `http://127.0.0.1:${vitePort}`,
          provider_requests: useFakeProvider ? providerRequests.map((item) => item.url) : undefined,
          provider_stream_requested: providerStreamRequested,
          first_delta_before_completion_ms: useFakeProvider ? completionSentAt - firstDeltaSentAt : undefined,
          page_state: pageState,
          persisted,
          runtime_events: runtimeEvents,
          screenshot: screenshotPath,
        },
        null,
        2,
      ),
    );
  } catch (error) {
    const diagnostics = {
      provider: providerMode,
      provider_config: providerConfig?.summary,
      runtime_events: await runtimeEventSummary(runtimePort),
      persisted: runtimePort ? await persistedAssistantSummary(runtimePort).catch((err) => ({ error: err.message })) : {},
      page: await pageStreamingSummary(page),
    };
    console.error(`streaming smoke diagnostics: ${JSON.stringify(diagnostics, null, 2)}`);
    throw error;
  } finally {
    await browser?.close().catch(() => undefined);
    await stopChild(vite);
    await stopChild(runtime);
    if (provider) await new Promise((resolve) => provider.close(resolve));
    fs.rmSync(tempRoot, { recursive: true, force: true });
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.stack || error.message : error);
  process.exitCode = 1;
});
