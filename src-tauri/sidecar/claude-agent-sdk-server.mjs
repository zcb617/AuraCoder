#!/usr/bin/env node
// Bridges the Claude Agent SDK to a stdio-based JSON-line protocol for AuraCoder.

const { createReadStream } = await import("no" + "de:fs");
const { readdir, stat } = await import("no" + "de:fs/promises");
const { default: os } = await import("no" + "de:os");

import { readFile } from "node:fs/promises";
import { ChildProcess, execFile } from "node:child_process";
import path from "node:path";
import { createInterface } from "node:readline";
import { Readable } from "node:stream";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
// 旧静态导入保留迁移留痕：依赖加载已迁移到可捕获的启动流程。
// import { fromJSONSchema } from "zod/v4";
let fromJSONSchema;

// 启动阶段依赖失败必须先输出结构化错误，便于宿主保留真实失败上下文。
function emitStartupDependencyError(message) {
  process.stdout.write(
    JSON.stringify({
      type: "error",
      message,
      recoverable: false,
      errorType: "startup_dependency_load_failed",
      isAuthError: false,
    }) + "\n",
  );
}

const [nodeMajorVersion, nodeMinorVersion] = process.versions.node
  .split(".")
  .map(Number);
const supportsDisposableChildProcessVersion =
  nodeMajorVersion > 20 ||
  (nodeMajorVersion === 20 && nodeMinorVersion >= 5);
if (
  !supportsDisposableChildProcessVersion ||
  typeof Symbol.dispose !== "symbol" ||
  typeof Symbol.asyncDispose !== "symbol" ||
  typeof ChildProcess.prototype[Symbol.dispose] !== "function"
) {
  // 旧的非结构化版本错误输出保留迁移留痕；新的结构化事件先行输出并退出。
  emitStartupDependencyError(
    `Claude runtime requires version 20.5 or newer with disposable child process support. AuraCoder resolved runtime ${process.version}.`,
  );
  process.exit(1);
  /*
  process.stdout.write(
    JSON.stringify({
      type: "error",
      message: `Claude requires Node.js 20.5 or newer with disposable child process support. AuraCoder resolved Node.js ${process.versions.node}.`,
    }) + "\n",
  );
  // 旧输出内容保留在迁移注释中。
  process.exit(1);
  */
}

let queryFn;
let toolFn;
let createSdkMcpServerFn;
let sdkEntryPath = null;
let sdkVersion = null;
let bundledClaudeCodeVersion = null;
const sdkModuleSpecifier = process.env.CLAUDE_AGENT_SDK_MODULE;
try {
  try {
    const zod = await import("zod/v4");
    fromJSONSchema = zod.fromJSONSchema;
    if (typeof fromJSONSchema !== "function") {
      throw new Error("zod/v4 does not export fromJSONSchema");
    }
  } catch (err) {
    const detail = err instanceof Error ? err.message : String(err);
    emitStartupDependencyError(`Failed to load zod/v4: ${detail}.`);
    process.exit(1);
  }
  const sdk = sdkModuleSpecifier
    ? await import(sdkModuleSpecifier)
    : await import("@anthropic-ai/claude-agent-sdk");
  queryFn = sdk.query;
  toolFn = sdk.tool;
  createSdkMcpServerFn = sdk.createSdkMcpServer;
  sdkEntryPath = sdkModuleSpecifier
    ? sdkModuleSpecifier.startsWith("file:")
      ? fileURLToPath(sdkModuleSpecifier)
      : sdkModuleSpecifier
    : fileURLToPath(import.meta.resolve("@anthropic-ai/claude-agent-sdk"));
  try {
    const sdkPackage = JSON.parse(
      await readFile(path.join(path.dirname(sdkEntryPath), "package.json"), "utf8"),
    );
    sdkVersion = typeof sdkPackage.version === "string" ? sdkPackage.version : null;
    bundledClaudeCodeVersion =
      typeof sdkPackage.claudeCodeVersion === "string"
        ? sdkPackage.claudeCodeVersion
        : null;
  } catch {
    // Runtime metadata is diagnostic only. Model discovery can continue without it.
  }
} catch (err) {
  const detail = err instanceof Error ? err.message : String(err);
  emitStartupDependencyError(
    sdkModuleSpecifier
      ? `Failed to load ${sdkModuleSpecifier}: ${detail}.`
      : `Failed to load default @anthropic-ai/claude-agent-sdk: ${detail}.`,
  );
  process.exit(1);
  /*
  process.stdout.write(
    JSON.stringify({
      type: "error",
      message: sdkModuleSpecifier
        ? `Failed to load ${sdkModuleSpecifier}: ${err.message}.`
        : `Failed to load bundled @anthropic-ai/claude-agent-sdk: ${err.message}.`,
    }) + "\n",
  );
  process.exit(1);
  */
}

const rl = createInterface({ input: process.stdin, crlfDelay: Infinity });
const activeQueries = new Map();
const sessionHandles = new Map();
const pendingApprovals = new Map();
let shuttingDown = false;
const claudeCodeExecutable = process.env.PANES_CLAUDE_CODE_EXECUTABLE?.trim() || null;
const execFileAsync = promisify(execFile);
const { inspect } = await import("no" + "de:util");

// 开发阶段将 Claude SDK 与 MCP 调用的完整原始链路写入 stderr，便于还原一次完整业务轮次。
function traceClaudeSdk(eventName, payload) {
  const inspectedPayload = inspect(payload, {
    depth: null,
    maxArrayLength: null,
    maxStringLength: null,
    breakLength: Infinity,
    compact: true,
  });
  process.stderr.write(
    `[claude-sdk-trace] ${new Date().toISOString()} event=${eventName} payload=${inspectedPayload}\n`,
  );
}

// 本机 Claude 会话扫描最多返回的摘要数量，避免历史文件过多阻塞 IPC。
const MAX_CLAUDE_SESSIONS = 500;
// 本机 Claude 会话摘要读取的最大 JSONL 行数，避免读取完整历史内容。
const MAX_CLAUDE_TRANSCRIPT_LINES = 200;
// Claude 会话文件名使用 UUID，完整历史读取只接受合法会话标识。
const CLAUDE_SESSION_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const claudeUsageUrl =
  process.env.PANES_CLAUDE_USAGE_URL?.trim() || "https://api.anthropic.com/api/oauth/usage";
const claudeUsageFetchDisabled = ["1", "true", "yes"].includes(
  String(process.env.PANES_DISABLE_CLAUDE_USAGE_FETCH || "").toLowerCase(),
);
const CLAUDE_USAGE_CACHE_TTL_MS = 60_000;
let claudeUsageCache = null;
const MAX_ATTACHMENTS_PER_TURN = 10;
const MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;
const MAX_TEXT_ATTACHMENT_CHARS = 40_000;
const TOOL_OUTPUT_CHUNK_SIZE = 8_192;
const TEXT_ATTACHMENT_EXTENSIONS = new Set([
  "txt",
  "md",
  "json",
  "js",
  "ts",
  "tsx",
  "jsx",
  "py",
  "rs",
  "go",
  "css",
  "html",
  "yaml",
  "yml",
  "toml",
  "xml",
  "sql",
  "sh",
  "csv",
  "svg",
]);
const IMAGE_ATTACHMENT_MEDIA_TYPES = new Map([
  ["png", "image/png"],
  ["jpg", "image/jpeg"],
  ["jpeg", "image/jpeg"],
  ["gif", "image/gif"],
  ["webp", "image/webp"],
]);
const SUPPORTED_IMAGE_MIME_TYPES = new Set(IMAGE_ATTACHMENT_MEDIA_TYPES.values());

function emit(obj) {
  process.stdout.write(JSON.stringify(obj) + "\n");
}

function chunkText(value, chunkSize) {
  if (typeof value !== "string" || value.length === 0) {
    return [];
  }

  const chunks = [];
  for (let index = 0; index < value.length; index += chunkSize) {
    chunks.push(value.slice(index, index + chunkSize));
  }
  return chunks;
}

function truncateTextToMaxChars(value, maxChars) {
  if ([...value].length <= maxChars) {
    return [value, false];
  }
  return [[...value].slice(0, maxChars).join(""), true];
}

function attachmentExtension(attachment) {
  const fileName = attachment?.fileName || attachment?.filePath || "";
  const extension = path.extname(fileName).replace(/^\./, "").toLowerCase();
  return extension || "";
}

function normalizeAttachmentMimeType(attachment) {
  const mimeType = attachment?.mimeType;
  return typeof mimeType === "string" && mimeType.trim()
    ? mimeType.trim().toLowerCase()
    : null;
}

function isSupportedTextMimeType(mimeType) {
  return (
    mimeType.startsWith("text/") ||
    mimeType.includes("json") ||
    mimeType.includes("xml") ||
    mimeType.includes("yaml") ||
    mimeType.includes("toml") ||
    mimeType.includes("javascript") ||
    mimeType.includes("typescript") ||
    mimeType.includes("x-rust") ||
    mimeType.includes("x-python") ||
    mimeType.includes("x-go") ||
    mimeType.includes("x-shellscript") ||
    mimeType.includes("sql") ||
    mimeType.includes("csv")
  );
}

function classifyAttachment(attachment) {
  const mimeType = normalizeAttachmentMimeType(attachment);
  const extension = attachmentExtension(attachment);

  if (mimeType && SUPPORTED_IMAGE_MIME_TYPES.has(mimeType)) {
    return {
      kind: "image",
      mediaType: mimeType,
    };
  }

  if (mimeType === "image/svg+xml") {
    return { kind: "text" };
  }

  if (mimeType && isSupportedTextMimeType(mimeType)) {
    return { kind: "text" };
  }

  if (IMAGE_ATTACHMENT_MEDIA_TYPES.has(extension)) {
    return {
      kind: "image",
      mediaType: IMAGE_ATTACHMENT_MEDIA_TYPES.get(extension),
    };
  }

  if (TEXT_ATTACHMENT_EXTENSIONS.has(extension)) {
    return { kind: "text" };
  }

  return null;
}

async function buildAttachmentContentBlock(attachment, cwd) {
  const resolvedPath = normalizePath(cwd, attachment?.filePath ?? attachment?.path);
  const fileName =
    (typeof attachment?.fileName === "string" && attachment.fileName.trim()) ||
    (resolvedPath ? path.basename(resolvedPath) : "attachment");

  if (!resolvedPath) {
    throw new Error(`Attachment "${fileName}" has an empty path.`);
  }

  const attachmentType = classifyAttachment(attachment);
  if (!attachmentType) {
    throw new Error(
      `Attachment "${fileName}" is not supported by the Claude sidecar. Only text and PNG/JPEG/GIF/WEBP image attachments are currently supported.`,
    );
  }

  let bytes;
  try {
    bytes = await readFile(resolvedPath);
  } catch (err) {
    throw new Error(
      `Attachment "${fileName}" could not be read at "${resolvedPath}": ${err.message || String(err)}`,
    );
  }

  const sizeBytes = Math.max(bytes.byteLength, Number(attachment?.sizeBytes) || 0);
  if (sizeBytes > MAX_ATTACHMENT_BYTES) {
    throw new Error(`Attachment "${fileName}" exceeds the 10 MB per-file limit.`);
  }

  if (attachmentType.kind === "image") {
    return {
      type: "image",
      source: {
        type: "base64",
        media_type: attachmentType.mediaType,
        data: bytes.toString("base64"),
      },
    };
  }

  const rawText = bytes.toString("utf8");
  const [truncatedText, wasTruncated] = truncateTextToMaxChars(
    rawText,
    MAX_TEXT_ATTACHMENT_CHARS,
  );
  let text = `Attached text file: ${fileName} (${resolvedPath})\n<attached-file-content>\n${truncatedText}\n</attached-file-content>`;
  if (wasTruncated) {
    text += `\n\n[Attachment content was truncated to ${MAX_TEXT_ATTACHMENT_CHARS} characters.]`;
  }

  return {
    type: "text",
    text,
  };
}

function buildPromptInput(prompt, attachments, cwd, sessionIdHint) {
  if (!Array.isArray(attachments) || attachments.length === 0) {
    return prompt;
  }

  if (attachments.length > MAX_ATTACHMENTS_PER_TURN) {
    throw new Error(
      `You can attach at most ${MAX_ATTACHMENTS_PER_TURN} files per Claude turn.`,
    );
  }

  return (async function* promptWithAttachments() {
    const content = [];
    if (typeof prompt === "string" && prompt.length > 0) {
      content.push({ type: "text", text: prompt });
    }

    for (const attachment of attachments) {
      content.push(await buildAttachmentContentBlock(attachment, cwd));
    }

    if (content.length === 0) {
      throw new Error(
        "Claude turn must include either a prompt or at least one supported attachment.",
      );
    }

    yield {
      type: "user",
      message: {
        role: "user",
        content,
      },
      parent_tool_use_id: null,
      session_id: sessionIdHint || "",
    };
  })();
}

function mapToolNameToActionType(toolName) {
  switch (toolName) {
    case "Read":
      return "file_read";
    case "Write":
      return "file_write";
    case "Edit":
      return "file_edit";
    case "Bash":
      return "command";
    case "WebFetch":
      return "search";
    case "Glob":
    case "Grep":
      return "search";
    default:
      return "other";
  }
}

function summarizeTool(toolName, toolInput) {
  if (!toolInput) return toolName;
  if (toolInput.command) return `${toolName}: ${toolInput.command}`;
  if (toolInput.file_path) return `${toolName}: ${toolInput.file_path}`;
  if (toolInput.pattern) return `${toolName}: ${toolInput.pattern}`;
  if (toolInput.url) return `${toolName}: ${toolInput.url}`;
  if (toolInput.prompt) return `${toolName}: ${toolInput.prompt.slice(0, 80)}`;
  return toolName;
}

function normalizePath(cwd, value) {
  if (typeof value !== "string" || !value.trim()) {
    return null;
  }
  return path.resolve(cwd, value);
}

function isWithinRoot(rootPath, targetPath) {
  const rel = path.relative(rootPath, targetPath);
  return rel === "" || (!rel.startsWith("..") && !path.isAbsolute(rel));
}

function isWithinAnyRoot(roots, targetPath) {
  return roots.some((root) => isWithinRoot(root, targetPath));
}

function collectCandidatePaths(toolName, toolInput, cwd) {
  const paths = [];
  const add = (value) => {
    const normalized = normalizePath(cwd, value);
    if (normalized) {
      paths.push(normalized);
    }
  };

  switch (toolName) {
    case "Read":
    case "Write":
    case "Edit":
      add(toolInput?.file_path ?? toolInput?.path);
      add(toolInput?.new_file_path);
      add(toolInput?.old_file_path);
      break;
    case "Glob":
    case "Grep":
      add(toolInput?.path);
      add(toolInput?.cwd);
      break;
    default:
      break;
  }

  return paths;
}

