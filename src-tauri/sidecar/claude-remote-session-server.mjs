const { default: http } = await import("no" + "de:http");
const { spawn } = await import("no" + "de:child_process");
const { randomUUID } = await import("no" + "de:crypto");
const { createReadStream } = await import("no" + "de:fs");
const { readdir, stat } = await import("no" + "de:fs/promises");
const { default: os } = await import("no" + "de:os");
const { default: path } = await import("no" + "de:path");
const { default: readline } = await import("no" + "de:readline");
const { fileURLToPath } = await import("no" + "de:url");

const MAX_SESSIONS = 500;
const MAX_TRANSCRIPT_LINES = 200;
// Claude Code 会话文件名使用 UUID 作为会话 ID；只接受该格式，避免把请求路径当作文件路径。
const SESSION_ID_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// 解析远端 Claude 会话服务的监听参数和 SSH Tunnel 注入的 MCP Gateway 地址。
function parseArguments(argv) {
  let host = "127.0.0.1";
  let port;
  let mcpGatewayUrl;
  for (let index = 0; index < argv.length; index += 1) {
    if (argv[index] === "--host") {
      host = argv[++index];
    } else if (argv[index] === "--port") {
      port = Number(argv[++index]);
    } else if (argv[index] === "--mcp-gateway-url") {
      mcpGatewayUrl = argv[++index];
    }
  }
  if (
    !["127.0.0.1", "::1"].includes(host) ||
    !Number.isInteger(port) ||
    port < 1 ||
    port > 65535
  ) {
    throw new Error(
      "Usage: claude-remote-session-server.mjs --host <127.0.0.1|::1> --port <port>",
    );
  }
  return { host, port, mcpGatewayUrl };
}

function writeJson(response, status, payload) {
  response.writeHead(status, { "content-type": "application/json; charset=utf-8" });
  response.end(JSON.stringify(payload));
}

/** 记录远端 HTTP 路由返回异常状态前的原始错误和请求上下文。 */
function logHttpError(request, requestUrl, operation, status, error) {
  const isError = error instanceof Error;
  const errorContext = {
    // 固定事件名，用于开发日志检索远端 HTTP 异常。
    event: "claude_remote_session_http_error",
    // 标识发生异常的远端业务操作。
    operation,
    // 记录触发异常请求使用的 HTTP 方法。
    method: request.method ?? "",
    // 记录请求传入的原始 URL，保留路径和查询参数。
    url: request.url ?? requestUrl.pathname,
    // 记录解析后的请求路径，便于按路由关联异常。
    pathname: requestUrl.pathname,
    // 记录即将返回给客户端的 HTTP 状态码。
    status,
    // 记录原始异常类型；非 Error 值使用其 JavaScript 类型。
    name: isError ? error.name : typeof error,
    // 记录未经改写的原始异常消息；非 Error 值转为原始字符串。
    message: isError ? error.message : String(error),
    // 记录原始异常堆栈；非 Error 值明确记录为空字符串。
    stack: isError ? error.stack ?? "" : "",
  };
  console.error(JSON.stringify(errorContext));
}

function projectDirectoryName(cwd) {
  return path.posix.resolve(cwd).replace(/[^a-zA-Z0-9-]/g, "-");
}

function projectsRoot() {
  return path.join(os.homedir(), ".claude", "projects");
}

