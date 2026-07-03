import {
  Activity,
  AlertTriangle,
  ArrowLeft,
  ArrowRight,
  ArrowUp,
  Bot,
  CheckCircle2,
  Circle,
  Database,
  Folder,
  FolderOpen,
  FolderPlus,
  GitBranch,
  GitCompare,
  History,
  MoreHorizontal,
  PanelRight,
  PencilLine,
  Play,
  PlugZap,
  Plus,
  Power,
  Radio,
  RefreshCw,
  RotateCcw,
  Search,
  Settings,
  ShieldCheck,
  Sidebar,
  Square,
  Terminal,
  Undo2,
  Wrench,
  XCircle,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { Fragment, FormEvent, KeyboardEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";

type JsonRecord = Record<string, unknown>;

class ApiError extends Error {
  status: number;
  method: string;
  path: string;
  body?: JsonRecord;

  constructor(method: string, path: string, status: number, body?: JsonRecord) {
    const bodyError = typeof body?.error === "string" ? body.error : "";
    super(bodyError ? `${method} ${path} ${status}: ${bodyError}` : `${method} ${path} ${status}`);
    this.name = "ApiError";
    this.status = status;
    this.method = method;
    this.path = path;
    this.body = body;
  }
}

type AppEvent = {
  event_id?: string;
  schema_version?: string;
  protocol_version?: number;
  sequence?: number;
  global_sequence?: number;
  method: string;
  params?: JsonRecord;
  created_at_ms?: number;
};

type SessionSummary = {
  session_id?: string;
  id?: string;
  title?: string;
  workspace?: string;
  status?: string;
  updated_at_ms?: number;
  message_count?: number;
};

type TurnJobSummary = {
  session_id?: string;
  turn_id?: string;
  status?: string;
  started_at_ms?: number;
  updated_at_ms?: number;
  queue_position?: number | null;
  queue_reason?: string | null;
  payload_persisted?: boolean;
  cancel_requested?: boolean;
  cancel_requested_at_ms?: number | null;
};

type TurnSchedulerSummary = {
  max_queued_turns_per_session?: number;
  max_running_turn_workers?: number;
  running_turn_workers?: number;
  turn_queue_lease_stale_ms?: number;
  turn_queue_timeout_ms?: number;
  expired_queued_turns?: number;
};

type TurnJobsPayload = {
  turns?: TurnJobSummary[];
  count?: number;
  running_count?: number;
  queued_count?: number;
  active_count?: number;
  terminal_count?: number;
  scheduler?: TurnSchedulerSummary;
  filters?: JsonRecord;
  source?: string;
  index_persisted?: boolean;
  error?: string;
};

type ProtocolPayload = {
  protocol?: string;
  protocol_version?: number;
  event_schema_version?: string;
  endpoints?: JsonRecord;
  terminal_methods?: string[];
};

type ProviderPayload = {
  healthy?: boolean;
  provider?: string;
  provider_label?: string;
  model?: string;
  model_count?: number;
  model_endpoint_ok?: boolean;
  configured_model_available?: boolean;
  api_key?: string;
  models?: Array<{ id?: string; default?: boolean }>;
};

type McpServerSummary = {
  name?: string;
  type?: string;
  enabled?: boolean;
  transport?: string;
  selected_transport?: string | null;
  status?: string;
  tool_count?: number;
  tools?: Array<{ name?: string; title?: string; description?: string; original_name?: string }>;
  remote_url_configured?: boolean;
  command?: string;
  args_count?: number;
  cwd_configured?: boolean;
  env_count?: number;
  header_count?: number;
  timeout_ms?: number;
  last_error?: string | null;
  last_refreshed_at?: number | null;
  lifecycle_status?: string | null;
  lifecycle_pid?: number | null;
  lifecycle_started_at?: number | null;
  lifecycle_last_refreshed_at?: number | null;
  lifecycle_tool_count?: number | null;
};

type McpPayload = {
  configured?: boolean;
  enabled?: boolean;
  server_count?: number;
  tool_count?: number;
  refresh_ttl_s?: number | null;
  source?: string;
  writable?: boolean;
  config_path?: string | null;
  readonly_reason?: string | null;
  status?: string;
  error?: string | null;
  servers?: McpServerSummary[];
};

type McpServerDraft = {
  mode: "remote" | "local";
  name: string;
  url: string;
  transport: string;
  command: string;
  args: string;
  cwd: string;
  env: string;
  headers: string;
  timeoutMs: string;
};

type PendingApproval = {
  kind?: string;
  status?: string;
  session_id?: string;
  turn_id?: string;
  request_id?: string;
  approval?: JsonRecord;
  session?: SessionSummary;
};

type PendingQuestion = {
  kind?: string;
  status?: string;
  session_id?: string;
  turn_id?: string;
  request_id?: string;
  question?: JsonRecord;
  session?: SessionSummary;
};

type SessionDiff = {
  undo_count?: number;
  redo_count?: number;
  latest?: JsonRecord | null;
  patches?: JsonRecord[];
  redo?: JsonRecord[];
};

type CheckpointSummary = {
  checkpoint_id?: string;
  kind?: string;
  run_id?: string;
  timestamp_ms?: number;
  file_count?: number;
  total_bytes?: number;
};

type CheckpointsPayload = {
  count?: number;
  latest?: CheckpointSummary | null;
  checkpoints?: CheckpointSummary[];
};

type FileEntry = {
  path?: string;
  name?: string;
  kind?: string;
  size_bytes?: number;
  text?: boolean;
};

type FilesPayload = {
  workspace?: string;
  path?: string;
  absolute_path?: string;
  exists?: boolean;
  is_file?: boolean;
  is_dir?: boolean;
  entries?: FileEntry[];
  entry_count?: number;
  truncated?: boolean;
  content?: string | null;
  error?: string;
};

type GitChange = {
  status?: string;
  index?: string;
  worktree?: string;
  path?: string;
};

type GitPayload = {
  workspace?: string;
  is_repo?: boolean;
  branch?: string;
  ahead?: number;
  behind?: number;
  changes?: GitChange[];
  change_count?: number;
  error?: string;
};

type TerminalRunResult = {
  command?: string;
  workspace?: string;
  cwd?: string;
  cwd_relative?: string;
  success?: boolean;
  exit_code?: number;
  timed_out?: boolean;
  timeout_ms?: number;
  duration_ms?: number;
  stdout?: string;
  stderr?: string;
  stdout_truncated?: boolean;
  stderr_truncated?: boolean;
};

type MessageInfo = {
  id?: string;
  role?: string;
  status?: string;
  seq?: number;
  created_at_ms?: number;
  run_id?: string | null;
  metadata?: JsonRecord;
};

type MessagePart = {
  id?: string;
  kind?: string;
  status?: string;
  content?: unknown;
  attributes?: JsonRecord;
  timestamp_ms?: number;
};

type MessageWithParts = {
  info?: MessageInfo;
  parts?: MessagePart[];
};

type SessionMessagesPayload = {
  session_id?: string;
  message_count?: number;
  message_v2_count?: number;
  limit?: number;
  messages?: JsonRecord[];
  messages_v2?: MessageWithParts[];
};

type StreamHealth = {
  status: string;
  resume_cursor: number;
  reconnect_attempts: number;
  recovered_count: number;
  last_batch_count: number;
  last_error?: string;
  last_connected_at_ms?: number;
  next_retry_ms?: number;
};

type InteractionSync = {
  last_synced_at_ms?: number;
  last_event_method?: string;
};

type StreamingDraft = {
  turnId: string;
  text: string;
  eventCount: number;
  completed: boolean;
};

type TrustHistoryItem = {
  id: string;
  kind: "approval" | "question";
  status: string;
  tone: "ok" | "warn" | "bad" | "neutral";
  title: string;
  summary: string;
  detail: string;
  requestId: string;
  callId: string;
};

type ElicitationFieldKind = "text" | "select" | "number" | "integer" | "boolean" | "multiselect";

type ElicitationOption = {
  label: string;
  value: string;
};

type ElicitationField = {
  id: string;
  index: number;
  label: string;
  description: string;
  value: string;
  values: string[];
  options: ElicitationOption[];
  required: boolean;
  kind: ElicitationFieldKind;
  placeholder: string;
  error: string;
  min?: number;
  max?: number;
};

type McpToolTrace = {
  toolName: string;
  originalTool: string;
  dynamicTool: string;
  server: string;
  transport: string;
  callId: string;
  status: string;
  output: string;
  error: string;
  nonTextBlockCount: number;
  lifecycleReused: boolean;
  lifecyclePid: number;
};

type DesktopDiagnosticPath = {
  source?: string;
  path?: string;
  exists?: boolean;
};

type DesktopDiagnostics = {
  runtime?: string;
  app_version?: string;
  os?: string;
  arch?: string;
  bridge_default_url?: string;
  bridge_url_env?: string | null;
  bridge_binary?: DesktopDiagnosticPath | null;
  bridge_binary_candidates?: DesktopDiagnosticPath[];
  workspace_default?: string;
  workspace_default_source?: string;
  session_root_default?: string;
  warnings?: string[];
};

type ManagedBridgeStatus = {
  running?: boolean;
  pid?: number | null;
  url?: string;
  port?: number;
  workspace?: string;
  session_root?: string;
  binary?: string | null;
  error?: string | null;
};

type ProjectPathInfo = {
  input?: string;
  path?: string;
  name?: string;
  exists?: boolean;
  is_dir?: boolean;
  canonical?: string | null;
  error?: string | null;
};

type DesktopAuthToken = {
  token?: string;
  path?: string;
  created?: boolean;
};

type DesktopProject = {
  id: string;
  name: string;
  path: string;
  last_opened_at_ms?: number;
};

type SseEventHandler = (events: AppEvent[]) => void | Promise<void>;

const DEFAULT_BRIDGE = import.meta.env.VITE_OPENAGENT_BRIDGE_URL ?? "http://127.0.0.1:8787";
const STORAGE_BRIDGE = "openagent.desktop.bridgeUrl";
const STORAGE_TOKEN = "openagent.desktop.token";
const STORAGE_PROJECTS = "openagent.desktop.projects";
const STORAGE_ACTIVE_PROJECT = "openagent.desktop.activeProject";

function storedValue(key: string, fallback: string): string {
  if (typeof window === "undefined") return fallback;
  return window.localStorage.getItem(key) ?? fallback;
}

function storedProjects(): DesktopProject[] {
  if (typeof window === "undefined") return [];
  try {
    const parsed = JSON.parse(window.localStorage.getItem(STORAGE_PROJECTS) ?? "[]");
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map((item) => {
        if (!item || typeof item !== "object") return null;
        const record = item as JsonRecord;
        const path = typeof record.path === "string" ? normalizeProjectPath(record.path) : "";
        if (!path) return null;
        return {
          id: path,
          name: typeof record.name === "string" && record.name.trim() ? record.name.trim() : projectNameFromPath(path),
          path,
          last_opened_at_ms:
            typeof record.last_opened_at_ms === "number" ? record.last_opened_at_ms : undefined,
        } satisfies DesktopProject;
      })
      .filter(Boolean) as DesktopProject[];
  } catch {
    return [];
  }
}

function isTauriRuntime(): boolean {
  if (typeof window === "undefined") return false;
  return Boolean((window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__);
}

function normalizeProjectPath(value?: string): string {
  const path = (value ?? "").trim();
  if (!path) return "";
  if (/^[A-Za-z]:\\?$/.test(path)) return path;
  return path.replace(/[\\/]+$/, "") || path;
}

function projectNameFromPath(path: string): string {
  const normalized = normalizeProjectPath(path);
  const parts = normalized.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? normalized;
}

function projectFromPath(path: string, name?: string): DesktopProject {
  const normalized = normalizeProjectPath(path);
  return {
    id: normalized,
    name: name?.trim() || projectNameFromPath(normalized),
    path: normalized,
    last_opened_at_ms: Date.now(),
  };
}

function upsertProject(projects: DesktopProject[], project: DesktopProject): DesktopProject[] {
  if (!project.path) return projects;
  const next = projects.filter((item) => normalizeProjectPath(item.path) !== normalizeProjectPath(project.path));
  return [project, ...next].slice(0, 12);
}

function sameProjectPath(left?: string, right?: string): boolean {
  return normalizeProjectPath(left) === normalizeProjectPath(right);
}

function sessionId(session: SessionSummary): string {
  return session.session_id ?? session.id ?? "";
}

function turnJobId(job: TurnJobSummary): string {
  return job.turn_id ?? "";
}

function turnJobSessionId(job: TurnJobSummary): string {
  return job.session_id ?? "";
}

function isTurnJobTerminal(job: TurnJobSummary): boolean {
  return ["completed", "failed", "interrupted", "expired"].includes(job.status ?? "");
}

function isTurnJobQueued(job: TurnJobSummary): boolean {
  return job.status === "queued";
}

function isTurnJobInterruptible(job: TurnJobSummary): boolean {
  return Boolean(turnJobId(job)) && !job.cancel_requested && !isTurnJobTerminal(job);
}

function turnJobLabel(job: TurnJobSummary): string {
  return compactId(turnJobId(job));
}

function normalizeTurnJobs(payload?: TurnJobsPayload): TurnJobsPayload {
  const turns = payload?.turns ?? [];
  const queuedCount = turns.filter(isTurnJobQueued).length;
  const runningCount = turns.filter((job) => !isTurnJobQueued(job) && !isTurnJobTerminal(job)).length;
  const terminalCount = turns.filter(isTurnJobTerminal).length;
  return {
    ...payload,
    turns,
    count: payload?.count ?? turns.length,
    running_count: payload?.running_count ?? runningCount,
    queued_count: payload?.queued_count ?? queuedCount,
    active_count: payload?.active_count ?? runningCount + queuedCount,
    terminal_count: payload?.terminal_count ?? terminalCount,
  };
}

function upsertTurnJob(payload: TurnJobsPayload, job: TurnJobSummary): TurnJobsPayload {
  const id = turnJobId(job);
  if (!id) return normalizeTurnJobs(payload);
  const now = Date.now();
  const current = payload.turns ?? [];
  const existing = current.find((item) => turnJobId(item) === id) ?? {};
  const nextJob = {
    started_at_ms: now,
    updated_at_ms: now,
    ...existing,
    ...job,
  };
  const next = [nextJob, ...current.filter((item) => turnJobId(item) !== id)].slice(0, 20);
  return normalizeTurnJobs({
    ...payload,
    turns: next,
    source: payload.source ?? "desktop",
  });
}

function eventSessionId(event: AppEvent): string {
  const params = event.params ?? {};
  const direct = params.session_id ?? params.thread_id;
  if (typeof direct === "string") return direct;
  const approval = params.approval;
  if (approval && typeof approval === "object" && "session_id" in approval) {
    const value = (approval as JsonRecord).session_id;
    if (typeof value === "string") return value;
  }
  return "";
}

function eventTurnId(event: AppEvent): string {
  const params = event.params ?? {};
  const direct = params.turn_id ?? params.run_id;
  return typeof direct === "string" ? direct : "";
}

function stableJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(stableJson).join(",")}]`;
  const record = value as JsonRecord;
  return `{${Object.keys(record)
    .sort()
    .map((key) => `${JSON.stringify(key)}:${stableJson(record[key])}`)
    .join(",")}}`;
}

function eventSemanticKey(event: AppEvent): string {
  const sessionId = eventSessionId(event);
  const turnId = eventTurnId(event);
  if (!sessionId || !turnId) return "";
  return `turn:${sessionId}:${turnId}:${event.method}:${stableJson(event.params ?? {})}`;
}

function eventKey(event: AppEvent): string {
  if (event.event_id) return `event:${event.event_id}`;
  const semantic = eventSemanticKey(event);
  if (semantic) return semantic;
  if (event.global_sequence) return `g:${event.global_sequence}`;
  if (event.sequence) return `s:${eventSessionId(event)}:${event.sequence}:${event.method}`;
  return `${event.method}:${event.created_at_ms ?? 0}:${JSON.stringify(event.params ?? {})}`;
}

function methodLabel(method: string): string {
  return method.replace("item/", "").replace("turn/", "").replace(/\//g, " ");
}

function statusClass(value?: string): string {
  if (!value) return "neutral";
  if (["completed", "healthy", "online", "idle", "ok", "listening", "polling", "resumed", "allowed", "answered"].includes(value)) {
    return "ok";
  }
  if (["running", "queued", "interrupting", "streaming", "waiting_approval", "waiting_question", "receiving", "connecting", "reconnecting", "pending"].includes(value)) {
    return "warn";
  }
  if (["failed", "interrupted", "expired", "missing", "offline", "denied", "dismissed", "error"].includes(value)) return "bad";
  return "neutral";
}

function stringField(record: JsonRecord | null | undefined, key: string): string {
  const value = record?.[key];
  return typeof value === "string" ? value : "";
}

function numberField(record: JsonRecord | null | undefined, key: string): number {
  const value = record?.[key];
  return typeof value === "number" ? value : 0;
}

function booleanField(record: JsonRecord | null | undefined, key: string): boolean {
  const value = record?.[key];
  return value === true || value === "true";
}

function compactId(value?: string): string {
  if (!value) return "-";
  return value.length > 28 ? `${value.slice(0, 18)}...${value.slice(-6)}` : value;
}

function compactPath(value?: string): string {
  if (!value) return "-";
  return value.length > 42 ? `${value.slice(0, 20)}...${value.slice(-18)}` : value;
}

function checkpointLabel(checkpoint: CheckpointSummary): string {
  return `${checkpoint.kind ?? "checkpoint"} · ${compactId(checkpoint.checkpoint_id)}`;
}

function fileBadge(entry: FileEntry): string {
  if (entry.kind === "dir") return "dir";
  if (entry.text) return "txt";
  return "bin";
}

function formatBytes(value?: number): string {
  if (!value) return "0 B";
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

function compactText(value: string, limit = 120): string {
  const normalized = value.replace(/\s+/g, " ").trim();
  if (normalized.length <= limit) return normalized;
  return `${normalized.slice(0, Math.max(0, limit - 1)).trimEnd()}…`;
}

function formatElapsed(startMs?: number, nowMs = Date.now()): string {
  if (!startMs || startMs > nowMs) return "";
  const totalSeconds = Math.max(0, Math.floor((nowMs - startMs) / 1000));
  if (totalSeconds < 60) return `${totalSeconds}s`;
  const totalMinutes = Math.floor(totalSeconds / 60);
  if (totalMinutes < 60) return `${totalMinutes}m`;
  const hours = Math.floor(totalMinutes / 60);
  const minutes = totalMinutes % 60;
  if (hours < 24) return minutes ? `${hours}h ${minutes}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  const remainingHours = hours % 24;
  return remainingHours ? `${days}d ${remainingHours}h` : `${days}d`;
}

function formatTime(value?: number): string {
  if (!value) return "-";
  return new Date(value).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" });
}

function turnSubmitState(status?: string, queued?: boolean): string {
  if (queued || status === "queued") return "queued";
  if (status === "running") return "running";
  return "idle";
}

function queuePositionForJob(job: TurnJobSummary, queuedJobs: TurnJobSummary[]): number {
  if (typeof job.queue_position === "number" && job.queue_position > 0) return job.queue_position;
  const sessionId = turnJobSessionId(job);
  const sameSessionQueued = queuedJobs.filter((item) => turnJobSessionId(item) === sessionId);
  const index = sameSessionQueued.findIndex((item) => turnJobId(item) === turnJobId(job));
  return index >= 0 ? index + 1 : 0;
}

function queueReasonLabel(reason?: string | null): string {
  if (reason === "global_worker_quota") return "worker quota";
  if (reason === "session_active") return "session active";
  if (reason === "recovered") return "recovered";
  return reason ? reason.replace(/_/g, " ") : "queued";
}

function queueReasonMessage(job: TurnJobSummary): string {
  if (job.queue_reason === "recovered") return "Recovered from the durable queue after runtime restart; waiting for a worker lease.";
  if (job.queue_reason === "global_worker_quota") return "Waiting for a runtime worker to free up.";
  if (job.queue_reason === "session_active") return "Waiting for the active turn in this session to finish.";
  if (job.payload_persisted) return "Queued payload is persisted and can recover after runtime restart.";
  return "Waiting for scheduler capacity.";
}

function schedulerValue(value: number | undefined, fallback = "-"): string {
  return typeof value === "number" && Number.isFinite(value) ? String(value) : fallback;
}

function schedulerDuration(valueMs: number | undefined): string {
  if (!valueMs || !Number.isFinite(valueMs)) return "-";
  if (valueMs < 1000) return `${Math.round(valueMs)}ms`;
  const seconds = Math.round(valueMs / 1000);
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.round(minutes / 60);
  return `${hours}h`;
}

function turnJobStatusLabel(job: TurnJobSummary): string {
  if (job.status === "expired") return "expired";
  if (job.status === "interrupted" && job.cancel_requested) return "stopped";
  if (job.cancel_requested && !isTurnJobTerminal(job)) return "stopping";
  return job.status ?? "unknown";
}

function turnJobLifecycleTone(job: TurnJobSummary): string {
  if (job.status === "expired" || job.status === "failed" || job.status === "interrupted") return "bad";
  if (job.queue_reason === "global_worker_quota") return "quota";
  if (job.queue_reason === "recovered") return "recovered";
  if (job.status === "queued" || job.cancel_requested) return "warn";
  return "neutral";
}

function turnJobLifecycleMessage(job: TurnJobSummary, scheduler: TurnSchedulerSummary): string {
  if (job.status === "expired") {
    const timeout = schedulerDuration(scheduler.turn_queue_timeout_ms);
    return timeout === "-"
      ? "Queued turn expired before a worker could resume it; the durable payload was removed."
      : `Queued turn expired after ${timeout} without a worker; the durable payload was removed.`;
  }
  if (job.status === "interrupted") {
    return job.cancel_requested
      ? "Stop was requested and the runtime marked this turn interrupted."
      : "Runtime no longer owns this turn; it was recovered as interrupted.";
  }
  if (job.cancel_requested) return "Stop requested; the runtime will interrupt this turn at the next safe point.";
  if (isTurnJobQueued(job)) return queueReasonMessage(job);
  return "";
}

function queueFullMessage(payload?: JsonRecord): string {
  const queued = typeof payload?.queued_count === "number" ? payload.queued_count : undefined;
  const maxQueued = typeof payload?.max_queued_turns_per_session === "number" ? payload.max_queued_turns_per_session : undefined;
  if (queued !== undefined && maxQueued !== undefined) {
    return `排队已满：当前会话已有 ${queued}/${maxQueued} 个等待任务。`;
  }
  return "排队已满：请等待当前任务完成后再提交。";
}