/*
function resolvePermissionMode(approvalPolicy, allowNetwork) {
  switch (approvalPolicy) {
    case "restricted":
    case "standard":
    case "trusted":
      return approvalPolicy;
    case "untrusted":
      return "restricted";
    case "never":
      return "trusted";
    case "on-failure":
      return "standard";
    case "on-request":
    default:
      return allowNetwork ? "trusted" : "standard";
  }
}
*/

/** 将 AuraCoder 权限策略映射为 Claude SDK 原生权限选项和审批决策模式。 */
function resolveClaudeSdkPermissionOptions(approvalPolicy, planMode) {
  if (approvalPolicy === "bypassPermissions") {
    return {
      sdkPermissionMode: "bypassPermissions",
      decisionMode: "full",
      allowDangerouslySkipPermissions: true,
    };
  }
  if (planMode) {
    return {
      sdkPermissionMode: "plan",
      decisionMode: "ask",
      allowDangerouslySkipPermissions: false,
    };
  }
  switch (approvalPolicy) {
    case "dontAsk":
    case "restricted":
      return { sdkPermissionMode: "dontAsk", decisionMode: "read-only", allowDangerouslySkipPermissions: false };
    case "default":
    case "standard":
      return { sdkPermissionMode: "default", decisionMode: "ask", allowDangerouslySkipPermissions: false };
    case "acceptEdits":
    case "trusted":
      return { sdkPermissionMode: "acceptEdits", decisionMode: "workspace-auto", allowDangerouslySkipPermissions: false };
    case "bypassPermissions":
    case "never":
      return { sdkPermissionMode: "bypassPermissions", decisionMode: "full", allowDangerouslySkipPermissions: true };
    case "untrusted":
    case "on-failure":
    case "on-request":
    default:
      return { sdkPermissionMode: "default", decisionMode: "ask", allowDangerouslySkipPermissions: false };
  }
}

/*
function requiresApproval(permissionMode, toolName) {
  if (
    typeof toolName === "string" &&
    toolName.startsWith("mcp__auracoder-computer-control__")
  ) {
    return false;
  }
  if (permissionMode === "trusted") {
    return false;
  }
  if (permissionMode === "restricted") {
    return true;
  }
  return !["Read", "Glob", "Grep", "ExitPlanMode", "EnterPlanMode"].includes(toolName);
}
*/
/*
// 旧 permissionMode 审批判定已由 resolveClaudeSdkPermissionOptions 和 buildPermissionHandler 接替。
function requiresApprovalLegacy(permissionMode, toolName) {
  return requiresApproval(permissionMode, toolName);
}
*/

/**
 * 将 Claude SDK 的后台任务生命周期消息转换为 AuraCoder 可展示的状态通知。
 * 该通知只传递任务元数据，不读取或解释后台任务输出文件。
 */
function emitClaudeBackgroundNotice(id, subtype, task = null, tasks = null, context = null) {
  const taskId = typeof task?.task_id === "string" ? task.task_id : null;
  const description = typeof task?.description === "string" ? task.description : "Claude 后台任务";
  const status = typeof task?.status === "string" ? task.status : null;
  const summary = typeof task?.summary === "string" ? task.summary : null;
  const activeTasks = Array.isArray(tasks) ? tasks : null;
  const backgroundTaskMetadata = context
    ? [...context.backgroundTaskDisplay.values()]
        .sort((left, right) => {
          const leftRunning = left.status === "running";
          const rightRunning = right.status === "running";
          if (leftRunning !== rightRunning) {
            return leftRunning ? -1 : 1;
          }
          if (!leftRunning) {
            const leftFinishedAt =
              typeof left.finishedAt === "number" ? left.finishedAt : Number.MAX_SAFE_INTEGER;
            const rightFinishedAt =
              typeof right.finishedAt === "number" ? right.finishedAt : Number.MAX_SAFE_INTEGER;
            if (leftFinishedAt !== rightFinishedAt) {
              return leftFinishedAt - rightFinishedAt;
            }
          }
          return left.taskId.localeCompare(right.taskId);
        })
        .map((displayTask) => ({
          // 后台任务稳定标识，用于前端在生命周期更新中复用同一行。
          taskId: displayTask.taskId,
          // 后台任务类型，用于保留 SDK 提供的任务元数据。
          taskType: displayTask.taskType,
          // 后台任务业务描述，用于展示任务名称。
          description: displayTask.description,
          // 后台任务当前生命周期状态，用于显示状态标签。
          status: displayTask.status,
          // 后台任务最新可展示摘要，不包含任务输出文件内容。
          ...(typeof displayTask.summary === "string" && displayTask.summary.length > 0
            ? { summary: displayTask.summary }
            : {}),
          // 后台任务开始时间，由 sidecar 在收到 SDK 生命周期事件时生成。
          startedAt: displayTask.startedAt,
          // 后台任务终态时间，仅终态任务拥有该字段。
          ...(typeof displayTask.finishedAt === "number"
            ? { finishedAt: displayTask.finishedAt }
            : {}),
        }))
    : null;
  let message;

  if (subtype === "background_tasks_changed") {
    message = activeTasks?.length
      ? `正在等待 ${activeTasks.length} 个 Claude 后台任务：${activeTasks
          .map((candidate) => candidate?.description || candidate?.task_id || "Claude 后台任务")
          .join("、")}`
      : "Claude 后台任务已全部结束，正在等待最终结果。";
  } else if (subtype === "task_started") {
    message = `Claude 后台任务已启动：${description}`;
  } else if (subtype === "task_updated") {
    message = `Claude 后台任务状态已更新：${description}`;
  } else if (subtype === "task_progress") {
    message = `Claude 后台任务进行中：${summary || description}`;
  } else {
    message = `Claude 后台任务${status || "已完成"}：${summary || description}`;
  }

  emit({
    // 关联当前 AuraCoder 查询，确保远端事件过滤不会丢失通知。
    id,
    // 使用既有 notice 协议向当前 assistant 消息展示后台任务状态。
    type: "notice",
    // 标识这是 Claude 后台任务业务通知。
    kind: "claude_background_tasks",
    // 后台任务生命周期属于进行中的信息提示。
    level: "info",
    // 为用户提供统一的后台任务标题。
    title: "Claude 后台任务",
    // 提示用户当前正在等待或继续处理后台任务。
    message,
    // 传递后台任务卡片所需的完整显示元数据。
    ...(backgroundTaskMetadata
      ? {
          metadata: {
            // display map 保留活动任务和已结束任务，供当前 assistant 消息完整展示。
            backgroundTasks: backgroundTaskMetadata,
            // activeTaskCount 仅取 SDK 权威任务集合，不能被 display map 影响。
            activeTaskCount: context.backgroundTasks.size,
          },
        }
      : {}),
    // 保留 SDK 生命周期子类型，避免后台消息被静默丢弃。
    sdkSubtype: subtype,
    // 传递任务稳定标识，供当前消息关联任务状态。
    ...(taskId ? { taskId } : {}),
    // 传递 SDK 提供的任务状态，不在 sidecar 解释状态含义。
    ...(status ? { status } : {}),
    // 传递 SDK 提供的任务摘要，供用户看到后台任务结果提示。
    ...(summary ? { summary } : {}),
  });
}

/**
 * 将一个 AuraCoder 用户输入写入 Claude SDK 的可持续输入流，供初始轮次和后续轮次复用。
 */
async function pushClaudePromptInput(messageInput, input, sessionId) {
  if (typeof input === "string") {
    messageInput.push({
      type: "user",
      message: {
        role: "user",
        content: input,
      },
      parent_tool_use_id: null,
      session_id: sessionId || "",
    });
    return;
  }

  for await (const message of input) {
    messageInput.push(message);
  }
}

/** 创建一个查询上下文，并保存该查询当前可变的权限策略状态。 */
function createQueryContext(id, approvalPolicy = null, planMode = false) {
  const normalizedApprovalPolicy = typeof approvalPolicy === "string" ? approvalPolicy : null;
  const normalizedPlanMode = planMode === true;
  return {
    id,
    threadId: id,
    // 当前查询使用的可持续输入流，后台任务完成后继续向同一查询注入通知。
    messageInput: null,
    query: null,
    actionCounter: 0,
    actionIdsByToolUseId: new Map(),
    streamToolUseIdsByIndex: new Map(),
    suppressedToolUseIds: new Set(),
    pendingApprovalIds: new Set(),
    cancelled: false,
    // 当前逻辑轮次是否已经向 AuraCoder 发出最终完成事件。
    turnCompleted: false,
    // 当前逻辑轮次是否已经收到 SDK result 消息。
    sdkResultReceived: false,
    // 当前逻辑轮次最近一个 SDK result 的终态，用于最终完成状态回传。
    sdkTerminalStatus: "completed",
    // 当前查询是否使用持久会话句柄，决定输入流在轮次完成后是否继续保留。
    isPersistentSession: false,
    // 当前仍在运行的 SDK 后台任务，按 task_id 维护最新任务元数据。
    backgroundTasks: new Map(),
    // 当前 assistant 消息展示的后台任务，包括活动任务和已结束任务。
    backgroundTaskDisplay: new Map(),
    // 权威后台任务集合当前是否为空，只能随 background_tasks_changed 快照更新。
    authoritativeBackgroundTasksEmpty: true,
    // 当前权威任务集合中仍等待 task_notification 的任务标识。
    pendingTaskNotificationIds: new Set(),
    // 已收到 task_notification 的任务标识，避免快照重放时重新等待同一通知。
    notifiedTaskIds: new Set(),
    // 当前逻辑轮次是否仍在等待一个或多个 task_notification。
    awaitingTaskNotification: false,
    // 已向 SDK 输入流注入的 task notification synthetic continuation 数量。
    backgroundContinuationInjectedCount: 0,
    // 已收到对应 synthetic continuation result 的数量。
    backgroundContinuationResultCount: 0,
    sessionId: null,
    tokenUsage: null,
    stopReason: null,
    pendingComputerControlCalls: new Map(),
    // 当前查询正在使用的 AuraCoder 权限策略，可由活动查询命令更新。
    approvalPolicy: normalizedApprovalPolicy,
    // 当前查询是否处于计划模式，影响非完全自主策略的 SDK 权限模式。
    planMode: normalizedPlanMode,
    // 当前查询每次工具调用读取的可变权限决策状态。
    permissionOptions: resolveClaudeSdkPermissionOptions(
      normalizedApprovalPolicy,
      normalizedPlanMode,
    ),
  };
}

function setContextSessionId(context, sessionId) {
  if (typeof sessionId === "string" && sessionId.length > 0) {
    context.sessionId = sessionId;
  }
}

function updateContextTokenUsage(context, tokenUsage) {
  if (!tokenUsage || typeof tokenUsage !== "object" || Array.isArray(tokenUsage)) {
    return;
  }

  const input = Number(tokenUsage.input);
  const output = Number(tokenUsage.output);
  if (!Number.isFinite(input) && !Number.isFinite(output)) {
    return;
  }

  context.tokenUsage = {
    input: Number.isFinite(input) ? Math.max(0, Math.round(input)) : 0,
    output: Number.isFinite(output) ? Math.max(0, Math.round(output)) : 0,
  };
}

function emitTurnCompleted(context, status) {
  if (context.turnCompleted) {
    return;
  }

  context.turnCompleted = true;
  const payload = {
    id: context.id,
    type: "turn_completed",
    status,
    sessionId: context.sessionId,
  };
  if (context.tokenUsage) {
    payload.tokenUsage = context.tokenUsage;
  }
  if (typeof context.stopReason === "string" && context.stopReason.length > 0) {
    payload.stopReason = context.stopReason;
  }
  emit(payload);
}

function serializeToolOutput(output) {
  if (typeof output === "string") {
    return output;
  }
  if (output == null) {
    return undefined;
  }
  try {
    return JSON.stringify(output);
  } catch {
    return String(output);
  }
}

function getActionIdForToolUse(context, toolUseId) {
  if (typeof toolUseId === "string" && toolUseId.length > 0) {
    const actionId = context.actionIdsByToolUseId.get(toolUseId);
    context.actionIdsByToolUseId.delete(toolUseId);
    if (actionId) {
      return actionId;
    }
  }

  return `claude-action-${context.actionCounter}`;
}

function formatSdkResultError(message) {
  if (Array.isArray(message?.errors) && message.errors.length > 0) {
    return message.errors.join("\n");
  }
  if (typeof message?.subtype === "string" && message.subtype.length > 0) {
    return `Claude query failed: ${message.subtype.replaceAll("_", " ")}`;
  }
  return "Claude query failed.";
}

function cleanupPendingApprovalsForQuery(queryId, denialMessage) {
  const context = activeQueries.get(queryId);
  if (!context) {
    return;
  }

  for (const approvalId of context.pendingApprovalIds) {
    const pending = pendingApprovals.get(approvalId);
    if (!pending) {
      continue;
    }
    pendingApprovals.delete(approvalId);
    pending.resolve({
      behavior: "deny",
      message: denialMessage,
    });
  }
  context.pendingApprovalIds.clear();
}

function computerControlCallResultToClaudeContent(value) {
  const source = Array.isArray(value?.content)
    ? value.content
    : Array.isArray(value?.contentItems)
      ? value.contentItems
      : null;
  if (source) {
    const content = source.flatMap((item) => {
      if (item?.type === "text" || item?.type === "inputText") {
        return [{ type: "text", text: String(item.text ?? "") }];
      }
      if (item?.type === "image") {
        return [{
          type: "image",
          data: String(item.data ?? ""),
          mimeType: String(item.mimeType ?? "image/png"),
        }];
      }
      if (item?.type === "inputImage") {
        const match = String(item.imageUrl ?? "").match(
          /^data:([^;]+);base64,(.+)$/,
        );
        return match
          ? [{ type: "image", data: match[2], mimeType: match[1] }]
          : [{ type: "text", text: String(item.imageUrl ?? "") }];
      }
      return [];
    });
    if (content.length > 0) {
      return content;
    }
  }

  return [{
    type: "text",
    text: typeof value === "string" ? value : JSON.stringify(value ?? null),
  }];
}

function waitForComputerControlResult(context, callId, toolName, arguments_, signal) {
  return new Promise((resolve) => {
    const pending = {
      resolve,
      abortHandler: null,
      signal,
    };
    const abortHandler = () => {
      context.pendingComputerControlCalls.delete(callId);
      resolve({ ok: false, error: "Claude 电脑操作工具调用已取消。" });
    };
    pending.abortHandler = abortHandler;
    context.pendingComputerControlCalls.set(callId, pending);
    signal?.addEventListener?.("abort", abortHandler, { once: true });
    emit({
      id: context.id,
      type: "computer_control_tool_call",
      callId,
      toolName,
      arguments: arguments_ ?? {},
      threadId: context.threadId,
      turnId: context.id,
    });
  });
}