function extractText(content) {
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

function titleFor(sessionId, candidate) {
  const title = candidate.trim().replace(/\s+/g, " ");
  return title ? title.slice(0, 120) : `Claude session ${sessionId.slice(0, 8)}`;
}

async function readSessionSummary(filePath, expectedCwd, strict = false) {
  const file = path.basename(filePath);
  const sessionId = file.endsWith(".jsonl") ? file.slice(0, -".jsonl".length) : "";
  if (!sessionId) {
    return null;
  }
  let firstPrompt = "";
  let cwd = "";
  let sawNonEmptyLine = false;
  let validRecordCount = 0;
  let parseError = false;
  const lines = readline.createInterface({
    input: createReadStream(filePath, { encoding: "utf8" }),
    crlfDelay: Infinity,
  });
  let count = 0;
  for await (const line of lines) {
    count += 1;
    if (count > MAX_TRANSCRIPT_LINES) {
      break;
    }
    sawNonEmptyLine ||= line.trim().length > 0;
    try {
      const record = JSON.parse(line);
      validRecordCount += 1;
      if (!cwd && typeof record.cwd === "string") {
        cwd = path.posix.resolve(record.cwd);
      }
      if (!firstPrompt && record.type === "user") {
        firstPrompt = extractText(record.message?.content);
      }
      if (cwd && firstPrompt) {
        break;
      }
    } catch {
      parseError = true;
      // Claude 正在追加的末行可能尚未形成完整 JSON。
    }
  }
  if (strict && sawNonEmptyLine && validRecordCount === 0 && parseError) {
    throw new Error(`Claude 会话文件不是有效的 JSONL：${filePath}`);
  }
  if (!cwd && strict) {
    throw new Error(`Claude 会话文件缺少 cwd：${filePath}`);
  }
  if (expectedCwd && cwd !== expectedCwd) {
    return null;
  }
  const fileStat = await stat(filePath);
  return {
    id: sessionId,
    cwd,
    title: titleFor(sessionId, firstPrompt),
    updatedAt: fileStat.mtime.toISOString(),
  };
}

/** 扫描指定 cwd 对应的远端 Claude 项目目录并返回所有会话文件。 */
async function listSessionFiles(cwd) {
  const expectedCwd = path.posix.resolve(cwd);
  const directory = path.join(projectsRoot(), projectDirectoryName(expectedCwd));
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

/** 扫描指定 cwd 对应的远端 Claude 项目目录并返回排序后的会话摘要。 */
async function listSessions(cwd) {
  const expectedCwd = path.posix.resolve(cwd);
  const sessions = [];
  for (const filePath of await listSessionFiles(expectedCwd)) {
    const summary = await readSessionSummary(filePath, expectedCwd);
    if (summary) {
      sessions.push(summary);
    }
  }
  sessions.sort((left, right) => right.updatedAt.localeCompare(left.updatedAt));
  return sessions.slice(0, MAX_SESSIONS);
}

/** 严格读取单个远端 Claude JSONL 文件的完整历史并校验会话标识。 */
async function readSessionHistory(filePath, expectedSessionId) {
  const file = path.basename(filePath);
  const sessionId = file.endsWith(".jsonl") ? file.slice(0, -".jsonl".length) : "";
  if (sessionId !== expectedSessionId) {
    throw new Error(`Claude session file ID mismatch: expected ${expectedSessionId}`);
  }
  const records = [];
  let cwd = "";
  let lineNumber = 0;
  const lines = readline.createInterface({
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
      const recordCwd = path.posix.resolve(record.cwd);
      if (cwd && cwd !== recordCwd) {
        throw new Error(
          `Claude session history contains multiple cwd values at line ${lineNumber}`,
        );
      }
      cwd = recordCwd;
    }
    records.push(record);
  }
  if (!cwd) {
    throw new Error(`Claude session history is missing cwd: ${expectedSessionId}`);
  }
  return { id: expectedSessionId, sessionId: expectedSessionId, cwd, records };
}

/** 在远端固定 Claude 数据根目录内按会话 ID查找唯一历史文件。 */
async function findSessionFiles(sessionId) {
  const directories = [projectsRoot()];
  const files = [];
  const targetFileName = `${sessionId}.jsonl`;
  while (directories.length > 0) {
    const directory = directories.pop();
    let entries;
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch (error) {
      if (error && error.code === "ENOENT") {
        continue;
      }
      throw error;
    }
    for (const entry of entries) {
      const entryPath = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        directories.push(entryPath);
      } else if (entry.isFile() && entry.name === targetFileName) {
        files.push(entryPath);
      }
    }
  }
  return files;
}

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const agentServerPath = path.join(scriptDirectory, "claude-agent-sdk-server.mjs");
const eventClients = new Set();
const activeQueryIds = new Set();
const pendingApprovalQueries = new Map();
const pendingAgentResponses = new Map();
let agentReady = false;
let agentError = "Claude Agent SDK 适配器尚未就绪";
const agent = spawn(process.execPath, [agentServerPath], {
  cwd: scriptDirectory,
  env: process.env,
  stdio: ["pipe", "pipe", "pipe"],
});
const agentLines = readline.createInterface({ input: agent.stdout, crlfDelay: Infinity });
agentLines.on("line", (line) => {
  try {
    const event = JSON.parse(line);
    if (event?.type === "ready") {
      agentReady = true;
      agentError = "";
    } else if (!agentReady && event?.type === "error" && typeof event.message === "string") {
      agentError = event.message;
    }
    if (event?.type === "turn_started" && typeof event.id === "string") {
      activeQueryIds.add(event.id);
    }
    if (
      event?.type === "approval_requested" &&
      typeof event.id === "string" &&
      typeof event.approvalId === "string"
    ) {
      pendingApprovalQueries.set(event.approvalId, event.id);
    }
    if (event?.type === "turn_completed" && typeof event.id === "string") {
      activeQueryIds.delete(event.id);
      for (const [approvalId, queryId] of pendingApprovalQueries) {
        if (queryId === event.id) pendingApprovalQueries.delete(approvalId);
      }
    }
    if (typeof event?.id === "string") {
      const pending = pendingAgentResponses.get(event.id);
      if (pending && (pending.expectedType === event.type || event.type === "error")) {
        pendingAgentResponses.delete(event.id);
        clearTimeout(pending.timer);
        if (event.type === "error") {
          pending.reject(new Error(event.message || "Claude 远端组件操作失败"));
        } else {
          pending.resolve(event);
        }
      }
    }
  } catch {
    // AuraCoder 客户端会忽略无法解析的适配器输出，服务端只负责原样路由 JSON 行。
  }
  for (const client of eventClients) {
    client.write(`${line}\n`);
  }
});
let agentStderr = "";
agent.stderr.on("data", (chunk) => {
  agentStderr = `${agentStderr}${chunk.toString("utf8")}`.slice(-4000);
});
agent.on("error", (error) => {
  agentReady = false;
  agentError = `Claude Agent SDK 适配器启动失败：${error.message}`;
});
agent.on("exit", (code, signal) => {
  agentReady = false;
  agentError = agentError || `Claude Agent SDK 适配器已退出：code=${code} signal=${signal}`;
  for (const pending of pendingAgentResponses.values()) {
    clearTimeout(pending.timer);
    pending.reject(new Error(agentError));
  }
  pendingAgentResponses.clear();
  for (const client of eventClients) {
    client.end();
  }
  eventClients.clear();
});

async function sendAgentCommandAndWait(command, expectedType) {
  if (!agentReady) {
    throw new Error(agentError || agentStderr || "Claude 适配器不可用");
  }
  if (typeof command?.id !== "string" || command.id.length === 0) {
    throw new Error("Claude 远端组件操作缺少请求编号");
  }

  const responsePromise = new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      pendingAgentResponses.delete(command.id);
      reject(new Error("Claude 远端组件操作等待超时"));
    }, 30_000);
    pendingAgentResponses.set(command.id, { expectedType, resolve, reject, timer });
  });

  try {
    await new Promise((resolve, reject) => {
      agent.stdin.write(`${JSON.stringify(command)}\n`, (error) =>
        error ? reject(error) : resolve(),
      );
    });
  } catch (error) {
    const pending = pendingAgentResponses.get(command.id);
    if (pending) {
      pendingAgentResponses.delete(command.id);
      clearTimeout(pending.timer);
      pending.reject(error);
    }
  }

  return responsePromise;
}

