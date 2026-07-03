const state = {
  activeSessionId: null,
  activeTurnId: null,
  pendingApproval: null,
  pendingQuestion: null,
  source: null,
  seenEvents: new Set(),
  textNodes: new Map(),
};

const els = {
  health: document.querySelector("#health-pill"),
  model: document.querySelector("#model-pill"),
  sessions: document.querySelector("#session-list"),
  tasks: document.querySelector("#task-list"),
  title: document.querySelector("#active-session-title"),
  prompt: document.querySelector("#prompt-input"),
  run: document.querySelector("#run-turn"),
  newSession: document.querySelector("#new-session"),
  approvalPanel: document.querySelector("#approval-panel"),
  approvalBody: document.querySelector("#approval-body"),
  approvalAllow: document.querySelector("#approval-allow"),
  approvalDeny: document.querySelector("#approval-deny"),
  approvalScope: document.querySelector("#approval-scope"),
  approvalNote: document.querySelector("#approval-note"),
  questionPanel: document.querySelector("#question-panel"),
  questionBody: document.querySelector("#question-body"),
  questionAnswer: document.querySelector("#question-answer"),
  questionReply: document.querySelector("#question-reply"),
  questionDismiss: document.querySelector("#question-dismiss"),
  timeline: document.querySelector("#timeline"),
  clear: document.querySelector("#clear-events"),
  turnState: document.querySelector("#turn-state"),
  detailSession: document.querySelector("#detail-session"),
  detailTurn: document.querySelector("#detail-turn"),
  detailStatus: document.querySelector("#detail-status"),
  detailTrace: document.querySelector("#detail-trace"),
  finalAnswer: document.querySelector("#final-answer"),
};

const eventNames = [
  "turn/started",
  "item/step/started",
  "item/step/completed",
  "item/agentMessage/started",
  "item/agentMessage/delta",
  "item/agentMessage/completed",
  "item/toolCall/started",
  "item/toolCall/completed",
  "runtime/warning",
  "item/patch/detected",
  "item/question/requested",
  "item/question/resolved",
  "turn/error",
  "turn/interrupt_requested",
  "turn/interrupted",
  "turn/approval_requested",
  "turn/approval_resolved",
  "turn/completed",
  "turn/failed",
];

async function boot() {
  await refreshHealth();
  await refreshModels();
  await refreshSessions();
  await refreshTasks();
  els.newSession.addEventListener("click", createSession);
  els.run.addEventListener("click", runTurn);
  els.clear.addEventListener("click", clearTimeline);
  els.approvalAllow.addEventListener("click", () => respondApproval("allow"));
  els.approvalDeny.addEventListener("click", () => respondApproval("deny"));
  els.questionReply.addEventListener("click", respondQuestion);
  els.questionDismiss.addEventListener("click", dismissQuestion);
}

async function refreshHealth() {
  try {
    const data = await getJSON("/api/health");
    els.health.textContent = data.ok ? "online" : "offline";
    els.health.className = data.ok ? "pill ok" : "pill bad";
  } catch {
    els.health.textContent = "offline";
    els.health.className = "pill bad";
  }
}

async function refreshModels() {
  try {
    const data = await getJSON("/api/models");
    const model = data.models?.[0];
    els.model.textContent = model ? model.id : "no model";
  } catch {
    els.model.textContent = "model unavailable";
  }
}

async function refreshSessions() {
  const data = await getJSON("/api/sessions");
  els.sessions.replaceChildren();
  if (!data.sessions?.length) {
    const empty = document.createElement("div");
    empty.className = "session-meta";
    empty.textContent = "还没有会话";
    els.sessions.append(empty);
    return;
  }
  data.sessions.forEach((session) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `session-item ${session.id === state.activeSessionId ? "active" : ""}`;
    button.innerHTML = `
      <div class="session-id">${escapeHTML(session.id)}</div>
      <div class="session-meta">${session.message_count || 0} messages · ${escapeHTML(session.status || "idle")}</div>
    `;
    button.addEventListener("click", () => selectSession(session.id));
    els.sessions.append(button);
  });
}