function convertToolInputSchemaToZod(toolName, inputSchema) {
  try {
    return fromJSONSchema(inputSchema);
  } catch (error) {
    const detail = error instanceof Error ? error.message : String(error);
    throw new Error(
      `工具 ${toolName} 的 inputSchema 无法转换为 Zod Schema：${detail}`,
      { cause: error },
    );
  }
}

function createAuraCoderComputerControlServer(context, toolSpecs) {
  if (typeof toolFn !== "function" || typeof createSdkMcpServerFn !== "function") {
    throw new Error("当前 Claude Agent SDK 不支持进程内自定义工具服务器。");
  }

  const tools = (Array.isArray(toolSpecs) ? toolSpecs : [])
    .filter(
      (spec) =>
        typeof spec?.name === "string" &&
        typeof spec?.description === "string" &&
        spec.inputSchema &&
        typeof spec.inputSchema === "object" &&
        !Array.isArray(spec.inputSchema),
    )
    .map(({ name, description, inputSchema }) =>
      // 旧实现把通用 JSON Schema 直接传给 Claude SDK，当前 SDK 不接受该对象。
      // toolFn(name, description, inputSchema, async (arguments_, extra = {}) => {
      toolFn(
        name,
        description,
        convertToolInputSchemaToZod(name, inputSchema),
        async (arguments_, extra = {}) => {
          const callId =
            extra.toolUseID ||
            extra.toolUseId ||
            `${context.id}-${name}-${context.pendingComputerControlCalls.size + 1}`;
          const result = await waitForComputerControlResult(
            context,
            callId,
            name,
            arguments_,
            extra.signal,
          );
          if (!result.ok) {
            return {
              isError: true,
              content: [{ type: "text", text: result.error }],
            };
          }
          return { content: computerControlCallResultToClaudeContent(result.value) };
        },
      ),
    );

  if (tools.length === 0) {
    return null;
  }

  return createSdkMcpServerFn({
    name: "auracoder-computer-control",
    version: "1.0.0",
    tools,
  });
}

function createAuraCoderThreadServer(context, toolSpecs) {
  if (typeof toolFn !== "function" || typeof createSdkMcpServerFn !== "function") {
    throw new Error("当前 Claude Agent SDK 不支持进程内自定义工具服务器。");
  }
  const tools = (Array.isArray(toolSpecs) ? toolSpecs : [])
    .filter(
      (spec) =>
        typeof spec?.name === "string" &&
        typeof spec?.description === "string" &&
        spec.inputSchema &&
        typeof spec.inputSchema === "object" &&
        !Array.isArray(spec.inputSchema),
    )
    .map(({ name, description, inputSchema }) =>
      // 旧实现把通用 JSON Schema 直接传给 Claude SDK，当前 SDK 不接受该对象。
      // toolFn(name, description, inputSchema, async (arguments_, extra = {}) => {
      toolFn(
        name,
        description,
        convertToolInputSchemaToZod(name, inputSchema),
        async (arguments_, extra = {}) => {
          const callId =
            extra.toolUseID ||
            extra.toolUseId ||
            `${context.id}-${name}-${context.pendingComputerControlCalls.size + 1}`;
          const result = await waitForComputerControlResult(
            context,
            callId,
            name,
            arguments_,
            extra.signal,
          );
          if (!result.ok) {
            return {
              isError: true,
              content: [{ type: "text", text: result.error }],
            };
          }
          return { content: computerControlCallResultToClaudeContent(result.value) };
        },
      ),
    );
  if (tools.length === 0) {
    return null;
  }
  return createSdkMcpServerFn({
    name: "auracoder-thread",
    version: "1.0.0",
    tools,
  });
}

function resolveComputerControlToolResult(params = {}) {
  const requestId = params.requestId || params.request_id || params.id;
  const callId = params.callId || params.call_id;
  if (!requestId || !callId) {
    return;
  }
  const context = activeQueries.get(requestId);
  const pending = context?.pendingComputerControlCalls.get(callId);
  if (!context || !pending) {
    return;
  }
  context.pendingComputerControlCalls.delete(callId);
  pending.signal?.removeEventListener?.("abort", pending.abortHandler);
  if (params.error) {
    pending.resolve({ ok: false, error: String(params.error) });
  } else {
    pending.resolve({ ok: true, value: params.result });
  }
}

function cleanupPendingComputerControlCalls(context, errorMessage) {
  for (const pending of context.pendingComputerControlCalls.values()) {
    pending.resolve({ ok: false, error: errorMessage });
  }
  context.pendingComputerControlCalls.clear();
}

function emitDeniedToolCompletion(context, toolUseId, errorMessage) {
  if (typeof toolUseId !== "string" || toolUseId.length === 0) {
    // toolUseId not provided by the SDK — the PreToolUse action_started
    // (if any) will remain dangling. This is a best-effort path.
    return;
  }

  const actionId = context.actionIdsByToolUseId.get(toolUseId);
  if (!actionId) {
    // Tool was denied before PreToolUse fired (e.g., content_block_start
    // no longer registers actionIds). No action_started was emitted, so
    // no action_completed is needed either.
    context.suppressedToolUseIds.add(toolUseId);
    return;
  }

  context.actionIdsByToolUseId.delete(toolUseId);
  context.suppressedToolUseIds.add(toolUseId);
  emit({
    id: context.id,
    type: "action_completed",
    actionId,
    success: false,
    error: errorMessage,
    durationMs: 0,
  });
}

/** 发出 AuraCoder 可展示的审批请求，并附加 Claude 子代理关联元数据。 */
function emitApprovalRequest(context, actionType, summary, details, metadata = {}) {
  const approvalId = `${context.id}:approval:${context.pendingApprovalIds.size + 1}:${Date.now()}`;
  const mergedDetails = {
    ...(details ?? {}),
    ...(typeof metadata.agentID === "string" && metadata.agentID.length > 0
      ? { _claudeAgentId: metadata.agentID }
      : {}),
    ...(typeof metadata.toolUseID === "string" && metadata.toolUseID.length > 0
      ? { _claudeToolUseId: metadata.toolUseID }
      : {}),
    ...(typeof metadata.requestId === "string" && metadata.requestId.length > 0
      ? { _claudeRequestId: metadata.requestId }
      : {}),
  };
  emit({
    id: context.id,
    type: "approval_requested",
    approvalId,
    actionType,
    summary,
    details: mergedDetails,
  });
  return approvalId;
}

/** 请求受控 Claude 工具授权，并将子代理标识传递到审批事件详情。 */
async function requestPermissionApproval(context, toolName, toolInput, suggestions = [], metadata = {}) {
  const approvalId = emitApprovalRequest(
    context,
    mapToolNameToActionType(toolName),
    summarizeTool(toolName, toolInput),
    toolInput ?? {},
    metadata,
  );

  const permission = await new Promise((resolve) => {
    pendingApprovals.set(approvalId, {
      queryId: context.id,
      suggestions,
      kind: "permission",
      resolve,
    });
    context.pendingApprovalIds.add(approvalId);
  });

  context.pendingApprovalIds.delete(approvalId);
  pendingApprovals.delete(approvalId);
  return permission;
}

function buildAskUserQuestionDetails(toolInput) {
  return {
    _serverMethod: "item/tool/requestuserinput",
    questions: Array.isArray(toolInput?.questions) ? toolInput.questions : [],
  };
}

function buildAskUserQuestionSummary(toolInput) {
  const questions = Array.isArray(toolInput?.questions) ? toolInput.questions : [];
  const firstQuestion = questions.find(
    (question) =>
      typeof question?.question === "string" && question.question.trim().length > 0,
  );
  if (firstQuestion) {
    return `AskUserQuestion: ${firstQuestion.question.trim()}`;
  }
  return "AskUserQuestion";
}

async function requestAskUserQuestionApproval(context, toolInput) {
  const approvalId = emitApprovalRequest(
    context,
    "other",
    buildAskUserQuestionSummary(toolInput),
    buildAskUserQuestionDetails(toolInput),
  );

  const permission = await new Promise((resolve) => {
    pendingApprovals.set(approvalId, {
      queryId: context.id,
      kind: "ask_user_question",
      toolInput,
      resolve,
    });
    context.pendingApprovalIds.add(approvalId);
  });

  context.pendingApprovalIds.delete(approvalId);
  pendingApprovals.delete(approvalId);
  return permission;
}

function normalizeAskUserQuestionAnswers(rawAnswers, questions) {
  if (
    typeof rawAnswers !== "object" ||
    rawAnswers === null ||
    Array.isArray(rawAnswers)
  ) {
    throw new Error("Claude AskUserQuestion responses require an `answers` object.");
  }

  const answers = {};
  for (let index = 0; index < questions.length; index += 1) {
    const question = questions[index];
    if (typeof question !== "object" || question === null || Array.isArray(question)) {
      continue;
    }

    const questionId =
      typeof question.id === "string" && question.id.trim()
        ? question.id.trim()
        : `question-${index + 1}`;
    const questionText =
      typeof question.question === "string" && question.question.trim()
        ? question.question.trim()
        : typeof question.header === "string" && question.header.trim()
          ? question.header.trim()
          : questionId;
    const answerValue = rawAnswers[questionId];
    const answerList = Array.isArray(answerValue?.answers)
      ? answerValue.answers
          .filter((value) => typeof value === "string")
          .map((value) => value.trim())
          .filter((value) => value.length > 0)
      : [];
    answers[questionText] = answerList.join(", ");
  }

  return answers;
}

function resolveAskUserQuestionResponse(response, toolInput) {
  if (!response || typeof response !== "object" || Array.isArray(response)) {
    throw new Error("Claude AskUserQuestion response must be a JSON object.");
  }

  if ("decision" in response) {
    const decision = normalizeApprovalDecision(response.decision);
    if (decision === "accept" || decision === "accept_for_session") {
      throw new Error("Claude AskUserQuestion requires `answers`, not a simple accept.");
    }
    return {
      behavior: "deny",
      message: "Claude AskUserQuestion was declined by the user.",
    };
  }

  if (!Object.prototype.hasOwnProperty.call(response, "answers")) {
    throw new Error("Claude AskUserQuestion response must include an `answers` object.");
  }

  const questions = Array.isArray(toolInput?.questions) ? toolInput.questions : [];
  return {
    behavior: "allow",
    updatedInput: {
      questions,
      answers: normalizeAskUserQuestionAnswers(response.answers, questions),
    },
  };
}

function emitToolOutputChunks(id, actionId, output) {
  const outputStr = serializeToolOutput(output);
  if (!outputStr) {
    return;
  }

  for (const content of chunkText(outputStr, TOOL_OUTPUT_CHUNK_SIZE)) {
    emit({
      id,
      type: "action_output_delta",
      actionId,
      stream: "stdout",
      content,
    });
  }
}

/** 为 Claude SDK 构建权限回调，每次工具调用读取查询当前权限状态。 */
function buildPermissionHandler({
  context,
  cwd,
  writableRoots,
  sandboxMode,
  allowNetwork,
}) {
  const normalizedRoots = writableRoots.map((root) => path.resolve(root));

  return async (toolName, input, options) => {
    const toolInput = input ?? {};
    const toolUseId = options?.toolUseID;
    const agentId = options?.agentID;
    const requestId = options?.requestId;
    const { decisionMode } = context.permissionOptions;

    if (toolName === "AskUserQuestion") {
      const permission = await requestAskUserQuestionApproval(context, toolInput);
      if (permission.behavior === "deny") {
        emitDeniedToolCompletion(context, toolUseId, permission.message);
      }
      return permission;
    }

    if (!allowNetwork && toolName === "WebFetch") {
      const permission = {
        behavior: "deny",
        message: "Network access is disabled for this repository.",
      };
      emitDeniedToolCompletion(context, toolUseId, permission.message);
      return permission;
    }

    if (options?.blockedPath) {
      const permission = {
        behavior: "deny",
        message: `Path outside the allowed workspace scope: ${options.blockedPath}`,
      };
      emitDeniedToolCompletion(context, toolUseId, permission.message);
      return permission;
    }

    // AuraCoder 自己的电脑操作代理会在每次真实 CUA 调用时弹出独立授权窗口；
    // 这里直接放行到代理，避免 Claude 的通用工具审批再弹一次。
    if (toolName.startsWith("mcp__auracoder-computer-control__")) {
      if (decisionMode === "read-only") {
        const permission = {
          behavior: "deny",
          message: "Tool execution is disabled in read-only mode.",
        };
        emitDeniedToolCompletion(context, toolUseId, permission.message);
        return permission;
      }
      return { behavior: "allow" };
    }

    if (toolName === "Write" || toolName === "Edit") {
      if (sandboxMode === "read-only") {
        const permission = {
          behavior: "deny",
          message: "File writes are disabled for this Claude thread.",
        };
        emitDeniedToolCompletion(context, toolUseId, permission.message);
        return permission;
      }

      const candidatePaths = collectCandidatePaths(toolName, toolInput, cwd);
      if (candidatePaths.length === 0) {
        const permission = {
          behavior: "deny",
          message: "Unable to verify the target path for this write operation.",
        };
        emitDeniedToolCompletion(context, toolUseId, permission.message);
        return permission;
      }

      if (!candidatePaths.every((candidate) => isWithinAnyRoot(normalizedRoots, candidate))) {
        const permission = {
          behavior: "deny",
          message: "This file path is outside the approved writable roots for the thread.",
        };
        emitDeniedToolCompletion(context, toolUseId, permission.message);
        return permission;
      }
    }

    if (decisionMode === "read-only") {
      if (
        ["Read", "Glob", "Grep", "Agent", "ExitPlanMode", "EnterPlanMode", "TaskOutput"].includes(
          toolName,
        )
      ) {
        return { behavior: "allow" };
      }
      const permission = {
        behavior: "deny",
        message: "Tool execution is disabled in read-only mode.",
      };
      emitDeniedToolCompletion(context, toolUseId, permission.message);
      return permission;
    }

    if (decisionMode === "workspace-auto" || decisionMode === "full") {
      return { behavior: "allow" };
    }

    const permission = await requestPermissionApproval(
      context,
      toolName,
      toolInput,
      options?.suggestions,
      { agentID: agentId, toolUseID: toolUseId, requestId },
    );
    if (permission.behavior === "deny") {
      emitDeniedToolCompletion(context, toolUseId, permission.message);
    }
    return permission;
  };
}

function normalizeApprovalDecision(value) {
  if (typeof value !== "string" || !value.trim()) {
    throw new Error("Claude approval responses require an explicit decision.");
  }

  const normalized = value.trim().toLowerCase();
  const compact = normalized.replaceAll("-", "").replaceAll("_", "");
  if (compact === "accept") {
    return "accept";
  }
  if (compact === "decline" || compact === "deny") {
    return "decline";
  }
  if (compact === "acceptforsession") {
    return "accept_for_session";
  }

  throw new Error(
    "Unsupported Claude approval decision. Expected one of: accept, decline, deny, accept_for_session.",
  );
}