async function readJsonBody(request) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > 1024 * 1024) {
      throw new Error("request body is too large");
    }
    chunks.push(chunk);
  }
  const text = Buffer.concat(chunks).toString("utf8");
  return JSON.parse(text || "{}");
}

const { host, port, mcpGatewayUrl } = parseArguments(process.argv.slice(2));
// 远端 Claude 服务只从启动进程环境读取 MCP 租约 Token，不接受 HTTP 请求携带的 Token。
const configuredMcpGatewayToken = process.env.AURACODER_MCP_TOKEN;
const mcpGatewayToken =
  typeof configuredMcpGatewayToken === "string" && configuredMcpGatewayToken.trim()
    ? configuredMcpGatewayToken.trim()
    : undefined;
const server = http.createServer(async (request, response) => {
  const requestUrl = new URL(request.url ?? "/", `http://${host}:${port}`);
  if (request.method === "GET" && requestUrl.pathname === "/health") {
    writeJson(response, agentReady ? 200 : 503, {
      healthy: agentReady,
      error: agentReady ? undefined : agentError || agentStderr || "Claude 适配器不可用",
    });
    return;
  }
  if (request.method === "GET" && requestUrl.pathname === "/events") {
    if (!agentReady) {
      writeJson(response, 503, { error: agentError || agentStderr || "Claude 适配器不可用" });
      return;
    }
    response.writeHead(200, {
      "content-type": "application/x-ndjson; charset=utf-8",
      "cache-control": "no-cache",
      connection: "keep-alive",
    });
    response.write(`${JSON.stringify({ type: "ready" })}\n`);
    eventClients.add(response);
    request.on("close", () => eventClients.delete(response));
    return;
  }
  if (request.method === "POST" && requestUrl.pathname === "/session-handles") {
    if (!agentReady) {
      writeJson(response, 503, { error: agentError || agentStderr || "Claude 适配器不可用" });
      return;
    }
    try {
      const params = await readJsonBody(request);
      const threadId = typeof params?.threadId === "string" ? params.threadId.trim() : "";
      if (!threadId) {
        writeJson(response, 400, { error: "threadId is required" });
        return;
      }
      const handleId = randomUUID();
      const { mcpGatewayToken: _requestMcpGatewayToken, ...requestParams } = params ?? {};
      const event = await sendAgentCommandAndWait(
        {
          id: handleId,
          method: "create_session_handle",
          params: {
            ...requestParams,
            ...(mcpGatewayUrl ? { mcpGatewayUrl } : {}),
            ...(mcpGatewayToken ? { mcpGatewayToken } : {}),
            threadId,
            handleId,
          },
        },
        "session_handle_created",
      );
      writeJson(response, event.reused === true ? 200 : 201, {
        threadId: event.threadId,
        handleId: event.handleId,
        sessionId: event.sessionId ?? null,
        reused: event.reused === true,
      });
    } catch (error) {
      logHttpError(request, requestUrl, "create_session_handle", 500, error);
      writeJson(response, 500, { error: error instanceof Error ? error.message : String(error) });
    }
    return;
  }
  if (
    request.method === "POST" &&
    requestUrl.pathname.startsWith("/session-handles/") &&
    requestUrl.pathname.endsWith("/messages")
  ) {
    if (!agentReady) {
      writeJson(response, 503, { error: agentError || agentStderr || "Claude 适配器不可用" });
      return;
    }
    try {
      const threadId = decodeURIComponent(
        requestUrl.pathname.slice(
          "/session-handles/".length,
          -"/messages".length,
        ),
      ).trim();
      if (!threadId) {
        writeJson(response, 400, { error: "threadId is required" });
        return;
      }
      const params = await readJsonBody(request);
      const { mcpGatewayToken: _requestMcpGatewayToken, ...requestParams } = params ?? {};
      const event = await sendAgentCommandAndWait(
        {
          id: randomUUID(),
          method: "send_session_message",
          params: {
            ...requestParams,
            ...(mcpGatewayUrl ? { mcpGatewayUrl } : {}),
            ...(mcpGatewayToken ? { mcpGatewayToken } : {}),
            threadId,
          },
        },
        "session_message_accepted",
      );
      writeJson(response, 202, {
        threadId: event.threadId,
        handleId: event.handleId,
        accepted: event.accepted === true,
      });
    } catch (error) {
      logHttpError(request, requestUrl, "send_session_message", 500, error);
      writeJson(response, 500, { error: error instanceof Error ? error.message : String(error) });
    }
    return;
  }
  if (
    request.method === "POST" &&
    requestUrl.pathname.startsWith("/session-handles/") &&
    requestUrl.pathname.endsWith("/interrupt")
  ) {
    if (!agentReady) {
      writeJson(response, 503, { error: agentError || agentStderr || "Claude 适配器不可用" });
      return;
    }
    try {
      const threadId = decodeURIComponent(
        requestUrl.pathname.slice(
          "/session-handles/".length,
          -"/interrupt".length,
        ),
      ).trim();
      if (!threadId) {
        writeJson(response, 400, { error: "threadId is required" });
        return;
      }
      const event = await sendAgentCommandAndWait(
        {
          id: randomUUID(),
          method: "interrupt_session_handle",
          params: { threadId },
        },
        "session_handle_interrupted",
      );
      writeJson(response, 200, {
        threadId: event.threadId,
        handleId: event.handleId,
        interrupted: event.interrupted === true,
      });
    } catch (error) {
      logHttpError(request, requestUrl, "interrupt_session_handle", 500, error);
      writeJson(response, 500, { error: error instanceof Error ? error.message : String(error) });
    }
    return;
  }
  if (request.method === "DELETE" && requestUrl.pathname.startsWith("/session-handles/")) {
    if (!agentReady) {
      writeJson(response, 503, { error: agentError || agentStderr || "Claude 适配器不可用" });
      return;
    }
    try {
      const threadId = decodeURIComponent(requestUrl.pathname.slice("/session-handles/".length)).trim();
      if (!threadId) {
        writeJson(response, 400, { error: "threadId is required" });
        return;
      }
      const event = await sendAgentCommandAndWait(
        {
          id: randomUUID(),
          method: "destroy_session_handle",
          params: { threadId },
        },
        "session_handle_destroyed",
      );
      writeJson(response, event.success === true ? 200 : 404, {
        threadId: event.threadId,
        handleId: event.handleId,
        success: event.success === true,
        error: event.error,
      });
    } catch (error) {
      logHttpError(request, requestUrl, "destroy_session_handle", 500, error);
      writeJson(response, 500, { error: error instanceof Error ? error.message : String(error) });
    }
    return;
  }
  if (request.method === "POST" && requestUrl.pathname === "/command") {
    if (!agentReady) {
      writeJson(response, 503, { error: agentError || agentStderr || "Claude 适配器不可用" });
      return;
    }
    try {
      const command = await readJsonBody(request);
      if (!command || typeof command !== "object" || Array.isArray(command)) {
        writeJson(response, 400, { error: "command must be a JSON object" });
        return;
      }
      if (
        ![
          "query",
          "cancel",
          "approval_response",
          "list_models",
          "get_usage_limits",
          "version",
        ].includes(command.method)
      ) {
        writeJson(response, 400, { error: "unsupported Claude command" });
        return;
      }
      if (command.method === "query" && typeof command.id !== "string") {
        writeJson(response, 400, { error: "Claude query requires a request id" });
        return;
      }
      if (command.method === "query" && typeof command.id === "string") {
        activeQueryIds.add(command.id);
      }
      if (command.method === "approval_response") {
        const approvalId = command.params?.approvalId ?? command.params?.approval_id;
        if (typeof approvalId !== "string" || !pendingApprovalQueries.has(approvalId)) {
          writeJson(response, 409, { error: "Claude approval route is no longer active" });
          return;
        }
        pendingApprovalQueries.delete(approvalId);
      }
      await new Promise((resolve, reject) => {
        agent.stdin.write(`${JSON.stringify(command)}\n`, (error) =>
          error ? reject(error) : resolve(),
        );
      });
      writeJson(response, 202, { accepted: true });
    } catch (error) {
      logHttpError(request, requestUrl, "command", 400, error);
      writeJson(response, 400, { error: error instanceof Error ? error.message : String(error) });
    }
    return;
  }
  // 按 ID 查询只在固定的 Claude 数据根目录内枚举文件名，不接受请求方传入目录。
  const rawPathname = (request.url ?? "/").split("?", 1)[0];
  if (
    request.method === "GET" &&
    rawPathname.startsWith("/sessions/") &&
    rawPathname.endsWith("/history")
  ) {
    let sessionId;
    try {
      const segments = rawPathname.split("/");
      if (segments.length !== 4 || segments[1] !== "sessions" || segments[3] !== "history") {
        throw new Error("session_id is required before /history");
      }
      sessionId = decodeURIComponent(segments[2]);
      if (
        !sessionId ||
        sessionId.includes("/") ||
        sessionId.includes("\\") ||
        sessionId.includes("..") ||
        !SESSION_ID_PATTERN.test(sessionId)
      ) {
        throw new Error("session_id is not a valid Claude session ID");
      }
    } catch (error) {
      logHttpError(request, requestUrl, "read_session_history_validate", 400, error);
      writeJson(response, 400, {
        error:
          error instanceof URIError
            ? "session_id contains invalid URI encoding"
            : error instanceof Error
              ? error.message
              : String(error),
      });
      return;
    }
    try {
      const files = await findSessionFiles(sessionId);
      if (files.length === 0) {
        writeJson(response, 404, { error: `Claude session not found: ${sessionId}` });
        return;
      }
      if (files.length > 1) {
        writeJson(response, 409, {
          error: `Multiple Claude session files found for session_id: ${sessionId}`,
        });
        return;
      }
      writeJson(response, 200, await readSessionHistory(files[0], sessionId));
    } catch (error) {
      logHttpError(request, requestUrl, "read_session_history", 500, error);
      writeJson(response, 500, { error: error instanceof Error ? error.message : String(error) });
    }
    return;
  }
  if (request.method === "GET" && rawPathname.startsWith("/sessions/")) {
    let sessionId;
    try {
      const encodedSessionId = rawPathname.slice("/sessions/".length);
      if (!encodedSessionId || encodedSessionId.includes("/")) {
        throw new Error("session_id is required and must be a single path segment");
      }
      sessionId = decodeURIComponent(encodedSessionId);
      if (
        !sessionId ||
        sessionId.includes("/") ||
        sessionId.includes("\\") ||
        sessionId.includes("..") ||
        !SESSION_ID_PATTERN.test(sessionId)
      ) {
        throw new Error("session_id is not a valid Claude session ID");
      }
    } catch (error) {
      logHttpError(request, requestUrl, "read_session_summary_validate", 400, error);
      writeJson(response, 400, {
        error:
          error instanceof URIError
            ? "session_id contains invalid URI encoding"
            : error instanceof Error
              ? error.message
              : String(error),
      });
      return;
    }
    try {
      const directories = [projectsRoot()];
      const files = [];
      const targetFileName = `${sessionId}.jsonl`;
      while (directories.length > 0) {
        const directory = directories.pop();
        let entries;
        try {
          entries = await readdir(directory, { withFileTypes: true });
        } catch (error) {
          if (error && error.code === "ENOENT") {
            continue;
          }
          throw error;
        }
        for (const entry of entries) {
          const entryPath = path.join(directory, entry.name);
          if (entry.isDirectory()) {
            directories.push(entryPath);
          } else if (entry.isFile() && entry.name === targetFileName) {
            files.push(entryPath);
          }
        }
      }
      if (files.length === 0) {
        writeJson(response, 404, { error: `Claude session not found: ${sessionId}` });
        return;
      }
      if (files.length > 1) {
        writeJson(response, 409, {
          error: `Multiple Claude session files found for session_id: ${sessionId}`,
        });
        return;
      }
      const summary = await readSessionSummary(files[0], undefined, true);
      if (!summary) {
        writeJson(response, 500, { error: `Claude session summary is unavailable: ${sessionId}` });
        return;
      }
      writeJson(response, 200, {
        ...summary,
        // 新接口提供显式 sessionId，同时保留现有摘要的 id 字段兼容客户端。
        sessionId: summary.id,
      });
    } catch (error) {
      logHttpError(request, requestUrl, "read_session_summary", 500, error);
      writeJson(response, 500, { error: error instanceof Error ? error.message : String(error) });
    }
    return;
  }
  if (request.method === "GET" && requestUrl.pathname === "/sessions") {
    const cwd = requestUrl.searchParams.get("cwd")?.trim();
    if (!cwd) {
      writeJson(response, 400, { error: "cwd is required" });
      return;
    }
    try {
      writeJson(response, 200, await listSessions(cwd));
    } catch (error) {
      logHttpError(request, requestUrl, "list_sessions", 500, error);
      writeJson(response, 500, { error: error instanceof Error ? error.message : String(error) });
    }
    return;
  }
  writeJson(response, 404, { error: "not found" });
});

server.listen(port, host);

function shutdown() {
  server.close();
  if (!agent.killed) {
    agent.kill("SIGTERM");
  }
}
process.on("SIGTERM", shutdown);
process.on("SIGINT", shutdown);
