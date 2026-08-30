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

function createQueryContext(id) {
  return {
    id,
    threadId: id,
    query: null,
    actionCounter: 0,
    actionIdsByToolUseId: new Map(),
    streamToolUseIdsByIndex: new Map(),
    suppressedToolUseIds: new Set(),
    pendingApprovalIds: new Set(),
    cancelled: false,
    turnCompleted: false,
    sessionId: null,
    tokenUsage: null,
    stopReason: null,
    pendingComputerControlCalls: new Map(),
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

function buildPermissionHandler({
  context,
  cwd,
  writableRoots,
  sandboxMode,
  allowNetwork,
  approvalPolicy,
}) {
  const normalizedRoots = writableRoots.map((root) => path.resolve(root));
  const resolvedApprovalPolicy = approvalPolicy ?? (sandboxMode === "read-only" ? "restricted" : undefined);
  const permissionOptions = resolveClaudeSdkPermissionOptions(resolvedApprovalPolicy, false);
  const { decisionMode } = permissionOptions;

  return async (toolName, input, options) => {
    const toolInput = input ?? {};
    const toolUseId = options?.toolUseID;
    const agentId = options?.agentID;
    const requestId = options?.requestId;

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
      if (["Read", "Glob", "Grep", "ExitPlanMode", "EnterPlanMode"].includes(toolName)) {
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

/** 扫描指定 cwd 对应的本机 Claude 项目目录并返回排序后的会话摘要。 */
async function listClaudeSessions(cwd) {
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
  const sessions = [];
  for (const filePath of files) {
    const summary = await readClaudeSessionSummary(filePath, expectedCwd);
    if (summary) {
      sessions.push(summary);
    }
  }
  sessions.sort((left, right) => right.updatedAt.localeCompare(left.updatedAt));
  return sessions.slice(0, MAX_CLAUDE_SESSIONS);
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

  const context = createQueryContext(id);
  context.threadId = threadId || sessionId || resume || id;
  activeQueries.set(id, context);

  const toolList = Array.isArray(allowedTools)
    ? [...allowedTools]
    : [
      "Read",
      "Write",
      "Edit",
      "Bash",
      "Glob",
      "Grep",
      ...(allowNetwork ? ["WebFetch"] : []),
    ];
  const resolvedApprovalPolicy = approvalPolicy ?? (sandboxMode === "read-only" ? "restricted" : undefined);
  const permissionOptions = resolveClaudeSdkPermissionOptions(resolvedApprovalPolicy, planMode);

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
        ? toolList.filter((toolName) => ["Read", "Glob", "Grep", "ExitPlanMode", "EnterPlanMode"].includes(toolName))
        : permissionOptions.decisionMode === "ask"
          ? toolList.filter((toolName) => ["Read", "Glob", "Grep", "ExitPlanMode", "EnterPlanMode"].includes(toolName))
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
        approvalPolicy: resolvedApprovalPolicy,
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
    const promptInput = persistentSession?.messageInput ?? buildPromptInput(
      prompt,
      attachments,
      sessionCwd,
      sessionId || resume || "",
    );
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
        if (persistentSession) {
          traceClaudeSdk("emit_turn_completed_before", {
            requestId: id,
            context,
            status: persistentSession.interruptRequested ? "interrupted" : terminalStatus,
            sawTextDelta,
            terminalStatus,
            contextStopReason: context.stopReason,
          });
          emitTurnCompleted(
            context,
            persistentSession.interruptRequested ? "interrupted" : terminalStatus,
          );
          traceClaudeSdk("emit_turn_completed_after", {
            requestId: id,
            context,
            sawTextDelta,
            terminalStatus,
            contextStopReason: context.stopReason,
          });
          persistentSession.interruptRequested = false;
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
    traceClaudeSdk("emit_turn_completed_before_final", {
      requestId: id,
      context,
      status: context.cancelled ? "interrupted" : terminalStatus,
      sawTextDelta,
      terminalStatus,
      contextStopReason: context.stopReason,
    });
    emitTurnCompleted(context, context.cancelled ? "interrupted" : terminalStatus);
    traceClaudeSdk("emit_turn_completed_after_final", {
      requestId: id,
      context,
      sawTextDelta,
      terminalStatus,
      contextStopReason: context.stopReason,
    });
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
    activeQueries.delete(id);
  }
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
    if (typeof initialInput === "string") {
      messageInput.push({
        type: "user",
        message: {
          role: "user",
          content: initialInput,
        },
        parent_tool_use_id: null,
        session_id: params.sessionId || params.resume || "",
      });
    } else {
      for await (const message of initialInput) {
        messageInput.push(message);
      }
    }

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

    entry.context.cancelled = false;
    entry.context.turnCompleted = false;
    entry.context.tokenUsage = null;
    entry.context.stopReason = null;
    emit({ id: entry.context.id, type: "turn_started" });
    if (typeof input === "string") {
      entry.messageInput.push({
        type: "user",
        message: {
          role: "user",
          content: input,
        },
        parent_tool_use_id: null,
        session_id: entry.context.sessionId || params.sessionId || params.resume || "",
      });
    } else {
      for await (const message of input) {
        entry.messageInput.push(message);
      }
    }
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

  sessionHandles.delete(threadId);
  entry.messageInput.push(null);
  entry.query?.close?.();
  await entry.runPromise;
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
    context.cancelled = true;
    cleanupPendingApprovalsForQuery(
      context.id,
      `Claude query was interrupted by ${signal}.`,
    );
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