function resolveApprovalDecision(response, suggestions = []) {
  const decision = normalizeApprovalDecision(response?.decision);
  if (decision === "accept") {
    return {
      behavior: "allow",
    };
  }
  if (decision === "accept_for_session") {
    return {
      behavior: "allow",
      ...(Array.isArray(suggestions) && suggestions.length > 0
        ? { updatedPermissions: suggestions }
        : {}),
    };
  }
  return {
    behavior: "deny",
    message: "Tool usage denied by the user.",
  };
}

function buildRateLimitUsageSnapshot(message) {
  const rateLimitInfo =
    typeof message?.rate_limit_info === "object" &&
    message.rate_limit_info !== null &&
    !Array.isArray(message.rate_limit_info)
      ? message.rate_limit_info
      : null;
  if (!rateLimitInfo) {
    return null;
  }

  const rateLimitType = String(rateLimitInfo.rateLimitType || "");
  const utilization = Number.isFinite(rateLimitInfo.utilization)
    ? Math.max(0, Math.round(rateLimitInfo.utilization * 100))
    : null;
  const resetsAt = Number.isFinite(rateLimitInfo.resetsAt)
    ? Math.round(rateLimitInfo.resetsAt)
    : null;
  const isFableWeeklyLimit =
    rateLimitType === "seven_day_overage_included" || rateLimitType === "seven_day_fable";

  const usage = {
    currentTokens: null,
    maxContextTokens: null,
    contextWindowPercent: null,
    fiveHourPercent: rateLimitType === "five_hour" ? utilization : null,
    weeklyPercent: rateLimitType === "seven_day" ? utilization : null,
    fableWeeklyPercent: isFableWeeklyLimit ? utilization : null,
    opusWeeklyPercent: rateLimitType === "seven_day_opus" ? utilization : null,
    sonnetWeeklyPercent: rateLimitType === "seven_day_sonnet" ? utilization : null,
    fiveHourResetsAt: rateLimitType === "five_hour" ? resetsAt : null,
    weeklyResetsAt: rateLimitType === "seven_day" ? resetsAt : null,
    fableWeeklyResetsAt: isFableWeeklyLimit ? resetsAt : null,
    opusWeeklyResetsAt: rateLimitType === "seven_day_opus" ? resetsAt : null,
    sonnetWeeklyResetsAt: rateLimitType === "seven_day_sonnet" ? resetsAt : null,
  };

  return Object.values(usage).some((value) => value !== null) ? usage : null;
}

function toUsagePercent(value) {
  const numeric = Number(value);
  if (!Number.isFinite(numeric)) {
    return null;
  }
  return Math.max(0, Math.min(100, Math.round(numeric)));
}

function toUnixTimestamp(value) {
  if (typeof value === "number" && Number.isFinite(value)) {
    return value > 10_000_000_000 ? Math.round(value / 1000) : Math.round(value);
  }
  if (typeof value !== "string" || value.trim().length === 0) {
    return null;
  }
  const timestamp = Date.parse(value);
  return Number.isFinite(timestamp) ? Math.round(timestamp / 1000) : null;
}

function readUsageWindow(value) {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return null;
  }
  const percent = toUsagePercent(value.utilization ?? value.percent ?? value.used_percentage);
  if (percent === null) {
    return null;
  }
  return {
    percent,
    resetsAt: toUnixTimestamp(value.resets_at ?? value.resetsAt),
  };
}

function buildUsageApiSnapshot(payload) {
  if (!payload || typeof payload !== "object" || Array.isArray(payload)) {
    return null;
  }

  const snapshot = {
    currentTokens: null,
    maxContextTokens: null,
    contextWindowPercent: null,
    fiveHourPercent: null,
    weeklyPercent: null,
    fableWeeklyPercent: null,
    opusWeeklyPercent: null,
    sonnetWeeklyPercent: null,
    fiveHourResetsAt: null,
    weeklyResetsAt: null,
    fableWeeklyResetsAt: null,
    opusWeeklyResetsAt: null,
    sonnetWeeklyResetsAt: null,
  };

  const assignWindow = (prefix, window) => {
    if (!window) return;
    snapshot[`${prefix}Percent`] = window.percent;
    snapshot[`${prefix}ResetsAt`] = window.resetsAt;
  };

  assignWindow("fiveHour", readUsageWindow(payload.five_hour));
  assignWindow("weekly", readUsageWindow(payload.seven_day));
  assignWindow(
    "fableWeekly",
    readUsageWindow(payload.seven_day_overage_included ?? payload.seven_day_fable),
  );
  assignWindow("opusWeekly", readUsageWindow(payload.seven_day_opus));
  assignWindow("sonnetWeekly", readUsageWindow(payload.seven_day_sonnet));

  if (Array.isArray(payload.limits)) {
    for (const limit of payload.limits) {
      const window = readUsageWindow(limit);
      if (!window) continue;
      if (limit.kind === "session") {
        assignWindow("fiveHour", window);
        continue;
      }
      if (limit.kind === "weekly_all") {
        assignWindow("weekly", window);
        continue;
      }
      if (limit.kind !== "weekly_scoped") continue;

      const modelName = String(
        limit.scope?.model?.display_name || limit.scope?.model?.id || "",
      ).toLowerCase();
      if (modelName.includes("fable")) {
        assignWindow("fableWeekly", window);
      } else if (modelName.includes("opus")) {
        assignWindow("opusWeekly", window);
      } else if (modelName.includes("sonnet")) {
        assignWindow("sonnetWeekly", window);
      }
    }
  }

  return Object.values(snapshot).some((value) => value !== null) ? snapshot : null;
}

async function readClaudeOauthAccessToken() {
  const environmentToken = process.env.CLAUDE_CODE_OAUTH_TOKEN?.trim();
  if (environmentToken) {
    return environmentToken;
  }

  if (process.platform === "darwin") {
    try {
      const { stdout } = await execFileAsync(
        "/usr/bin/security",
        ["find-generic-password", "-s", "Claude Code-credentials", "-w"],
        { encoding: "utf8", timeout: 2_000, maxBuffer: 1024 * 1024 },
      );
      const credentials = JSON.parse(stdout);
      const token = credentials?.claudeAiOauth?.accessToken;
      if (typeof token === "string" && token.trim().length > 0) {
        return token.trim();
      }
    } catch {
      // Fall through to the credentials file used on Linux and Windows.
    }
  }

  try {
    const homeDirectory = process.env.HOME || process.env.USERPROFILE;
    if (!homeDirectory) return null;
    const configDirectory =
      process.env.CLAUDE_CONFIG_DIR?.trim() || path.join(homeDirectory, ".claude");
    const credentials = JSON.parse(
      await readFile(path.join(configDirectory, ".credentials.json"), "utf8"),
    );
    const token = credentials?.claudeAiOauth?.accessToken;
    return typeof token === "string" && token.trim().length > 0 ? token.trim() : null;
  } catch {
    return null;
  }
}

async function fetchClaudeUsageSnapshot() {
  if (claudeUsageFetchDisabled) {
    return null;
  }
  const now = Date.now();
  if (claudeUsageCache && claudeUsageCache.expiresAt > now) {
    return claudeUsageCache.snapshot;
  }

  const token = await readClaudeOauthAccessToken();
  if (!token) {
    return null;
  }

  const controller = new AbortController();
  const timeout = setTimeout(() => controller.abort(), 5_000);
  try {
    const response = await fetch(claudeUsageUrl, {
      headers: {
        Authorization: `Bearer ${token}`,
        "anthropic-beta": "oauth-2025-04-20",
      },
      signal: controller.signal,
    });
    if (!response.ok) {
      return null;
    }
    const snapshot = buildUsageApiSnapshot(await response.json());
    if (snapshot) {
      claudeUsageCache = {
        expiresAt: now + CLAUDE_USAGE_CACHE_TTL_MS,
        snapshot,
      };
    }
    return snapshot;
  } catch {
    return null;
  } finally {
    clearTimeout(timeout);
  }
}

function inferClaudeContextWindowTokens(model) {
  const normalized = String(model || "").toLowerCase();
  const millionTokenMatch = normalized.match(/\[(\d+)m\]/);
  if (millionTokenMatch) {
    return Number(millionTokenMatch[1]) * 1_000_000;
  }
  return 200_000;
}

function buildContextUsageSnapshot(streamEvent, model) {
  if (streamEvent?.type !== "message_start") {
    return null;
  }

  const rawUsage = streamEvent.message?.usage;
  if (!rawUsage || typeof rawUsage !== "object" || Array.isArray(rawUsage)) {
    return null;
  }

  const inputTokenFields = [
    rawUsage.input_tokens,
    rawUsage.cache_creation_input_tokens,
    rawUsage.cache_read_input_tokens,
  ];
  const currentTokens = inputTokenFields.reduce((total, value) => {
    const numeric = Number(value);
    return Number.isFinite(numeric) ? total + Math.max(0, numeric) : total;
  }, 0);
  if (currentTokens <= 0) {
    return null;
  }

  const maxContextTokens = inferClaudeContextWindowTokens(model);
  const remainingPercent = Math.max(
    0,
    Math.min(100, Math.round(((maxContextTokens - currentTokens) / maxContextTokens) * 100)),
  );

  return {
    currentTokens: null,
    maxContextTokens: null,
    contextWindowPercent: remainingPercent,
    fiveHourPercent: null,
    weeklyPercent: null,
    fableWeeklyPercent: null,
    opusWeeklyPercent: null,
    sonnetWeeklyPercent: null,
    fiveHourResetsAt: null,
    weeklyResetsAt: null,
    fableWeeklyResetsAt: null,
    opusWeeklyResetsAt: null,
    sonnetWeeklyResetsAt: null,
  };
}

function buildStatusNotice(message) {
  if (message?.type !== "system" || message?.subtype !== "status") {
    return null;
  }

  if (message.status === "compacting") {
    return {
      kind: "claude_status",
      level: "info",
      title: "Claude status",
      message: "Claude is compacting context.",
    };
  }

  return null;
}

function formatAssistantMessageError(message) {
  const errorType =
    typeof message?.error === "string" && message.error.length > 0
      ? message.error
      : "unknown";

  switch (errorType) {
    case "authentication_failed":
      return {
        errorType,
        isAuthError: true,
        message: "Claude authentication failed. Sign in again or refresh your credentials.",
        recoverable: false,
      };
    case "billing_error":
      return {
        errorType,
        isAuthError: false,
        message: "Claude rejected the request because billing or subscription access failed.",
        recoverable: false,
      };
    case "rate_limit":
      return {
        errorType,
        isAuthError: false,
        message: "Claude rate limit reached. Wait for the limit window to reset and retry.",
        recoverable: true,
      };
    case "invalid_request":
      return {
        errorType,
        isAuthError: false,
        message: "Claude rejected the request as invalid.",
        recoverable: false,
      };
    case "server_error":
      return {
        errorType,
        isAuthError: false,
        message: "Claude returned a server error.",
        recoverable: true,
      };
    case "max_output_tokens":
      return {
        errorType,
        isAuthError: false,
        message: "Claude stopped because it reached the maximum output token limit.",
        recoverable: true,
      };
    default:
      return {
        errorType,
        isAuthError: false,
        message: "Claude returned an assistant error.",
        recoverable: false,
      };
  }
}

function updateTokenUsageFromStreamEvent(context, streamEvent) {
  if (!streamEvent || typeof streamEvent !== "object" || Array.isArray(streamEvent)) {
    return;
  }

  if (streamEvent.type === "message_start") {
    updateContextTokenUsage(context, {
      input: streamEvent.message?.usage?.input_tokens,
      output: streamEvent.message?.usage?.output_tokens,
    });
    return;
  }

  if (streamEvent.type === "message_delta") {
    updateContextTokenUsage(context, {
      input: context.tokenUsage?.input ?? 0,
      output: streamEvent.usage?.output_tokens,
    });
    if (typeof streamEvent.delta?.stop_reason === "string") {
      context.stopReason = streamEvent.delta.stop_reason;
    }
  }
}

function normalizeSandboxMode(value) {
  if (value == null || value === "") {
    return "workspace-write";
  }

  if (typeof value !== "string") {
    throw new Error("Claude sandboxMode must be a string.");
  }

  const normalized = value.trim().toLowerCase();
  const compact = normalized.replaceAll("-", "").replaceAll("_", "");
  if (compact === "readonly") {
    return "read-only";
  }
  if (compact === "workspacewrite") {
    return "workspace-write";
  }
  if (compact === "dangerfullaccess") {
    throw new Error(
      "Claude does not support sandboxMode=danger-full-access. Use read-only or workspace-write.",
    );
  }

  throw new Error(
    "Unsupported Claude sandboxMode. Expected one of: read-only, workspace-write.",
  );
}

function normalizeWritableRoots(cwd, writableRoots) {
  const normalizedRoots = Array.isArray(writableRoots)
    ? writableRoots
    .map((root) => (typeof root === "string" && root.trim() ? path.resolve(root) : null))
    .filter(Boolean)
    : [];

  if (normalizedRoots.length > 0) {
    return normalizedRoots;
  }

  return [path.resolve(cwd)];
}

function additionalDirectoriesForSandbox(cwd, sandboxMode, writableRoots) {
  if (sandboxMode !== "workspace-write") {
    return [];
  }

  return writableRoots.filter((root) => root !== path.resolve(cwd));
}

function allowWriteRootsForSandbox(sandboxMode, writableRoots) {
  if (sandboxMode !== "workspace-write") {
    return [];
  }

  return writableRoots;
}

function applyClaudeRuntime(options) {
  if (claudeCodeExecutable) {
    options.pathToClaudeCodeExecutable = claudeCodeExecutable;
  }
  return options;
}

async function* holdModelDiscoveryOpen() {
  await new Promise(() => {});
}

/** 根据 Claude 会话 cwd 计算本机历史目录名称，保持与远端会话服务一致。 */
function claudeProjectDirectoryName(cwd) {
  return path.resolve(cwd).replace(/[^a-zA-Z0-9-]/g, "-");
}

/** 返回本机 Claude 历史文件所在的用户项目目录。 */
function claudeProjectsRoot() {
  return path.join(os.homedir(), ".claude", "projects");
}

/** 从 Claude 用户消息内容提取首条可展示文本。 */
function extractClaudeSessionText(content) {
  if (typeof content === "string") {
    return content.trim();
  }
  if (!Array.isArray(content)) {
    return "";
  }
  return content
    .filter((item) => item && item.type === "text" && typeof item.text === "string")
    .map((item) => item.text.trim())
    .find(Boolean) ?? "";
}

/** 生成本机 Claude 会话标题，保持首条用户文本的业务展示语义。 */
function claudeSessionTitle(sessionId, candidate) {
  const title = candidate.trim().replace(/\s+/g, " ");
  return title ? title.slice(0, 120) : `Claude session ${sessionId.slice(0, 8)}`;
}