function messageKey(message: MessageWithParts, index: number): string {
  return message.info?.id || `${message.info?.role ?? "message"}:${message.info?.created_at_ms ?? index}`;
}

function messageRoleLabel(message: MessageWithParts): string {
  return message.info?.role || "message";
}

function messageContent(message: MessageWithParts): string {
  const parts = message.parts ?? [];
  const text = parts
    .filter((part) => part.kind === "text")
    .map((part) => valueText(part.content))
    .filter(Boolean)
    .join("\n\n");
  if (text.trim()) return text.trim();
  return "";
}

function interactionRequestKey(item: TrustHistoryItem | null): string {
  if (!item) return "";
  const identifier = item.requestId || item.callId;
  return identifier ? `${item.kind}:${identifier}` : "";
}

function isPendingInteractionStatus(status: string): boolean {
  return ["", "pending", "running", "waiting", "waiting_approval", "waiting_question"].includes(status);
}

function resolvedInteractionKeys(messages: MessageWithParts[]): Set<string> {
  const keys = new Set<string>();
  for (const message of messages) {
    for (const part of message.parts ?? []) {
      const item = interactionHistoryItem(part);
      const key = interactionRequestKey(item);
      if (key && item && !isPendingInteractionStatus(item.status)) {
        keys.add(key);
      }
    }
  }
  return keys;
}

function isSupersededPendingInteraction(part: MessagePart, resolvedKeys: Set<string>): boolean {
  const item = interactionHistoryItem(part);
  const key = interactionRequestKey(item);
  return Boolean(key && item && isPendingInteractionStatus(item.status) && resolvedKeys.has(key));
}

function visibleMessageParts(message: MessageWithParts, resolvedKeys: Set<string> = new Set()): MessagePart[] {
  return (message.parts ?? []).filter((part) => part.kind !== "text" && !isSupersededPendingInteraction(part, resolvedKeys));
}

function valueText(value: unknown): string {
  if (typeof value === "string") return value;
  if (value === null || value === undefined) return "";
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  if (typeof value === "object") {
    const record = value as JsonRecord;
    for (const key of ["text", "output", "error", "command", "path"]) {
      const field = record[key];
      if (typeof field === "string" && field.trim()) return field;
    }
    return JSON.stringify(value, null, 2);
  }
  return "";
}