async function createSession() {
  const data = await postJSON("/api/sessions", {});
  selectSession(data.session.id);
  await refreshSessions();
}

async function selectSession(sessionId) {
  state.activeSessionId = sessionId;
  const data = await getJSON(`/api/sessions/${encodeURIComponent(sessionId)}`);
  els.title.textContent = shortId(sessionId);
  els.detailSession.textContent = sessionId;
  els.detailStatus.textContent = data.session.status || "idle";
  await refreshSessions();
  await refreshTasks(sessionId);
}

async function runTurn() {
  const input = els.prompt.value.trim();
  if (!input) return;
  if (!state.activeSessionId) {
    const data = await postJSON("/api/sessions", {});
    state.activeSessionId = data.session.id;
    els.detailSession.textContent = state.activeSessionId;
  }

  clearTimeline();
  clearApproval();
  clearQuestion();
  els.run.disabled = true;
  els.turnState.textContent = "starting";
  els.finalAnswer.textContent = "-";

  try {
    const data = await postJSON(`/api/sessions/${encodeURIComponent(state.activeSessionId)}/turns`, {
      input,
    });
    state.activeTurnId = data.turn?.id || data.turn_id;
    els.detailTurn.textContent = state.activeTurnId;
    if (Array.isArray(data.events)) {
      data.events.forEach(handleAppEvent);
    } else {
      streamTurn(state.activeTurnId);
    }
    await refreshTasks(state.activeSessionId);
  } catch (error) {
    els.run.disabled = false;
    els.turnState.textContent = "failed";
    addEventRow("turn/failed", error.message, "error");
  }
}

function streamTurn(turnId) {
  if (state.source) state.source.close();
  const source = new EventSource(`/api/turns/${encodeURIComponent(turnId)}/events`);
  state.source = source;
  eventNames.forEach((name) => {
    source.addEventListener(name, (message) => handleAppEvent(JSON.parse(message.data)));
  });
  source.onerror = () => {
    if (els.detailStatus.textContent !== "completed" && els.detailStatus.textContent !== "failed") {
      els.turnState.textContent = "connection retrying";
    }
  };
}

function handleAppEvent(appEvent) {
  const key = eventKey(appEvent);
  if (state.seenEvents.has(key)) return;
  state.seenEvents.add(key);
  const { method, params } = appEvent;
  els.turnState.textContent = method;
  els.detailStatus.textContent = method.includes("failed") ? "failed" : method.includes("completed") ? "completed" : "running";

  if (method === "item/agentMessage/delta") {
    renderTextDelta(params.event);
    return;
  }
  if (method === "turn/completed" || method === "turn/failed") {
    els.run.disabled = false;
    els.turnState.textContent = method === "turn/completed" ? "completed" : "failed";
    els.finalAnswer.textContent = params.final_answer || "-";
    els.detailTrace.textContent = traceLabel(params.trace);
    clearApproval();
    clearQuestion();
    if (state.source) state.source.close();
    refreshTasks(state.activeSessionId);
  }
  if (method === "turn/interrupted") {
    els.run.disabled = false;
    els.turnState.textContent = "interrupted";
    els.finalAnswer.textContent = params.final_answer || "-";
    els.detailTrace.textContent = traceLabel(params.trace);
    clearApproval();
    clearQuestion();
    if (state.source) state.source.close();
    refreshTasks(state.activeSessionId);
  }
  if (method === "turn/approval_requested") {
    showApproval(params.approval);
  }
  if (method === "turn/approval_resolved") {
    clearApproval();
  }
  if (method === "item/question/requested" || method === "turn/question_requested") {
    showQuestion(params.event || params.question || params);
  }
  if (method === "item/question/resolved") {
    clearQuestion();
  }
  if (method === "item/toolCall/completed" || method === "item/toolCall/failed") {
    const metadata = params.metadata || {};
    if (metadata.tool === "task" || params.name === "task") {
      refreshTasks(state.activeSessionId);
    }
  }

  const severity = method.includes("warning") ? "warning" : method.includes("failed") || method.includes("error") ? "error" : "";
  addEventRow(method, summarizeParams(params), severity, classForMethod(method));
}