/** 读取单个 Claude JSONL 文件的会话摘要并精确校验 cwd。 */
async function readClaudeSessionSummary(filePath, expectedCwd) {
  const fileName = path.basename(filePath);
  const sessionId = fileName.endsWith(".jsonl")
    ? fileName.slice(0, -".jsonl".length)
    : "";
  if (!sessionId) {
    return null;
  }
  let sessionCwd = "";
  let firstPrompt = "";
  const lines = createInterface({
    input: createReadStream(filePath, { encoding: "utf8" }),
    crlfDelay: Infinity,
  });
  let lineCount = 0;
  for await (const line of lines) {
    lineCount += 1;
    if (lineCount > MAX_CLAUDE_TRANSCRIPT_LINES) {
      break;
    }
    try {
      const record = JSON.parse(line);
      if (!sessionCwd && typeof record.cwd === "string") {
        sessionCwd = path.resolve(record.cwd);
      }
      if (!firstPrompt && record.type === "user") {
        firstPrompt = extractClaudeSessionText(record.message?.content);
      }
      if (sessionCwd && firstPrompt) {
        break;
      }
    } catch {
      // Claude 正在追加的末行可能尚未形成完整 JSON，只忽略该行。
    }
  }
  if (sessionCwd !== expectedCwd) {
    return null;
  }
  const fileStat = await stat(filePath);
  return {
    id: sessionId,
    cwd: sessionCwd,
    title: claudeSessionTitle(sessionId, firstPrompt),
    updatedAt: fileStat.mtime.toISOString(),
  };
}

/** 扫描指定 cwd 对应的本机 Claude 项目目录并返回所有会话文件。 */
async function listClaudeSessionFiles(cwd) {
  const expectedCwd = path.resolve(cwd);
  const directory = path.join(
    claudeProjectsRoot(),
    claudeProjectDirectoryName(expectedCwd),
  );
  let entries;
  try {
    entries = await readdir(directory, { withFileTypes: true });
  } catch (error) {
    if (error && error.code === "ENOENT") {
      return [];
    }
    throw error;
  }
  const files = entries
    .filter((entry) => entry.isFile() && entry.name.endsWith(".jsonl"))
    .map((entry) => path.join(directory, entry.name));
  const nestedFiles = await Promise.all(
    entries
      .filter((entry) => entry.isDirectory())
      .map(async (entry) => {
        const nestedDirectory = path.join(directory, entry.name);
        const nestedEntries = await readdir(nestedDirectory, { withFileTypes: true });
        return nestedEntries
          .filter((nested) => nested.isFile() && nested.name.endsWith(".jsonl"))
          .map((nested) => path.join(nestedDirectory, nested.name));
      }),
  );
  files.push(...nestedFiles.flat());
  return files;
}

/** 扫描指定 cwd 对应的本机 Claude 项目目录并返回排序后的会话摘要。 */
async function listClaudeSessions(cwd) {
  const expectedCwd = path.resolve(cwd);
  const sessions = [];
  for (const filePath of await listClaudeSessionFiles(expectedCwd)) {
    const summary = await readClaudeSessionSummary(filePath, expectedCwd);
    if (summary) {
      sessions.push(summary);
    }
  }
  sessions.sort((left, right) => right.updatedAt.localeCompare(left.updatedAt));
  return sessions.slice(0, MAX_CLAUDE_SESSIONS);
}

/** 严格读取单个 Claude JSONL 文件的完整历史，并校验会话标识和工作目录。 */
async function readClaudeSessionHistory(filePath, expectedCwd, expectedSessionId) {
  const fileName = path.basename(filePath);
  const sessionId = fileName.endsWith(".jsonl")
    ? fileName.slice(0, -".jsonl".length)
    : "";
  if (sessionId !== expectedSessionId) {
    throw new Error(`Claude session file ID mismatch: expected ${expectedSessionId}`);
  }

  const records = [];
  let sessionCwd = "";
  let lineNumber = 0;
  const lines = createInterface({
    input: createReadStream(filePath, { encoding: "utf8" }),
    crlfDelay: Infinity,
  });
  for await (const line of lines) {
    lineNumber += 1;
    if (!line.trim()) {
      continue;
    }
    let record;
    try {
      record = JSON.parse(line);
    } catch (error) {
      throw new Error(
        `Claude session history contains invalid JSON at line ${lineNumber}: ${
          error instanceof Error ? error.message : String(error)
        }`,
      );
    }
    if (!record || typeof record !== "object" || Array.isArray(record)) {
      throw new Error(`Claude session history record at line ${lineNumber} is not an object`);
    }
    if (
      typeof record.sessionId === "string" &&
      record.sessionId.trim() &&
      record.sessionId !== expectedSessionId
    ) {
      throw new Error(
        `Claude session history record has mismatched sessionId at line ${lineNumber}`,
      );
    }
    if (typeof record.cwd === "string" && record.cwd.trim()) {
      const recordCwd = path.resolve(record.cwd);
      if (sessionCwd && sessionCwd !== recordCwd) {
        throw new Error(
          `Claude session history contains multiple cwd values at line ${lineNumber}`,
        );
      }
      sessionCwd = recordCwd;
      if (sessionCwd !== expectedCwd) {
        throw new Error(
          `Claude session history cwd does not match requested workspace: expected ${expectedCwd}, got ${sessionCwd}`,
        );
      }
    }
    records.push(record);
  }
  if (!sessionCwd) {
    throw new Error(`Claude session history is missing cwd: ${expectedSessionId}`);
  }
  return {
    sessionId: expectedSessionId,
    cwd: sessionCwd,
    records,
  };
}

/** 处理本机 Claude 历史会话查询命令并返回关联请求 ID 的协议事件。 */
async function handleListSessions(req) {
  const { id, params = {} } = req;
  const cwd = typeof params.cwd === "string" ? params.cwd.trim() : "";
  if (!cwd) {
    emit({
      id,
      type: "error",
      message: "Claude list_sessions requires a non-empty cwd.",
      recoverable: false,
    });
    return;
  }
  try {
    emit({ id, type: "sessions", sessions: await listClaudeSessions(cwd) });
  } catch (error) {
    emit({
      id,
      type: "error",
      message: error instanceof Error ? error.message : String(error),
      recoverable: false,
    });
  }
}

/** 处理本机 Claude 完整历史读取命令，避免通过 resume 或新 query 获取历史。 */
async function handleReadSessionHistory(req) {
  const { id, params = {} } = req;
  const cwd = typeof params.cwd === "string" ? params.cwd.trim() : "";
  const sessionId = typeof params.sessionId === "string" ? params.sessionId.trim() : "";
  if (!cwd || !sessionId) {
    emit({
      id,
      type: "error",
      message: "Claude read_session_history requires cwd and sessionId.",
      recoverable: false,
    });
    return;
  }
  if (!CLAUDE_SESSION_ID_PATTERN.test(sessionId)) {
    emit({
      id,
      type: "error",
      message: "Claude read_session_history received an invalid sessionId.",
      recoverable: false,
    });
    return;
  }
  try {
    const expectedCwd = path.resolve(cwd);
    const matchingFiles = (await listClaudeSessionFiles(expectedCwd)).filter(
      (filePath) => path.basename(filePath) === `${sessionId}.jsonl`,
    );
    if (matchingFiles.length === 0) {
      throw new Error(`Claude session not found: ${sessionId}`);
    }
    if (matchingFiles.length > 1) {
      throw new Error(`Multiple Claude session files found for sessionId: ${sessionId}`);
    }
    const history = await readClaudeSessionHistory(
      matchingFiles[0],
      expectedCwd,
      sessionId,
    );
    emit({
      id,
      type: "session_history",
      sessionId: history.sessionId,
      cwd: history.cwd,
      records: history.records,
    });
  } catch (error) {
    emit({
      id,
      type: "error",
      message: error instanceof Error ? error.message : String(error),
      recoverable: false,
    });
  }
}

async function handleListModels(req) {
  const { id, params = {} } = req;
  const options = applyClaudeRuntime({
    cwd: params.cwd || process.cwd(),
    settingSources: ["user"],
  });
  const query = queryFn({ prompt: holdModelDiscoveryOpen(), options });

  try {
    const models = await query.supportedModels();
    emit({
      id,
      type: "models",
      models: Array.isArray(models) ? models : [],
      runtimeSource: claudeCodeExecutable ? "system" : "bundled",
      runtimeExecutable: claudeCodeExecutable || undefined,
      sdkVersion: sdkVersion || undefined,
      bundledClaudeCodeVersion: bundledClaudeCodeVersion || undefined,
    });
  } catch (error) {
    emit({
      id,
      type: "error",
      message: `Failed to discover Claude models: ${error.message || String(error)}`,
      recoverable: true,
    });
  } finally {
    query.close?.();
  }
}

async function handleUsageLimits(req) {
  const usage = await fetchClaudeUsageSnapshot();
  if (usage) {
    emit({ id: req.id, type: "usage_limits_updated", usage });
    return;
  }
  emit({
    id: req.id,
    type: "error",
    message: "Claude usage limits are unavailable for the current account.",
    recoverable: true,
  });
}