function renderInlineText(text: string, keyPrefix: string) {
  return text.split(/(`[^`\n]+`)/g).map((part, index) => {
    if (part.startsWith("`") && part.endsWith("`") && part.length > 2) {
      return (
        <code className="inline-code" key={`${keyPrefix}:code:${index}`}>
          {part.slice(1, -1)}
        </code>
      );
    }
    return <Fragment key={`${keyPrefix}:text:${index}`}>{part}</Fragment>;
  });
}

function TextContent({ text }: { text: string }) {
  const blocks = text.split(/(```[\s\S]*?```)/g).filter((block) => block.length > 0);
  return (
    <div className="event-text">
      {blocks.map((block, blockIndex) => {
        if (block.startsWith("```") && block.endsWith("```")) {
          const raw = block.slice(3, -3).replace(/^\n/, "");
          const firstLineBreak = raw.indexOf("\n");
          const firstLine = firstLineBreak >= 0 ? raw.slice(0, firstLineBreak).trim() : "";
          const hasLanguage = /^[A-Za-z0-9_+.#-]{1,24}$/.test(firstLine);
          const language = hasLanguage ? firstLine : "";
          const code = hasLanguage && firstLineBreak >= 0 ? raw.slice(firstLineBreak + 1) : raw;
          return (
            <pre className="event-code-block" data-language={language || undefined} key={`code-block:${blockIndex}`}>
              <code>{code.trimEnd()}</code>
            </pre>
          );
        }
        return block.split(/\n{2,}/).map((paragraph, paragraphIndex) => {
          const lines = paragraph.split("\n");
          return (
            <p className="event-paragraph" key={`paragraph:${blockIndex}:${paragraphIndex}`}>
              {lines.map((line, lineIndex) => (
                <Fragment key={`line:${blockIndex}:${paragraphIndex}:${lineIndex}`}>
                  {lineIndex > 0 ? <br /> : null}
                  {renderInlineText(line, `${blockIndex}:${paragraphIndex}:${lineIndex}`)}
                </Fragment>
              ))}
            </p>
          );
        });
      })}
    </div>
  );
}

function jsonRecord(value: unknown): JsonRecord | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  return value as JsonRecord;
}

function jsonArray(value: unknown): JsonRecord[] {
  if (!Array.isArray(value)) return [];
  return value.filter((item): item is JsonRecord => Boolean(jsonRecord(item)));
}

function firstText(...values: unknown[]): string {
  for (const value of values) {
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

function partIcon(kind?: string) {
  if (kind === "tool") return <Wrench size={14} />;
  if (kind === "approval") return <ShieldCheck size={14} />;
  if (kind === "question") return <Bot size={14} />;
  if (kind === "patch") return <GitCompare size={14} />;
  if (kind === "context") return <History size={14} />;
  return <Database size={14} />;
}

function partTitle(part: MessagePart): string {
  const kind = part.kind ?? "part";
  const content = jsonRecord(part.content);
  if (kind === "tool") {
    const mcp = mcpToolTraceFromPart(part);
    if (mcp) return mcp.toolName ? `MCP: ${mcp.toolName}` : "MCP tool";
    const name = firstText(content?.name, part.attributes?.name);
    return name ? `Tool: ${name}` : "Tool result";
  }
  if (kind === "approval") return "Approval";
  if (kind === "question") return "Question";
  if (kind === "patch") return "Patch";
  if (kind === "context" && content?.kind === "checkpoint") return "Checkpoint";
  return kind;
}

function partSummary(part: MessagePart): string {
  const kind = part.kind ?? "part";
  const content = jsonRecord(part.content);
  if (kind === "tool") {
    const mcp = mcpToolTraceFromPart(part);
    if (mcp) {
      const result = mcp.error || mcp.output || part.status || "completed";
      const prefix = [mcp.server, mcp.transport].filter(Boolean).join(" · ");
      return prefix ? `${prefix}: ${result}` : result;
    }
    const error = firstText(content?.error);
    if (error) return error;
    return firstText(content?.output, content?.command) || part.status || "completed";
  }
  if (kind === "approval" || kind === "question") {
    return interactionHistoryItem(part)?.summary ?? part.status ?? "pending";
  }
  if (kind === "patch") {
    const added = numberField(content, "added") || numberField(part.attributes, "added");
    const modified = numberField(content, "modified") || numberField(part.attributes, "modified");
    const deleted = numberField(content, "deleted") || numberField(part.attributes, "deleted");
    return `+${added} ~${modified} -${deleted}`;
  }
  if (kind === "context" && content?.kind === "checkpoint") {
    return `${compactId(firstText(content.snapshot_start))} -> ${compactId(firstText(content.snapshot_end))}`;
  }
  return valueText(part.content) || part.status || "completed";
}

function nonEmptyRows(rows: Array<[string, string]>): Array<[string, string]> {
  return rows.filter(([, value]) => value && value !== "-");
}

function toolPartMetadata(part: MessagePart): JsonRecord {
  return nestedRecord(jsonRecord(part.content), "metadata") ?? {};
}

function mcpToolTraceFromPart(part: MessagePart): McpToolTrace | null {
  if (part.kind !== "tool") return null;
  const content = jsonRecord(part.content);
  const metadata = toolPartMetadata(part);
  const backend = firstText(metadata.backend);
  const server = firstText(metadata.mcp_server);
  const originalTool = firstText(metadata.mcp_original_tool_name);
  const transport = firstText(metadata.mcp_transport);
  const dynamicTool = firstText(metadata.mcp_tool_name, content?.name, part.attributes?.name);
  if (backend !== "mcp" && !server && !originalTool && !dynamicTool.startsWith("mcp_tool_")) return null;
  const nonTextBlocks = Array.isArray(metadata.mcp_non_text_blocks) ? metadata.mcp_non_text_blocks.length : 0;
  const lifecyclePid = numberField(metadata, "mcp_lifecycle_pid");
  return {
    toolName: originalTool || dynamicTool || "mcp tool",
    originalTool,
    dynamicTool,
    server,
    transport,
    callId: firstText(content?.call_id, part.attributes?.call_id),
    status: part.status ?? "completed",
    output: firstText(content?.output),
    error: firstText(content?.error),
    nonTextBlockCount: nonTextBlocks,
    lifecycleReused: booleanField(metadata, "mcp_lifecycle_reused"),
    lifecyclePid,
  };
}

function mcpToolTracesFromMessages(messages: MessageWithParts[]): McpToolTrace[] {
  const traces: McpToolTrace[] = [];
  for (const message of messages) {
    for (const part of message.parts ?? []) {
      const trace = mcpToolTraceFromPart(part);
      if (trace) traces.push(trace);
    }
  }
  return traces;
}

function mcpEndpointLabel(server: McpServerSummary): string {
  if (server.command) return `${server.command}${server.args_count ? ` +${server.args_count}` : ""}`;
  if (server.remote_url_configured) return "remote URL";
  return "no endpoint";
}

function mcpTransportLabel(server: McpServerSummary): string {
  const configured = server.transport ?? "";
  const selected = server.selected_transport ?? "";
  if (configured && selected && configured !== selected) return `${configured} -> ${selected}`;
  return selected || configured || "-";
}

function mcpToolLabel(tool: { name?: string; title?: string; original_name?: string }): string {
  return tool.title || tool.original_name || tool.name || "mcp tool";
}

function defaultMcpServerDraft(): McpServerDraft {
  return {
    mode: "remote",
    name: "",
    url: "",
    transport: "http",
    command: "",
    args: "",
    cwd: "",
    env: "",
    headers: "",
    timeoutMs: "",
  };
}

function mcpCheckedLabel(server: McpServerSummary): string {
  if (!server.last_refreshed_at) return "-";
  const value = server.last_refreshed_at;
  const milliseconds = value < 10_000_000_000 ? value * 1000 : value;
  return formatTime(milliseconds);
}

function mcpLifecycleTimeLabel(value?: number | null): string {
  if (!value) return "-";
  const milliseconds = value < 10_000_000_000 ? value * 1000 : value;
  return formatTime(milliseconds);
}

function mcpLifecycleStatusLabel(server: McpServerSummary): string {
  if (server.type !== "local") return "-";
  return server.lifecycle_status || "stopped";
}

function mcpLifecycleStatusClass(server: McpServerSummary): string {
  const status = mcpLifecycleStatusLabel(server);
  if (["running", "ready", "connected"].includes(status)) return "ok";
  if (["starting", "stopping", "restarting", "refreshing"].includes(status)) return "warn";
  if (["failed", "error", "exited", "stale"].includes(status)) return "bad";
  return "neutral";
}

function parseMcpList(value: string): string[] {
  return value
    .split(/\r?\n|,/)
    .map((item) => item.trim())
    .filter(Boolean);
}

function parseMcpMap(value: string, label: string): Record<string, string> {
  const result: Record<string, string> = {};
  const lines = value.split(/\r?\n/);
  for (const rawLine of lines) {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) continue;
    const equalsIndex = line.indexOf("=");
    const colonIndex = line.indexOf(":");
    const splitIndex =
      equalsIndex >= 0 && colonIndex >= 0
        ? Math.min(equalsIndex, colonIndex)
        : equalsIndex >= 0
          ? equalsIndex
          : colonIndex;
    if (splitIndex <= 0) {
      throw new Error(`${label} must use KEY=value lines.`);
    }
    const key = line.slice(0, splitIndex).trim();
    const next = line.slice(splitIndex + 1).trim();
    if (!key) throw new Error(`${label} contains an empty key.`);
    result[key] = next;
  }
  return result;
}

function partRows(part: MessagePart): Array<[string, string]> {
  const kind = part.kind ?? "part";
  const content = jsonRecord(part.content);
  const attributes = part.attributes ?? {};
  if (kind === "tool") {
    const mcp = mcpToolTraceFromPart(part);
    if (mcp) {
      return nonEmptyRows([
        ["Server", mcp.server],
        ["Tool", mcp.originalTool || mcp.toolName],
        ["Transport", mcp.transport],
        ["Call", compactId(mcp.callId)],
        ["Dynamic", compactId(mcp.dynamicTool)],
        ["Blocks", mcp.nonTextBlockCount ? String(mcp.nonTextBlockCount) : ""],
      ]);
    }
    return nonEmptyRows([
      ["Name", firstText(content?.name, attributes.name)],
      ["Call", compactId(firstText(content?.call_id, attributes.call_id))],
      ["Status", part.status ?? "completed"],
    ]);
  }
  if (kind === "approval" || kind === "question") {
    const item = interactionHistoryItem(part);
    return nonEmptyRows([
      ["Status", item?.status ?? part.status ?? "pending"],
      ["Request", compactId(item?.requestId)],
      ["Call", compactId(item?.callId)],
    ]);
  }
  if (kind === "patch") {
    return nonEmptyRows([
      ["Added", String(numberField(content, "added") || numberField(attributes, "added"))],
      ["Modified", String(numberField(content, "modified") || numberField(attributes, "modified"))],
      ["Deleted", String(numberField(content, "deleted") || numberField(attributes, "deleted"))],
      [
        "From",
        compactId(firstText(content?.before_checkpoint_id, attributes.before_checkpoint_id)),
      ],
      ["To", compactId(firstText(content?.after_checkpoint_id, attributes.after_checkpoint_id))],
    ]);
  }
  if (kind === "context" && content?.kind === "checkpoint") {
    return nonEmptyRows([
      ["Start", compactId(firstText(content.snapshot_start))],
      ["End", compactId(firstText(content.snapshot_end))],
    ]);
  }
  return [["Status", part.status ?? "completed"]];
}

function patchEntries(part: MessagePart): JsonRecord[] {
  const entries = jsonRecord(part.content)?.entries;
  if (!Array.isArray(entries)) return [];
  return entries.filter((entry): entry is JsonRecord => Boolean(jsonRecord(entry)));
}

function sideBySideRows(patch: JsonRecord | null | undefined): JsonRecord[] {
  const sideBySide = nestedRecord(patch, "side_by_side");
  return nestedArray(sideBySide, "rows");
}

function diffCellText(value: unknown): string {
  return typeof value === "string" ? value : "";
}

function partPreText(part: MessagePart): string {
  const kind = part.kind ?? "part";
  const content = jsonRecord(part.content);
  if (kind === "tool") return firstText(content?.error);
  if (kind === "patch" || (kind === "context" && content?.kind === "checkpoint")) return "";
  if (kind === "approval" || kind === "question") return "";
  return valueText(part.content);
}

function nestedRecord(record: JsonRecord | null | undefined, key: string): JsonRecord | null {
  return jsonRecord(record?.[key]);
}

function nestedArray(record: JsonRecord | null | undefined, key: string): JsonRecord[] {
  return jsonArray(record?.[key]);
}

function interactionQuestions(content: JsonRecord | null, request: JsonRecord | null): JsonRecord[] {
  const direct = nestedArray(content, "questions");
  if (direct.length) return direct;
  const requestQuestions = nestedArray(request, "questions");
  if (requestQuestions.length) return requestQuestions;
  const requestToolInput = nestedRecord(request, "tool_input");
  const toolQuestions = nestedArray(requestToolInput, "questions");
  if (toolQuestions.length) return toolQuestions;
  const wrappedQuestion = nestedRecord(content, "question") ?? nestedRecord(request, "question");
  return nestedArray(wrappedQuestion, "questions");
}

function interactionQuestionPrompt(content: JsonRecord | null, request: JsonRecord | null): string {
  const [first] = interactionQuestions(content, request);
  return firstText(first?.question, first?.header, request?.question, content?.question) || "Question";
}

function interactionTarget(content: JsonRecord | null, request: JsonRecord | null): string {
  const preview = nestedRecord(request, "preview") ?? nestedRecord(content, "preview");
  const toolInput = nestedRecord(request, "tool_input") ?? nestedRecord(content, "tool_input") ?? nestedRecord(content, "input");
  return firstText(
    preview?.path,
    preview?.command,
    toolInput?.file_path,
    toolInput?.path,
    toolInput?.command,
    content?.command,
    request?.permission_pattern,
  );
}

function interactionResolutionText(kind: "approval" | "question", resolution: JsonRecord | null, status: string): string {
  if (kind === "approval") {
    const action = firstText(resolution?.action, status);
    const scope = firstText(resolution?.scope);
    return scope ? `${action} · ${scope}` : action;
  }
  if (resolution?.dismissed === true || status === "dismissed") return "dismissed";
  const answers = resolution?.answers;
  if (Array.isArray(answers) && answers.length) {
    return answers
      .map((answer) => (Array.isArray(answer) ? answer.join(", ") : valueText(answer)))
      .filter(Boolean)
      .join("; ");
  }
  return status;
}

function interactionHistoryItem(part: MessagePart): TrustHistoryItem | null {
  const kind = part.kind === "approval" || part.kind === "question" ? part.kind : null;
  if (!kind) return null;
  const content = jsonRecord(part.content);
  const request = nestedRecord(content, "request") ?? nestedRecord(content, kind) ?? content;
  const resolution = nestedRecord(content, "resolution");
  const rawStatus = firstText(content?.status, part.attributes?.resolution_status, part.status, "pending");
  const status = rawStatus === "completed" ? "answered" : rawStatus;
  const callId = firstText(content?.call_id, request?.call_id, request?.tool_call_id, part.attributes?.call_id);
  const requestId = firstText(content?.request_id, request?.request_id, part.attributes?.request_id);
  const name = firstText(content?.name, request?.tool_name, request?.tool, part.attributes?.name, kind);
  const target = interactionTarget(content, request);
  const prompt = interactionQuestionPrompt(content, request);
  const resolutionText = interactionResolutionText(kind, resolution, status);
  const title = kind === "approval" ? `Approval · ${name}` : prompt;
  const summary =
    kind === "approval"
      ? status === "pending"
        ? `Waiting for permission to run ${name}`
        : `Permission ${resolutionText}`
      : status === "pending"
        ? "Waiting for user answer"
        : `Question ${resolutionText}`;
  const detail = kind === "approval" ? target || compactId(callId) : prompt;
  return {
    id: part.id ?? `${kind}:${requestId || callId || String(part.timestamp_ms ?? "")}`,
    kind,
    status,
    tone: statusClass(status) as TrustHistoryItem["tone"],
    title,
    summary,
    detail,
    requestId,
    callId,
  };
}

function trustHistoryFromMessages(messages: MessageWithParts[]): TrustHistoryItem[] {
  const resolvedKeys = resolvedInteractionKeys(messages);
  return messages
    .flatMap((message) => message.parts ?? [])
    .filter((part) => !isSupersededPendingInteraction(part, resolvedKeys))
    .map(interactionHistoryItem)
    .filter((item): item is TrustHistoryItem => Boolean(item))
    .reverse()
    .slice(0, 8);
}

function questionSchema(item: JsonRecord): JsonRecord | null {
  return nestedRecord(item, "schema") ?? nestedRecord(item, "json_schema") ?? nestedRecord(item, "input_schema");
}

function optionPrimitiveText(value: unknown): string {
  if (typeof value === "string") return value.trim();
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  const record = jsonRecord(value);
  if (record) {
    return optionPrimitiveText(record.value ?? record.id ?? record.name ?? record.label ?? record.title);
  }
  return "";
}

function questionItems(question?: JsonRecord): JsonRecord[] {
  const direct = jsonArray(question?.questions);
  if (direct.length) return direct;
  const toolInput = nestedRecord(question ?? null, "tool_input");
  const nested = nestedArray(toolInput, "questions");
  if (nested.length) return nested;
  const prompt = firstText(question?.question, question?.prompt, question?.message);
  return prompt ? [{ question: prompt }] : [{ question: "Answer" }];
}

function questionOption(option: unknown): ElicitationOption | null {
  if (typeof option === "string" || typeof option === "number" || typeof option === "boolean") {
    const value = optionPrimitiveText(option);
    return value ? { label: value, value } : null;
  }
  const record = jsonRecord(option);
  if (!record) return null;
  const value = optionPrimitiveText(record.value ?? record.id ?? record.name ?? record.label ?? record.title);
  if (!value) return null;
  return {
    label: firstText(record.label, record.title, record.name) || value,
    value,
  };
}

function questionOptions(item: JsonRecord): ElicitationOption[] {
  const schema = questionSchema(item);
  const itemSchema = nestedRecord(schema, "items");
  const rawOptions = [item.options, item.choices, item.enum, schema?.enum, itemSchema?.enum].find(Array.isArray);
  if (!Array.isArray(rawOptions)) return [];
  const seen = new Set<string>();
  return rawOptions
    .map(questionOption)
    .filter((option): option is ElicitationOption => Boolean(option))
    .filter((option) => {
      if (seen.has(option.value)) return false;
      seen.add(option.value);
      return true;
    });
}

function schemaTypeText(schema: JsonRecord | null): string {
  const raw = schema?.type;
  if (typeof raw === "string") return raw;
  if (Array.isArray(raw)) return raw.find((item): item is string => typeof item === "string") ?? "";
  return "";
}

function questionKind(item: JsonRecord, options: ElicitationOption[]): ElicitationFieldKind {
  const schema = questionSchema(item);
  const rawType = [
    item.type,
    item.input_type,
    item.control,
    item.kind,
    item.format,
    schemaTypeText(schema),
  ]
    .map((value) => (typeof value === "string" ? value.toLowerCase() : ""))
    .filter(Boolean)
    .join(" ");
  if (item.multiple === true || item.multi === true || item.multiselect === true) return "multiselect";
  if (rawType.includes("array") || rawType.includes("multi")) return "multiselect";
  if (rawType.includes("bool") || rawType.includes("checkbox") || rawType.includes("toggle") || rawType.includes("switch")) {
    return "boolean";
  }
  if (rawType.includes("integer") || rawType.includes("int")) return "integer";
  if (rawType.includes("number") || rawType.includes("float")) return "number";
  return options.length ? "select" : "text";
}

function questionBooleanText(value: unknown): string {
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") return value === 0 ? "false" : "true";
  if (typeof value !== "string") return "";
  const normalized = value.trim().toLowerCase();
  if (["true", "yes", "y", "1", "on", "allow", "enabled"].includes(normalized)) return "true";
  if (["false", "no", "n", "0", "off", "deny", "disabled"].includes(normalized)) return "false";
  return "";
}

function questionOptionValue(value: string, options: ElicitationOption[]): string {
  const trimmed = value.trim();
  const match = options.find((option) => option.value === trimmed || option.label === trimmed);
  return match?.value ?? trimmed;
}

function questionValuesFromUnknown(value: unknown, kind: ElicitationFieldKind, options: ElicitationOption[]): string[] {
  const rawValues =
    Array.isArray(value) || (kind === "multiselect" && typeof value === "string" && value.includes(","))
      ? (Array.isArray(value) ? value : value.split(","))
      : [value];
  const values = rawValues.map(optionPrimitiveText).map((item) => item.trim()).filter(Boolean);
  if (kind === "boolean") {
    const bool = questionBooleanText(values[0]);
    return bool ? [bool] : [];
  }
  if (kind === "select" || kind === "multiselect") {
    const seen = new Set<string>();
    return values
      .map((item) => questionOptionValue(item, options))
      .filter(Boolean)
      .filter((item) => {
        if (seen.has(item)) return false;
        seen.add(item);
        return true;
      });
  }
  return values.length ? [values[0]] : [];
}

function questionIsBarePrompt(item: JsonRecord): boolean {
  return Boolean(
    firstText(item.question) &&
      !firstText(item.id, item.key, item.name, item.field, item.header, item.label, item.title) &&
      item.type === undefined &&
      item.input_type === undefined &&
      item.options === undefined &&
      item.default === undefined &&
      item.default_value === undefined &&
      item.value === undefined &&
      item.answer === undefined,
  );
}

function questionDefaultValues(item: JsonRecord, kind: ElicitationFieldKind, options: ElicitationOption[]): string[] {
  for (const candidate of [item.answer, item.answers, item.default, item.default_value, item.value, item.values, item.selected, item.checked]) {
    if (candidate === undefined || candidate === null) continue;
    const values = questionValuesFromUnknown(candidate, kind, options);
    if (values.length) return values;
  }
  if (kind === "boolean") return ["false"];
  if (kind === "select" && options[0]) return [options[0].value];
  if (questionIsBarePrompt(item)) return ["yes"];
  return [];
}

function numberValue(value: unknown): number | undefined {
  if (typeof value === "number" && Number.isFinite(value)) return value;
  if (typeof value !== "string" || !value.trim()) return undefined;
  const parsed = Number(value.trim());
  return Number.isFinite(parsed) ? parsed : undefined;
}

function questionNumberBound(item: JsonRecord, keys: string[]): number | undefined {
  const schema = questionSchema(item);
  for (const key of keys) {
    const value = numberValue(item[key]) ?? numberValue(schema?.[key]);
    if (value !== undefined) return value;
  }
  return undefined;
}

function questionFieldId(requestId: string, item: JsonRecord, index: number): string {
  return firstText(item.id, item.key, item.name, item.field, `${requestId || "question"}:${index}`);
}

function questionFieldError(
  label: string,
  kind: ElicitationFieldKind,
  required: boolean,
  values: string[],
  options: ElicitationOption[],
  min?: number,
  max?: number,
): string {
  const nonEmpty = values.filter((value) => value.trim());
  if (required && kind !== "boolean" && nonEmpty.length === 0) return `${label} is required`;
  if (nonEmpty.length === 0) return "";
  if (kind === "select" || kind === "multiselect") {
    const valid = new Set(options.map((option) => option.value));
    const invalid = nonEmpty.find((value) => options.length > 0 && !valid.has(value));
    if (invalid) return `${label} has an invalid option`;
  }
  if (kind === "number" || kind === "integer") {
    const parsed = Number(nonEmpty[0]);
    if (!Number.isFinite(parsed)) return `${label} must be a number`;
    if (kind === "integer" && !Number.isInteger(parsed)) return `${label} must be an integer`;
    if (min !== undefined && parsed < min) return `${label} must be at least ${min}`;
    if (max !== undefined && parsed > max) return `${label} must be at most ${max}`;
  }
  return "";
}

function questionElicitationFields(
  question: JsonRecord | undefined,
  requestId: string,
  drafts: Record<string, string[][]>,
): ElicitationField[] {
  const draft = requestId ? drafts[requestId] ?? [] : [];
  return questionItems(question).map((item, index) => {
    const options = questionOptions(item);
    const kind = questionKind(item, options);
    const defaultValues = questionDefaultValues(item, kind, options);
    const values = questionValuesFromUnknown(draft[index] ?? defaultValues, kind, options);
    const value = values[0] ?? "";
    const label = firstText(item.header, item.label, item.title, item.question, `Question ${index + 1}`);
    const min = questionNumberBound(item, ["min", "minimum"]);
    const max = questionNumberBound(item, ["max", "maximum"]);
    return {
      id: questionFieldId(requestId, item, index),
      index,
      label,
      description: firstText(item.question, item.description, item.help),
      value,
      values,
      options,
      required: item.required !== false,
      kind,
      placeholder: firstText(item.placeholder, item.example),
      error: questionFieldError(label, kind, item.required !== false, values, options, min, max),
      min,
      max,
    };
  });
}

function questionAnswers(question: JsonRecord | undefined, requestId: string, drafts: Record<string, string[][]>): string[][] {
  return questionElicitationFields(question, requestId, drafts).map((field) => {
    if (field.kind === "multiselect") return field.values.filter((value) => value.trim());
    if (field.kind === "boolean") return [field.value === "true" ? "true" : "false"];
    const value = field.value.trim();
    return value ? [value] : [];
  });
}

function questionValidationErrors(question: JsonRecord | undefined, requestId: string, drafts: Record<string, string[][]>): string[] {
  return questionElicitationFields(question, requestId, drafts)
    .map((field) => field.error)
    .filter(Boolean);
}

function eventIcon(method: string) {
  if (method.includes("toolCall")) return <Wrench size={16} />;
  if (method.includes("question")) return <Bot size={16} />;
  if (method.includes("approval")) return <ShieldCheck size={16} />;
  if (method.includes("completed")) return <CheckCircle2 size={16} />;
  if (method.includes("failed") || method.includes("interrupted")) return <XCircle size={16} />;
  if (method.includes("agentMessage")) return <Bot size={16} />;
  return <Circle size={14} />;
}

function messageIcon(role?: string) {
  if (role === "assistant") return <Bot size={16} />;
  if (role === "tool") return <Wrench size={16} />;
  if (role === "system") return <Terminal size={16} />;
  return <Circle size={14} />;
}

function retryDelayMs(attempt: number): number {
  return Math.min(5000, 750 * Math.max(1, attempt));
}

function streamStateAfterEvents(events: AppEvent[], fallback: string): string {
  return events.reduce((state, event) => {
    const status = stringParam(event.params ?? {}, "status");
    if (event.method === "turn/completed") return "idle";
    if (event.method === "turn/failed") return "failed";
    if (event.method === "turn/interrupted") return "interrupted";
    if (event.method === "turn/approval_requested") return "waiting_approval";
    if (event.method === "item/question/requested") return "waiting_question";
    if (event.method === "turn/approval_resolved" || event.method === "item/question/resolved") {
      return status === "denied" || status === "dismissed" ? "idle" : "running";
    }
    if (event.method === "item/agentMessage/delta") return "streaming";
    if (event.method === "turn/started" || event.method === "item/toolCall/started") return "running";
    if (status === "queued" || status === "waiting_approval" || status === "waiting_question" || status === "running") return status;
    return state;
  }, fallback);
}

function activeTurnIdFromEvents(events: AppEvent[]): string {
  let activeTurnId = "";
  for (const event of events) {
    const turnId = eventTurnId(event);
    if (event.method === "turn/started" && turnId) {
      activeTurnId = turnId;
      continue;
    }
    if (
      turnId &&
      (event.method === "item/agentMessage/delta" ||
        event.method === "item/toolCall/started" ||
        event.method === "turn/approval_requested" ||
        event.method === "item/question/requested")
    ) {
      activeTurnId = turnId;
      continue;
    }
    if (
      turnId &&
      activeTurnId === turnId &&
      (event.method === "turn/completed" || event.method === "turn/failed" || event.method === "turn/interrupted")
    ) {
      activeTurnId = "";
    }
  }
  return activeTurnId;
}

function activeStreamingDraftFromEvents(events: AppEvent[]): StreamingDraft | null {
  let currentTurnId = "";
  let text = "";
  let eventCount = 0;
  let completed = false;
  for (const event of events) {
    const turnId = eventTurnId(event) || currentTurnId || "current";
    if (event.method === "turn/started") {
      currentTurnId = turnId;
      text = "";
      eventCount = 0;
      completed = false;
      continue;
    }
    if (event.method === "item/agentMessage/delta") {
      const delta = stringParam(event.params ?? {}, "delta");
      if (!delta) continue;
      if (turnId !== currentTurnId) {
        currentTurnId = turnId;
        text = "";
        eventCount = 0;
        completed = false;
      }
      text += delta;
      eventCount += 1;
      completed = false;
      continue;
    }
    if (
      currentTurnId &&
      (event.method === "turn/completed" ||
        event.method === "turn/failed" ||
        event.method === "turn/interrupted") &&
      turnId === currentTurnId
    ) {
      completed = true;
    }
  }
  if (!text) return null;
  return {
    turnId: currentTurnId || "current",
    text,
    eventCount,
    completed,
  };
}

function hasPersistedAssistantForTurn(messages: MessageWithParts[], turnId: string): boolean {
  return messages.some((message) => {
    if (message.info?.role !== "assistant") return false;
    if (!messageContent(message)) return false;
    const runId = message.info?.run_id;
    return !turnId || turnId === "current" || !runId || runId === turnId;
  });
}

function interactionEventChanged(method: string): boolean {
  return (
    method === "turn/approval_requested" ||
    method === "turn/approval_resolved" ||
    method === "item/question/requested" ||
    method === "item/question/resolved"
  );
}

function sessionEventChanged(method: string): boolean {
  return (
    method.startsWith("turn/") ||
    method.includes("toolCall") ||
    method.includes("question") ||
    method.includes("approval") ||
    method.includes("checkpoint") ||
    method.includes("patch")
  );
}

function eventSessionIds(events: AppEvent[]): Set<string> {
  return events.reduce((ids, event) => {
    const id = eventSessionId(event);
    if (id) ids.add(id);
    return ids;
  }, new Set<string>());
}

function restoredCheckpointIdFromEvents(events: AppEvent[]): string {
  for (const event of [...events].reverse()) {
    if (event.method !== "checkpoint/restored") continue;
    const value = event.params?.checkpoint_id;
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return "";
}

function isInitialBridgeFetchError(err: unknown): boolean {
  const message = err instanceof Error ? err.message : String(err);
  return message === "Failed to fetch" || message === "Load failed" || message.includes("NetworkError");
}

export function App() {
  const [bridgeUrl, setBridgeUrl] = useState(() => storedValue(STORAGE_BRIDGE, DEFAULT_BRIDGE));
  const [token, setToken] = useState(() => storedValue(STORAGE_TOKEN, ""));
  const [projects, setProjects] = useState<DesktopProject[]>(() => storedProjects());
  const [activeProjectPath, setActiveProjectPath] = useState(() => storedValue(STORAGE_ACTIVE_PROJECT, ""));
  const [projectPathInput, setProjectPathInput] = useState(() => storedValue(STORAGE_ACTIVE_PROJECT, ""));
  const [projectError, setProjectError] = useState("");
  const [projectBusy, setProjectBusy] = useState("");
  const [protocol, setProtocol] = useState<ProtocolPayload | null>(null);
  const [provider, setProvider] = useState<ProviderPayload | null>(null);
  const [mcp, setMcp] = useState<McpPayload | null>(null);
  const [mcpRefreshing, setMcpRefreshing] = useState(false);
  const [mcpServerDraft, setMcpServerDraft] = useState<McpServerDraft>(() => defaultMcpServerDraft());
  const [mcpMutationBusy, setMcpMutationBusy] = useState("");
  const [mcpMutationError, setMcpMutationError] = useState("");
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [approvals, setApprovals] = useState<PendingApproval[]>([]);
  const [questions, setQuestions] = useState<PendingQuestion[]>([]);
  const [interactionSync, setInteractionSync] = useState<InteractionSync>({});
  const [respondingInteractionId, setRespondingInteractionId] = useState("");
  const [questionDrafts, setQuestionDrafts] = useState<Record<string, string[][]>>({});
  const [restoringCheckpointId, setRestoringCheckpointId] = useState("");
  const [restoredCheckpointId, setRestoredCheckpointId] = useState("");
  const [sessionDiff, setSessionDiff] = useState<SessionDiff | null>(null);
  const [checkpoints, setCheckpoints] = useState<CheckpointsPayload | null>(null);
  const [fileTree, setFileTree] = useState<FilesPayload | null>(null);
  const [filePreview, setFilePreview] = useState<FilesPayload | null>(null);
  const [gitStatus, setGitStatus] = useState<GitPayload | null>(null);
  const [sessionMessages, setSessionMessages] = useState<SessionMessagesPayload | null>(null);
  const [turnJobs, setTurnJobs] = useState<TurnJobsPayload>({ turns: [], count: 0, running_count: 0, terminal_count: 0 });
  const [selectedTurnJobId, setSelectedTurnJobId] = useState("");
  const [activeSessionId, setActiveSessionId] = useState("");
  const [events, setEvents] = useState<AppEvent[]>([]);
  const [prompt, setPrompt] = useState("");
  const [permission, setPermission] = useState("PLAN_ONLY");
  const [model, setModel] = useState("");
  const [terminalCommand, setTerminalCommand] = useState("pwd");
  const [terminalResult, setTerminalResult] = useState<TerminalRunResult | null>(null);
  const [terminalBusy, setTerminalBusy] = useState(false);
  const [terminalError, setTerminalError] = useState("");
  const [connection, setConnection] = useState("offline");
  const [streamState, setStreamState] = useState("idle");
  const [activeTurnId, setActiveTurnId] = useState("");
  const [interruptingTurnId, setInterruptingTurnId] = useState("");
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [streamHealth, setStreamHealth] = useState<StreamHealth>({
    status: "idle",
    resume_cursor: 0,
    reconnect_attempts: 0,
    recovered_count: 0,
    last_batch_count: 0,
  });
  const [desktopRuntime, setDesktopRuntime] = useState(() => (isTauriRuntime() ? "tauri" : "web preview"));
  const [desktopDiagnostics, setDesktopDiagnostics] = useState<DesktopDiagnostics | null>(null);
  const [desktopDiagnosticError, setDesktopDiagnosticError] = useState("");
  const [desktopAuth, setDesktopAuth] = useState<DesktopAuthToken | null>(null);
  const [desktopAuthError, setDesktopAuthError] = useState("");
  const [desktopAuthReady, setDesktopAuthReady] = useState(() => !isTauriRuntime());
  const [managedBridge, setManagedBridge] = useState<ManagedBridgeStatus | null>(null);
  const [managedBridgeBusy, setManagedBridgeBusy] = useState("");
  const [managedBridgeError, setManagedBridgeError] = useState("");
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const [inspectorMode, setInspectorMode] = useState<"overview" | "review">("overview");
  const [error, setError] = useState("");
  const lastGlobalId = useRef(0);
  const streamReconnectAttempts = useRef(0);
  const managedBridgeAutoSyncKey = useRef("");
  const activeProject = projects.find((project) => sameProjectPath(project.path, activeProjectPath));
  const selectedProjectPath = activeProject?.path || activeProjectPath || desktopDiagnostics?.workspace_default || managedBridge?.workspace || "";
  const managedBridgeBusyAny = Boolean(managedBridgeBusy);
  const managedBridgeWorkspaceMismatch =
    Boolean(managedBridge?.running && managedBridge.workspace && selectedProjectPath) &&
    !sameProjectPath(managedBridge?.workspace, selectedProjectPath);
  const bridgeSwitchInProgress = isTauriRuntime() && (managedBridgeBusyAny || managedBridgeWorkspaceMismatch);
  const bridgeApiReady = desktopAuthReady && (!isTauriRuntime() || Boolean(managedBridge?.running));

  useEffect(() => {
    if (!isTauriRuntime()) return;
    invoke<DesktopDiagnostics>("desktop_diagnostics")
      .then((payload) => {
        setDesktopRuntime(payload.runtime || "tauri");
        setDesktopDiagnostics(payload);
        setDesktopDiagnosticError("");
        if (payload.bridge_default_url && !window.localStorage.getItem(STORAGE_BRIDGE)) {
          setBridgeUrl(payload.bridge_default_url);
        }
        if (payload.workspace_default) {
          const project = projectFromPath(payload.workspace_default);
          setProjects((current) => upsertProject(current, project));
          if (payload.workspace_default_source === "env") {
            setActiveProjectPath(project.path);
            setProjectPathInput(project.path);
          } else {
            setActiveProjectPath((current) => current || project.path);
            setProjectPathInput((current) => current || project.path);
          }
        }
      })
      .catch((diagnosticError) => {
        setDesktopRuntime("tauri");
        setDesktopDiagnosticError(
          diagnosticError instanceof Error ? diagnosticError.message : String(diagnosticError),
        );
      });
    invoke<ManagedBridgeStatus>("app_bridge_status")
      .then((payload) => {
        setManagedBridge(payload);
        setManagedBridgeError(payload.error ?? "");
        if (payload.running && payload.url) {
          setBridgeUrl(payload.url);
        }
      })
      .catch((statusError) => {
        setManagedBridgeError(statusError instanceof Error ? statusError.message : String(statusError));
      });
    invoke<DesktopAuthToken>("desktop_auth_token")
      .then((payload) => {
        setDesktopAuth(payload);
        setDesktopAuthError("");
        if (payload.token) {
          setToken(payload.token);
        }
      })
      .catch((authError) => {
        setDesktopAuthError(authError instanceof Error ? authError.message : String(authError));
      })
      .finally(() => {
        setDesktopAuthReady(true);
      });
  }, []);

  const api = useCallback(
    async <T,>(path: string, init: RequestInit = {}): Promise<T> => {
      const headers = new Headers(init.headers);
      headers.set("accept", headers.get("accept") ?? "application/json");
      if (init.body && !headers.has("content-type")) {
        headers.set("content-type", "application/json");
      }
      if (token.trim()) headers.set("authorization", `Bearer ${token.trim()}`);
      const response = await fetch(`${bridgeUrl.replace(/\/$/, "")}${path}`, {
        ...init,
        headers,
      });
      if (!response.ok) {
        let body: JsonRecord | undefined;
        const contentType = response.headers.get("content-type") ?? "";
        if (contentType.includes("application/json")) {
          const parsed = (await response.json().catch(() => null)) as unknown;
          if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
            body = parsed as JsonRecord;
          }
        }
        throw new ApiError(init.method ?? "GET", path, response.status, body);
      }
      return (await response.json()) as T;
    },
    [bridgeUrl, token],
  );

  const refreshMcp = useCallback(async () => {
    setMcpRefreshing(true);
    try {
      const payload = await api<McpPayload>("/api/mcp?refresh=true");
      setMcp(payload);
      setMcpMutationError("");
      setConnection("online");
      return payload;
    } catch (err) {
      const message = err instanceof Error ? err.message : String(err);
      setMcp({
        configured: false,
        enabled: false,
        server_count: 0,
        tool_count: 0,
        source: "unavailable",
        status: "error",
        error: message,
        servers: [],
      });
      if (!isInitialBridgeFetchError(err)) setError(message);
      throw err;
    } finally {
      setMcpRefreshing(false);
    }
  }, [api]);

  const commitMcpMutation = useCallback(
    async (busyKey: string, path: string, init: RequestInit) => {
      setMcpMutationBusy(busyKey);
      setMcpMutationError("");
      try {
        const payload = await api<McpPayload>(path, init);
        setMcp(payload);
        setConnection("online");
        return payload;
      } catch (err) {
        const message = err instanceof Error ? err.message : String(err);
        setMcpMutationError(message);
        return null;
      } finally {
        setMcpMutationBusy("");
      }
    },
    [api],
  );

  const addMcpServer = useCallback(
    async (event: FormEvent) => {
      event.preventDefault();
      const name = mcpServerDraft.name.trim();
      const url = mcpServerDraft.url.trim();
      const command = mcpServerDraft.command.trim();
      const transport = mcpServerDraft.transport.trim() || "http";
      if (!mcp?.writable) {
        setMcpMutationError("MCP config is read-only.");
        return;
      }
      if (!name) {
        setMcpMutationError("Name is required.");
        return;
      }
      if (mcpServerDraft.mode === "remote" && !url) {
        setMcpMutationError("Remote MCP URL is required.");
        return;
      }
      if (mcpServerDraft.mode === "local" && !command) {
        setMcpMutationError("Local MCP command is required.");
        return;
      }
      let env: Record<string, string>;
      let headers: Record<string, string>;
      try {
        env = parseMcpMap(mcpServerDraft.env, "Env");
        headers = parseMcpMap(mcpServerDraft.headers, "Headers");
      } catch (err) {
        setMcpMutationError(err instanceof Error ? err.message : String(err));
        return;
      }
      const timeoutText = mcpServerDraft.timeoutMs.trim();
      let timeoutMs: number | undefined;
      if (timeoutText) {
        const parsedTimeout = Number.parseInt(timeoutText, 10);
        if (!Number.isFinite(parsedTimeout) || parsedTimeout <= 0) {
          setMcpMutationError("Timeout must be a positive number of milliseconds.");
          return;
        }
        timeoutMs = parsedTimeout;
      }
      const body =
        mcpServerDraft.mode === "local"
          ? {
              name,
              type: "local",
              command,
              args: parseMcpList(mcpServerDraft.args),
              cwd: mcpServerDraft.cwd.trim() || undefined,
              env,
              headers,
              timeout_ms: timeoutMs,
              enabled: true,
            }
          : {
              name,
              type: "remote",
              url,
              transport,
              env,
              headers,
              timeout_ms: timeoutMs,
              enabled: true,
            };
      const payload = await commitMcpMutation("add", "/api/mcp/servers", {
        method: "POST",
        body: JSON.stringify(body),
      });
      if (payload) setMcpServerDraft(defaultMcpServerDraft());
    },
    [commitMcpMutation, mcp?.writable, mcpServerDraft],
  );

  const toggleMcpServer = useCallback(
    async (server: McpServerSummary) => {
      const name = server.name?.trim();
      if (!name) return;
      await commitMcpMutation(`toggle:${name}`, `/api/mcp/servers/${encodeURIComponent(name)}`, {
        method: "PATCH",
        body: JSON.stringify({ enabled: !server.enabled }),
      });
    },
    [commitMcpMutation],
  );

  const deleteMcpServer = useCallback(
    async (server: McpServerSummary) => {
      const name = server.name?.trim();
      if (!name) return;
      await commitMcpMutation(`delete:${name}`, `/api/mcp/servers/${encodeURIComponent(name)}`, {
        method: "DELETE",
      });
    },
    [commitMcpMutation],
  );

  const testMcpServer = useCallback(
    async (server: McpServerSummary) => {
      const name = server.name?.trim();
      if (!name) return;
      await commitMcpMutation(`test:${name}`, `/api/mcp/servers/${encodeURIComponent(name)}/test`, {
        method: "POST",
      });
    },
    [commitMcpMutation],
  );

  const controlMcpServerLifecycle = useCallback(
    async (server: McpServerSummary, action: "start" | "stop" | "restart") => {
      const name = server.name?.trim();
      if (!name || server.type !== "local") return;
      await commitMcpMutation(`lifecycle:${action}:${name}`, `/api/mcp/servers/${encodeURIComponent(name)}/${action}`, {
        method: "POST",
      });
    },
    [commitMcpMutation],
  );

  const addEvents = useCallback((incoming: AppEvent[]) => {
    if (!incoming.length) return;
    lastGlobalId.current = incoming.reduce((cursor, event) => {
      return Math.max(cursor, event.global_sequence ?? 0);
    }, lastGlobalId.current);

    setEvents((current) => {
      const seen = new Set(current.map(eventKey));
      const next = [...current];
      for (const event of incoming) {
        const key = eventKey(event);
        if (seen.has(key)) continue;
        seen.add(key);
        next.push(event);
      }
      return next.slice(-300);
    });
  }, []);

  const refreshWorkspaceContext = useCallback(
    async (focusPath = "") => {
      const normalizedPath = focusPath.trim();
      const previewPath = normalizedPath ? `?path=${encodeURIComponent(normalizedPath)}&content=true` : "?depth=2";
      const [treePayload, previewPayload, gitPayload] = await Promise.all([
        api<FilesPayload>("/api/files?depth=2"),
        api<FilesPayload>(`/api/files${previewPath}`),
        api<GitPayload>("/api/git"),
      ]);
      setFileTree(treePayload);
      setFilePreview(previewPayload);
      setGitStatus(gitPayload);
    },
    [api],
  );

  const refreshSessionMessages = useCallback(
    async (session: string) => {
      if (!session) {
        setSessionMessages(null);
        return;
      }
      const payload = await api<SessionMessagesPayload>(`/api/sessions/${session}/messages?limit=100`);
      setSessionMessages(payload);
    },
    [api],
  );

  const refreshInteractions = useCallback(
    async (lastEventMethod = "") => {
      const [approvalsPayload, questionsPayload] = await Promise.all([
        api<{ approvals?: PendingApproval[] }>("/api/approvals"),
        api<{ questions?: PendingQuestion[] }>("/api/questions"),
      ]);
      setApprovals(approvalsPayload.approvals ?? []);
      setQuestions(questionsPayload.questions ?? []);
      setInteractionSync({
        last_synced_at_ms: Date.now(),
        last_event_method: lastEventMethod || undefined,
      });
    },
    [api],
  );

  const refreshTurnJobs = useCallback(async () => {
    const payload = await api<TurnJobsPayload>("/api/turns").catch((err: unknown) => ({
      turns: [],
      count: 0,
      running_count: 0,
      terminal_count: 0,
      source: "unavailable",
      error: err instanceof Error ? err.message : String(err),
    }));
    const normalized = normalizeTurnJobs(payload);
    setTurnJobs(normalized);
    return normalized;
  }, [api]);

  const refreshSessionTrust = useCallback(
    async (session: string) => {
      if (!session) {
        setSessionDiff(null);
        setCheckpoints(null);
        await refreshWorkspaceContext();
        return;
      }
      const [diffPayload, checkpointsPayload] = await Promise.all([
        api<SessionDiff>(`/api/sessions/${session}/diff`),
        api<CheckpointsPayload>(`/api/sessions/${session}/checkpoints`),
      ]);
      setSessionDiff(diffPayload);
      setCheckpoints(checkpointsPayload);
      await refreshWorkspaceContext(stringField(diffPayload.latest ?? undefined, "path"));
    },
    [api, refreshWorkspaceContext],
  );

  const refresh = useCallback(async () => {
    setError("");
    const [protocolPayload, providerPayload, mcpPayload, sessionsPayload, approvalsPayload, questionsPayload, turnJobsPayload] = await Promise.all([
      api<ProtocolPayload>("/api/protocol"),
      api<ProviderPayload>("/api/models?check=true").catch((err: unknown) => ({
        healthy: false,
        model_endpoint_ok: false,
        provider: "openai",
        model: model || undefined,
        error: err instanceof Error ? err.message : String(err),
        models: [],
      })),
      api<McpPayload>("/api/mcp?refresh=true").catch((err: unknown) => ({
        configured: false,
        enabled: false,
        server_count: 0,
        tool_count: 0,
        source: "unavailable",
        status: "error",
        error: err instanceof Error ? err.message : String(err),
        servers: [],
      })),
      api<{ sessions?: SessionSummary[] }>("/api/sessions"),
      api<{ approvals?: PendingApproval[] }>("/api/approvals"),
      api<{ questions?: PendingQuestion[] }>("/api/questions"),
      api<TurnJobsPayload>("/api/turns").catch((err: unknown) => ({
        turns: [],
        count: 0,
        running_count: 0,
        terminal_count: 0,
        source: "unavailable",
        error: err instanceof Error ? err.message : String(err),
      })),
    ]);
    setProtocol(protocolPayload);
    setProvider(providerPayload);
    setMcp(mcpPayload);
    setTurnJobs(normalizeTurnJobs(turnJobsPayload));
    setApprovals(approvalsPayload.approvals ?? []);
    setQuestions(questionsPayload.questions ?? []);
    setInteractionSync({ last_synced_at_ms: Date.now(), last_event_method: "refresh" });
    setModel((current) => current || providerPayload.model || "");
    const records = sessionsPayload.sessions ?? [];
    setSessions(records);
    setActiveSessionId((current) => {
      const currentRecord = records.find((session) => sessionId(session) === current);
      if (
        currentRecord &&
        (!selectedProjectPath || !currentRecord.workspace || sameProjectPath(currentRecord.workspace, selectedProjectPath))
      ) {
        return current;
      }
      const preferred = selectedProjectPath
        ? records.find((session) => sameProjectPath(session.workspace, selectedProjectPath))
        : undefined;
      return sessionId(preferred ?? records[0] ?? {});
    });
    await refreshWorkspaceContext();
    setConnection("online");
  }, [api, model, refreshWorkspaceContext, selectedProjectPath]);

  const createSession = useCallback(async () => {
    if (bridgeSwitchInProgress) {
      throw new Error("App Bridge is switching to the selected project. Try again in a moment.");
    }
    const payload = await api<{ session_id?: string; id?: string }>("/api/sessions", {
      method: "POST",
      body: JSON.stringify({
        cwd: selectedProjectPath || undefined,
        title: activeProject?.name ? `${activeProject.name} session` : undefined,
      }),
    });
    const id = payload.session_id ?? payload.id ?? "";
    await refresh();
    setActiveSessionId(id);
    return id;
  }, [activeProject?.name, api, bridgeSwitchInProgress, refresh, selectedProjectPath]);

  const refreshFromEvents = useCallback(
    async (incoming: AppEvent[]) => {
      if (!incoming.length) return;
      const methods = incoming.map((event) => event.method);
      const touchedSessions = eventSessionIds(incoming);
      const lastInteractionMethod = [...methods].reverse().find(interactionEventChanged) ?? "";
      const interactionChanged = Boolean(lastInteractionMethod);
      const sessionChanged = methods.some(sessionEventChanged);
      const restoredId = restoredCheckpointIdFromEvents(incoming);
      if (restoredId) setRestoredCheckpointId(restoredId);
      const activeTouched = activeSessionId
        ? touchedSessions.size === 0 || touchedSessions.has(activeSessionId)
        : false;
      const tasks: Array<Promise<unknown>> = [];

      if (interactionChanged) tasks.push(refreshInteractions(lastInteractionMethod));
      if (sessionChanged) tasks.push(refreshTurnJobs());
      if (sessionChanged) tasks.push(api<{ sessions?: SessionSummary[] }>("/api/sessions").then((payload) => {
        const records = payload.sessions ?? [];
        setSessions(records);
        if (!activeSessionId && touchedSessions.size > 0) {
          const touched = records.find((session) => touchedSessions.has(sessionId(session)));
          const touchedId = sessionId(touched ?? {});
          if (touchedId) {
            setActiveSessionId(touchedId);
            void refreshSessionMessages(touchedId);
            void refreshSessionTrust(touchedId);
          }
        }
      }));
      if (activeTouched && sessionChanged) {
        tasks.push(refreshSessionMessages(activeSessionId));
        tasks.push(refreshSessionTrust(activeSessionId));
      }

      if (tasks.length === 0) return;
      const results = await Promise.allSettled(tasks);
      const rejected = results.find((result) => result.status === "rejected");
      if (rejected && rejected.status === "rejected") {
        setInteractionSync((current) => ({
          ...current,
          last_event_method: `sync failed: ${rejected.reason instanceof Error ? rejected.reason.message : String(rejected.reason)}`,
        }));
      }
    },
    [
      activeSessionId,
      api,
      refreshInteractions,
      refreshSessionMessages,
      refreshSessionTrust,
      refreshTurnJobs,
    ],
  );

  const respondApproval = useCallback(
    async (approval: PendingApproval, action: "allow" | "deny") => {
      const requestId = approval.request_id;
      if (!requestId) return;
      setError("");
      setRespondingInteractionId(requestId);
      try {
        const payload = await api<{ events?: AppEvent[] }>(`/api/approvals/${requestId}`, {
          method: "POST",
          body: JSON.stringify({ action, scope: "once" }),
        });
        const incoming = payload.events ?? [];
        addEvents(incoming);
        if (incoming.length) {
          await refreshFromEvents(incoming);
        } else {
          await refreshInteractions("turn/approval_resolved");
          await refreshSessionMessages(approval.session_id || activeSessionId);
          await refreshSessionTrust(approval.session_id || activeSessionId);
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setRespondingInteractionId("");
      }
    },
    [
      activeSessionId,
      addEvents,
      api,
      refreshFromEvents,
      refreshInteractions,
      refreshSessionMessages,
      refreshSessionTrust,
    ],
  );

  const updateQuestionDraft = useCallback((requestId: string | undefined, index: number, fieldValues: string[]) => {
    if (!requestId) return;
    setQuestionDrafts((current) => {
      const next = { ...current };
      const draftValues = [...(next[requestId] ?? [])];
      draftValues[index] = fieldValues;
      next[requestId] = draftValues;
      return next;
    });
  }, []);

  const respondQuestion = useCallback(
    async (question: PendingQuestion, dismissed = false) => {
      const requestId = question.request_id;
      if (!requestId) return;
      setError("");
      if (!dismissed) {
        const validationErrors = questionValidationErrors(question.question, requestId, questionDrafts);
        if (validationErrors.length) {
          setError(validationErrors[0]);
          return;
        }
      }
      setRespondingInteractionId(requestId);
      try {
        const payload = await api<{ events?: AppEvent[] }>(`/api/questions/${requestId}/reply`, {
          method: "POST",
          body: JSON.stringify(
            dismissed
              ? { dismissed: true, note: "dismissed from Desktop" }
              : { answers: questionAnswers(question.question, requestId, questionDrafts) },
          ),
        });
        const incoming = payload.events ?? [];
        addEvents(incoming);
        if (incoming.length) {
          await refreshFromEvents(incoming);
        } else {
          await refreshInteractions("item/question/resolved");
          await refreshSessionMessages(question.session_id || activeSessionId);
          await refreshSessionTrust(question.session_id || activeSessionId);
        }
        setQuestionDrafts((current) => {
          const next = { ...current };
          delete next[requestId];
          return next;
        });
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setRespondingInteractionId("");
      }
    },
    [
      activeSessionId,
      addEvents,
      api,
      questionDrafts,
      refreshFromEvents,
      refreshInteractions,
      refreshSessionMessages,
      refreshSessionTrust,
    ],
  );

  const runPatchAction = useCallback(
    async (action: "undo" | "redo") => {
      if (!activeSessionId) return;
      setError("");
      try {
        const payload = await api<{ events?: AppEvent[] }>(`/api/sessions/${activeSessionId}/${action}`, {
          method: "POST",
        });
        addEvents(payload.events ?? []);
        await refreshSessionMessages(activeSessionId);
        await refreshSessionTrust(activeSessionId);
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      }
    },
    [activeSessionId, addEvents, api, refreshSessionMessages, refreshSessionTrust],
  );

  const restoreCheckpoint = useCallback(
    async (checkpointId?: string) => {
      if (!activeSessionId || !checkpointId) return;
      setError("");
      setRestoringCheckpointId(checkpointId);
      try {
        const payload = await api<{ events?: AppEvent[] }>(
          `/api/sessions/${activeSessionId}/checkpoints/${checkpointId}/restore`,
          { method: "POST" },
        );
        const incoming = payload.events ?? [];
        addEvents(incoming);
        if (incoming.length) {
          await refreshFromEvents(incoming);
        } else {
          setRestoredCheckpointId(checkpointId);
          await refreshSessionMessages(activeSessionId);
          await refreshSessionTrust(activeSessionId);
        }
      } catch (err) {
        setError(err instanceof Error ? err.message : String(err));
      } finally {
        setRestoringCheckpointId("");
      }
    },
    [activeSessionId, addEvents, api, refreshFromEvents, refreshSessionMessages, refreshSessionTrust],
  );

  const runTerminalCommand = useCallback(
    async (event: FormEvent) => {
      event.preventDefault();
      const command = terminalCommand.trim();
      if (!command) return;
      setTerminalBusy(true);
      setTerminalError("");
      try {
        const payload = await api<TerminalRunResult>("/api/terminal/run", {
          method: "POST",
          body: JSON.stringify({ command }),
        });
        setTerminalResult(payload);
      } catch (err) {
        setTerminalError(err instanceof Error ? err.message : String(err));
      } finally {
        setTerminalBusy(false);
      }
    },
    [api, terminalCommand],
  );

  const submitPrompt = useCallback(
    async (event: FormEvent) => {
      event.preventDefault();
      if (bridgeSwitchInProgress) {
        setError("App Bridge is switching to the selected project. Try again in a moment.");
        return;
      }
      const text = prompt.trim();
      if (!text) return;
      setError("");
      setStreamState("running");
      try {
        const session = activeSessionId || (await createSession());
        const payload = await api<{
          events?: AppEvent[];
          status?: string;
          turn_id?: string;
          queued?: boolean;
          queue_position?: number;
          queue_reason?: string;
          scheduler?: TurnSchedulerSummary;
        }>(`/api/sessions/${session}/turns`, {
          method: "POST",
          body: JSON.stringify({
            input: text,
            model: model || undefined,
            permission,
            stream: true,
            async: true,
          }),
        });
        const incoming = payload.events ?? [];
        const hasStreamedDeltas = incoming.some((event) => event.method === "item/agentMessage/delta");
        if (payload.turn_id) {
          setActiveTurnId(payload.turn_id);
          setTurnJobs((current) =>
            upsertTurnJob(current, {
              session_id: session,
              turn_id: payload.turn_id,
              status: payload.status ?? (payload.queued ? "queued" : "running"),
              queue_position: payload.queue_position,
              queue_reason: payload.queue_reason,
              started_at_ms: Date.now(),
              updated_at_ms: Date.now(),
            }),
          );
        }
        addEvents(incoming);
        setStreamState(streamStateAfterEvents(incoming, turnSubmitState(payload.status, payload.queued)));
        setPrompt("");
        if (hasStreamedDeltas) {
          await nextPaint();
          await sleepMs(320);
        }
        window.setTimeout(() => {
          refresh().catch((err: unknown) => {
            if (!isInitialBridgeFetchError(err)) setError(err instanceof Error ? err.message : String(err));
          });
          refreshTurnJobs().catch((err: unknown) => {
            if (!isInitialBridgeFetchError(err)) setError(err instanceof Error ? err.message : String(err));
          });
          refreshSessionMessages(session).catch((err: unknown) => {
            if (!isInitialBridgeFetchError(err)) setError(err instanceof Error ? err.message : String(err));
          });
          refreshSessionTrust(session).catch((err: unknown) => {
            if (!isInitialBridgeFetchError(err)) setError(err instanceof Error ? err.message : String(err));
          });
        }, 250);
      } catch (err) {
        if (err instanceof ApiError && err.body?.error_code === "turn_queue_full") {
          setStreamState("idle");
          setError(queueFullMessage(err.body));
          await refreshTurnJobs().catch(() => undefined);
        } else {
          setStreamState("failed");
          setError(err instanceof Error ? err.message : String(err));
        }
      }
    },
    [
      activeSessionId,
      addEvents,
      api,
      bridgeSwitchInProgress,
      createSession,
      model,
      permission,
      prompt,
      refresh,
      refreshSessionMessages,
      refreshSessionTrust,
      refreshTurnJobs,
    ],
  );

  const interruptTurn = useCallback(async (turnId: string) => {
    if (!turnId || interruptingTurnId) return;
    setError("");
    setInterruptingTurnId(turnId);
    setTurnJobs((current) =>
      upsertTurnJob(current, {
        turn_id: turnId,
        status: "interrupting",
        cancel_requested: true,
        cancel_requested_at_ms: Date.now(),
        updated_at_ms: Date.now(),
      }),
    );
    try {
      const payload = await api<{ events?: AppEvent[]; status?: string; job?: TurnJobSummary }>(`/api/turns/${turnId}/interrupt`, {
        method: "POST",
      });
      const incoming = payload.events ?? [];
      addEvents(incoming);
      setStreamState(streamStateAfterEvents(incoming, payload.status ?? "interrupted"));
      await refreshFromEvents(incoming);
      setTurnJobs((current) =>
        upsertTurnJob(current, {
          ...(payload.job ?? {}),
          turn_id: turnId,
          status: payload.status ?? payload.job?.status ?? "interrupted",
          cancel_requested: true,
          updated_at_ms: Date.now(),
        }),
      );
      if (turnId === activeTurnId) setActiveTurnId("");
      await refreshTurnJobs();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setInterruptingTurnId("");
    }
  }, [activeTurnId, addEvents, api, interruptingTurnId, refreshFromEvents, refreshTurnJobs]);

  const interruptActiveTurn = useCallback(async () => {
    await interruptTurn(activeTurnId);
  }, [activeTurnId, interruptTurn]);

  useEffect(() => {
    const timer = window.setInterval(() => setNowMs(Date.now()), 30_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    window.localStorage.setItem(STORAGE_BRIDGE, bridgeUrl);
  }, [bridgeUrl]);

  useEffect(() => {
    window.localStorage.setItem(STORAGE_TOKEN, token);
  }, [token]);

  useEffect(() => {
    window.localStorage.setItem(STORAGE_PROJECTS, JSON.stringify(projects));
  }, [projects]);

  useEffect(() => {
    if (activeProjectPath) {
      window.localStorage.setItem(STORAGE_ACTIVE_PROJECT, activeProjectPath);
    }
  }, [activeProjectPath]);

  useEffect(() => {
    if (!bridgeApiReady) return;
    refreshSessionMessages(activeSessionId).catch((err: unknown) => {
      if (!isInitialBridgeFetchError(err)) setError(err instanceof Error ? err.message : String(err));
    });
  }, [activeSessionId, bridgeApiReady, refreshSessionMessages]);

  useEffect(() => {
    if (!bridgeApiReady) return;
    refreshSessionTrust(activeSessionId).catch((err: unknown) => {
      if (!isInitialBridgeFetchError(err)) setError(err instanceof Error ? err.message : String(err));
    });
  }, [activeSessionId, bridgeApiReady, refreshSessionTrust]);

  useEffect(() => {
    if (!bridgeApiReady) return;
    refresh().catch((err: unknown) => {
      setConnection("offline");
      if (!isInitialBridgeFetchError(err)) setError(err instanceof Error ? err.message : String(err));
    });
  }, [bridgeApiReady, refresh]);

  useEffect(() => {
    if (!bridgeApiReady) return;
    let cancelled = false;
    let controller: AbortController | null = null;

    async function readLoop() {
      while (!cancelled) {
        controller = new AbortController();
        const resumeCursor = lastGlobalId.current;
        setStreamHealth((current) => ({
          ...current,
          status: streamReconnectAttempts.current > 0 ? "reconnecting" : "polling",
          resume_cursor: resumeCursor,
        }));
        try {
          const headers = new Headers({ accept: "text/event-stream" });
          if (token.trim()) headers.set("authorization", `Bearer ${token.trim()}`);
          const response = await fetch(
            `${bridgeUrl.replace(/\/$/, "")}/api/events?last_event_id=${resumeCursor}&live_timeout_ms=300000`,
            { headers, signal: controller.signal },
          );
          if (!response.ok) throw new Error(`events ${response.status}`);
          const parsed = await readSse(response, async (incoming) => {
            if (cancelled) return;
            addEvents(incoming);
            setStreamState((current) => streamStateAfterEvents(incoming, current));
            setStreamHealth((current) => ({
              ...current,
              status: "receiving",
              resume_cursor: lastGlobalId.current,
              last_batch_count: incoming.length,
            }));
            await refreshFromEvents(incoming);
          });
          const recovered = streamReconnectAttempts.current > 0;
          streamReconnectAttempts.current = 0;
          setConnection("online");
          setStreamHealth((current) => ({
            ...current,
            status: recovered ? "resumed" : "listening",
            resume_cursor: lastGlobalId.current,
            reconnect_attempts: 0,
            recovered_count: current.recovered_count + (recovered ? 1 : 0),
            last_batch_count: parsed.length,
            last_error: undefined,
            last_connected_at_ms: Date.now(),
            next_retry_ms: undefined,
          }));
        } catch (err) {
          if (!cancelled) {
            streamReconnectAttempts.current += 1;
            const attempts = streamReconnectAttempts.current;
            const retryMs = retryDelayMs(attempts);
            setConnection("offline");
            setStreamHealth((current) => ({
              ...current,
              status: "reconnecting",
              resume_cursor: lastGlobalId.current,
              reconnect_attempts: attempts,
              last_error: err instanceof Error ? err.message : String(err),
              next_retry_ms: retryMs,
            }));
            await new Promise((resolve) => window.setTimeout(resolve, retryMs));
          }
        }
      }
    }

    readLoop();
    return () => {
      cancelled = true;
      controller?.abort();
    };
  }, [addEvents, bridgeApiReady, bridgeUrl, refreshFromEvents, token]);

  const activeEvents = useMemo(() => {
    if (!activeSessionId) return events;
    const filtered = events.filter((event) => {
      const id = eventSessionId(event);
      return !id || id === activeSessionId;
    });
    return filtered.length ? filtered : events;
  }, [activeSessionId, events]);
  const eventActiveTurnId = useMemo(() => activeTurnIdFromEvents(activeEvents), [activeEvents]);
  const activeTurnTerminalSeen = useMemo(
    () =>
      Boolean(
        activeTurnId &&
          activeEvents.some((event) => {
            const method = event.method;
            return (
              eventTurnId(event) === activeTurnId &&
              (method === "turn/completed" || method === "turn/failed" || method === "turn/interrupted")
            );
          }),
      ),
    [activeEvents, activeTurnId],
  );
  useEffect(() => {
    if (eventActiveTurnId && eventActiveTurnId !== activeTurnId) {
      setActiveTurnId(eventActiveTurnId);
      return;
    }
    if (activeTurnTerminalSeen) {
      setActiveTurnId("");
      setInterruptingTurnId("");
    }
  }, [activeTurnId, activeTurnTerminalSeen, eventActiveTurnId]);
  const rawStreamingDraft = useMemo(() => activeStreamingDraftFromEvents(activeEvents), [activeEvents]);
  const visibleLiveEvents = useMemo(
    () => activeEvents.filter((event) => event.method !== "item/agentMessage/delta"),
    [activeEvents],
  );

  const activeMessages = sessionMessages?.messages_v2 ?? [];
  const activeStreamingDraft = useMemo(() => {
    if (!rawStreamingDraft) return null;
    if (rawStreamingDraft.completed && hasPersistedAssistantForTurn(activeMessages, rawStreamingDraft.turnId)) {
      return null;
    }
    return rawStreamingDraft;
  }, [activeMessages, rawStreamingDraft]);
  const activeResolvedInteractionKeys = useMemo(() => resolvedInteractionKeys(activeMessages), [activeMessages]);
  const trustHistory = useMemo(() => trustHistoryFromMessages(activeMessages), [activeMessages]);
  const modelOptions = provider?.models?.map((item) => item.id).filter(Boolean) as string[] | undefined;
  const mcpServers = mcp?.servers ?? [];
  const mcpToolTraces = useMemo(() => mcpToolTracesFromMessages(activeMessages), [activeMessages]);
  const latestMcpToolTrace = mcpToolTraces[mcpToolTraces.length - 1];
  const mcpStatus = mcp?.status ?? (mcp?.configured ? "idle" : "unconfigured");
  const mcpStatusClass = mcp?.error ? "bad" : mcp?.enabled ? "ok" : mcp?.configured ? "neutral" : "missing";
  const mcpWritable = Boolean(mcp?.writable);
  const mcpConfigPath = mcp?.config_path ?? "";
  const activeSession = sessions.find((session) => sessionId(session) === activeSessionId);
  const visibleSessions = selectedProjectPath
    ? sessions.filter((session) => sameProjectPath(session.workspace, selectedProjectPath))
    : sessions;
  const sessionById = useMemo(() => {
    const records = new Map<string, SessionSummary>();
    for (const session of sessions) {
      const id = sessionId(session);
      if (id) records.set(id, session);
    }
    return records;
  }, [sessions]);
  const visibleTurnJobs = useMemo(() => {
    const jobs = turnJobs.turns ?? [];
    return jobs
      .filter((job) => {
        if (!selectedProjectPath) return true;
        const session = sessionById.get(turnJobSessionId(job));
        return !session?.workspace || sameProjectPath(session.workspace, selectedProjectPath);
      })
      .sort((left, right) => {
        const rightTime = right.updated_at_ms ?? right.started_at_ms ?? 0;
        const leftTime = left.updated_at_ms ?? left.started_at_ms ?? 0;
        return rightTime - leftTime;
      });
  }, [selectedProjectPath, sessionById, turnJobs.turns]);
  const activeTurnJobs = visibleTurnJobs.filter((job) => !isTurnJobTerminal(job));
  const queuedTurnJobs = activeTurnJobs
    .filter(isTurnJobQueued)
    .sort((left, right) => {
      const leftTime = left.started_at_ms ?? left.updated_at_ms ?? 0;
      const rightTime = right.started_at_ms ?? right.updated_at_ms ?? 0;
      return leftTime - rightTime;
    });
  const runningTurnJobs = activeTurnJobs.filter((job) => !isTurnJobQueued(job));
  const recentTerminalTurnJobs = visibleTurnJobs.filter(isTurnJobTerminal).slice(0, 3);
  const sidebarTurnJobs = (activeTurnJobs.length ? [...runningTurnJobs, ...queuedTurnJobs] : recentTerminalTurnJobs).slice(0, 5);
  const selectedTurnJob =
    visibleTurnJobs.find((job) => turnJobId(job) === selectedTurnJobId) ??
    runningTurnJobs[0] ??
    queuedTurnJobs[0] ??
    visibleTurnJobs[0];
  const selectedTurnJobIdResolved = turnJobId(selectedTurnJob ?? {});
  const selectedTurnJobSession = selectedTurnJob ? sessionById.get(turnJobSessionId(selectedTurnJob)) : undefined;
  const selectedTurnJobEvents = selectedTurnJobIdResolved
    ? events.filter((event) => eventTurnId(event) === selectedTurnJobIdResolved).slice(-8).reverse()
    : [];
  const scheduler = turnJobs.scheduler ?? {};
  const runningWorkerCount = scheduler.running_turn_workers ?? turnJobs.running_count ?? runningTurnJobs.length;
  const maxRunningWorkers = scheduler.max_running_turn_workers;
  const maxQueuedPerSession = scheduler.max_queued_turns_per_session;
  const persistedQueuedCount = queuedTurnJobs.filter((job) => job.payload_persisted).length;
  const globalQuotaQueuedCount = queuedTurnJobs.filter((job) => job.queue_reason === "global_worker_quota").length;
  const recoveredQueuedCount = queuedTurnJobs.filter((job) => job.queue_reason === "recovered").length;
  const expiredTurnCount = scheduler.expired_queued_turns ?? visibleTurnJobs.filter((job) => job.status === "expired").length;
  const queueTimeoutLabel = schedulerDuration(scheduler.turn_queue_timeout_ms);
  const leaseStaleLabel = schedulerDuration(scheduler.turn_queue_lease_stale_ms);
  const activeProjectLabel = activeProject?.name || projectNameFromPath(selectedProjectPath || "Workspace");
  const activeProjectDisplayPath = selectedProjectPath || "No project selected";
  const pendingInteractionCount = approvals.length + questions.length;
  const trustSyncLabel = interactionSync.last_synced_at_ms
    ? `${new Date(interactionSync.last_synced_at_ms).toLocaleTimeString()}${
        interactionSync.last_event_method ? ` · ${methodLabel(interactionSync.last_event_method)}` : ""
      }`
    : "not synced";
  const bridgeManagedLabel = !isTauriRuntime() ? "web preview" : managedBridge?.running ? "running" : "stopped";
  const bridgeManagedClass = !isTauriRuntime() ? "neutral" : managedBridge?.running ? "ok" : "neutral";
  const bridgeAuthLabel = desktopAuth?.token ? (desktopAuth.created ? "created" : "local") : token.trim() ? "manual" : "none";
  const bridgeAuthClass = desktopAuth?.token || token.trim() ? "ok" : "neutral";
  const latestPatch = sessionDiff?.latest ?? null;
  const latestPatchPath = stringField(latestPatch, "path") || "latest patch";
  const latestPatchStatus =
    stringField(latestPatch, "status") ||
    (latestPatch ? `${sessionDiff?.undo_count ?? 0} undo · ${sessionDiff?.redo_count ?? 0} redo` : "");
  const latestCheckpoint = checkpoints?.latest ?? (checkpoints?.checkpoints ?? [])[0];
  const showWorkspaceDock =
    pendingInteractionCount > 0 ||
    Boolean(latestPatch) ||
    Boolean(latestCheckpoint) ||
    Boolean(restoredCheckpointId);
  const showComposerContext =
    showWorkspaceDock ||
    streamState !== "idle" ||
    Boolean(activeStreamingDraft) ||
    Boolean(activeSessionId) ||
    Boolean(selectedProjectPath);
  const timelineEmpty =
    activeMessages.length === 0 && visibleLiveEvents.length === 0 && !activeStreamingDraft;
  const projectBridgeStatusLabel = bridgeSwitchInProgress
    ? "syncing"
    : connection === "online"
      ? "online"
      : connection;
  const jobSectionLabel = turnJobs.error
    ? "offline"
    : runningTurnJobs.length || queuedTurnJobs.length
      ? `${runningTurnJobs.length} running · ${queuedTurnJobs.length} queued`
      : visibleTurnJobs.length
        ? "recent"
        : "idle";
  const latestUserActivity = useMemo(() => {
    for (const message of [...activeMessages].reverse()) {
      if (message.info?.role !== "user") continue;
      const text = messageContent(message);
      if (!text.trim()) continue;
      return {
        text,
        created_at_ms: message.info?.created_at_ms,
      };
    }
    return null;
  }, [activeMessages]);
  const latestTurnStartedAtMs = useMemo(() => {
    for (const event of [...activeEvents].reverse()) {
      if (event.method === "turn/started" && event.created_at_ms) return event.created_at_ms;
    }
    return undefined;
  }, [activeEvents]);
  const conversationPhaseLabel =
    streamState === "queued"
      ? "排队中"
      : streamState === "running" || streamState === "streaming"
      ? "正在思考"
      : streamState === "waiting_approval"
        ? "等待权限"
        : streamState === "waiting_question"
          ? "等待回答"
          : streamState === "interrupted"
            ? "已中断"
          : activeSessionId
            ? "可以继续发这段"
            : "新任务";
  const isTurnInterruptible =
    Boolean(activeTurnId) &&
    ["queued", "running", "streaming", "waiting_approval", "waiting_question"].includes(streamState);
  const interruptBusy = Boolean(interruptingTurnId);
  const activityStartedAtMs = latestTurnStartedAtMs || latestUserActivity?.created_at_ms || activeSession?.updated_at_ms;
  const activityElapsedLabel = activeSessionId ? formatElapsed(activityStartedAtMs, nowMs) : "";
  const activityTitle = latestUserActivity
    ? streamState === "queued"
      ? "排队中的目标"
      : streamState === "running" || streamState === "streaming"
      ? "进行中的目标"
      : streamState === "waiting_approval" || streamState === "waiting_question"
        ? "等待处理"
        : "最近任务"
    : activeSessionId
      ? "当前会话"
      : "新任务";
  const activityDetailFull = latestUserActivity?.text || activeSession?.title || activeProjectDisplayPath;
  const activityDetail = compactText(activityDetailFull, 148);
  const activityMetaLabel =
    activityElapsedLabel && activeSessionId
      ? `${activityElapsedLabel}${streamState !== "idle" ? ` · ${conversationPhaseLabel}` : ""}`
      : conversationPhaseLabel;

  const managedBridgeStartOptions = useCallback(
    (workspaceOverride = "") => ({
      workspace: workspaceOverride || selectedProjectPath || activeSession?.workspace || desktopDiagnostics?.workspace_default,
      sessionRoot: desktopDiagnostics?.session_root_default,
      port: managedBridge?.port || 8787,
      authToken: token.trim() || undefined,
    }),
    [
      activeSession?.workspace,
      desktopDiagnostics?.session_root_default,
      desktopDiagnostics?.workspace_default,
      managedBridge?.port,
      selectedProjectPath,
      token,
    ],
  );

  const runManagedBridgeCommand = useCallback(
    async (action: "start" | "restart", workspaceOverride = "") => {
      if (!isTauriRuntime()) return;
      setManagedBridgeBusy(action);
      setManagedBridgeError("");
      try {
        const payload = await invoke<ManagedBridgeStatus>(
          action === "start" ? "app_bridge_start" : "app_bridge_restart",
          { options: managedBridgeStartOptions(workspaceOverride) },
        );
        setManagedBridge(payload);
        setManagedBridgeError(payload.error ?? "");
        if (payload.url) {
          setBridgeUrl(payload.url);
        }
        window.setTimeout(() => {
          refresh().catch((err: unknown) => {
            if (!isInitialBridgeFetchError(err)) setError(err instanceof Error ? err.message : String(err));
          });
        }, 250);
      } catch (err) {
        managedBridgeAutoSyncKey.current = "";
        setManagedBridgeError(err instanceof Error ? err.message : String(err));
      } finally {
        setManagedBridgeBusy("");
      }
    },
    [managedBridgeStartOptions, refresh],
  );

  const refreshManagedBridge = useCallback(async () => {
    if (!isTauriRuntime()) return;
    setManagedBridgeBusy("status");
    try {
      const payload = await invoke<ManagedBridgeStatus>("app_bridge_status");
      setManagedBridge(payload);
      setManagedBridgeError(payload.error ?? "");
      if (payload.running && payload.url) {
        setBridgeUrl(payload.url);
      }
    } catch (err) {
      setManagedBridgeError(err instanceof Error ? err.message : String(err));
    } finally {
      setManagedBridgeBusy("");
    }
  }, []);

  const startManagedBridge = useCallback(async () => {
    await runManagedBridgeCommand("start");
  }, [runManagedBridgeCommand]);

  const restartManagedBridge = useCallback(async () => {
    await runManagedBridgeCommand("restart");
  }, [runManagedBridgeCommand]);

  const stopManagedBridge = useCallback(async () => {
    if (!isTauriRuntime()) return;
    setManagedBridgeBusy("stop");
    setManagedBridgeError("");
    try {
      const payload = await invoke<ManagedBridgeStatus>("app_bridge_stop");
      setManagedBridge(payload);
      setManagedBridgeError(payload.error ?? "");
      setConnection("offline");
    } catch (err) {
      setManagedBridgeError(err instanceof Error ? err.message : String(err));
    } finally {
      setManagedBridgeBusy("");
    }
  }, []);

  const syncManagedBridgeToWorkspace = useCallback(
    (workspace: string) => {
      const target = normalizeProjectPath(workspace);
      if (!isTauriRuntime() || !target || managedBridgeBusyAny) return;
      if (managedBridge?.running && sameProjectPath(managedBridge.workspace, target)) {
        if (managedBridge.url) setBridgeUrl(managedBridge.url);
        return;
      }
      const action = managedBridge?.running ? "restart" : "start";
      void runManagedBridgeCommand(action, target);
    },
    [managedBridge, managedBridgeBusyAny, runManagedBridgeCommand],
  );

  useEffect(() => {
    if (!desktopAuthReady || !isTauriRuntime() || managedBridgeBusyAny) return;
    const target = normalizeProjectPath(
      activeProject?.path || activeProjectPath || desktopDiagnostics?.workspace_default || "",
    );
    if (!target) return;

    if (managedBridge?.running && sameProjectPath(managedBridge.workspace, target)) {
      managedBridgeAutoSyncKey.current = `ready:${target}`;
      if (managedBridge.url) setBridgeUrl(managedBridge.url);
      return;
    }

    const action = managedBridge?.running ? "restart" : "start";
    const source = managedBridge?.running ? normalizeProjectPath(managedBridge.workspace) : "stopped";
    const key = `${action}:${target}:${source}`;
    if (managedBridgeAutoSyncKey.current === key) return;
    managedBridgeAutoSyncKey.current = key;
    syncManagedBridgeToWorkspace(target);
  }, [
    activeProject?.path,
    activeProjectPath,
    desktopAuthReady,
    desktopDiagnostics?.workspace_default,
    managedBridge?.running,
    managedBridge?.url,
    managedBridge?.workspace,
    managedBridgeBusyAny,
    syncManagedBridgeToWorkspace,
  ]);

  const selectProject = useCallback((project: DesktopProject) => {
    const nextProject = { ...project, last_opened_at_ms: Date.now() };
    setProjects((current) => upsertProject(current, nextProject));
    setActiveProjectPath(nextProject.path);
    setProjectPathInput(nextProject.path);
    setProjectError("");
    setActiveSessionId("");
    setSessionMessages(null);
    setSessionDiff(null);
    setCheckpoints(null);
    setFileTree(null);
    setFilePreview(null);
    setGitStatus(null);
    syncManagedBridgeToWorkspace(nextProject.path);
  }, [syncManagedBridgeToWorkspace]);

  const registerProject = useCallback((project: DesktopProject) => {
    setProjects((current) => upsertProject(current, project));
    setActiveProjectPath(project.path);
    setProjectPathInput(project.path);
    setActiveSessionId("");
    setSessionMessages(null);
    setSessionDiff(null);
    setCheckpoints(null);
    setFileTree(null);
    setFilePreview(null);
    setGitStatus(null);
    syncManagedBridgeToWorkspace(project.path);
  }, [syncManagedBridgeToWorkspace]);

  const addProject = useCallback(
    async (event: FormEvent) => {
      event.preventDefault();
      const requested = projectPathInput.trim();
      if (!requested) {
        setProjectError("Project path is required");
        return;
      }

      setProjectBusy("add");
      setProjectError("");
      try {
        let info: ProjectPathInfo | null = null;
        if (isTauriRuntime()) {
          info = await invoke<ProjectPathInfo>("project_path_info", {
            request: { path: requested },
          });
          if (!info.is_dir) {
            setProjectError(info.error || "Project path must be a directory");
            return;
          }
        }
        const project = projectFromPath(info?.canonical || info?.path || requested, info?.name);
        registerProject(project);
      } catch (err) {
        setProjectError(err instanceof Error ? err.message : String(err));
      } finally {
        setProjectBusy("");
      }
    },
    [projectPathInput, registerProject],
  );

  const chooseProjectFolder = useCallback(async () => {
    if (!isTauriRuntime()) return;
    setProjectBusy("choose");
    setProjectError("");
    try {
      const info = await invoke<ProjectPathInfo | null>("choose_project_folder");
      if (!info) return;
      if (!info.is_dir) {
        setProjectError(info.error || "Project path must be a directory");
        return;
      }
      registerProject(projectFromPath(info.canonical || info.path || info.input || "", info.name));
    } catch (err) {
      setProjectError(err instanceof Error ? err.message : String(err));
    } finally {
      setProjectBusy("");
    }
  }, [registerProject]);

  const openOverviewPanel = useCallback(() => {
    setInspectorMode("overview");
    setInspectorOpen(true);
  }, []);

  const openReviewPanel = useCallback(() => {
    setInspectorMode("review");
    setInspectorOpen(true);
  }, []);

  const handleReviewKeyDown = useCallback(
    (event: KeyboardEvent<HTMLElement>) => {
      if (event.key !== "Enter" && event.key !== " ") return;
      event.preventDefault();
      openReviewPanel();
    },
    [openReviewPanel],
  );

  return (
    <main className={`app-shell ${inspectorOpen ? "inspector-visible" : ""}`}>
      <aside className="rail">
        <div className="brand-row">
          <div className="traffic-lights" aria-hidden="true">
            <span className="traffic-close" />
            <span className="traffic-minimize" />
            <span className="traffic-zoom" />
          </div>
          <div className="brand-nav-actions">
            <button className="chrome-button ghost" type="button" title="Toggle sidebar">
              <Sidebar size={15} />
            </button>
            <button className="chrome-button ghost" type="button" title="Back">
              <ArrowLeft size={15} />
            </button>
            <button className="chrome-button ghost" type="button" title="Forward">
              <ArrowRight size={15} />
            </button>
          </div>
          <button className="chrome-button ghost brand-refresh" onClick={refresh} type="button" title="Refresh">
            <RefreshCw size={14} />
          </button>
        </div>

        <nav className="primary-nav" aria-label="OpenAgent navigation">
          <button
            className="nav-action"
            disabled={bridgeSwitchInProgress}
            onClick={createSession}
            type="button"
            title={bridgeSwitchInProgress ? "App Bridge is switching project" : "New session"}
          >
            <PencilLine size={17} />
            <span>新对话</span>
          </button>
          <button className="nav-action" type="button">
            <Search size={17} />
            <span>搜索</span>
          </button>
          <button className="nav-action" type="button">
            <History size={17} />
            <span>已安排</span>
            {pendingInteractionCount ? <b>{pendingInteractionCount}</b> : null}
          </button>
          <button className="nav-action" type="button">
            <PlugZap size={17} />
            <span>插件</span>
          </button>
        </nav>

        <section className="rail-section pinned">
          <div className="section-title">置顶</div>
          <button className="topic-row selected" type="button" title={activeProjectDisplayPath}>
            <span>{activeProjectLabel}</span>
            <small>{projectBridgeStatusLabel}</small>
          </button>
          <button className="topic-row" type="button" title={provider?.model ?? "model"}>
            <span>{provider?.model ?? "模型未连接"}</span>
            <small>{streamState}</small>
          </button>
        </section>

        <section className="rail-section jobs" data-testid="turn-jobs-section">
          <div className="section-title section-title-row">
            <span>任务</span>
            <small>{jobSectionLabel}</small>
          </div>
          <div className="job-list">
            {sidebarTurnJobs.map((job) => {
              const id = turnJobId(job);
              const session = sessionById.get(turnJobSessionId(job));
              const sessionLabel = session?.title || compactId(turnJobSessionId(job));
              const status = job.status ?? "idle";
              return (
                <div className={`job-row ${isTurnJobTerminal(job) ? "terminal" : "active"}`} data-turn-id={id} key={id}>
                  <button
                    className={`job-main ${id === selectedTurnJobIdResolved ? "selected" : ""}`}
                    onClick={() => {
                      const sessionIdForJob = turnJobSessionId(job);
                      if (sessionIdForJob) setActiveSessionId(sessionIdForJob);
                      setSelectedTurnJobId(id);
                      setInspectorMode("overview");
                      setInspectorOpen(true);
                    }}
                    title={`${id} · ${sessionLabel}${job.queue_reason ? ` · ${queueReasonLabel(job.queue_reason)}` : ""}`}
                    type="button"
                  >
                    <Activity size={14} />
                    <span>{sessionLabel || "turn"}</span>
                    <small>
                      {isTurnJobQueued(job)
                        ? `Queue #${queuePositionForJob(job, queuedTurnJobs)} · ${queueReasonLabel(job.queue_reason)}`
                        : isTurnJobTerminal(job)
                          ? `${turnJobStatusLabel(job)} · ${turnJobLabel(job)}`
                          : turnJobLabel(job)}
                    </small>
                  </button>
                  {isTurnJobInterruptible(job) ? (
                    <button
                      aria-label={`Interrupt ${id}`}
                      className="job-stop-button"
                      disabled={interruptingTurnId === id}
                      onClick={() => interruptTurn(id)}
                      title={interruptingTurnId === id ? "Interrupting" : `Interrupt ${compactId(id)}`}
                      type="button"
                    >
                      <Square size={11} />
                    </button>
                  ) : (
                    <small className={`job-status ${statusClass(status)}`}>{turnJobStatusLabel(job)}</small>
                  )}
                </div>
              );
            })}
            {sidebarTurnJobs.length === 0 ? <p className="muted-line">暂无任务</p> : null}
            {turnJobs.error ? <p className="muted-line job-error">{turnJobs.error}</p> : null}
          </div>
        </section>

        <section className="rail-section projects">
          <div className="section-title section-title-row">
            <span>项目</span>
            <button
              className="icon-button"
              disabled={!isTauriRuntime() || projectBusy === "choose" || managedBridgeBusyAny}
              onClick={chooseProjectFolder}
              type="button"
              title="Choose project folder"
            >
              <FolderOpen size={14} />
            </button>
          </div>
          <form className="project-add" onSubmit={addProject}>
            <input
              aria-label="Project path"
              value={projectPathInput}
              onChange={(event) => setProjectPathInput(event.target.value)}
              placeholder="/path/to/project"
            />
            <button disabled={projectBusy === "add"} type="submit" title="Add project">
              <FolderPlus size={14} />
            </button>
          </form>
          {projectError ? <p className="project-error">{projectError}</p> : null}
          <div className="project-list">
            {projects.map((project) => (
              <button
                className={`project-row ${sameProjectPath(project.path, selectedProjectPath) ? "selected" : ""}`}
                disabled={managedBridgeBusyAny}
                key={project.id}
                onClick={() => selectProject(project)}
                title={project.path}
                type="button"
              >
                <Folder size={15} />
                <span>{project.name}</span>
                <small>{compactPath(project.path)}</small>
              </button>
            ))}
          </div>
        </section>

        <section className="rail-section sessions">
          <div className="section-title section-title-row">
            <span>会话</span>
            <button className="icon-button" onClick={refresh} type="button" title="Refresh">
              <RefreshCw size={14} />
            </button>
          </div>
          <div className="session-list">
            {visibleSessions.map((session) => {
              const id = sessionId(session);
              return (
                <button
                  className={`session-row ${id === activeSessionId ? "selected" : ""}`}
                  key={id}
                  onClick={() => setActiveSessionId(id)}
                  type="button"
                >
                  <span>{session.title || id || "session"}</span>
                  <small>{session.status ?? "idle"}</small>
                </button>
              );
            })}
            {visibleSessions.length === 0 ? <p className="muted-line">暂无会话</p> : null}
          </div>
        </section>

        <section className="rail-section bridge-settings">
          <button className="status-row" onClick={openOverviewPanel} type="button">
            <PlugZap size={15} />
            <span>App Bridge</span>
            <small className={bridgeManagedClass}>{bridgeManagedLabel}</small>
          </button>
          <button className="status-row" onClick={openOverviewPanel} type="button">
            <Radio size={15} />
            <span>{provider?.model ?? "Model"}</span>
            <small className={statusClass(provider?.healthy ? "healthy" : "missing")}>
              {provider?.healthy ? "ready" : "check"}
            </small>
          </button>
          {managedBridgeWorkspaceMismatch ? (
            <div className="project-warning project-warning-action">
              <span>Bridge on {compactPath(managedBridge?.workspace)}</span>
              <button
                disabled={!isTauriRuntime() || managedBridgeBusyAny}
                onClick={restartManagedBridge}
                type="button"
                title="Restart managed App Bridge on selected project"
              >
                <RotateCcw size={12} />
                Restart
              </button>
            </div>
          ) : null}
        </section>

        <div className="sidebar-profile">
          <div className="profile-avatar">OA</div>
          <div>
            <strong>OpenAgent</strong>
            <span>{desktopRuntime}</span>
          </div>
        </div>
      </aside>

      <section className={`workspace ${showWorkspaceDock ? "has-dock" : ""} ${showComposerContext ? "has-context" : ""}`}>
        <header className="topbar">
          <div className="title-cluster">
            <div className="title-icon">
              <Sidebar size={16} />
            </div>
            <h1>{activeSession?.title || activeProjectLabel || "OpenAgent"}</h1>
            <button className="chrome-button ghost" type="button" title="More">
              <MoreHorizontal size={17} />
            </button>
          </div>
          <div className="topbar-actions">
            <button className={`chrome-button topbar-state ${statusClass(connection)}`} type="button" title={`Bridge ${connection}: ${bridgeUrl}`}>
              <GitBranch size={15} />
            </button>
            <button
              className={`chrome-button topbar-state ${statusClass(provider?.healthy ? "healthy" : "missing")}`}
              type="button"
              title={provider?.model ?? "Model"}
            >
              <Square size={15} />
            </button>
            <button className={`chrome-button topbar-state ${statusClass(streamState)}`} type="button" title={streamState}>
              <Activity size={15} />
            </button>
            <button
              className="chrome-button"
              onClick={() => {
                if (inspectorOpen) {
                  setInspectorOpen(false);
                } else {
                  openOverviewPanel();
                }
              }}
              type="button"
              title="Toggle details"
            >
              <PanelRight size={16} />
            </button>
          </div>
        </header>

        <section className={`timeline ${timelineEmpty ? "empty" : ""}`} aria-live="polite">
          {timelineEmpty ? (
            <div className="empty-state">
              <span>开始一个任务</span>
              <small>{activeProjectDisplayPath}</small>
            </div>
          ) : (
            <>
              {activeMessages.map((message, index) => (
                <article
                  className={`event-row message-row role-${messageRoleLabel(message)}`}
                  key={messageKey(message, index)}
                >
                  <div className="event-glyph">{messageIcon(message.info?.role)}</div>
                  <div className="event-body">
                    <div className="event-heading">
                      <strong>{messageRoleLabel(message)}</strong>
                      <span>{message.info?.status ?? "completed"}</span>
                    </div>
                    {messageContent(message) ? (
                      <TextContent text={messageContent(message)} />
                    ) : visibleMessageParts(message, activeResolvedInteractionKeys).length === 0 ? (
                      <pre>{JSON.stringify(message.parts ?? [], null, 2)}</pre>
                    ) : null}
                    <MessagePartCards parts={visibleMessageParts(message, activeResolvedInteractionKeys)} />
                    <div className="message-meta">
                      <span>{compactId(message.info?.id)}</span>
                      <span>{message.parts?.length ?? 0} parts</span>
                    </div>
                  </div>
                </article>
              ))}
              {activeStreamingDraft ? (
                <article
                  className="event-row message-row role-assistant streaming-draft"
                  data-testid="streaming-assistant-draft"
                  key={`streaming-draft:${activeStreamingDraft.turnId}`}
                >
                  <div className="event-glyph">{messageIcon("assistant")}</div>
                  <div className="event-body">
                    <div className="event-heading">
                      <strong>assistant</strong>
                      <span>{activeStreamingDraft.eventCount} chunks</span>
                    </div>
                    <TextContent text={activeStreamingDraft.text} />
                  </div>
                </article>
              ) : null}
              {visibleLiveEvents.map((event, index) => (
                <article className="event-row live-event-row" key={`${eventKey(event)}:${index}`}>
                  <div className="event-glyph">{eventIcon(event.method)}</div>
                  <div className="event-body">
                    <div className="event-heading">
                      <strong>{methodLabel(event.method)}</strong>
                      <span>#{event.global_sequence ?? event.sequence ?? index + 1}</span>
                    </div>
                    <EventContent event={event} />
                  </div>
                </article>
              ))}
            </>
          )}
        </section>

        <div className={`composer-dock ${showComposerContext ? "with-context" : "bare"}`}>
          {showWorkspaceDock ? (
            <section className="workspace-dock" aria-label="Workspace activity">
              {approvals.slice(0, 2).map((item) => {
                const approval = item.approval ?? {};
                const preview = approval.preview as JsonRecord | undefined;
                const isResponding = respondingInteractionId === item.request_id;
                return (
                  <article className="dock-item dock-item-attention" key={item.request_id}>
                    <ShieldCheck size={15} />
                    <div>
                      <strong>{stringField(approval, "tool_name") || "Approval needed"}</strong>
                      <span>{stringField(preview, "path") || compactId(item.request_id)}</span>
                    </div>
                    <button type="button" disabled={isResponding} onClick={() => respondApproval(item, "allow")}>
                      {isResponding ? "Working" : "Allow"}
                    </button>
                    <button type="button" disabled={isResponding} onClick={() => respondApproval(item, "deny")}>
                      Deny
                    </button>
                  </article>
                );
              })}
              {questions.slice(0, 2).map((item) => {
                const question = item.question ?? {};
                const fields = questionElicitationFields(question, item.request_id ?? "", questionDrafts);
                const hasErrors = fields.some((field) => field.error);
                const isResponding = respondingInteractionId === item.request_id;
                return (
                  <article className="dock-item dock-item-attention" key={item.request_id}>
                    <Bot size={15} />
                    <div>
                      <strong>{fields[0]?.label || "Question"}</strong>
                      <span>{hasErrors ? "Open details to answer required fields" : fields[0]?.description || compactId(item.request_id)}</span>
                    </div>
                    <button type="button" disabled={isResponding || hasErrors} onClick={() => respondQuestion(item)}>
                      {isResponding ? "Working" : "Reply"}
                    </button>
                    <button type="button" disabled={isResponding} onClick={() => respondQuestion(item, true)}>
                      Dismiss
                    </button>
                  </article>
                );
              })}
              {latestPatch ? (
                <article
                  className="dock-item dock-item-clickable"
                  onClick={openReviewPanel}
                  onKeyDown={handleReviewKeyDown}
                  role="button"
                  tabIndex={0}
                >
                  <GitCompare size={15} />
                  <div>
                    <strong>{latestPatchPath}</strong>
                    <span>{latestPatchStatus || "workspace changed"}</span>
                  </div>
                  <button
                    disabled={!sessionDiff?.undo_count}
                    onClick={(event) => {
                      event.stopPropagation();
                      runPatchAction("undo");
                    }}
                    type="button"
                  >
                    Undo
                  </button>
                  <button
                    disabled={!sessionDiff?.redo_count}
                    onClick={(event) => {
                      event.stopPropagation();
                      runPatchAction("redo");
                    }}
                    type="button"
                  >
                    Redo
                  </button>
                </article>
              ) : null}
              {latestCheckpoint ? (
                <article
                  className="dock-item dock-item-clickable"
                  onClick={openReviewPanel}
                  onKeyDown={handleReviewKeyDown}
                  role="button"
                  tabIndex={0}
                >
                  <History size={15} />
                  <div>
                    <strong>
                      {restoredCheckpointId ? `Restored ${compactId(restoredCheckpointId)}` : checkpointLabel(latestCheckpoint)}
                    </strong>
                    <span>
                      {numberField(latestCheckpoint as JsonRecord, "file_count")} files ·{" "}
                      {formatBytes(numberField(latestCheckpoint as JsonRecord, "total_bytes"))}
                    </span>
                  </div>
                  <button
                    data-checkpoint-id={latestCheckpoint.checkpoint_id}
                    disabled={Boolean(restoringCheckpointId)}
                    onClick={(event) => {
                      event.stopPropagation();
                      restoreCheckpoint(latestCheckpoint.checkpoint_id);
                    }}
                    type="button"
                  >
                    {restoringCheckpointId === latestCheckpoint.checkpoint_id ? "Restoring" : "Restore"}
                  </button>
                </article>
              ) : restoredCheckpointId ? (
                <article className="dock-item">
                  <History size={15} />
                  <div>
                    <strong>Restored {compactId(restoredCheckpointId)}</strong>
                    <span>checkpoint restored</span>
                  </div>
                </article>
              ) : null}
            </section>
          ) : null}
          {showComposerContext ? (
            <>
              <div className="composer-step-pill" aria-live="polite">
                <span className={`step-dot ${statusClass(streamState)}`} />
                {conversationPhaseLabel}
              </div>
              <div className="composer-context-bar" aria-label="Current workspace context" data-testid="composer-context-bar">
                <Activity size={14} />
                <strong>{activityTitle}</strong>
                <span data-testid="composer-activity-detail" title={activityDetailFull}>
                  {activityDetail}
                </span>
                <small>{activityMetaLabel}</small>
                <button onClick={openOverviewPanel} type="button" title="Open details">
                  <PencilLine size={13} />
                </button>
              </div>
            </>
          ) : null}
          <form className="composer" onSubmit={submitPrompt}>
            <textarea
              value={prompt}
              onChange={(event) => setPrompt(event.target.value)}
              placeholder="要求后续变更"
              rows={2}
            />
            <div className="composer-footer">
              <button className="composer-tool-button" type="button" title="Attach">
                <Plus size={18} />
              </button>
              <div className="composer-controls">
                <select
                  value={permission}
                  onChange={(event) => setPermission(event.target.value)}
                  title="Permission"
                >
                  <option value="PLAN_ONLY">计划模式</option>
                  <option value="READONLY">只读</option>
                  <option value="FULL">完全访问</option>
                </select>
                <select value={model} onChange={(event) => setModel(event.target.value)} title="Model">
                  {(modelOptions?.length ? modelOptions : [model || "server-local"]).map((item) => (
                    <option key={item} value={item}>
                      {item}
                    </option>
                  ))}
                </select>
                <button className="composer-goal-button" type="button" title="Goal">
                  <Activity size={14} />
                  目标
                </button>
              </div>
              <button
                aria-label={isTurnInterruptible ? "Interrupt active turn" : "Run prompt"}
                className={`send-button ${isTurnInterruptible ? "stop-button" : ""}`}
                disabled={bridgeSwitchInProgress || interruptBusy}
                onClick={isTurnInterruptible ? interruptActiveTurn : undefined}
                type={isTurnInterruptible ? "button" : "submit"}
                title={
                  bridgeSwitchInProgress
                    ? "App Bridge is switching project"
                    : isTurnInterruptible
                      ? interruptBusy
                        ? "Interrupting"
                        : `Interrupt ${compactId(activeTurnId)}`
                      : "Run"
                }
              >
                {isTurnInterruptible ? <Square size={15} /> : <ArrowUp size={18} />}
              </button>
              {isTurnInterruptible ? (
                <button
                  aria-label="Queue prompt"
                  className="send-button queue-button"
                  disabled={bridgeSwitchInProgress || !prompt.trim()}
                  title="Queue prompt after the active turn"
                  type="submit"
                >
                  <ArrowUp size={16} />
                </button>
              ) : null}
            </div>
          </form>
          {error ? <div className="error-line">{error}</div> : null}
        </div>
      </section>

      <button
        aria-label="Close details"
        className="inspector-scrim"
        onClick={() => setInspectorOpen(false)}
        type="button"
      />
      <aside className={`inspector ${inspectorOpen ? "open" : ""}`}>
        <div className="inspector-header">
          <div>
            <strong>{inspectorMode === "review" ? "Review" : "详情"}</strong>
            <span>{activeProjectLabel}</span>
          </div>
          <div className="inspector-tabs" role="tablist" aria-label="Inspector mode">
            <button
              className={inspectorMode === "overview" ? "active" : ""}
              onClick={() => setInspectorMode("overview")}
              role="tab"
              type="button"
            >
              Overview
            </button>
            <button
              className={inspectorMode === "review" ? "active" : ""}
              onClick={() => setInspectorMode("review")}
              role="tab"
              type="button"
            >
              Review
            </button>
          </div>
          <button className="chrome-button" onClick={() => setInspectorOpen(false)} type="button" title="Close">
            <PanelRight size={16} />
          </button>
        </div>
        {inspectorMode === "review" ? (
          <section className="review-panel" aria-label="Diff and checkpoint review">
            <div className="review-summary">
              <div>
                <span>Pending</span>
                <strong>{pendingInteractionCount}</strong>
              </div>
              <div>
                <span>Undo</span>
                <strong>{sessionDiff?.undo_count ?? 0}</strong>
              </div>
              <div>
                <span>Checkpoints</span>
                <strong>{checkpoints?.count ?? 0}</strong>
              </div>
            </div>

            <div className="inspector-card review-card review-primary">
              <div className="inspector-title">
                <GitCompare size={15} />
                Change Review
                <span className={`trust-count ${latestPatch ? "pending" : "clear"}`}>
                  {latestPatch ? "changed" : "clear"}
                </span>
              </div>
              {latestPatch ? (
                <>
                  <div className="review-focus">
                    <strong>{latestPatchPath}</strong>
                    <span>{latestPatchStatus || "workspace changed"}</span>
                  </div>
                  {sideBySideRows(latestPatch).length ? (
                    <div className="review-split-diff" role="table" aria-label="Side-by-side file diff">
                      <div className="review-split-header" role="row">
                        <span>Before</span>
                        <span>After</span>
                      </div>
                      {sideBySideRows(latestPatch).map((row, index) => {
                        const kind = stringField(row, "kind") || "context";
                        return (
                          <div className={`review-split-row ${kind}`} role="row" key={`${kind}:${index}`}>
                            <div className="review-split-cell old" role="cell">
                              <span className="line-no">{numberField(row, "old_line") || ""}</span>
                              <code>{diffCellText(row.old)}</code>
                            </div>
                            <div className="review-split-cell new" role="cell">
                              <span className="line-no">{numberField(row, "new_line") || ""}</span>
                              <code>{diffCellText(row.new)}</code>
                            </div>
                          </div>
                        );
                      })}
                      {nestedRecord(latestPatch, "side_by_side")?.truncated ? (
                        <p className="muted-line">
                          Diff truncated · {numberField(nestedRecord(latestPatch, "side_by_side"), "omitted_rows")} rows omitted.
                        </p>
                      ) : null}
                    </div>
                  ) : stringField(latestPatch, "diff") ? (
                    <pre className="review-diff-code">{stringField(latestPatch, "diff")}</pre>
                  ) : (
                    <p className="muted-line">No diff payload for this patch.</p>
                  )}
                  <div className="inline-actions">
                    <button
                      disabled={!sessionDiff?.undo_count}
                      onClick={() => runPatchAction("undo")}
                      type="button"
                    >
                      <Undo2 size={13} />
                      Undo
                    </button>
                    <button
                      disabled={!sessionDiff?.redo_count}
                      onClick={() => runPatchAction("redo")}
                      type="button"
                    >
                      <RefreshCw size={13} />
                      Redo
                    </button>
                  </div>
                </>
              ) : (
                <p className="muted-line">No patch for the active session yet.</p>
              )}
            </div>

            <div className="inspector-card review-card">
              <div className="inspector-title">
                <History size={15} />
                Checkpoint Browser
                {restoredCheckpointId ? <span className="checkpoint-restored">restored</span> : null}
              </div>
              {restoredCheckpointId ? (
                <p className="restore-state">Restored {compactId(restoredCheckpointId)}</p>
              ) : null}
              <div className="review-checkpoint-list">
                {(checkpoints?.checkpoints ?? []).slice(0, 8).map((checkpoint) => {
                  const isRestoring = restoringCheckpointId === checkpoint.checkpoint_id;
                  const isRestored = restoredCheckpointId === checkpoint.checkpoint_id;
                  return (
                    <div
                      className={`review-checkpoint-row ${isRestored ? "restored" : ""}`}
                      key={checkpoint.checkpoint_id}
                    >
                      <History size={14} />
                      <div>
                        <strong>{checkpointLabel(checkpoint)}</strong>
                        <span>
                          {numberField(checkpoint as JsonRecord, "file_count")} files ·{" "}
                          {formatBytes(numberField(checkpoint as JsonRecord, "total_bytes"))}
                        </span>
                      </div>
                      <button
                        data-checkpoint-id={checkpoint.checkpoint_id}
                        disabled={Boolean(restoringCheckpointId)}
                        onClick={() => restoreCheckpoint(checkpoint.checkpoint_id)}
                        type="button"
                      >
                        {isRestoring ? "Restoring" : "Restore"}
                      </button>
                    </div>
                  );
                })}
                {(checkpoints?.checkpoints ?? []).length === 0 ? (
                  <p className="muted-line">No checkpoints for this session yet.</p>
                ) : null}
              </div>
            </div>

            <div className="inspector-card review-card">
              <div className="inspector-title">
                <Folder size={15} />
                Context
              </div>
              <dl>
                <dt>Focus</dt>
                <dd>{filePreview?.path || fileTree?.path || "."}</dd>
                <dt>Git</dt>
                <dd>
                  {gitStatus?.is_repo
                    ? `${gitStatus.branch || "-"} · ${gitStatus.change_count ?? 0} changes`
                    : gitStatus?.error || "No git repository"}
                </dd>
                <dt>Workspace</dt>
                <dd>{fileTree?.workspace ?? activeProjectDisplayPath}</dd>
              </dl>
              {filePreview?.exists === false ? (
                <p className="muted-line warning-line">
                  <AlertTriangle size={13} />
                  {filePreview.path || "File"} no longer exists
                </p>
              ) : filePreview?.content !== undefined && filePreview?.content !== null ? (
                <pre className="mini-diff file-preview">{filePreview.content}</pre>
              ) : (
                <p className="muted-line">No text preview selected.</p>
              )}
            </div>
          </section>
        ) : null}
        <div className="inspector-card jobs-inspector-card" data-testid="jobs-inspector-card">
          <div className="inspector-title">
            <Activity size={15} />
            Jobs
            <span className={`trust-count ${activeTurnJobs.length ? "pending" : expiredTurnCount ? "bad" : "clear"}`}>
              {activeTurnJobs.length
                ? `${runningTurnJobs.length} running · ${queuedTurnJobs.length} queued`
                : expiredTurnCount
                  ? `${expiredTurnCount} expired`
                  : "idle"}
            </span>
          </div>
          <div className="job-metrics" aria-label="Job registry summary">
            <div>
              <span>Workers</span>
              <strong>
                {schedulerValue(runningWorkerCount)}
                {maxRunningWorkers ? `/${maxRunningWorkers}` : ""}
              </strong>
            </div>
            <div>
              <span>Queued</span>
              <strong>{turnJobs.queued_count ?? queuedTurnJobs.length}</strong>
            </div>
            <div>
              <span>Durable</span>
              <strong>{persistedQueuedCount ? `${persistedQueuedCount} saved` : "clear"}</strong>
            </div>
            <div>
              <span>Timeout</span>
              <strong>{queueTimeoutLabel}</strong>
            </div>
            <div>
              <span>Expired</span>
              <strong>{expiredTurnCount ? `${expiredTurnCount} pruned` : "clear"}</strong>
            </div>
          </div>
          <div className="scheduler-strip" data-testid="scheduler-strip">
            <span>session queue {maxQueuedPerSession ? `max ${maxQueuedPerSession}` : "default"}</span>
            <span>{globalQuotaQueuedCount ? `${globalQuotaQueuedCount} waiting for worker quota` : "worker quota clear"}</span>
            <span>{recoveredQueuedCount ? `${recoveredQueuedCount} recovered from disk` : "recovery idle"}</span>
            <span>stale lease takeover {leaseStaleLabel}</span>
            <span>{turnJobs.index_persisted ? "job index persisted" : turnJobs.source ?? "runtime registry"}</span>
          </div>
          {selectedTurnJob ? (
            <div className="job-detail" data-testid="selected-job-detail" data-turn-id={selectedTurnJobIdResolved}>
              <div className="job-detail-heading">
                <div>
                  <strong>{selectedTurnJobSession?.title || compactId(turnJobSessionId(selectedTurnJob))}</strong>
                  <span>{compactId(selectedTurnJobIdResolved)}</span>
                </div>
                <span className={`stream-state ${statusClass(selectedTurnJob.status)}`}>
                  {turnJobStatusLabel(selectedTurnJob)}
                </span>
              </div>
              <dl>
                <dt>Turn</dt>
                <dd title={selectedTurnJobIdResolved}>{compactId(selectedTurnJobIdResolved)}</dd>
                <dt>Session</dt>
                <dd title={turnJobSessionId(selectedTurnJob)}>{compactId(turnJobSessionId(selectedTurnJob))}</dd>
                <dt>Started</dt>
                <dd>{formatTime(selectedTurnJob.started_at_ms)}</dd>
                <dt>Updated</dt>
                <dd>{formatTime(selectedTurnJob.updated_at_ms)}</dd>
                <dt>Duration</dt>
                <dd>{formatElapsed(selectedTurnJob.started_at_ms, nowMs) || "-"}</dd>
                <dt>Queue</dt>
                <dd>
                  {isTurnJobQueued(selectedTurnJob)
                    ? `#${queuePositionForJob(selectedTurnJob, queuedTurnJobs)} waiting`
                    : "-"}
                </dd>
                <dt>Reason</dt>
                <dd>{isTurnJobQueued(selectedTurnJob) ? queueReasonLabel(selectedTurnJob.queue_reason) : "-"}</dd>
                <dt>Payload</dt>
                <dd>
                  {selectedTurnJob.payload_persisted
                    ? "persisted"
                    : selectedTurnJob.status === "expired"
                      ? "removed"
                      : isTurnJobQueued(selectedTurnJob)
                        ? "memory"
                        : "-"}
                </dd>
                <dt>Cancel</dt>
                <dd>{selectedTurnJob.cancel_requested ? "requested" : "none"}</dd>
                <dt>Timeout</dt>
                <dd>{queueTimeoutLabel}</dd>
              </dl>
              {turnJobLifecycleMessage(selectedTurnJob, scheduler) ? (
                <p className={`job-queue-note ${turnJobLifecycleTone(selectedTurnJob)}`}>
                  <Circle size={12} />
                  {turnJobLifecycleMessage(selectedTurnJob, scheduler)}
                </p>
              ) : null}
              <div className="inline-actions">
                <button
                  disabled={!isTurnJobInterruptible(selectedTurnJob) || interruptingTurnId === selectedTurnJobIdResolved}
                  onClick={() => interruptTurn(selectedTurnJobIdResolved)}
                  type="button"
                  title={isTurnJobInterruptible(selectedTurnJob) ? `Interrupt ${compactId(selectedTurnJobIdResolved)}` : "Job is not interruptible"}
                >
                  <Square size={13} />
                  {interruptingTurnId === selectedTurnJobIdResolved ? "Stopping" : "Stop"}
                </button>
                <button onClick={refreshTurnJobs} type="button">
                  <RefreshCw size={13} />
                  Refresh
                </button>
              </div>
              <div className="job-trace">
                <span>Trace</span>
                {selectedTurnJobEvents.length ? (
                  selectedTurnJobEvents.map((event) => (
                    <div className="job-trace-row" key={eventKey(event)}>
                      <small>{formatTime(event.created_at_ms)}</small>
                      <strong>{methodLabel(event.method)}</strong>
                    </div>
                  ))
                ) : (
                  <p className="muted-line">No live events loaded for this job.</p>
                )}
              </div>
            </div>
          ) : (
            <p className="muted-line">No active or recent jobs.</p>
          )}
          {visibleTurnJobs.length ? (
            <div className="job-inspector-list" aria-label="Recent jobs">
              {visibleTurnJobs.slice(0, 8).map((job) => {
                const id = turnJobId(job);
                const session = sessionById.get(turnJobSessionId(job));
                return (
                  <button
                    className={id === selectedTurnJobIdResolved ? "selected" : ""}
                    data-turn-id={id}
                    key={id}
                    onClick={() => {
                      setSelectedTurnJobId(id);
                      const sessionIdForJob = turnJobSessionId(job);
                      if (sessionIdForJob) setActiveSessionId(sessionIdForJob);
                    }}
                    type="button"
                  >
                    <span className={`step-dot ${statusClass(job.status)}`} />
                    <strong>{session?.title || compactId(turnJobSessionId(job))}</strong>
                    <small>
                      {isTurnJobQueued(job)
                        ? `queued #${queuePositionForJob(job, queuedTurnJobs)} · ${queueReasonLabel(job.queue_reason)}`
                        : turnJobStatusLabel(job)}{" "}
                      · {formatElapsed(job.updated_at_ms ?? job.started_at_ms, nowMs) || "now"}
                    </small>
                  </button>
                );
              })}
            </div>
          ) : null}
          {turnJobs.error ? <p className="stream-error">{turnJobs.error}</p> : null}
        </div>
        <div className="inspector-card connection-card">
          <div className="inspector-title">
            <PlugZap size={15} />
            Connection
          </div>
          <label className="field-stack">
            <span>Bridge URL</span>
            <input value={bridgeUrl} onChange={(event) => setBridgeUrl(event.target.value)} />
          </label>
          <label className="field-stack">
            <span>Token</span>
            <input
              value={token}
              onChange={(event) => setToken(event.target.value)}
              placeholder={isTauriRuntime() ? "managed local token" : "optional"}
              type="password"
            />
          </label>
        </div>

        <div className="inspector-card">
          <div className="inspector-title">
            <PanelRight size={15} />
            Protocol
          </div>
          <dl>
            <dt>Name</dt>
            <dd>{protocol?.protocol ?? "unknown"}</dd>
            <dt>Version</dt>
            <dd>{protocol?.protocol_version ?? "-"}</dd>
            <dt>Event schema</dt>
            <dd>{protocol?.event_schema_version ?? "-"}</dd>
          </dl>
        </div>

        <div className="inspector-card desktop-diagnostics">
          <div className="inspector-title">
            <Settings size={15} />
            Desktop
          </div>
          <dl>
            <dt>Runtime</dt>
            <dd>{desktopRuntime}</dd>
            <dt>Platform</dt>
            <dd>
              {desktopDiagnostics ? `${desktopDiagnostics.os ?? "-"} / ${desktopDiagnostics.arch ?? "-"}` : "-"}
            </dd>
            <dt>Bridge</dt>
            <dd>
              <span className={`stream-state ${statusClass(desktopDiagnostics?.bridge_binary?.exists ? "ok" : "missing")}`}>
                {desktopDiagnostics?.bridge_binary?.exists ? "found" : "external"}
              </span>
            </dd>
            <dt>Default URL</dt>
            <dd>{desktopDiagnostics?.bridge_default_url ?? DEFAULT_BRIDGE}</dd>
            <dt>Binary</dt>
            <dd title={desktopDiagnostics?.bridge_binary?.path ?? ""}>
              {compactPath(desktopDiagnostics?.bridge_binary?.path)}
            </dd>
            <dt>Sessions</dt>
            <dd title={desktopDiagnostics?.session_root_default ?? ""}>
              {compactPath(desktopDiagnostics?.session_root_default)}
            </dd>
            <dt>Auth</dt>
            <dd title={desktopAuth?.path ?? desktopAuthError}>
              <span className={`stream-state ${bridgeAuthClass}`}>{bridgeAuthLabel}</span>
            </dd>
            <dt>Managed</dt>
            <dd>
              <span className={`stream-state ${bridgeManagedClass}`}>{bridgeManagedLabel}</span>
            </dd>
            <dt>PID</dt>
            <dd>{managedBridge?.pid ?? "-"}</dd>
            <dt>Workspace</dt>
            <dd title={managedBridge?.workspace ?? desktopDiagnostics?.workspace_default ?? ""}>
              {compactPath(managedBridge?.workspace ?? desktopDiagnostics?.workspace_default)}
            </dd>
          </dl>
          <div className="inline-actions bridge-actions">
            <button
              type="button"
              disabled={!isTauriRuntime() || managedBridgeBusyAny || managedBridge?.running}
              onClick={startManagedBridge}
              title="Start managed App Bridge"
            >
              <Power size={13} />
              {managedBridgeBusy === "start" ? "Starting" : "Start"}
            </button>
            <button
              type="button"
              disabled={!isTauriRuntime() || managedBridgeBusyAny || !managedBridge?.running}
              onClick={restartManagedBridge}
              title="Restart managed App Bridge on selected project"
            >
              <RotateCcw size={13} />
              {managedBridgeBusy === "restart" ? "Restarting" : "Restart"}
            </button>
            <button
              type="button"
              disabled={!isTauriRuntime() || managedBridgeBusyAny || !managedBridge?.running}
              onClick={stopManagedBridge}
              title="Stop managed App Bridge"
            >
              <Square size={13} />
              {managedBridgeBusy === "stop" ? "Stopping" : "Stop"}
            </button>
            <button
              type="button"
              disabled={!isTauriRuntime() || managedBridgeBusyAny}
              onClick={refreshManagedBridge}
              title="Refresh managed App Bridge status"
            >
              <RefreshCw size={13} />
              {managedBridgeBusy === "status" ? "Checking" : "Status"}
            </button>
          </div>
          {desktopDiagnosticError ? <p className="diagnostic-warning">{desktopDiagnosticError}</p> : null}
          {desktopAuthError ? <p className="diagnostic-warning">{desktopAuthError}</p> : null}
          {managedBridgeError ? <p className="diagnostic-warning">{managedBridgeError}</p> : null}
          {desktopDiagnostics?.warnings?.length ? (
            <p className="diagnostic-warning">{desktopDiagnostics.warnings[0]}</p>
          ) : null}
        </div>

        <div className="inspector-card">
          <div className="inspector-title">
            <Radio size={15} />
            Provider
          </div>
          <dl>
            <dt>Status</dt>
            <dd>{provider?.healthy ? "healthy" : "attention"}</dd>
            <dt>Provider</dt>
            <dd>{provider?.provider_label ?? provider?.provider ?? "-"}</dd>
            <dt>Models</dt>
            <dd>{provider?.model_count ?? "-"}</dd>
            <dt>Key</dt>
            <dd>{provider?.api_key ?? "-"}</dd>
          </dl>
        </div>

        <div className="inspector-card" data-testid="mcp-card">
          <div className="inspector-title">
            <PlugZap size={15} />
            MCP
            <span className={`stream-state ${mcpStatusClass}`}>{mcpStatus}</span>
            <button
              className="icon-button mini"
              type="button"
              title="Refresh MCP"
              disabled={mcpRefreshing}
              onClick={() => {
                refreshMcp().catch(() => {});
              }}
            >
              <RefreshCw size={13} className={mcpRefreshing ? "spin" : ""} />
            </button>
          </div>
          <dl>
            <dt>Source</dt>
            <dd>{mcp?.source ?? "none"}</dd>
            <dt>Configured</dt>
            <dd>{mcp?.configured ? "yes" : "no"}</dd>
            <dt>Servers</dt>
            <dd>{mcp?.server_count ?? 0}</dd>
            <dt>Tools</dt>
            <dd>{mcp?.tool_count ?? 0}</dd>
            <dt>Refresh TTL</dt>
            <dd>{mcp?.refresh_ttl_s ?? "-"}</dd>
            <dt>Write</dt>
            <dd>{mcpWritable ? "enabled" : "read-only"}</dd>
            <dt>Config</dt>
            <dd title={mcpConfigPath}>{mcpConfigPath ? compactText(mcpConfigPath, 34) : "-"}</dd>
          </dl>
          {mcp?.error ? <p className="stream-error">{mcp.error}</p> : null}
          <form className="mcp-config-form" data-testid="mcp-server-form" onSubmit={addMcpServer}>
            <label>
              <span>Mode</span>
              <select
                value={mcpServerDraft.mode}
                disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                onChange={(event) =>
                  setMcpServerDraft((draft) => ({ ...draft, mode: event.target.value === "local" ? "local" : "remote" }))
                }
              >
                <option value="remote">Remote HTTP/SSE</option>
                <option value="local">Local stdio</option>
              </select>
            </label>
            <label>
              <span>Name</span>
              <input
                value={mcpServerDraft.name}
                disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, name: event.target.value }))}
                placeholder={mcpServerDraft.mode === "local" ? "local-tools" : "remote-tools"}
              />
            </label>
            {mcpServerDraft.mode === "remote" ? (
              <>
                <label className="full">
                  <span>URL</span>
                  <input
                    value={mcpServerDraft.url}
                    disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                    onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, url: event.target.value }))}
                    placeholder="http://127.0.0.1:3000/mcp"
                  />
                </label>
                <label>
                  <span>Transport</span>
                  <select
                    value={mcpServerDraft.transport}
                    disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                    onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, transport: event.target.value }))}
                  >
                    <option value="http">HTTP</option>
                    <option value="sse">SSE</option>
                    <option value="auto">Auto</option>
                  </select>
                </label>
              </>
            ) : (
              <>
                <label className="full">
                  <span>Command</span>
                  <input
                    value={mcpServerDraft.command}
                    disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                    onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, command: event.target.value }))}
                    placeholder="npx"
                  />
                </label>
                <label className="full">
                  <span>Args</span>
                  <textarea
                    rows={2}
                    value={mcpServerDraft.args}
                    disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                    onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, args: event.target.value }))}
                    placeholder="@modelcontextprotocol/server-filesystem&#10;/Users/william/project"
                  />
                </label>
                <label className="full">
                  <span>Cwd</span>
                  <input
                    value={mcpServerDraft.cwd}
                    disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                    onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, cwd: event.target.value }))}
                    placeholder="optional working directory"
                  />
                </label>
              </>
            )}
            <label>
              <span>Timeout ms</span>
              <input
                value={mcpServerDraft.timeoutMs}
                disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                inputMode="numeric"
                onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, timeoutMs: event.target.value }))}
                placeholder="5000"
              />
            </label>
            <label className="full">
              <span>Env</span>
              <textarea
                rows={2}
                value={mcpServerDraft.env}
                disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, env: event.target.value }))}
                placeholder="API_KEY=..."
              />
            </label>
            <label className="full">
              <span>Headers</span>
              <textarea
                rows={2}
                value={mcpServerDraft.headers}
                disabled={!mcpWritable || Boolean(mcpMutationBusy)}
                onChange={(event) => setMcpServerDraft((draft) => ({ ...draft, headers: event.target.value }))}
                placeholder="Authorization: Bearer ..."
              />
            </label>
            <button
              className="mcp-action-button"
              type="submit"
              disabled={
                !mcpWritable ||
                Boolean(mcpMutationBusy) ||
                !mcpServerDraft.name.trim() ||
                (mcpServerDraft.mode === "remote" ? !mcpServerDraft.url.trim() : !mcpServerDraft.command.trim())
              }
              title={mcpWritable ? "Add MCP server" : "MCP config is read-only"}
            >
              <Plus size={13} />
              Add
            </button>
          </form>
          {mcp?.readonly_reason && !mcpWritable ? <p className="muted-line">{mcp.readonly_reason}</p> : null}
          {mcpMutationError ? (
            <p className="stream-error" data-testid="mcp-mutation-error">
              {mcpMutationError}
            </p>
          ) : null}
          {latestMcpToolTrace ? (
            <div className="mcp-latest-call" data-testid="mcp-latest-call">
              <div>
                <span>Latest call</span>
                <strong>{latestMcpToolTrace.toolName}</strong>
              </div>
              <dl>
                <dt>Server</dt>
                <dd>{latestMcpToolTrace.server || "-"}</dd>
                <dt>Transport</dt>
                <dd>{latestMcpToolTrace.transport || "-"}</dd>
                <dt>Call</dt>
                <dd>{compactId(latestMcpToolTrace.callId)}</dd>
                <dt>Status</dt>
                <dd>{latestMcpToolTrace.status}</dd>
                <dt>Lifecycle</dt>
                <dd>{latestMcpToolTrace.lifecycleReused ? "reused" : "-"}</dd>
                <dt>PID</dt>
                <dd>{latestMcpToolTrace.lifecyclePid || "-"}</dd>
              </dl>
              <p>{compactText(latestMcpToolTrace.error || latestMcpToolTrace.output || "No textual output", 180)}</p>
            </div>
          ) : null}
          {mcpServers.length ? (
            <div className="mcp-server-list" data-testid="mcp-server-list">
              {mcpServers.map((server) => {
                const tools = server.tools ?? [];
                const serverName = server.name?.trim() ?? "";
                const serverBusy = serverName ? mcpMutationBusy.endsWith(`:${serverName}`) : false;
                const localServer = server.type === "local";
                const lifecycleStatus = mcpLifecycleStatusLabel(server);
                const lifecycleRunning = ["running", "ready", "connected"].includes(lifecycleStatus);
                const lifecycleBusy = serverName ? mcpMutationBusy.startsWith("lifecycle:") && mcpMutationBusy.endsWith(`:${serverName}`) : false;
                return (
                  <section className="mcp-server-row" key={server.name ?? mcpEndpointLabel(server)}>
                    <div className="mcp-server-heading">
                      <span className="file-badge">{server.type ?? "mcp"}</span>
                      <div className="mcp-server-copy">
                        <strong>{server.name ?? "mcp server"}</strong>
                        <span>
                          {server.enabled ? "enabled" : "disabled"} · {mcpTransportLabel(server)} · {mcpEndpointLabel(server)}
                        </span>
                      </div>
                      <div className="mcp-server-actions">
                        <span className={`stream-state ${statusClass(server.status)}`}>{server.status ?? "-"}</span>
                        {localServer ? (
                          <>
                            <button
                              className="icon-button mini"
                              type="button"
                              aria-label={`Start MCP server ${server.name ?? ""}`}
                              title="Start MCP server"
                              disabled={Boolean(mcpMutationBusy) || !server.name || lifecycleRunning}
                              onClick={() => {
                                controlMcpServerLifecycle(server, "start");
                              }}
                            >
                              <Play size={12} />
                            </button>
                            <button
                              className="icon-button mini"
                              type="button"
                              aria-label={`Stop MCP server ${server.name ?? ""}`}
                              title="Stop MCP server"
                              disabled={Boolean(mcpMutationBusy) || !server.name || !lifecycleRunning}
                              onClick={() => {
                                controlMcpServerLifecycle(server, "stop");
                              }}
                            >
                              <Square size={12} />
                            </button>
                            <button
                              className="icon-button mini"
                              type="button"
                              aria-label={`Restart MCP server ${server.name ?? ""}`}
                              title="Restart MCP server"
                              disabled={Boolean(mcpMutationBusy) || !server.name}
                              onClick={() => {
                                controlMcpServerLifecycle(server, "restart");
                              }}
                            >
                              <RotateCcw size={12} className={mcpMutationBusy === `lifecycle:restart:${serverName}` ? "spin" : ""} />
                            </button>
                          </>
                        ) : null}
                        <button
                          className="icon-button mini"
                          type="button"
                          aria-label={`Test MCP server ${server.name ?? ""}`}
                          title="Test MCP server"
                          disabled={Boolean(mcpMutationBusy) || !server.name}
                          onClick={() => {
                            testMcpServer(server);
                          }}
                        >
                          <RefreshCw size={12} className={mcpMutationBusy === `test:${serverName}` ? "spin" : ""} />
                        </button>
                        <button
                          className="icon-button mini"
                          type="button"
                          aria-label={`${server.enabled ? "Disable" : "Enable"} MCP server ${server.name ?? ""}`}
                          title={server.enabled ? "Disable MCP server" : "Enable MCP server"}
                          disabled={!mcpWritable || Boolean(mcpMutationBusy) || !server.name}
                          onClick={() => {
                            toggleMcpServer(server);
                          }}
                        >
                          <Power size={12} />
                        </button>
                        <button
                          className="icon-button mini danger"
                          type="button"
                          aria-label={`Delete MCP server ${server.name ?? ""}`}
                          title="Delete MCP server"
                          disabled={!mcpWritable || Boolean(mcpMutationBusy) || !server.name}
                          onClick={() => {
                            deleteMcpServer(server);
                          }}
                        >
                          <XCircle size={12} />
                        </button>
                      </div>
                    </div>
                    {localServer ? (
                      <div className="mcp-lifecycle-strip">
                        <span className={`stream-state ${mcpLifecycleStatusClass(server)}`}>{lifecycleBusy ? "updating" : lifecycleStatus}</span>
                        <span>pid {server.lifecycle_pid ?? "-"}</span>
                        <span>started {mcpLifecycleTimeLabel(server.lifecycle_started_at)}</span>
                        <span>runtime tools {server.lifecycle_tool_count ?? "-"}</span>
                      </div>
                    ) : null}
                    <dl className="mcp-server-meta">
                      <div>
                        <dt>Tools</dt>
                        <dd>{server.tool_count ?? tools.length}</dd>
                      </div>
                      <div>
                        <dt>Timeout</dt>
                        <dd>{server.timeout_ms ? `${server.timeout_ms}ms` : "-"}</dd>
                      </div>
                      <div>
                        <dt>Auth</dt>
                        <dd>{server.header_count ? `${server.header_count} headers` : "none"}</dd>
                      </div>
                      <div>
                        <dt>Env</dt>
                        <dd>{server.env_count ? `${server.env_count} vars` : "none"}</dd>
                      </div>
                      <div>
                        <dt>Checked</dt>
                        <dd>{serverBusy ? "testing" : mcpCheckedLabel(server)}</dd>
                      </div>
                    </dl>
                    {server.last_error ? <p className="stream-error">{server.last_error}</p> : null}
                    {tools.length ? (
                      <div className="mcp-tool-list">
                        {tools.slice(0, 6).map((tool) => (
                          <div className="mcp-tool-row" key={tool.name ?? mcpToolLabel(tool)}>
                            <strong>{mcpToolLabel(tool)}</strong>
                            <span>{compactId(tool.name)}</span>
                            {tool.description ? <p>{compactText(tool.description, 150)}</p> : null}
                          </div>
                        ))}
                        {tools.length > 6 ? <span className="part-more">+{tools.length - 6} more tools</span> : null}
                      </div>
                    ) : (
                      <p className="muted-line">No tools discovered</p>
                    )}
                  </section>
                );
              })}
            </div>
          ) : (
            <p className="muted-line">No MCP servers configured</p>
          )}
        </div>

        <div className="inspector-card">
          <div className="inspector-title">
            <Terminal size={15} />
            Stream
          </div>
          <dl>
            <dt>Status</dt>
            <dd>
              <span className={`stream-state ${statusClass(streamHealth.status)}`}>
                {streamHealth.status}
              </span>
            </dd>
            <dt>Events</dt>
            <dd>{events.length}</dd>
            <dt>Messages</dt>
            <dd>{sessionMessages?.message_v2_count ?? sessionMessages?.message_count ?? 0}</dd>
            <dt>Cursor</dt>
            <dd>{lastGlobalId.current}</dd>
            <dt>Resume</dt>
            <dd>{streamHealth.resume_cursor}</dd>
            <dt>Attempts</dt>
            <dd>{streamHealth.reconnect_attempts}</dd>
            <dt>Recovered</dt>
            <dd>{streamHealth.recovered_count}</dd>
            <dt>Batch</dt>
            <dd>{streamHealth.last_batch_count}</dd>
            <dt>Session</dt>
            <dd>{activeSessionId || "-"}</dd>
          </dl>
          {streamHealth.last_error ? (
            <p className="stream-error">
              {streamHealth.last_error}
              {streamHealth.next_retry_ms ? ` · retry ${streamHealth.next_retry_ms}ms` : ""}
            </p>
          ) : null}
        </div>

        <div className="inspector-card terminal-card">
          <div className="inspector-title">
            <Terminal size={15} />
            Terminal
            {terminalResult ? (
              <span className={`stream-state ${terminalResult.success ? "ok" : "bad"}`}>
                {terminalResult.timed_out ? "timeout" : terminalResult.success ? "ok" : "exit"}
              </span>
            ) : null}
          </div>
          <form className="terminal-run-form" onSubmit={runTerminalCommand}>
            <input
              aria-label="Terminal command"
              value={terminalCommand}
              onChange={(event) => setTerminalCommand(event.target.value)}
              placeholder="pwd"
            />
            <button disabled={terminalBusy || !terminalCommand.trim()} type="submit" title="Run terminal command">
              {terminalBusy ? "Running" : "Run"}
            </button>
          </form>
          {terminalResult ? (
            <>
              <dl>
                <dt>CWD</dt>
                <dd title={terminalResult.cwd ?? ""}>{terminalResult.cwd_relative || compactPath(terminalResult.cwd)}</dd>
                <dt>Exit</dt>
                <dd>{terminalResult.exit_code ?? "-"}</dd>
                <dt>Time</dt>
                <dd>{terminalResult.duration_ms ?? 0}ms</dd>
              </dl>
              <div className="terminal-output" data-testid="terminal-output">
                {terminalResult.stdout || terminalResult.stdout_truncated ? (
                  <div>
                    <span>stdout{terminalResult.stdout_truncated ? " truncated" : ""}</span>
                    <pre>{terminalResult.stdout}</pre>
                  </div>
                ) : null}
                {terminalResult.stderr || terminalResult.stderr_truncated ? (
                  <div>
                    <span>stderr{terminalResult.stderr_truncated ? " truncated" : ""}</span>
                    <pre>{terminalResult.stderr}</pre>
                  </div>
                ) : null}
              </div>
            </>
          ) : null}
          {terminalError ? <p className="stream-error">{terminalError}</p> : null}
        </div>

        <div className="inspector-card">
          <div className="inspector-title">
            <ShieldCheck size={15} />
            Trust
            <span className={`trust-count ${pendingInteractionCount ? "pending" : "clear"}`}>
              {pendingInteractionCount ? `${pendingInteractionCount} pending` : "clear"}
            </span>
          </div>
          <p className="trust-sync">{trustSyncLabel}</p>
          {approvals.length === 0 && questions.length === 0 ? (
            <p className="muted-line">No pending interaction</p>
          ) : null}
          <div className="trust-list">
            {approvals.map((item) => {
              const approval = item.approval ?? {};
              const preview = approval.preview as JsonRecord | undefined;
              const isResponding = respondingInteractionId === item.request_id;
              return (
                <div className="trust-item" key={item.request_id}>
                  <strong>{stringField(approval, "tool_name") || "approval"}</strong>
                  <span>{stringField(preview, "path") || compactId(item.request_id)}</span>
                  {stringField(preview, "diff") ? (
                    <pre className="mini-diff">{stringField(preview, "diff")}</pre>
                  ) : null}
                  <div className="inline-actions">
                    <button type="button" disabled={isResponding} onClick={() => respondApproval(item, "allow")}>
                      {isResponding ? "Working" : "Allow"}
                    </button>
                    <button type="button" disabled={isResponding} onClick={() => respondApproval(item, "deny")}>
                      Deny
                    </button>
                  </div>
                </div>
              );
            })}
            {questions.map((item) => {
              const question = item.question ?? {};
              const isResponding = respondingInteractionId === item.request_id;
              const fields = questionElicitationFields(question, item.request_id ?? "", questionDrafts);
              return (
                <div className="trust-item trust-question-form" key={item.request_id} data-testid="pending-question-form">
                  <strong>{fields[0]?.label || "Question"}</strong>
                  <span>{fields.length > 1 ? `${fields.length} fields` : fields[0]?.description || compactId(item.request_id)}</span>
                  <QuestionElicitationForm
                    fields={fields}
                    isResponding={isResponding}
                    onChange={(index, value) => updateQuestionDraft(item.request_id, index, value)}
                    onSubmit={() => respondQuestion(item)}
                    onDismiss={() => respondQuestion(item, true)}
                  />
                </div>
              );
            })}
          </div>
          <div className="trust-history">
            <div className="trust-history-title">
              <span>Recent history</span>
              <small>{trustHistory.length}</small>
            </div>
            {trustHistory.length ? (
              trustHistory.map((item) => <TrustHistoryCard item={item} key={item.id} compact />)
            ) : (
              <p className="muted-line">No trust history yet</p>
            )}
          </div>
        </div>

        <div className="inspector-card">
          <div className="inspector-title">
            <GitCompare size={15} />
            Diff
          </div>
          <dl>
            <dt>Undo</dt>
            <dd>{sessionDiff?.undo_count ?? 0}</dd>
            <dt>Redo</dt>
            <dd>{sessionDiff?.redo_count ?? 0}</dd>
          </dl>
          <div className="inline-actions">
            <button
              disabled={!sessionDiff?.undo_count}
              onClick={() => runPatchAction("undo")}
              type="button"
            >
              <Undo2 size={13} />
              Undo
            </button>
            <button
              disabled={!sessionDiff?.redo_count}
              onClick={() => runPatchAction("redo")}
              type="button"
            >
              <RefreshCw size={13} />
              Redo
            </button>
          </div>
          {sessionDiff?.latest ? (
            <div className="patch-preview">
              <strong>{stringField(sessionDiff.latest, "path") || "latest patch"}</strong>
              <span>{stringField(sessionDiff.latest, "status")}</span>
              <pre className="mini-diff">{stringField(sessionDiff.latest, "diff")}</pre>
            </div>
          ) : (
            <p className="muted-line">No file patch yet</p>
          )}
        </div>

        <div className="inspector-card">
          <div className="inspector-title">
            <History size={15} />
            Checkpoints
            {restoredCheckpointId ? <span className="checkpoint-restored">restored</span> : null}
          </div>
          {restoredCheckpointId ? (
            <p className="restore-state">Restored {compactId(restoredCheckpointId)}</p>
          ) : null}
          <dl>
            <dt>Total</dt>
            <dd>{checkpoints?.count ?? 0}</dd>
            <dt>Latest</dt>
            <dd>{checkpoints?.latest?.kind ?? "-"}</dd>
          </dl>
          <div className="checkpoint-list">
            {(checkpoints?.checkpoints ?? []).slice(0, 5).map((checkpoint) => {
              const isRestoring = restoringCheckpointId === checkpoint.checkpoint_id;
              const isRestored = restoredCheckpointId === checkpoint.checkpoint_id;
              return (
                <div className={`checkpoint-row ${isRestored ? "restored" : ""}`} key={checkpoint.checkpoint_id}>
                  <div>
                    <strong>{checkpointLabel(checkpoint)}</strong>
                    <span>
                      {numberField(checkpoint as JsonRecord, "file_count")} files ·{" "}
                      {numberField(checkpoint as JsonRecord, "total_bytes")} bytes
                    </span>
                  </div>
                  <button
                    data-checkpoint-id={checkpoint.checkpoint_id}
                    disabled={Boolean(restoringCheckpointId)}
                    title={isRestoring ? "Restoring" : "Restore"}
                    type="button"
                    onClick={() => restoreCheckpoint(checkpoint.checkpoint_id)}
                  >
                    <RotateCcw size={13} />
                  </button>
                </div>
              );
            })}
          </div>
        </div>

        <div className="inspector-card">
          <div className="inspector-title">
            <Folder size={15} />
            Files
          </div>
          <dl>
            <dt>Workspace</dt>
            <dd>{fileTree?.workspace ?? "-"}</dd>
            <dt>Entries</dt>
            <dd>
              {fileTree?.entry_count ?? 0}
              {fileTree?.truncated ? " +" : ""}
            </dd>
            <dt>Focus</dt>
            <dd>{filePreview?.path || fileTree?.path || "."}</dd>
          </dl>
          <div className="file-list">
            {(fileTree?.entries ?? []).slice(0, 8).map((entry) => (
              <div className="file-row" key={entry.path || entry.name}>
                <span className="file-badge">{fileBadge(entry)}</span>
                <strong>{entry.path || entry.name || "-"}</strong>
                <span>{entry.kind === "dir" ? "folder" : formatBytes(entry.size_bytes)}</span>
              </div>
            ))}
          </div>
          {filePreview?.exists === false ? (
            <p className="muted-line warning-line">
              <AlertTriangle size={13} />
              {filePreview.path || "File"} no longer exists
            </p>
          ) : filePreview?.content !== undefined && filePreview?.content !== null ? (
            <pre className="mini-diff file-preview">{filePreview.content}</pre>
          ) : (
            <p className="muted-line">No text preview selected</p>
          )}
        </div>

        <div className="inspector-card">
          <div className="inspector-title">
            <GitBranch size={15} />
            Git
          </div>
          {gitStatus?.is_repo ? (
            <>
              <dl>
                <dt>Branch</dt>
                <dd>{gitStatus.branch || "-"}</dd>
                <dt>Changes</dt>
                <dd>{gitStatus.change_count ?? 0}</dd>
                <dt>Ahead</dt>
                <dd>{gitStatus.ahead ?? 0}</dd>
                <dt>Behind</dt>
                <dd>{gitStatus.behind ?? 0}</dd>
              </dl>
              <div className="file-list">
                {(gitStatus.changes ?? []).slice(0, 8).map((change) => (
                  <div className="file-row git-change" key={`${change.status}:${change.path}`}>
                    <span className="file-badge">{change.status || "?"}</span>
                    <strong>{change.path || "-"}</strong>
                    <span>
                      {change.index || " "}
                      {change.worktree || " "}
                    </span>
                  </div>
                ))}
              </div>
              {(gitStatus.change_count ?? 0) === 0 ? <p className="muted-line">Clean workspace</p> : null}
            </>
          ) : (
            <p className="muted-line warning-line">
              <AlertTriangle size={13} />
              {gitStatus?.error || "No git repository"}
            </p>
          )}
        </div>
      </aside>
    </main>
  );
}