function eventKey(appEvent) {
  if (appEvent.event_id) return `event:${appEvent.event_id}`;
  if (appEvent.global_sequence) return `global:${appEvent.global_sequence}`;
  if (appEvent.sequence) {
    const params = appEvent.params || {};
    return `turn:${params.session_id || params.thread_id || ""}:${params.turn_id || params.run_id || ""}:${appEvent.sequence}:${appEvent.method}`;
  }
  return `${appEvent.method}:${JSON.stringify(appEvent.params || {})}`;
}

async function refreshTasks(sessionId = state.activeSessionId) {
  if (!els.tasks) return;
  els.tasks.replaceChildren();
  if (!sessionId) {
    const empty = document.createElement("div");
    empty.className = "task-empty";
    empty.textContent = "选择会话后显示任务";
    els.tasks.append(empty);
    return;
  }
  try {
    const data = await getJSON(`/api/sessions/${encodeURIComponent(sessionId)}/tasks`);
    const tree = data.tree || data.tasks || [];
    if (!tree.length) {
      const empty = document.createElement("div");
      empty.className = "task-empty";
      empty.textContent = "暂无 subagent 任务";
      els.tasks.append(empty);
      return;
    }
    const list = document.createElement("div");
    list.className = "task-tree";
    tree.forEach((task) => list.append(renderTaskNode(task, 0)));
    els.tasks.append(list);
  } catch (error) {
    const row = document.createElement("div");
    row.className = "task-empty error";
    row.textContent = error.message;
    els.tasks.append(row);
  }
}

function renderTaskNode(task, depth) {
  const wrap = document.createElement("div");
  wrap.className = "task-node";
  wrap.style.setProperty("--task-depth", depth);
  const row = document.createElement("div");
  row.className = `task-row ${taskStatusClass(task.status)}`;
  const main = document.createElement("button");
  main.type = "button";
  main.className = "task-open";
  main.addEventListener("click", () => selectSession(task.session_id || task.task_id));
  main.innerHTML = `
    <span class="task-title">${escapeHTML(task.title || task.description || task.subagent_type || "task")}</span>
    <span class="task-meta">${escapeHTML(task.subagent_type || "subagent")} · ${escapeHTML(task.status || "unknown")}${task.background ? " · bg" : ""}</span>
  `;
  row.append(main);
  if (task.background && task.status === "queued") {
    const run = document.createElement("button");
    run.type = "button";
    run.className = "task-action";
    run.textContent = "Run";
    run.addEventListener("click", (event) => {
      event.stopPropagation();
      runBackgroundTask(task);
    });
    const cancel = document.createElement("button");
    cancel.type = "button";
    cancel.className = "task-action danger";
    cancel.textContent = "Cancel";
    cancel.addEventListener("click", (event) => {
      event.stopPropagation();
      cancelBackgroundTask(task);
    });
    row.append(run, cancel);
  }
  wrap.append(row);
  (task.children || []).forEach((child) => wrap.append(renderTaskNode(child, depth + 1)));
  return wrap;
}

function taskStatusClass(status = "") {
  if (status === "queued") return "queued";
  if (status === "running") return "running";
  if (status === "failed" || status === "canceled") return "failed";
  if (status === "completed") return "completed";
  return "";
}

async function runBackgroundTask(task) {
  const parent = task.parent_session_id || task.task_parent_session_id || state.activeSessionId;
  const taskId = task.session_id || task.task_id;
  if (!parent || !taskId) return;
  try {
    const data = await postJSON(
      `/api/sessions/${encodeURIComponent(parent)}/tasks/${encodeURIComponent(taskId)}/run`,
      {},
    );
    if (Array.isArray(data.result?.events)) data.result.events.forEach(handleAppEvent);
    await refreshTasks(state.activeSessionId);
  } catch (error) {
    addEventRow("task/run_failed", error.message, "error");
  }
}