// 处理 Claude 查询请求，并为本地会话接入统一 Gateway 的 HTTP MCP server。
async function handleQuery(req, persistentSession = null) {
  const { id, params = {} } = req;
  traceClaudeSdk("handle_query_enter", { request: req, persistentSession });
  const {
    prompt,
    attachments = [],
    cwd,
    model,
    allowedTools,
    systemPrompt,
    resume,
    sessionId,
    maxTurns,
    planMode,
    approvalPolicy,
    allowNetwork,
    writableRoots = [],
    sandboxMode,
    reasoningEffort,
    threadId,
    computerControlTools = [],
    auracoderThreadTools = [],
    mcpGatewayUrl,
    mcpGatewayToken,
    settingSources,
    strictMcpConfig,
    enforceApprovalRouting,
  } = params;

  const resolvedApprovalPolicy = approvalPolicy ?? (sandboxMode === "read-only" ? "restricted" : null);
  const context = createQueryContext(id, resolvedApprovalPolicy, planMode);
  context.threadId = threadId || sessionId || resume || id;
  activeQueries.set(id, context);

  const toolList = [];
  const requestedTools = Array.isArray(allowedTools)
    ? allowedTools
    : [
      "Read",
      "Write",
      "Edit",
      "Bash",
      "Glob",
      "Grep",
      "Agent",
      ...(allowNetwork ? ["WebFetch"] : []),
    ];
  for (const toolName of requestedTools) {
    // TaskOutput 是后台任务续跑所需的原生工具，只保留一次并保留首次出现的位置。
    if (toolName === "TaskOutput" && toolList.includes("TaskOutput")) {
      continue;
    }
    toolList.push(toolName);
  }
  if (!toolList.includes("TaskOutput")) {
    // 无论调用方是否提供工具白名单，都必须允许 Claude 读取后台任务结果。
    toolList.push("TaskOutput");
  }
  const permissionOptions = context.permissionOptions;

  const sessionCwd = cwd || process.cwd();
  let actualSessionId = null;
  try {
    const normalizedSandboxMode = normalizeSandboxMode(sandboxMode);
    const normalizedWritableRoots = normalizeWritableRoots(sessionCwd, writableRoots);
    // 旧实现保留迁移留痕：统一 Gateway 接替进程内自定义工具服务器。
    // const auracoderComputerControlServer = createAuraCoderComputerControlServer(
    //   context,
    //   computerControlTools,
    // );
    // const auracoderThreadServer = createAuraCoderThreadServer(context, auracoderThreadTools);
    // const auracoderGatewayServer = typeof mcpGatewayUrl === "string" && mcpGatewayUrl.trim()
    //   ? { type: "http", url: mcpGatewayUrl.trim() }
    //   : null;
    const hasMcpGatewayUrl = typeof mcpGatewayUrl === "string" && mcpGatewayUrl.trim().length > 0;
    const hasMcpGatewayToken = typeof mcpGatewayToken === "string" && mcpGatewayToken.length > 0;
    if (hasMcpGatewayUrl !== hasMcpGatewayToken) {
      const internalError = new Error(
        `MCP Gateway 配置不完整：url_present=${hasMcpGatewayUrl}, token_present=${hasMcpGatewayToken}`,
      );
      console.error("Claude MCP Gateway 配置异常", internalError);
      throw new Error("AuraCoder MCP 配置不完整，Claude 会话无法启动", { cause: internalError });
    }
    const mcpServers = {};
    if (hasMcpGatewayUrl && hasMcpGatewayToken) {
      if (Object.prototype.hasOwnProperty.call(mcpServers, "auracoder")) {
        throw new Error("AuraCoder MCP 名称冲突，Claude 会话无法启动");
      }
      mcpServers.auracoder = {
        type: "http",
        url: mcpGatewayUrl.trim(),
        headers: { Authorization: `Bearer ${mcpGatewayToken}` },
      };
      toolList.push("mcp__auracoder__*");
    }
    const automaticallyAllowedTools = enforceApprovalRouting
      ? permissionOptions.decisionMode === "read-only"
        ? toolList.filter((toolName) =>
            [
              "Read",
              "Glob",
              "Grep",
              "Agent",
              "ExitPlanMode",
              "EnterPlanMode",
              "TaskOutput",
            ].includes(toolName),
          )
        : permissionOptions.decisionMode === "ask"
          ? toolList.filter((toolName) =>
              ["Read", "Glob", "Grep", "ExitPlanMode", "EnterPlanMode", "TaskOutput"].includes(
                toolName,
              ),
            )
          : toolList
      : toolList;

    const options = applyClaudeRuntime({
      cwd: sessionCwd,
      additionalDirectories: additionalDirectoriesForSandbox(
        sessionCwd,
        normalizedSandboxMode,
        normalizedWritableRoots,
      ),
      permissionMode: permissionOptions.sdkPermissionMode,
      ...(permissionOptions.allowDangerouslySkipPermissions
        ? { allowDangerouslySkipPermissions: true }
        : {}),
      allowedTools: automaticallyAllowedTools,
      ...(Object.keys(mcpServers).length > 0 ? { mcpServers } : {}),
      canUseTool: buildPermissionHandler({
        context,
        cwd: sessionCwd,
        writableRoots: normalizedWritableRoots,
        sandboxMode: normalizedSandboxMode,
        allowNetwork: Boolean(allowNetwork),
      }),
      settingSources: Array.isArray(settingSources)
        ? settingSources.filter((source) => ["user", "project", "local"].includes(source))
        : ["user", "project"],
      strictMcpConfig: Boolean(strictMcpConfig),
      sandbox: {
        enabled: true,
        failIfUnavailable: process.platform !== "win32",
        autoAllowBashIfSandboxed: true,
        allowUnsandboxedCommands: false,
        filesystem: {
          allowWrite: allowWriteRootsForSandbox(
            normalizedSandboxMode,
            normalizedWritableRoots,
          ),
        },
        ...(allowNetwork
          ? {}
          : {
              network: {
                allowedDomains: [],
                allowLocalBinding: false,
                allowUnixSockets: [],
              },
            }),
      },
      settings: {
        permissions: {
          defaultMode: permissionOptions.sdkPermissionMode,
          ...(permissionOptions.decisionMode === "full"
            ? {}
            : { disableBypassPermissionsMode: "disable" }),
        },
      },
      includePartialMessages: true,
      hooks: {
      PreToolUse: [
        {
          matcher: ".*",
          hooks: [
            async (hookInput) => {
              traceClaudeSdk("pre_tool_use", { requestId: id, hookInput });
              const toolName = hookInput?.tool_name || hookInput?.name || "unknown";
              if (toolName === "AskUserQuestion") {
                return {};
              }
              if (toolName === "ExitPlanMode" || toolName === "EnterPlanMode") {
                return {
                  decision: "block",
                  reason: `${toolName} handled by AuraCoder. The plan is ready and will be presented to the user for review.`,
                };
              }
              const toolInput = hookInput?.tool_input || hookInput?.input || {};
              const toolUseId =
                hookInput?.tool_use_id || hookInput?.toolUseID || hookInput?.toolUseId;
              if (
                typeof toolUseId === "string" &&
                toolUseId.length > 0 &&
                context.actionIdsByToolUseId.has(toolUseId)
              ) {
                return {};
              }
              const actionId = `claude-action-${++context.actionCounter}`;
              if (typeof toolUseId === "string" && toolUseId.length > 0) {
                context.actionIdsByToolUseId.set(toolUseId, actionId);
              }

              emit({
                id,
                type: "action_started",
                actionId,
                actionType: mapToolNameToActionType(toolName),
                toolName,
                summary: summarizeTool(toolName, toolInput),
                details: toolInput,
              });

              return {};
            },
          ],
        },
      ],
      PostToolUse: [
        {
          matcher: ".*",
          hooks: [
            async (hookInput) => {
              traceClaudeSdk("post_tool_use", { requestId: id, hookInput });
              const toolName = hookInput?.tool_name || hookInput?.name || "unknown";
              if (toolName === "AskUserQuestion") {
                return {};
              }
              const toolUseId =
                hookInput?.tool_use_id || hookInput?.toolUseID || hookInput?.toolUseId;
              if (
                typeof toolUseId === "string" &&
                context.suppressedToolUseIds.has(toolUseId)
              ) {
                context.suppressedToolUseIds.delete(toolUseId);
                return {};
              }
              const actionId = getActionIdForToolUse(context, toolUseId);
              const output =
                hookInput?.tool_response ??
                hookInput?.tool_result ??
                hookInput?.result;
              emitToolOutputChunks(id, actionId, output);

              emit({
                id,
                type: "action_completed",
                actionId,
                success: true,
                output: serializeToolOutput(output) || undefined,
                durationMs: 0,
              });

              return {};
            },
          ],
        },
      ],
      PostToolUseFailure: [
        {
          matcher: ".*",
          hooks: [
            async (hookInput) => {
              traceClaudeSdk("post_tool_use_failure", { requestId: id, hookInput });
              const toolName = hookInput?.tool_name || hookInput?.name || "unknown";
              if (toolName === "AskUserQuestion") {
                return {};
              }
              const toolUseId =
                hookInput?.tool_use_id || hookInput?.toolUseID || hookInput?.toolUseId;
              if (
                typeof toolUseId === "string" &&
                context.suppressedToolUseIds.has(toolUseId)
              ) {
                context.suppressedToolUseIds.delete(toolUseId);
                return {};
              }
              const actionId = getActionIdForToolUse(context, toolUseId);

              emit({
                id,
                type: "action_completed",
                actionId,
                success: false,
                error:
                  hookInput?.error?.message ||
                  hookInput?.error ||
                  "Tool execution failed",
                durationMs: 0,
              });

              return {};
            },
          ],
        },
      ],
      },
    });

    if (model) options.model = model;
    if (systemPrompt) options.systemPrompt = systemPrompt;
    if (resume) options.resume = resume;
    if (sessionId) options.sessionId = sessionId;
    if (maxTurns) options.maxTurns = maxTurns;
    if (reasoningEffort) options.effort = reasoningEffort;

    emit({ id, type: "turn_started" });

    let sawTextDelta = false;
    let terminalStatus = "completed";
    let promptInput;
    if (persistentSession) {
      context.isPersistentSession = true;
      context.messageInput = persistentSession.messageInput;
      promptInput = persistentSession.messageInput;
    } else {
      const messageInput = new Readable({
        objectMode: true,
        read() {},
      });
      context.messageInput = messageInput;
      const initialInput = buildPromptInput(
        prompt,
        attachments,
        sessionCwd,
        sessionId || resume || "",
      );
      await pushClaudePromptInput(
        messageInput,
        initialInput,
        sessionId || resume || "",
      );
      promptInput = messageInput;
    }
    traceClaudeSdk("query_create", { requestId: id, promptInput, options });
    const query = queryFn({ prompt: promptInput, options });
    context.query = query;
    if (persistentSession) {
      persistentSession.query = query;
      persistentSession.context = context;
      emit({
        id,
        type: "session_handle_created",
        threadId: persistentSession.threadId,
        handleId: persistentSession.handleId,
        reused: false,
      });
    }

    /**
     * 根据 Claude 后台任务状态机统一判断当前逻辑轮次是否可以完成。
     * 只有权威任务集合为空、无需等待任务通知且所有续跑结果均到达时，才发送 turn_completed。
     */
    const maybeCompleteTurn = ({
      allowMissingSdkResult = false,
      forceStatus = null,
    } = {}) => {
      if (context.turnCompleted) {
        return true;
      }
      if (forceStatus) {
        emitTurnCompleted(context, forceStatus);
        return true;
      }
      if (context.cancelled || persistentSession?.interruptRequested) {
        emitTurnCompleted(context, "interrupted");
        return true;
      }
      if (!allowMissingSdkResult && !context.sdkResultReceived) {
        return false;
      }

      const authoritativeBackgroundTasksEmpty =
        context.authoritativeBackgroundTasksEmpty && context.backgroundTasks.size === 0;
      const allInjectedContinuationsCompleted =
        context.backgroundContinuationResultCount >= context.backgroundContinuationInjectedCount;
      if (
        !authoritativeBackgroundTasksEmpty ||
        context.awaitingTaskNotification ||
        !allInjectedContinuationsCompleted
      ) {
        return false;
      }

      emitTurnCompleted(context, context.sdkTerminalStatus || terminalStatus);
      if (!persistentSession) {
        context.messageInput?.push(null);
      }
      return true;
    };

    void fetchClaudeUsageSnapshot().then((usage) => {
      if (usage && activeQueries.has(id)) {
        emit({ id, type: "usage_limits_updated", usage });
      }
    });

    for await (const message of query) {
      traceClaudeSdk(`sdk_message_${id}`, { requestId: id, message });
      if (context.cancelled) {
        break;
      }

      if (message.type === "system" && message.subtype === "init") {
        actualSessionId = message.session_id;
        setContextSessionId(context, actualSessionId);
        emit({ id, type: "session_init", sessionId: actualSessionId });
      } else if (message.type === "system" && message.subtype === "permission_denied") {
        // 将 SDK 权限拒绝保留为可展示、可持久化的失败动作，避免丢失原始拒绝元数据。
        const rawToolName =
          typeof message.tool_name === "string" && message.tool_name.length > 0
            ? message.tool_name
            : null;
        const permissionToolName = rawToolName ?? "unknown";
        const toolUseId =
          typeof message.tool_use_id === "string" && message.tool_use_id.length > 0
            ? message.tool_use_id
            : null;
        const decisionReasonType =
          typeof message.decision_reason_type === "string" &&
          message.decision_reason_type.length > 0
            ? message.decision_reason_type
            : null;
        const decisionReason =
          typeof message.decision_reason === "string" && message.decision_reason.length > 0
            ? message.decision_reason
            : null;
        const permissionMessage =
          typeof message.message === "string" && message.message.length > 0
            ? message.message
            : null;
        const actionId = `claude-action-${++context.actionCounter}-permission-denied${
          toolUseId ? `-${toolUseId}` : ""
        }`;

        emit({
          id,
          type: "action_started",
          actionId,
          actionType: mapToolNameToActionType(permissionToolName),
          toolName: permissionToolName,
          summary: `${permissionToolName} permission denied`,
          details: {
            toolName: rawToolName,
            toolUseId,
            decisionReasonType,
            decisionReason,
            message: permissionMessage,
          },
        });
        emit({
          id,
          type: "action_completed",
          actionId,
          success: false,
          error: permissionMessage,
          durationMs: 0,
        });
      } else if (message.type === "assistant" && typeof message.error === "string") {
        const assistantError = formatAssistantMessageError(message);
        terminalStatus = "failed";
        emit({
          id,
          type: "error",
          message: assistantError.message,
          recoverable: assistantError.recoverable,
          errorType: assistantError.errorType,
          isAuthError: assistantError.isAuthError,
        });
      } else if (message.type === "rate_limit_event") {
        const usage = buildRateLimitUsageSnapshot(message);
        if (usage) {
          emit({
            id,
            type: "usage_limits_updated",
            usage,
          });
        }
      } else if (message.type === "system" && message.subtype === "status") {
        const notice = buildStatusNotice(message);
        if (notice) {
          emit({
            id,
            type: "notice",
            ...notice,
          });
        }
      } else if (
        message.type === "system" &&
        message.subtype === "background_tasks_changed"
      ) {
        const tasks = Array.isArray(message.tasks) ? message.tasks : [];
        const currentTaskIds = new Set();
        // 仅使用 SDK 的完整快照替换权威后台任务集合。
        context.backgroundTasks.clear();
        for (const task of tasks) {
          const taskId = typeof task?.task_id === "string" ? task.task_id : "";
          if (!taskId) {
            continue;
          }
          currentTaskIds.add(taskId);
          context.backgroundTasks.set(taskId, {
            // 记录后台任务的稳定标识，用于后续通知关联。
            task_id: taskId,
            // 记录 SDK 提供的后台任务类型，供状态展示使用。
            task_type: typeof task.task_type === "string" ? task.task_type : "",
            // 记录 SDK 提供的任务描述，供当前 assistant 消息展示。
            description:
              typeof task.description === "string" ? task.description : "Claude 后台任务",
          });
          const existingDisplayTask = context.backgroundTaskDisplay.get(taskId);
          if (existingDisplayTask) {
            // 快照只更新活动任务的类型，不能覆盖 sidecar 已记录的开始时间。
            existingDisplayTask.taskType =
              typeof task.task_type === "string" ? task.task_type : existingDisplayTask.taskType;
            // 快照只更新活动任务的描述，终态摘要和终态时间仍由 task_notification 保留。
            existingDisplayTask.description =
              typeof task.description === "string"
                ? task.description
                : existingDisplayTask.description;
          } else {
            context.backgroundTaskDisplay.set(taskId, {
              // 后台任务稳定标识，用于前端在生命周期更新中复用同一行。
              taskId,
              // 后台任务类型，用于保留 SDK 提供的任务元数据。
              taskType: typeof task.task_type === "string" ? task.task_type : "",
              // 后台任务业务描述，用于展示任务名称。
              description:
                typeof task.description === "string" ? task.description : "Claude 后台任务",
              // 完整快照首次出现时，任务默认为运行中。
              status: "running",
              // 收到完整快照的时刻作为任务开始时间。
              startedAt: Date.now(),
            });
          }
        }
        for (const taskId of context.notifiedTaskIds) {
          if (!currentTaskIds.has(taskId)) {
            context.notifiedTaskIds.delete(taskId);
          }
        }
        for (const taskId of currentTaskIds) {
          if (!context.notifiedTaskIds.has(taskId)) {
            context.pendingTaskNotificationIds.add(taskId);
          }
        }
        context.authoritativeBackgroundTasksEmpty = context.backgroundTasks.size === 0;
        context.awaitingTaskNotification = context.pendingTaskNotificationIds.size > 0;
        emitClaudeBackgroundNotice(id, message.subtype, null, tasks, context);
        // 快照变为空时也必须检查此前已收到的 SDK result 是否可以完成。
        maybeCompleteTurn();
      } else if (message.type === "system" && message.subtype === "task_started") {
        // task_started 只负责发送生命周期通知，权威任务集合仍由 background_tasks_changed 提供。
        const taskId = typeof message.task_id === "string" ? message.task_id : "";
        if (taskId) {
          const existingDisplayTask = context.backgroundTaskDisplay.get(taskId);
          if (existingDisplayTask) {
            // task_started 重新标记活动任务，但保留首次开始时间供耗时计算。
            existingDisplayTask.status = "running";
            // 启动事件携带新类型时更新任务类型。
            if (typeof message.task_type === "string") {
              existingDisplayTask.taskType = message.task_type;
            }
            // 启动事件携带新描述时更新任务描述。
            if (typeof message.description === "string") {
              existingDisplayTask.description = message.description;
            }
            // 重新启动后的任务不沿用上一次终态摘要和终态时间。
            delete existingDisplayTask.summary;
            delete existingDisplayTask.finishedAt;
          } else {
            context.backgroundTaskDisplay.set(taskId, {
              // 后台任务稳定标识，用于前端在生命周期更新中复用同一行。
              taskId,
              // 启动事件提供的后台任务类型。
              taskType: typeof message.task_type === "string" ? message.task_type : "",
              // 启动事件提供的后台任务业务描述。
              description:
                typeof message.description === "string"
                  ? message.description
                  : "Claude 后台任务",
              // task_started 表示任务已经进入运行状态。
              status: "running",
              // 收到启动事件的时刻作为任务开始时间。
              startedAt: Date.now(),
            });
          }
        }
        emitClaudeBackgroundNotice(id, message.subtype, message, null, context);
      } else if (message.type === "system" && message.subtype === "task_updated") {
        const taskId = typeof message.task_id === "string" ? message.task_id : "";
        const patch = message.patch && typeof message.patch === "object"
          ? message.patch
          : {};
        const displayTask = context.backgroundTaskDisplay.get(taskId);
        const patchSummary =
          typeof patch.summary === "string"
            ? patch.summary
            : typeof patch.error === "string"
              ? patch.error
              : null;
        if (displayTask) {
          // task_updated 只更新展示描述，不改动权威后台任务集合。
          if (typeof patch.description === "string") {
            displayTask.description = patch.description;
          }
          // task_updated 的摘要仅来自 SDK 更新补丁，不读取后台任务输出文件。
          if (patchSummary !== null) {
            if (patchSummary.length > 0) {
              displayTask.summary = patchSummary;
            } else {
              delete displayTask.summary;
            }
          }
        }
        emitClaudeBackgroundNotice(id, message.subtype, {
          // 传递 SDK 更新消息中的任务标识，供 notice 关联当前业务任务。
          task_id: taskId,
          // 优先展示 display map 中的最新描述，再回退到 SDK 更新消息描述。
          description:
            displayTask?.description ||
            (typeof patch.description === "string" ? patch.description : "Claude 后台任务"),
          // 传递 SDK 更新消息中的状态，不写入权威任务集合。
          status: typeof patch.status === "string" ? patch.status : undefined,
          // 传递 SDK 更新消息中的摘要，不读取后台任务输出。
          summary: patchSummary || undefined,
        }, null, context);
      } else if (message.type === "system" && message.subtype === "task_progress") {
        const taskId = typeof message.task_id === "string" ? message.task_id : "";
        const displayTask = context.backgroundTaskDisplay.get(taskId);
        if (displayTask) {
          // task_progress 只更新展示描述，不改动权威后台任务集合和开始时间。
          if (typeof message.description === "string") {
            displayTask.description = message.description;
          }
          // 进度摘要只来自 SDK 的 summary 字段，不读取后台任务输出文件。
          if (typeof message.summary === "string") {
            if (message.summary.length > 0) {
              displayTask.summary = message.summary;
            } else {
              delete displayTask.summary;
            }
          }
        }
        emitClaudeBackgroundNotice(id, message.subtype, {
          // 传递 SDK 进度消息中的任务标识，供 notice 关联当前业务任务。
          task_id: taskId,
          // 优先展示 display map 中的最新描述，再回退到 SDK 当前描述。
          description:
            displayTask?.description ||
            (typeof message.description === "string"
              ? message.description
              : "Claude 后台任务"),
          // 进度消息表示 SDK 报告任务仍在运行，仅作为 notice 元数据。
          status: "running",
          // 传递 SDK 最新摘要，避免进度消息被静默丢弃。
          summary: typeof message.summary === "string" ? message.summary : undefined,
        }, null, context);
      } else if (message.type === "system" && message.subtype === "task_notification") {
        const taskId = typeof message.task_id === "string" ? message.task_id : "";
        const terminalStatus = ["completed", "failed", "stopped"].includes(message.status)
          ? message.status
          : "failed";
        const finishedAt = Date.now();
        if (taskId) {
          // task_notification 只结算对应通知等待状态，不修改权威后台任务集合。
          context.pendingTaskNotificationIds.delete(taskId);
          context.notifiedTaskIds.add(taskId);
          context.awaitingTaskNotification = context.pendingTaskNotificationIds.size > 0;
          const displayTask = context.backgroundTaskDisplay.get(taskId);
          if (displayTask) {
            // 终态状态只由 SDK task_notification 写入展示 map。
            displayTask.status = terminalStatus;
            // 终态摘要只接受 task_notification.message.summary，禁止读取输出文件。
            if (typeof message.summary === "string" && message.summary.length > 0) {
              displayTask.summary = message.summary;
            } else {
              delete displayTask.summary;
            }
            // 记录 sidecar 收到终态生命周期事件的时刻。
            displayTask.finishedAt = finishedAt;
          } else {
            context.backgroundTaskDisplay.set(taskId, {
              // 后台任务稳定标识，用于前端在生命周期更新中复用同一行。
              taskId,
              // task_notification 缺少类型时保留空类型元数据。
              taskType: typeof message.task_type === "string" ? message.task_type : "",
              // task_notification 缺少描述时使用统一后台任务占位描述。
              description:
                typeof message.description === "string"
                  ? message.description
                  : "Claude 后台任务",
              // 非法或缺失的 SDK 终态统一作为失败展示。
              status: terminalStatus,
              // 没有先前生命周期事件时，以收到终态事件的时刻开始计时。
              startedAt: finishedAt,
              // 记录 sidecar 收到终态生命周期事件的时刻。
              finishedAt,
              // 终态摘要只来自 SDK task_notification.message.summary。
              ...(typeof message.summary === "string" && message.summary.length > 0
                ? { summary: message.summary }
                : {}),
            });
          }
        }
        // task_notification 只发送最终通知和续跑输入，不修改权威任务集合。
        emitClaudeBackgroundNotice(id, message.subtype, message, null, context);

        const persistentSessionIsAlive =
          !persistentSession || sessionHandles.get(persistentSession.threadId) === persistentSession;
        const canInjectContinuation =
          Boolean(taskId) &&
          Boolean(context.messageInput) &&
          !context.cancelled &&
          !shuttingDown &&
          !persistentSession?.interruptRequested &&
          persistentSessionIsAlive;
        if (canInjectContinuation) {
          // 记录已注入的 synthetic continuation，等待对应 SDK result 到达。
          context.backgroundContinuationInjectedCount += 1;
          const syntheticTaskNotification = {
            type: "user",
            message: {
              role: "user",
              content: `Claude 后台任务通知：${JSON.stringify({
                task_id: taskId,
                status: message.status,
                output_file: message.output_file,
                summary: message.summary,
              })}。请立即调用原生 TaskOutput 工具读取该任务结果，输入必须为 ${JSON.stringify({
                task_id: taskId,
                block: false,
                timeout: 1000,
              })}，读取后继续原任务；在完成此前未交付的原任务并给出最终结论/交付结果前，不得结束当前逻辑轮次。`,
            },
            parent_tool_use_id: null,
            isSynthetic: true,
            priority: "now",
            shouldQuery: true,
            session_id: context.sessionId || message.session_id || "",
          };
          context.messageInput.push(syntheticTaskNotification);
        }
        maybeCompleteTurn();
      } else if (message.type === "result") {
        traceClaudeSdk("sdk_result_before_processing", {
          requestId: id,
          message,
          sawTextDelta,
          terminalStatus,
          context,
          contextStopReason: context.stopReason,
        });
        actualSessionId = message.session_id || actualSessionId;
        setContextSessionId(context, actualSessionId);
        updateContextTokenUsage(context, {
          input: message.usage?.input_tokens,
          output: message.usage?.output_tokens,
        });
        if (message.subtype === "success") {
          if (
            typeof message.result === "string" &&
            message.result.length > 0 &&
            !sawTextDelta
          ) {
            emit({ id, type: "text_delta", content: message.result });
          }
        } else {
          terminalStatus = "failed";
          emit({
            id,
            type: "error",
            message: formatSdkResultError(message),
            recoverable: false,
          });
        }
        const hadSdkResult = context.sdkResultReceived;
        context.sdkResultReceived = true;
        context.sdkTerminalStatus = terminalStatus;
        if (
          hadSdkResult &&
          context.backgroundContinuationResultCount < context.backgroundContinuationInjectedCount
        ) {
          // 按输入流顺序消费一个已注入的 task notification continuation result。
          context.backgroundContinuationResultCount += 1;
        }
        if (context.backgroundTasks.size > 0) {
          context.awaitingTaskNotification = context.pendingTaskNotificationIds.size > 0;
        }
        // 初始 result 和续跑 result 都通过同一个状态机门控完成。
        maybeCompleteTurn();

        // 中间 result 只结束当前 SDK 子轮次，后台任务仍可通过同一输入流续跑。
        if (!context.turnCompleted || context.isPersistentSession) {
          sawTextDelta = false;
          terminalStatus = "completed";
        }
      } else if (message.type === "stream_event") {
        const streamEvent = message.event;
        updateTokenUsageFromStreamEvent(context, streamEvent);
        const contextUsage = buildContextUsageSnapshot(streamEvent, model);
        if (contextUsage) {
          emit({
            id,
            type: "usage_limits_updated",
            usage: contextUsage,
          });
        }

        if (streamEvent?.type === "content_block_start") {
          const block = streamEvent.content_block;
          if (block?.type === "tool_use") {
            const toolUseId = block.id || block.tool_use_id;
            if (
              typeof toolUseId === "string" &&
              toolUseId.length > 0
            ) {
              // Track index→toolUseId for content_block_stop, but do NOT emit
              // action_started here — block.input is empty at this point.
              // PreToolUse will emit action_started with the complete tool input.
              if (Number.isInteger(streamEvent.index)) {
                context.streamToolUseIdsByIndex.set(streamEvent.index, toolUseId);
              }
            }
          }
          continue;
        }

        if (streamEvent?.type === "content_block_stop") {
          // Clean up the index tracking. action_progress_updated is only emitted
          // if PreToolUse already registered the actionId; otherwise the tool
          // hasn't started from AuraCoder' perspective yet and the event is skipped.
          const toolUseId = context.streamToolUseIdsByIndex.get(streamEvent.index);
          if (typeof toolUseId === "string") {
            context.streamToolUseIdsByIndex.delete(streamEvent.index);
          }
          continue;
        }

        if (
          streamEvent?.type === "message_start" ||
          streamEvent?.type === "message_delta" ||
          streamEvent?.type === "message_stop"
        ) {
          continue;
        }

        if (streamEvent?.type !== "content_block_delta") {
          continue;
        }

        const delta = streamEvent.delta;
        if (delta?.type === "text_delta" && typeof delta.text === "string" && delta.text.length > 0) {
          sawTextDelta = true;
          emit({ id, type: "text_delta", content: delta.text });
        } else if (
          delta?.type === "thinking_delta" &&
          typeof delta.thinking === "string" &&
          delta.thinking.length > 0
        ) {
          emit({ id, type: "thinking_delta", content: delta.thinking });
        }
      }
    }

    setContextSessionId(context, actualSessionId);
    const completedAfterIterator = maybeCompleteTurn({ allowMissingSdkResult: true });
    const hasPendingBackgroundContinuation =
      !context.authoritativeBackgroundTasksEmpty ||
      context.awaitingTaskNotification ||
      context.backgroundContinuationResultCount < context.backgroundContinuationInjectedCount;
    if (!completedAfterIterator && !context.cancelled && hasPendingBackgroundContinuation) {
      // SDK 查询异常结束时明确失败，避免后台任务待处理却让 UI 永久保持 streaming。
      emit({
        // 关联发生异常的 AuraCoder 查询。
        id,
        // 使用统一错误事件告知调用方当前轮次无法继续。
        type: "error",
        // 说明 SDK query 在后台任务待处理时意外结束，后台结果尚未返回。
        message: "Claude SDK query 在后台任务待处理时意外结束，后台任务结果尚未返回。",
        // 该错误无法由当前 query 自动恢复，必须由调用方重新发起处理。
        recoverable: false,
      });
      // 通过唯一完成门控发出失败终态，避免异常路径再次重复完成。
      maybeCompleteTurn({ forceStatus: "failed" });
    }
  } catch (err) {
    traceClaudeSdk("handle_query_error", {
      requestId: id,
      error: err,
      stack: err?.stack,
      context,
    });
    emit({
      id,
      type: "error",
      message: err.message || String(err),
      recoverable: false,
    });
    setContextSessionId(context, actualSessionId);
    emitTurnCompleted(context, "failed");
  } finally {
    traceClaudeSdk("handle_query_finally", { requestId: id, context });
    cleanupPendingApprovalsForQuery(id, "Claude query was canceled.");
    cleanupPendingComputerControlCalls(context, "Claude query was canceled.");
    if (!context.isPersistentSession && context.messageInput && !context.messageInput.readableEnded) {
      context.messageInput.push(null);
    }
    // 查询生命周期结束后清除后台任务展示状态，避免下一轮复用旧任务。
    context.backgroundTaskDisplay.clear();
    activeQueries.delete(id);
  }
}