function EventContent({ event }: { event: AppEvent }) {
  const params = event.params ?? {};
  const text =
    stringParam(params, "delta") ||
    stringParam(params, "final_answer") ||
    stringParam(params, "output") ||
    stringParam(params, "error") ||
    stringParam(params, "status");

  if (text) {
    return <TextContent text={text} />;
  }
  return <pre>{JSON.stringify(params, null, 2)}</pre>;
}

function MessagePartCards({ parts }: { parts: MessagePart[] }) {
  if (parts.length === 0) return null;
  return (
    <div className="message-parts">
      {parts.map((part, index) => (
        <MessagePartCard key={`${part.id ?? "part"}:${part.kind ?? "part"}:${index}`} part={part} />
      ))}
    </div>
  );
}

function MessagePartCard({ part }: { part: MessagePart }) {
  const kind = part.kind ?? "part";
  const interaction = interactionHistoryItem(part);
  if (interaction) return <TrustHistoryCard item={interaction} variant="timeline" />;
  const mcpTrace = mcpToolTraceFromPart(part);
  const entries = patchEntries(part);
  const rows = partRows(part);
  const preText = partPreText(part);

  return (
    <section
      className={`message-part-card part-${kind}${mcpTrace ? " part-mcp-tool" : ""}`}
      data-part-kind={kind}
      data-testid={mcpTrace ? "mcp-tool-card" : undefined}
    >
      <div className="part-heading">
        <strong>
          {mcpTrace ? <PlugZap size={14} /> : partIcon(kind)}
          {partTitle(part)}
        </strong>
        <span className={`part-status ${statusClass(part.status)}`}>{part.status ?? "completed"}</span>
      </div>
      {mcpTrace ? (
        <div className="mcp-trace-strip" aria-label="MCP tool trace">
          <span>{mcpTrace.server || "mcp server"}</span>
          <span>{mcpTrace.transport || "transport"}</span>
          <span>{mcpTrace.dynamicTool || mcpTrace.toolName}</span>
          {mcpTrace.lifecycleReused ? <span>lifecycle reused</span> : null}
          {mcpTrace.lifecyclePid ? <span>pid {mcpTrace.lifecyclePid}</span> : null}
        </div>
      ) : null}
      <p className="part-summary">{partSummary(part)}</p>
      {rows.length > 0 ? (
        <dl className="part-grid">
          {rows.map(([label, value]) => (
            <div key={`${label}:${value}`}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      ) : null}
      {entries.length > 0 ? (
        <div className="part-path-list">
          {entries.slice(0, 6).map((entry, index) => {
            const path = firstText(entry.path) || `entry-${index + 1}`;
            const change = firstText(entry.change, entry.status) || "changed";
            return (
              <div className="part-path-row" key={`${path}:${index}`}>
                <span className={`change-chip change-${change}`}>{change}</span>
                <strong>{path}</strong>
              </div>
            );
          })}
          {entries.length > 6 ? <span className="part-more">+{entries.length - 6} more</span> : null}
        </div>
      ) : null}
      {preText ? <pre className="part-pre">{preText}</pre> : null}
    </section>
  );
}

function QuestionElicitationForm({
  fields,
  isResponding,
  onChange,
  onSubmit,
  onDismiss,
}: {
  fields: ElicitationField[];
  isResponding: boolean;
  onChange: (index: number, values: string[]) => void;
  onSubmit: () => void;
  onDismiss: () => void;
}) {
  const hasErrors = fields.some((field) => field.error);
  return (
    <div className="elicitation-form" data-testid="elicitation-form">
      {fields.map((field) => {
        const inputId = `elicitation-${field.id.replace(/[^a-zA-Z0-9_-]/g, "-")}`;
        return (
          <div className={`elicitation-field elicitation-field-${field.kind}`} key={field.id}>
            <label htmlFor={inputId}>
              {field.label}
              <small>{field.required ? field.kind : "optional"}</small>
            </label>
            {field.description && field.description !== field.label ? <em>{field.description}</em> : null}
            {field.kind === "boolean" ? (
              <label className="elicitation-check" htmlFor={inputId}>
                <input
                  id={inputId}
                  type="checkbox"
                  checked={field.value === "true"}
                  disabled={isResponding}
                  onChange={(event) => onChange(field.index, [event.target.checked ? "true" : "false"])}
                />
                <span>{field.value === "true" ? "Yes" : "No"}</span>
              </label>
            ) : field.kind === "multiselect" ? (
              field.options.length ? (
                <div className="elicitation-options" role="group" aria-labelledby={inputId}>
                  {field.options.map((option) => (
                    <label className="elicitation-check" key={option.value}>
                      <input
                        type="checkbox"
                        checked={field.values.includes(option.value)}
                        disabled={isResponding}
                        onChange={(event) => {
                          const selected = event.target.checked
                            ? [...field.values, option.value]
                            : field.values.filter((value) => value !== option.value);
                          onChange(field.index, selected);
                        }}
                      />
                      <span>{option.label}</span>
                    </label>
                  ))}
                </div>
              ) : (
                <textarea
                  id={inputId}
                  value={field.values.join(", ")}
                  disabled={isResponding}
                  rows={2}
                  placeholder={field.placeholder || "Separate answers with commas"}
                  onChange={(event) => onChange(field.index, event.target.value.split(",").map((value) => value.trim()).filter(Boolean))}
                />
              )
            ) : field.kind === "select" ? (
              <select
                id={inputId}
                value={field.value}
                disabled={isResponding}
                onChange={(event) => onChange(field.index, [event.target.value])}
              >
                {field.options.map((option) => (
                  <option key={option.value} value={option.value}>
                    {option.label}
                  </option>
                ))}
              </select>
            ) : field.kind === "number" || field.kind === "integer" ? (
              <input
                id={inputId}
                type="number"
                value={field.value}
                disabled={isResponding}
                min={field.min}
                max={field.max}
                step={field.kind === "integer" ? 1 : "any"}
                placeholder={field.placeholder}
                onChange={(event) => onChange(field.index, [event.target.value])}
              />
          ) : (
            <textarea
              id={inputId}
              value={field.value}
              disabled={isResponding}
              rows={2}
              placeholder={field.placeholder}
              onChange={(event) => onChange(field.index, [event.target.value])}
            />
          )}
            {field.error ? <strong className="elicitation-error">{field.error}</strong> : null}
          </div>
        );
      })}
      <div className="inline-actions">
        <button type="button" disabled={isResponding || hasErrors} onClick={onSubmit}>
          {isResponding ? "Working" : "Reply"}
        </button>
        <button type="button" disabled={isResponding} onClick={onDismiss}>
          Dismiss
        </button>
      </div>
    </div>
  );
}

function TrustHistoryCard({
  item,
  compact = false,
  variant = "dock",
}: {
  item: TrustHistoryItem;
  compact?: boolean;
  variant?: "dock" | "timeline";
}) {
  return (
    <section
      className={`trust-history-item ${item.tone} ${compact ? "compact" : ""} ${variant}`}
      data-part-kind={item.kind}
      data-interaction-status={item.status}
    >
      <div className="trust-history-heading">
        <strong>
          {item.kind === "approval" ? <ShieldCheck size={13} /> : <Bot size={13} />}
          {item.title}
        </strong>
        <span className={`part-status ${statusClass(item.status)}`}>{item.status}</span>
      </div>
      <p>{item.summary}</p>
      {item.detail ? <span className="trust-history-detail">{item.detail}</span> : null}
      {!compact ? (
        <dl className="part-grid">
          {nonEmptyRows([
            ["Request", compactId(item.requestId)],
            ["Call", compactId(item.callId)],
          ]).map(([label, value]) => (
            <div key={`${item.id}:${label}`}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      ) : null}
    </section>
  );
}

function stringParam(params: JsonRecord, key: string): string {
  const value = params[key];
  return typeof value === "string" ? value : "";
}

async function readSse(response: Response, onEvents?: SseEventHandler): Promise<AppEvent[]> {
  const reader = response.body?.getReader();
  if (!reader) return [];
  const decoder = new TextDecoder();
  let buffer = "";
  const events: AppEvent[] = [];

  async function emit(event: AppEvent | null) {
    if (!event) return;
    events.push(event);
    await onEvents?.([event]);
  }

  while (true) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    let split = buffer.indexOf("\n\n");
    while (split >= 0) {
      const frame = buffer.slice(0, split);
      buffer = buffer.slice(split + 2);
      const event = parseFrame(frame);
      await emit(event);
      if (shouldYieldStreamPaint(event)) await nextPaint();
      split = buffer.indexOf("\n\n");
    }
  }
  const event = parseFrame(buffer);
  await emit(event);
  if (shouldYieldStreamPaint(event)) await nextPaint();
  return events;
}

function shouldYieldStreamPaint(event: AppEvent | null): boolean {
  return event?.method === "item/agentMessage/delta";
}

function nextPaint(): Promise<void> {
  return new Promise((resolve) => {
    if (typeof window.requestAnimationFrame === "function") {
      window.requestAnimationFrame(() => resolve());
    } else {
      window.setTimeout(resolve, 16);
    }
  });
}

function sleepMs(ms: number): Promise<void> {
  return new Promise((resolve) => window.setTimeout(resolve, ms));
}

function parseFrame(frame: string): AppEvent | null {
  const lines = frame
    .split(/\r?\n/)
    .map((line) => line.trimEnd())
    .filter((line) => line.startsWith("data:"));
  if (!lines.length) return null;
  const data = lines.map((line) => line.replace(/^data:\s?/, "")).join("\n").trim();
  if (!data || data === "[DONE]") return null;
  try {
    const parsed = JSON.parse(data) as AppEvent;
    return parsed.method ? parsed : null;
  } catch {
    return null;
  }
}