async function cancelBackgroundTask(task) {
  const parent = task.parent_session_id || task.task_parent_session_id || state.activeSessionId;
  const taskId = task.session_id || task.task_id;
  if (!parent || !taskId) return;
  try {
    await postJSON(
      `/api/sessions/${encodeURIComponent(parent)}/tasks/${encodeURIComponent(taskId)}/cancel`,
      {},
    );
    await refreshTasks(state.activeSessionId);
  } catch (error) {
    addEventRow("task/cancel_failed", error.message, "error");
  }
}

function renderTextDelta(event) {
  const id = event.id || "assistant";
  let node = state.textNodes.get(id);
  if (!node) {
    node = addEventRow("item/agentMessage/delta", "", "", "agent-message");
    state.textNodes.set(id, node.querySelector(".event-body"));
  }
  node.textContent += event.text || "";
  els.timeline.scrollTop = els.timeline.scrollHeight;
}

function addEventRow(method, body, severity = "", extraClass = "") {
  removeEmptyState();
  const row = document.createElement("article");
  row.className = ["event-row", severity, extraClass].filter(Boolean).join(" ");
  const time = new Date().toLocaleTimeString();
  row.innerHTML = `
    <div class="event-head">
      <span class="event-method">${escapeHTML(method)}</span>
      <span>${time}</span>
    </div>
    <div class="event-body">${escapeHTML(body)}</div>
  `;
  els.timeline.append(row);
  els.timeline.scrollTop = els.timeline.scrollHeight;
  return row;
}

function classForMethod(method) {
  if (method.includes("toolCall")) return "tool-event";
  if (method.includes("agentMessage")) return "agent-message";
  return "";
}

function summarizeParams(params) {
  const event = params.event || {};
  if (event.type === "tool-call") {
    return `${event.name} ${JSON.stringify(event.input || {})}`;
  }
  if (event.type === "tool-result") {
    return event.error ? `${event.call_id}\nERROR: ${event.error}` : `${event.call_id}\n${event.output || ""}`;
  }
  if (event.type === "runtime-warning") {
    return `${event.code}\n${event.message}`;
  }
  if (event.type === "step-start") {
    return `snapshot: ${event.snapshot_id || "-"}`;
  }
  if (event.type === "step-finish") {
    return `finish: ${event.finish_reason || "-"}\ntokens: ${JSON.stringify(event.tokens || {})}\ncost: ${event.cost ?? 0}`;
  }
  if (event.type === "patch") {
    return `files: ${(event.files || []).length}\nhash: ${event.hash || "-"}`;
  }
  if (event.type === "error") {
    return event.error || "Unknown error";
  }
  if (params.approval) {
    return approvalSummary(params.approval);
  }
  return JSON.stringify(params, null, 2);
}

function showApproval(approval) {
  if (!approval) return;
  state.pendingApproval = approval;
  els.approvalBody.textContent = approvalSummary(approval);
  els.approvalPanel.hidden = false;
  els.turnState.textContent = "waiting approval";
}

function clearApproval() {
  state.pendingApproval = null;
  els.approvalBody.textContent = "-";
  els.approvalScope.value = "once";
  els.approvalNote.value = "";
  els.approvalPanel.hidden = true;
}

async function respondApproval(action) {
  const approval = state.pendingApproval;
  if (!approval?.turn_id || !approval?.request_id) return;
  els.approvalAllow.disabled = true;
  els.approvalDeny.disabled = true;
  const body = { action };
  if (action === "allow") body.scope = els.approvalScope.value || "once";
  const note = els.approvalNote.value.trim();
  if (note) body.note = note;
  try {
    const data = await postJSON(
      `/api/turns/${encodeURIComponent(approval.turn_id)}/approvals/${encodeURIComponent(approval.request_id)}`,
      body,
    );
    els.turnState.textContent = `approval ${action} sent`;
    if (Array.isArray(data.events)) data.events.forEach(handleAppEvent);
  } catch (error) {
    addEventRow("turn/approval_failed", error.message, "error");
  } finally {
    els.approvalAllow.disabled = false;
    els.approvalDeny.disabled = false;
  }
}