/** 关闭并移除指定 persistent session，保证替换和销毁共用同一生命周期清理语义。 */
async function closeAndRemovePersistentSessionEntry(threadId, entry) {
  // 只有 map 仍指向当前 entry 时才允许移除，避免误删已完成替换的新会话。
  if (sessionHandles.get(threadId) !== entry) {
    return;
  }
  sessionHandles.delete(threadId);
  entry.interruptRequested = true;
  if (entry.context) {
    // 关闭持久会话时清除当前 assistant 卡片中的任务行。
    entry.context.backgroundTaskDisplay.clear();
    entry.context.cancelled = true;
    cleanupPendingApprovalsForQuery(
      entry.context.id,
      "Claude persistent session was destroyed before approval was answered.",
    );
    cleanupPendingComputerControlCalls(
      entry.context,
      "Claude persistent session was destroyed before the computer operation completed.",
    );
  }
  if (!entry.messageInput.readableEnded) {
    entry.messageInput.push(null);
  }
  entry.query?.close?.();
  await entry.runPromise;
}

async function createPersistentSessionHandle(req) {
  const { id, params = {} } = req;
  const threadId = typeof params.threadId === "string" ? params.threadId.trim() : "";
  const handleId = typeof params.handleId === "string" ? params.handleId.trim() : "";
  if (!threadId || !handleId) {
    emit({
      id,
      type: "error",
      message: "Claude persistent session requires threadId and handleId.",
      recoverable: false,
    });
    return;
  }

  const existing = sessionHandles.get(threadId);
  if (existing) {
    if (params.replaceExisting === true) {
      try {
        await closeAndRemovePersistentSessionEntry(threadId, existing);
      } catch (error) {
        // 保留旧 session 清理失败的原始消息，由当前 create 命令向 HTTP 调用方上报。
        emit({
          // 关联当前请求，确保 cleanup 失败作为当前 create 命令返回。
          id,
          // 使用统一错误事件表示旧 session 清理未完成。
          type: "error",
          // 保留旧 session cleanup 的原始错误消息。
          message: error instanceof Error ? error.message : String(error),
          // 清理失败不可由当前命令自动恢复。
          recoverable: false,
        });
        return;
      }
    } else {
      /*
      // 无 replaceExisting 标记时原有复用行为保留为迁移留痕。
      emit({
        id,
        type: "session_handle_created",
        threadId,
        handleId: existing.handleId,
        sessionId: existing.context?.sessionId ?? null,
        reused: true,
      });
      return;
      */
      emit({
        id,
        type: "session_handle_created",
        threadId,
        handleId: existing.handleId,
        sessionId: existing.context?.sessionId ?? null,
        reused: true,
      });
      return;
    }
  }

  const messageInput = new Readable({
    objectMode: true,
    read() {},
  });
  const entry = {
    threadId,
    handleId,
    messageInput,
    query: null,
    context: null,
    runPromise: null,
    interruptRequested: false,
  };
  sessionHandles.set(threadId, entry);

  try {
    const sessionCwd = params.cwd || process.cwd();
    const initialInput = buildPromptInput(
      params.prompt,
      params.attachments || [],
      sessionCwd,
      params.sessionId || params.resume || "",
    );
    await pushClaudePromptInput(
      messageInput,
      initialInput,
      params.sessionId || params.resume || "",
    );

    entry.runPromise = handleQuery(req, entry);
    await entry.runPromise;
  } catch (error) {
    if (sessionHandles.get(threadId) === entry) {
      sessionHandles.delete(threadId);
    }
    messageInput.destroy();
    emit({
      id,
      type: "error",
      message: error instanceof Error ? error.message : String(error),
      recoverable: false,
    });
  }
}

async function sendPersistentSessionMessage(req) {
  const { id, params = {} } = req;
  const threadId = typeof params.threadId === "string" ? params.threadId.trim() : "";
  const entry = sessionHandles.get(threadId);
  if (!entry?.query || !entry.context) {
    emit({
      id,
      type: "error",
      message: "Claude persistent session handle was not found or is not ready.",
      recoverable: false,
    });
    return;
  }
  if (!entry.context.turnCompleted) {
    emit({
      id,
      type: "error",
      message: "Claude persistent session already has an active turn.",
      recoverable: true,
    });
    return;
  }

  try {
    const sessionCwd = params.cwd || process.cwd();
    const input = buildPromptInput(
      params.prompt,
      params.attachments || [],
      sessionCwd,
      entry.context.sessionId || params.sessionId || params.resume || "",
    );

    // 每个复用会话轮次都以 AuraCoder 当前配置更新仍在运行的 Claude query。
    // 模型必须先更新，思考强度的合法范围取决于当前模型。
    const nextModel = typeof params.model === "string" && params.model.trim()
      ? params.model.trim()
      : undefined;
    await entry.query.setModel(nextModel);
    const nextReasoningEffort = typeof params.reasoningEffort === "string" && params.reasoningEffort.trim()
      ? params.reasoningEffort.trim()
      : null;
    await entry.query.applyFlagSettings({ effortLevel: nextReasoningEffort });

    // 新逻辑轮次重新开始展示，清除上一轮保留的终态任务行。
    entry.context.backgroundTaskDisplay.clear();
    entry.context.cancelled = false;
    entry.context.turnCompleted = false;
    // 新逻辑轮次重新等待 SDK result，并恢复默认终态。
    entry.context.sdkResultReceived = false;
    entry.context.sdkTerminalStatus = "completed";
    // 新轮次不重写权威任务 Map，只根据现有快照重建通知等待状态。
    entry.context.pendingTaskNotificationIds.clear();
    entry.context.notifiedTaskIds.clear();
    for (const taskId of entry.context.backgroundTasks.keys()) {
      entry.context.pendingTaskNotificationIds.add(taskId);
    }
    entry.context.authoritativeBackgroundTasksEmpty = entry.context.backgroundTasks.size === 0;
    entry.context.awaitingTaskNotification = entry.context.pendingTaskNotificationIds.size > 0;
    // 新轮次重新统计已注入和已收到的续跑结果。
    entry.context.backgroundContinuationInjectedCount = 0;
    entry.context.backgroundContinuationResultCount = 0;
    entry.context.tokenUsage = null;
    entry.context.stopReason = null;
    entry.interruptRequested = false;
    emit({ id: entry.context.id, type: "turn_started" });
    await pushClaudePromptInput(
      entry.messageInput,
      input,
      entry.context.sessionId || params.sessionId || params.resume || "",
    );
    emit({
      id,
      type: "session_message_accepted",
      threadId,
      handleId: entry.handleId,
      accepted: true,
    });
  } catch (error) {
    entry.context.turnCompleted = true;
    emit({
      id,
      type: "error",
      message: error instanceof Error ? error.message : String(error),
      recoverable: false,
    });
  }
}

async function interruptPersistentSessionHandle(req) {
  const { id, params = {} } = req;
  const threadId = typeof params.threadId === "string" ? params.threadId.trim() : "";
  const entry = sessionHandles.get(threadId);
  if (!entry?.query || !entry.context) {
    emit({
      id,
      type: "error",
      message: "Claude persistent session handle was not found or is not ready.",
      recoverable: false,
    });
    return;
  }

  try {
    entry.interruptRequested = true;
    entry.context.cancelled = true;
    cleanupPendingApprovalsForQuery(
      entry.context.id,
      "Claude persistent session was interrupted before approval was answered.",
    );
    cleanupPendingComputerControlCalls(
      entry.context,
      "Claude persistent session was interrupted before the computer operation completed.",
    );
    await entry.query.interrupt();
    emit({
      id,
      type: "session_handle_interrupted",
      threadId,
      handleId: entry.handleId,
      interrupted: true,
    });
  } catch (error) {
    entry.interruptRequested = false;
    entry.context.cancelled = false;
    emit({
      id,
      type: "error",
      message: error instanceof Error ? error.message : String(error),
      recoverable: false,
    });
  }
}

async function destroyPersistentSessionHandle(req) {
  const { id, params = {} } = req;
  const threadId = typeof params.threadId === "string" ? params.threadId.trim() : "";
  if (!threadId) {
    emit({
      id,
      type: "session_handle_destroyed",
      threadId: null,
      handleId: null,
      success: false,
      error: "Claude persistent session destroy requires threadId.",
    });
    return;
  }

  const entry = sessionHandles.get(threadId);
  if (!entry) {
    emit({
      id,
      type: "session_handle_destroyed",
      threadId,
      handleId: null,
      success: false,
      error: "Claude persistent session handle was not found.",
    });
    return;
  }

  /*
  // 原有 destroy 直接执行清理的实现保留为迁移留痕，实际改由统一 helper 执行。
  sessionHandles.delete(threadId);
  entry.interruptRequested = true;
  if (entry.context) {
    // 销毁持久会话时清除当前 assistant 卡片中的任务行。
    entry.context.backgroundTaskDisplay.clear();
    entry.context.cancelled = true;
    cleanupPendingApprovalsForQuery(
      entry.context.id,
      "Claude persistent session was destroyed before approval was answered.",
    );
    cleanupPendingComputerControlCalls(
      entry.context,
      "Claude persistent session was destroyed before the computer operation completed.",
    );
  }
  entry.messageInput.push(null);
  entry.query?.close?.();
  await entry.runPromise;
  */
  await closeAndRemovePersistentSessionEntry(threadId, entry);
  emit({
    id,
    type: "session_handle_destroyed",
    threadId,
    handleId: entry.handleId,
    success: true,
  });
}

function handleCancel(params = {}) {
  const requestId =
    params.requestId || params.request_id || params.id || null;
  if (!requestId) {
    return;
  }

  const context = activeQueries.get(requestId);
  if (!context) {
    return;
  }

  // 显式取消时清除当前 assistant 卡片中的任务行，避免残留旧轮次展示。
  context.backgroundTaskDisplay.clear();
  context.cancelled = true;
  cleanupPendingApprovalsForQuery(
    requestId,
    "Claude query was canceled before approval was answered.",
  );
  cleanupPendingComputerControlCalls(
    context,
    "Claude query was canceled before the computer operation completed.",
  );
  context.query?.close();
}

function assertClaudeApprovalResponseShape(response) {
  if (!response || typeof response !== "object" || Array.isArray(response)) {
    throw new Error("Claude approval response must be a JSON object.");
  }

  const keys = Object.keys(response);
  if (keys.length !== 1 || !Object.prototype.hasOwnProperty.call(response, "decision")) {
    throw new Error(
      "Claude approval response must include only an explicit decision field.",
    );
  }

  normalizeApprovalDecision(response.decision);
}

/** 向 Rust 返回审批响应处理结果，确保调用方只依据明确回执判断审批是否成功。 */
function emitApprovalResponseResult(requestId, approvalId, success, error) {
  const result = {
    // 将审批响应命令的顶层请求标识回传给等待方。
    ...(requestId ? { id: requestId } : {}),
    // 回传本次审批的业务标识，缺失时使用 null 以保持协议字段稳定。
    approvalId: approvalId ?? null,
    // 标识 sidecar 是否成功处理并结算了审批响应。
    success,
    // 失败时保留 sidecar 原始错误文本，便于 Rust 和前端展示。
    ...(error ? { error } : {}),
  };
  traceClaudeSdk("approval_response_result", {
    // 记录命令请求标识，便于关联 Rust 的审批等待日志。
    requestId: requestId ?? null,
    // 记录审批业务标识，便于定位未知或失效审批。
    approvalId: approvalId ?? null,
    // 记录本次审批处理结果。
    success,
    // 记录原始异常信息，不在 sidecar 日志层吞掉错误。
    ...(error ? { error } : {}),
  });
  emit({
    // 标识这是审批响应回执，而不是普通审批请求事件。
    type: "approval_response_result",
    ...result,
  });
}

/** 向 Rust 返回活动查询权限策略更新回执，保留命令和查询关联标识。 */
function emitPermissionPolicyUpdateResult(requestId, queryId, success, error) {
  const result = {
    ...(requestId ? { id: requestId } : {}),
    queryId: queryId ?? null,
    success,
    ...(error ? { error } : {}),
  };
  traceClaudeSdk("permission_policy_update_result", result);
  emit({
    type: "permission_policy_update_result",
    ...result,
  });
}

/** 更新活动 Claude 查询的权限策略，并明确返回更新成功或失败回执。 */
function handlePermissionPolicyUpdate(params = {}, requestId) {
  const queryId = typeof params.queryId === "string" ? params.queryId.trim() : "";
  if (!queryId) {
    emitPermissionPolicyUpdateResult(
      requestId,
      null,
      false,
      "Claude permission policy update is missing a query ID.",
    );
    return;
  }

  const context = activeQueries.get(queryId);
  if (!context) {
    emitPermissionPolicyUpdateResult(
      requestId,
      queryId,
      false,
      `Claude query ${queryId} is unknown or no longer active.`,
    );
    return;
  }

  const approvalPolicy = params.approvalPolicy;
  if (approvalPolicy !== null && typeof approvalPolicy !== "string") {
    emitPermissionPolicyUpdateResult(
      requestId,
      queryId,
      false,
      "Claude permission policy update requires approvalPolicy to be a string or null.",
    );
    return;
  }

  const hasPlanMode = Object.prototype.hasOwnProperty.call(params, "planMode");
  if (hasPlanMode && typeof params.planMode !== "boolean") {
    emitPermissionPolicyUpdateResult(
      requestId,
      queryId,
      false,
      "Claude permission policy update requires planMode to be a boolean when provided.",
    );
    return;
  }

  const nextPlanMode = hasPlanMode ? params.planMode : context.planMode;
  context.approvalPolicy = approvalPolicy;
  context.planMode = nextPlanMode;
  context.permissionOptions = resolveClaudeSdkPermissionOptions(
    context.approvalPolicy,
    context.planMode,
  );
  emitPermissionPolicyUpdateResult(requestId, queryId, true);
}

/** 处理客户端审批响应，结算 pending approval 并同步发送处理回执。 */
function handleApprovalResponse(params = {}, requestId) {
  const approvalId = params.approvalId || params.approval_id;
  if (!approvalId) {
    emitApprovalResponseResult(
      requestId,
      null,
      false,
      "Claude approval response is missing an approval ID.",
    );
    return;
  }

  const pending = pendingApprovals.get(approvalId);
  if (!pending) {
    emitApprovalResponseResult(
      requestId,
      approvalId,
      false,
      `Claude approval ID ${approvalId} is unknown or no longer pending.`,
    );
    return;
  }

  try {
    const response = params.response || {};
    const permission =
      pending.kind === "ask_user_question"
        ? resolveAskUserQuestionResponse(response, pending.toolInput)
        : (() => {
            assertClaudeApprovalResponseShape(response);
            return resolveApprovalDecision(response, pending.suggestions);
          })();
    pendingApprovals.delete(approvalId);
    const context = activeQueries.get(pending.queryId);
    context?.pendingApprovalIds.delete(approvalId);
    pending.resolve(permission);
    emitApprovalResponseResult(requestId, approvalId, true);
  } catch (error) {
    pendingApprovals.delete(approvalId);
    const context = activeQueries.get(pending.queryId);
    context?.pendingApprovalIds.delete(approvalId);
    const errorMessage = error.message || String(error);
    pending.resolve({
      behavior: "deny",
      message: "Claude approval response was invalid and was denied.",
    });
    emit({
      id: pending.queryId,
      type: "error",
      message: errorMessage,
      recoverable: true,
    });
    emitApprovalResponseResult(requestId, approvalId, false, errorMessage);
  }
}

function handleShutdown(signal) {
  shuttingDown = true;
  for (const context of activeQueries.values()) {
    // sidecar 关闭时清除所有查询的后台任务展示状态。
    context.backgroundTaskDisplay.clear();
    context.cancelled = true;
    cleanupPendingApprovalsForQuery(
      context.id,
      `Claude query was interrupted by ${signal}.`,
    );
    cleanupPendingComputerControlCalls(
      context,
      `Claude query was interrupted by ${signal}.`,
    );
    if (context.messageInput && !context.messageInput.readableEnded) {
      context.messageInput.push(null);
    }
    context.query?.close?.();
    emitTurnCompleted(context, "interrupted");
  }

  rl.close();
  if (process.stdout.writableEnded) {
    process.exit(0);
  } else {
    process.stdout.end(() => process.exit(0));
  }
}

rl.on("line", (line) => {
  let req;
  try {
    req = JSON.parse(line);
  } catch {
    emit({ type: "error", message: "invalid JSON input" });
    return;
  }

  if (req.method === "cancel") {
    handleCancel(req.params || {});
    return;
  }

  if (req.method === "approval_response") {
    handleApprovalResponse(req.params || {}, req.id);
    return;
  }

  if (req.method === "update_permission_policy") {
    handlePermissionPolicyUpdate(req.params || {}, req.id);
    return;
  }

  if (req.method === "computer_control_tool_result") {
    resolveComputerControlToolResult(req.params || {});
    return;
  }

  if (req.method === "version") {
    emit({
      id: req.id,
      type: "version",
      version: "1.0.0",
      runtimeSource: claudeCodeExecutable ? "system" : "bundled",
      runtimeExecutable: claudeCodeExecutable || undefined,
      sdkVersion: sdkVersion || undefined,
      bundledClaudeCodeVersion: bundledClaudeCodeVersion || undefined,
    });
    return;
  }

  if (req.method === "list_models") {
    void handleListModels(req);
    return;
  }

  if (req.method === "list_sessions") {
    void handleListSessions(req);
    return;
  }

  if (req.method === "read_session_history") {
    void handleReadSessionHistory(req);
    return;
  }

  if (req.method === "get_usage_limits") {
    void handleUsageLimits(req);
    return;
  }

  if (req.method === "create_session_handle") {
    void createPersistentSessionHandle(req);
    return;
  }

  if (req.method === "send_session_message") {
    void sendPersistentSessionMessage(req);
    return;
  }

  if (req.method === "interrupt_session_handle") {
    void interruptPersistentSessionHandle(req);
    return;
  }

  if (req.method === "destroy_session_handle") {
    void destroyPersistentSessionHandle(req);
    return;
  }

  if (req.method === "query") {
    void handleQuery(req);
  }
});

rl.on("close", () => {
  if (!shuttingDown) {
    process.exit(0);
  }
});
process.on("SIGTERM", () => handleShutdown("SIGTERM"));
process.on("SIGINT", () => handleShutdown("SIGINT"));
emit({ type: "ready" });