function showQuestion(question) {
  if (!question) return;
  state.pendingQuestion = question;
  els.questionBody.textContent = questionSummary(question);
  els.questionAnswer.value = "";
  els.questionPanel.hidden = false;
  els.turnState.textContent = "waiting answer";
}

function clearQuestion() {
  state.pendingQuestion = null;
  els.questionBody.textContent = "-";
  els.questionAnswer.value = "";
  els.questionPanel.hidden = true;
}

async function respondQuestion() {
  const question = state.pendingQuestion;
  const answer = els.questionAnswer.value.trim();
  if (!question?.turn_id || !question?.request_id || !answer) return;
  await sendQuestionResponse(question, { answer });
}

async function dismissQuestion() {
  const question = state.pendingQuestion;
  if (!question?.turn_id || !question?.request_id) return;
  await sendQuestionResponse(question, { dismissed: true, note: els.questionAnswer.value.trim() });
}

async function sendQuestionResponse(question, body) {
  els.questionReply.disabled = true;
  els.questionDismiss.disabled = true;
  try {
    const data = await postJSON(
      `/api/turns/${encodeURIComponent(question.turn_id)}/questions/${encodeURIComponent(question.request_id)}/reply`,
      body,
    );
    els.turnState.textContent = body.dismissed ? "question dismissed" : "question answered";
    clearQuestion();
    if (Array.isArray(data.events)) data.events.forEach(handleAppEvent);
  } catch (error) {
    addEventRow("turn/question_failed", error.message, "error");
  } finally {
    els.questionReply.disabled = false;
    els.questionDismiss.disabled = false;
  }
}

function approvalSummary(approval) {
  return [
    `tool: ${approval.tool_name || "tool"}`,
    `request: ${approval.request_id || "-"}`,
    approval.call_id ? `call: ${approval.call_id}` : null,
    approval.permission_pattern ? `pattern: ${approval.permission_pattern}` : null,
    `input: ${JSON.stringify(approval.tool_input || {}, null, 2)}`,
  ]
    .filter(Boolean)
    .join("\n");
}

function questionSummary(question) {
  const questions = question.questions || question.event?.questions || [];
  if (!questions.length) return JSON.stringify(question, null, 2);
  return questions
    .map((item, index) => {
      const options = (item.options || [])
        .map((option) => `  - ${option.label || option}`)
        .join("\n");
      return `${index + 1}. ${item.header || "Question"}\n${item.question || ""}${options ? `\n${options}` : ""}`;
    })
    .join("\n\n");
}

function clearTimeline() {
  state.textNodes.clear();
  state.seenEvents.clear();
  els.timeline.innerHTML = `
    <div class="empty-state">
      <div class="empty-title">等待任务开始</div>
      <div class="empty-copy">模型输出、工具调用、runtime warning 和最终结果会流式出现在这里。</div>
    </div>
  `;
}

function removeEmptyState() {
  const empty = els.timeline.querySelector(".empty-state");
  if (empty) empty.remove();
}

async function getJSON(url) {
  const response = await fetch(url);
  if (!response.ok) throw new Error(await response.text());
  return response.json();
}

async function postJSON(url, body) {
  const response = await fetch(url, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const text = await response.text();
    throw new Error(text);
  }
  return response.json();
}

function shortId(id) {
  return id ? `${id.slice(0, 18)}…` : "-";
}

function traceLabel(trace) {
  if (!trace) return "-";
  return trace.trace_id || trace.run_id || trace.trace_path || "-";
}

function escapeHTML(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

boot().catch((error) => {
  els.health.textContent = "offline";
  els.health.className = "pill bad";
  addEventRow("app/error", error.message, "error");
});
