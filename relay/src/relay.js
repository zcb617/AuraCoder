import { createHash, randomUUID } from "node:crypto";
import { createServer } from "node:http";
import { WebSocket, WebSocketServer } from "ws";
import { readFile, readdir, rename, rm, mkdir, writeFile } from "fs/promises";
import { join } from "path";
import { timingSafeEqual } from "crypto";

// 旧值 1 MiB 会把手机端会话分页响应误判为超限帧导致桌面断线，保留记录：
// const DEFAULT_MAX_PAYLOAD_BYTES = 1024 * 1024;
const DEFAULT_MAX_PAYLOAD_BYTES = 100 * 1024 * 1024;
const DEFAULT_MAX_BUFFERED_BYTES = 100 * 1024 * 1024;
const DEFAULT_HANDSHAKE_TIMEOUT_MS = 10_000;
const DEFAULT_HEARTBEAT_INTERVAL_MS = 25_000;
const DEFAULT_ATTACHMENT_MAX_BYTES = 100 * 1024 * 1024;
const DEFAULT_ATTACHMENT_RETENTION_MS = 24 * 60 * 60 * 1000;
const DEFAULT_ATTACHMENT_CLEANUP_INTERVAL_MS = 60 * 60 * 1000;
const MAX_MULTIPART_OVERHEAD_BYTES = 20 * 1024 * 1024;
const MAX_DEVICE_OR_BATCH_ID_LENGTH = 128;
const MAX_FILE_NAME_LENGTH = 255;
const MAX_MIME_TYPE_LENGTH = 127;
const TUNNEL_ID_PATTERN = /^[A-Za-z0-9_-]{16,128}$/;
const MIME_TYPE_PATTERN = /^[A-Za-z0-9!#$&^_.+-]+\/[A-Za-z0-9!#$&^_.+-]+$/;
const ATTACHMENT_KEY_PATTERN = /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i;

class HttpError extends Error {
  constructor(statusCode, code, message = code) {
    super(message);
    this.statusCode = statusCode;
    this.code = code;
  }
}

function jsonResponse(response, statusCode, payload) {
  const body = JSON.stringify(payload);
  response.writeHead(statusCode, {
    "content-type": "application/json; charset=utf-8",
    "content-length": Buffer.byteLength(body),
    "cache-control": "no-store",
  });
  response.end(body);
}

function sendJson(socket, payload) {
  if (socket.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify(payload));
  }
}

function credentialHash(credential) {
  return createHash("sha256").update(credential, "utf8").digest("hex");
}

function constantTimeEqual(left, right) {
  const leftBuffer = Buffer.from(left, "utf8");
  const rightBuffer = Buffer.from(right, "utf8");
  if (leftBuffer.length !== rightBuffer.length) return false;
  return timingSafeEqual(leftBuffer, rightBuffer);
}

function headerValue(headers, name) {
  const value = headers[name];
  if (Array.isArray(value)) return value[0] ?? "";
  return typeof value === "string" ? value : "";
}

function safeHeaderValue(value) {
  const sanitized = String(value).replace(/[\u0000-\u001f\u007f]/g, "");
  if (/^[\x20-\x7e]*$/.test(sanitized)) return sanitized;
  return encodeURIComponent(sanitized);
}

function safeFileNameHeader(value) {
  // 文件名可能包含非 ASCII 字符；编码后由 PC 端通过 decodeURIComponent 还原。
  const sanitized = String(value).replace(/[\uD800-\uDFFF]/g, "�");
  return encodeURIComponent(sanitized);
}

function attachmentPaths(attachmentDirectory, attachmentKey) {
  return {
    // 二进制内容文件只使用随机 UUID 作为文件名，避免路径穿越。
    dataPath: join(attachmentDirectory, `${attachmentKey}.bin`),
    // 元数据文件与二进制文件同名，便于原子写入和过期清理。
    metadataPath: join(attachmentDirectory, `${attachmentKey}.json`),
  };
}

function readFormString(form, name, maxLength, { required = true } = {}) {
  const value = form.get(name);
  if (typeof value !== "string") {
    if (!required && (value === null || value === undefined)) return "";
    throw new HttpError(400, "invalid_form", `${name} must be a text field`);
  }
  const normalized = value.trim();
  if (required && normalized.length === 0) {
    throw new HttpError(400, "invalid_form", `${name} is required`);
  }
  if (normalized.length > maxLength) {
    throw new HttpError(413, "field_too_large", `${name} is too long`);
  }
  return normalized;
}

async function readMultipartForm(request, maxBodyBytes) {
  const contentType = headerValue(request.headers, "content-type");
  if (!/^multipart\/form-data(?:\s*;|$)/i.test(contentType)) {
    throw new HttpError(400, "invalid_content_type", "multipart/form-data is required");
  }

  const declaredLength = Number.parseInt(headerValue(request.headers, "content-length"), 10);
  if (Number.isInteger(declaredLength) && declaredLength > maxBodyBytes) {
    request.resume();
    throw new HttpError(413, "payload_too_large", "multipart body is too large");
  }

  const chunks = [];
  let totalBytes = 0;
  try {
    for await (const chunk of request) {
      const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
      totalBytes += buffer.length;
      if (totalBytes > maxBodyBytes) {
        request.resume();
        throw new HttpError(413, "payload_too_large", "multipart body is too large");
      }
      chunks.push(buffer);
    }
  } catch (error) {
    request.resume();
    throw error;
  }

  const body = Buffer.concat(chunks, totalBytes);
  try {
    // 运行时的标准 Request/FormData 实现直接解析 multipart，不引入额外依赖。
    return await new Request("http://relay.local/api/mobile/attachments", {
      method: "POST",
      headers: { "content-type": contentType },
      body,
    }).formData();
  } catch {
    throw new HttpError(400, "invalid_multipart", "multipart body could not be parsed");
  }
}

export function createRelayServer(options = {}) {
  const host = options.host ?? "0.0.0.0";
  const port = options.port ?? 18_080;
  const maxPayloadBytes = options.maxPayloadBytes ?? DEFAULT_MAX_PAYLOAD_BYTES;
  const maxBufferedBytes = options.maxBufferedBytes ?? DEFAULT_MAX_BUFFERED_BYTES;
  const handshakeTimeoutMs = options.handshakeTimeoutMs ?? DEFAULT_HANDSHAKE_TIMEOUT_MS;
  const heartbeatIntervalMs = options.heartbeatIntervalMs ?? DEFAULT_HEARTBEAT_INTERVAL_MS;
  const attachmentMaxBytes = options.attachmentMaxBytes ?? DEFAULT_ATTACHMENT_MAX_BYTES;
  const attachmentRetentionMs = options.attachmentRetentionMs ?? DEFAULT_ATTACHMENT_RETENTION_MS;
  const attachmentCleanupIntervalMs = options.attachmentCleanupIntervalMs
    ?? DEFAULT_ATTACHMENT_CLEANUP_INTERVAL_MS;
  const attachmentDirectory = options.attachmentDirectory
    ?? process.env.ATTACHMENT_DIR
    ?? join(process.cwd(), "data", "attachments");
  const multipartMaxBodyBytes = attachmentMaxBytes + MAX_MULTIPART_OVERHEAD_BYTES;
  const logger = options.logger ?? console;
  const tunnels = new Map();
  let attachmentCleanupTimer = null;

  async function ensureAttachmentDirectory() {
    await mkdir(attachmentDirectory, { recursive: true });
  }

  function authenticateTunnel(tunnelId, credential) {
    if (!credential || credential.length < 32 || credential.length > 512) {
      throw new HttpError(401, "unauthorized", "relay credential is required");
    }
    if (!TUNNEL_ID_PATTERN.test(tunnelId)) {
      throw new HttpError(400, "invalid_tunnel_id", "tunnel_id is invalid");
    }
    const tunnel = tunnels.get(tunnelId);
    if (!tunnel) {
      throw new HttpError(404, "tunnel_not_found", "tunnel is not connected");
    }
    if (!constantTimeEqual(tunnel.credentialHash, credentialHash(credential))) {
      throw new HttpError(403, "forbidden", "relay credential is invalid");
    }
    return tunnel;
  }

  async function readAttachmentMetadata(attachmentKey) {
    if (!ATTACHMENT_KEY_PATTERN.test(attachmentKey)) return null;
    const { metadataPath } = attachmentPaths(attachmentDirectory, attachmentKey);
    try {
      const text = await readFile(metadataPath, "utf8");
      const metadata = JSON.parse(text);
      if (!metadata || metadata.attachment_key !== attachmentKey) return null;
      return metadata;
    } catch (error) {
      if (error?.code === "ENOENT") return null;
      logger.warn?.("attachment metadata read failed", {
        // 便于定位损坏的元数据文件。
        attachment_key: attachmentKey,
        // 仅记录错误摘要，不记录凭据或文件内容。
        message: error instanceof Error ? error.message : String(error),
      });
      return null;
    }
  }

  async function removeAttachmentFiles(attachmentKey) {
    const { dataPath, metadataPath } = attachmentPaths(attachmentDirectory, attachmentKey);
    await Promise.all([
      rm(dataPath, { force: true }),
      rm(metadataPath, { force: true }),
    ]);
  }

  async function cleanupExpiredAttachments(now = Date.now()) {
    let entries;
    try {
      entries = await readdir(attachmentDirectory, { withFileTypes: true });
    } catch (error) {
      if (error?.code === "ENOENT") return 0;
      logger.warn?.("attachment cleanup scan failed", {
        // 清理任务的错误摘要。
        message: error instanceof Error ? error.message : String(error),
      });
      return 0;
    }

    let removedCount = 0;
    for (const entry of entries) {
      if (!entry.isFile() || !entry.name.endsWith(".json")) continue;
      const attachmentKey = entry.name.slice(0, -".json".length);
      if (!ATTACHMENT_KEY_PATTERN.test(attachmentKey)) continue;
      const metadata = await readAttachmentMetadata(attachmentKey);
      if (!metadata) continue;
      const expiresAt = Date.parse(String(metadata.expires_at ?? ""));
      if (!Number.isFinite(expiresAt) || expiresAt > now) continue;
      await removeAttachmentFiles(attachmentKey);
      removedCount += 1;
    }
    return removedCount;
  }

  function assertAttachmentMetadata(metadata, tunnelId) {
    if (!metadata || metadata.tunnel_id !== tunnelId) {
      throw new HttpError(404, "attachment_not_found", "attachment is not available");
    }
    const expiresAt = Date.parse(String(metadata.expires_at ?? ""));
    if (!Number.isFinite(expiresAt) || expiresAt <= Date.now()) {
      void removeAttachmentFiles(metadata.attachment_key);
      throw new HttpError(404, "attachment_expired", "attachment has expired");
    }
  }

  async function persistAttachment(attachmentKey, content, metadata) {
    const { dataPath, metadataPath } = attachmentPaths(attachmentDirectory, attachmentKey);
    const temporaryDataPath = `${dataPath}.${randomUUID()}.tmp`;
    const temporaryMetadataPath = `${metadataPath}.${randomUUID()}.tmp`;
    try {
      await writeFile(temporaryDataPath, content, { flag: "wx" });
      await rename(temporaryDataPath, dataPath);
      await writeFile(temporaryMetadataPath, JSON.stringify(metadata), { flag: "wx" });
      await rename(temporaryMetadataPath, metadataPath);
    } catch (error) {
      await Promise.allSettled([
        rm(temporaryDataPath, { force: true }),
        rm(temporaryMetadataPath, { force: true }),
        rm(dataPath, { force: true }),
        rm(metadataPath, { force: true }),
      ]);
      throw error;
    }
  }

  function sendHttpError(response, error) {
    if (response.headersSent) {
      response.destroy();
      return;
    }
    const statusCode = error instanceof HttpError ? error.statusCode : 500;
    const code = error instanceof HttpError ? error.code : "internal_error";
    const message = error instanceof HttpError ? error.message : "internal server error";
    if (!(error instanceof HttpError)) {
      logger.error?.("attachment request failed", {
        // 错误摘要不包含上传内容。
        message: error instanceof Error ? error.message : String(error),
      });
    }
    jsonResponse(response, statusCode, { error: code, message });
  }

  async function handleAttachmentUpload(request, response) {
    try {
      const credential = headerValue(request.headers, "x-auracoder-relay-credential");
      if (!credential) {
        request.resume();
        throw new HttpError(401, "unauthorized", "relay credential is required");
      }
      const form = await readMultipartForm(request, multipartMaxBodyBytes);
      const tunnelId = readFormString(form, "tunnel_id", 128);
      authenticateTunnel(tunnelId, credential);
      const deviceId = readFormString(form, "device_id", MAX_DEVICE_OR_BATCH_ID_LENGTH);
      const batchId = readFormString(form, "batch_id", MAX_DEVICE_OR_BATCH_ID_LENGTH);
      const fileName = readFormString(form, "file_name", MAX_FILE_NAME_LENGTH);
      const mimeType = readFormString(form, "mime_type", MAX_MIME_TYPE_LENGTH).toLowerCase();
      const attachmentKind = readFormString(form, "attachment_kind", 16).toLowerCase();
      if (!["image", "file"].includes(attachmentKind)) {
        throw new HttpError(400, "invalid_attachment_kind", "attachment_kind must be image or file");
      }
      if (!MIME_TYPE_PATTERN.test(mimeType)) {
        throw new HttpError(400, "invalid_mime_type", "mime_type is invalid");
      }
      if (/[\u0000-\u001f\u007f]/.test(fileName)) {
        throw new HttpError(400, "invalid_file_name", "file_name contains control characters");
      }

      const filePart = form.get("file");
      if (!filePart || typeof filePart !== "object" || typeof filePart.arrayBuffer !== "function") {
        throw new HttpError(400, "missing_file", "file part is required");
      }
      const declaredFileSize = Number(filePart.size);
      if (Number.isFinite(declaredFileSize) && declaredFileSize > attachmentMaxBytes) {
        throw new HttpError(413, "attachment_too_large", "single attachment cannot exceed 10 MiB");
      }
      const content = Buffer.from(await filePart.arrayBuffer());
      if (content.length > attachmentMaxBytes) {
        throw new HttpError(413, "attachment_too_large", "single attachment cannot exceed 10 MiB");
      }

      const attachmentKey = randomUUID();
      const createdAt = new Date();
      const expiresAt = new Date(createdAt.getTime() + attachmentRetentionMs);
      const metadata = {
        // 不可预测的附件引用，GET/DELETE 只接受此 UUID。
        attachment_key: attachmentKey,
        // 附件所属的活动隧道。
        tunnel_id: tunnelId,
        // 上传设备标识。
        device_id: deviceId,
        // 发送批次标识。
        batch_id: batchId,
        // 客户端展示用文件名。
        file_name: fileName,
        // 客户端声明的 MIME 类型。
        mime_type: mimeType,
        // image 或 file。
        attachment_kind: attachmentKind,
        // 实际保存的字节数。
        size_bytes: content.length,
        // 创建时间，用于排查和清理。
        created_at: createdAt.toISOString(),
        // 过期时间，默认保留 24 小时。
        expires_at: expiresAt.toISOString(),
      };
      await persistAttachment(attachmentKey, content, metadata);
      jsonResponse(response, 201, {
        // 供后续 message.send 使用的附件引用。
        attachment_key: attachmentKey,
        // 原始文件名。
        file_name: fileName,
        // 实际保存大小。
        size_bytes: content.length,
        // 文件 MIME 类型。
        mime_type: mimeType,
        // 上传设备标识。
        device_id: deviceId,
        // 发送批次标识。
        batch_id: batchId,
      });
    } catch (error) {
      sendHttpError(response, error);
    }
  }

  async function handleAttachmentDownload(request, response, requestUrl, attachmentKey) {
    try {
      const credential = headerValue(request.headers, "x-auracoder-relay-credential");
      const tunnelId = requestUrl.searchParams.get("tunnel_id")?.trim() ?? "";
      authenticateTunnel(tunnelId, credential);
      const metadata = await readAttachmentMetadata(attachmentKey);
      assertAttachmentMetadata(metadata, tunnelId);
      const { dataPath } = attachmentPaths(attachmentDirectory, attachmentKey);
      let content;
      try {
        content = await readFile(dataPath);
      } catch (error) {
        if (error?.code === "ENOENT") {
          throw new HttpError(404, "attachment_not_found", "attachment is not available");
        }
        throw error;
      }
      response.writeHead(200, {
        // 原始二进制内容的类型由上传表单字段决定。
        "content-type": metadata.mime_type,
        // 便于客户端按字节读取响应。
        "content-length": content.length,
        // GET 不应被代理或浏览器缓存。
        "cache-control": "no-store",
        // 设备和批次用于桌面端归属校验。
        "x-auracoder-device-id": safeHeaderValue(metadata.device_id),
        "x-auracoder-batch-id": safeHeaderValue(metadata.batch_id),
        // 文件名使用 URI 编码，避免中文直接进入响应头。
        "x-auracoder-file-name": safeFileNameHeader(metadata.file_name),
      });
      response.end(content);
    } catch (error) {
      sendHttpError(response, error);
    }
  }

  async function handleAttachmentDelete(request, response, requestUrl, attachmentKey) {
    try {
      const credential = headerValue(request.headers, "x-auracoder-relay-credential");
      const tunnelId = requestUrl.searchParams.get("tunnel_id")?.trim() ?? "";
      authenticateTunnel(tunnelId, credential);
      const metadata = await readAttachmentMetadata(attachmentKey);
      assertAttachmentMetadata(metadata, tunnelId);
      await removeAttachmentFiles(attachmentKey);
      response.writeHead(204);
      response.end();
    } catch (error) {
      sendHttpError(response, error);
    }
  }

  const httpServer = createServer((request, response) => {
    const requestUrl = new URL(request.url ?? "/", "http://relay.local");
    if (request.method === "GET" && request.url === "/healthz") {
      let desktopConnections = 0;
      let mobileConnections = 0;
      for (const tunnel of tunnels.values()) {
        if (tunnel.desktop?.readyState === WebSocket.OPEN) desktopConnections += 1;
        for (const mobile of tunnel.mobiles.values()) {
          if (mobile.readyState === WebSocket.OPEN) mobileConnections += 1;
        }
      }
      jsonResponse(response, 200, {
        status: "ok",
        tunnels: tunnels.size,
        desktop_connections: desktopConnections,
        mobile_connections: mobileConnections,
      });
      return;
    }

    if (requestUrl.pathname === "/api/mobile/attachments" && request.method === "POST") {
      void handleAttachmentUpload(request, response);
      return;
    }

    const attachmentMatch = /^\/api\/mobile\/attachments\/([^/]+)$/.exec(requestUrl.pathname);
    if (attachmentMatch && (request.method === "GET" || request.method === "DELETE")) {
      let attachmentKey;
      try {
        attachmentKey = decodeURIComponent(attachmentMatch[1]);
      } catch {
        jsonResponse(response, 404, { error: "attachment_not_found" });
        return;
      }
      if (!ATTACHMENT_KEY_PATTERN.test(attachmentKey)) {
        jsonResponse(response, 404, { error: "attachment_not_found" });
        return;
      }
      if (request.method === "GET") {
        void handleAttachmentDownload(request, response, requestUrl, attachmentKey);
      } else {
        void handleAttachmentDelete(request, response, requestUrl, attachmentKey);
      }
      return;
    }

    jsonResponse(response, 404, { error: "not_found" });
  });

  const websocketServer = new WebSocketServer({
    noServer: true,
    maxPayload: maxPayloadBytes,
    perMessageDeflate: false,
  });

  httpServer.on("upgrade", (request, socket, head) => {
    const requestUrl = new URL(request.url ?? "/", "http://relay.local");
    if (requestUrl.pathname !== "/ws/tunnel" && requestUrl.pathname !== "/") {
      socket.write("HTTP/1.1 404 Not Found\r\nConnection: close\r\n\r\n");
      socket.destroy();
      return;
    }
    websocketServer.handleUpgrade(request, socket, head, (websocket) => {
      websocketServer.emit("connection", websocket, request);
    });
  });

  websocketServer.on("connection", (socket, request) => {
    socket.isAlive = true;
    socket.tunnelId = null;
    socket.role = null;
    socket.connectionId = randomUUID();
    socket.remoteAddress = request.headers["x-forwarded-for"]?.split(",")[0]?.trim()
      ?? request.socket.remoteAddress
      ?? "unknown";

    socket.on("pong", () => {
      socket.isAlive = true;
    });

    const handshakeTimer = setTimeout(() => {
      if (!socket.tunnelId) socket.close(4408, "handshake timeout");
    }, handshakeTimeoutMs);

    socket.once("message", (data, isBinary) => {
      clearTimeout(handshakeTimer);
      if (isBinary) {
        socket.close(4400, "text handshake required");
        return;
      }

      let hello;
      try {
        hello = JSON.parse(data.toString("utf8"));
      } catch {
        socket.close(4400, "invalid handshake json");
        return;
      }

      const role = hello?.role;
      const tunnelId = hello?.tunnel_id;
      const credential = hello?.credential;
      if (
        hello?.version !== 1
        || hello?.type !== "tunnel.hello"
        || (role !== "desktop" && role !== "mobile")
        || typeof tunnelId !== "string"
        || !TUNNEL_ID_PATTERN.test(tunnelId)
        || typeof credential !== "string"
        || credential.length < 32
        || credential.length > 512
      ) {
        socket.close(4400, "invalid handshake");
        return;
      }

      const hash = credentialHash(credential);
      let tunnel = tunnels.get(tunnelId);
      if (!tunnel) {
        tunnel = {
          credentialHash: hash,
          desktop: null,
          mobiles: new Map(),
          requestRoutes: new Map(),
        };
        tunnels.set(tunnelId, tunnel);
      } else if (tunnel.credentialHash !== hash) {
        socket.close(4403, "invalid credential");
        return;
      }

      if (role === "desktop") {
        const previous = tunnel.desktop;
        if (previous && previous !== socket && previous.readyState !== WebSocket.CLOSED) {
          previous.close(4409, "connection replaced");
        }
        tunnel.desktop = socket;
      } else {
        tunnel.mobiles.set(socket.connectionId, socket);
      }

      socket.tunnelId = tunnelId;
      socket.role = role;
      const peerOnline = role === "desktop"
        ? [...tunnel.mobiles.values()].some((mobile) => mobile.readyState === WebSocket.OPEN)
        : tunnel.desktop?.readyState === WebSocket.OPEN;

      sendJson(socket, {
        version: 1,
        type: "tunnel.ready",
        connection_id: socket.connectionId,
        peer_online: peerOnline,
      });
      if (peerOnline) {
        if (role === "desktop") {
          for (const mobile of tunnel.mobiles.values()) {
            sendJson(mobile, { version: 1, type: "tunnel.peer_online", role });
          }
        } else {
          sendJson(tunnel.desktop, {
            version: 1,
            type: "tunnel.peer_online",
            role,
            mobile_count: tunnel.mobiles.size,
          });
        }
      }

      logger.info?.("tunnel connection ready", {
        tunnel_id: tunnelId,
        role,
        connection_id: socket.connectionId,
        remote_address: socket.remoteAddress,
      });

      socket.on("message", (payload, payloadIsBinary) => {
        socket.isAlive = true;
        const current = tunnels.get(tunnelId);
        if (!current) return;
        if (payloadIsBinary) {
          // 附件二进制必须通过 HTTPS multipart；WebSocket 只转发文本业务帧。
          sendJson(socket, {
            version: 1,
            type: "tunnel.binary_unsupported",
            code: "attachment_http_required",
          });
          return;
        }

        if (role === "mobile") {
          const target = current.desktop;
          if (!target || target.readyState !== WebSocket.OPEN) {
            sendJson(socket, { version: 1, type: "tunnel.peer_offline", role: "desktop" });
            return;
          }
          if (target.bufferedAmount > maxBufferedBytes) {
            target.close(1013, "slow consumer");
            sendJson(socket, { version: 1, type: "tunnel.peer_offline", role: "desktop" });
            return;
          }
          if (!payloadIsBinary) {
            try {
              const message = JSON.parse(payload.toString("utf8"));
              if (message?.kind === "request" && typeof message.id === "string") {
                current.requestRoutes.set(message.id, socket.connectionId);
              }
            } catch {
              // 非 JSON 文本仍按透明隧道转发。
            }
          }
          target.send(payload, { binary: payloadIsBinary });
          return;
        }

        let targets = [];
        let routedResponse = false;
        if (!payloadIsBinary) {
          try {
            const message = JSON.parse(payload.toString("utf8"));
            if (message?.kind === "response" && typeof message.id === "string") {
              routedResponse = true;
              const connectionId = current.requestRoutes.get(message.id);
              current.requestRoutes.delete(message.id);
              const mobile = connectionId ? current.mobiles.get(connectionId) : null;
              if (mobile?.readyState === WebSocket.OPEN) targets = [mobile];
            }
          } catch {
            // 非 JSON 文本广播给当前通道内的所有移动设备。
          }
        }
        if (!routedResponse) {
          targets = [...current.mobiles.values()].filter((mobile) => mobile.readyState === WebSocket.OPEN);
        }
        if (targets.length === 0) {
          if (current.mobiles.size === 0) {
            sendJson(socket, { version: 1, type: "tunnel.peer_offline", role: "mobile" });
          }
          return;
        }
        for (const target of targets) {
          if (target.bufferedAmount > maxBufferedBytes) {
            target.close(1013, "slow consumer");
            continue;
          }
          target.send(payload, { binary: payloadIsBinary });
        }
      });
    });

    socket.on("error", (error) => {
      logger.warn?.("tunnel websocket error", {
        connection_id: socket.connectionId,
        message: error.message,
      });
    });

    socket.on("close", () => {
      clearTimeout(handshakeTimer);
      const tunnelId = socket.tunnelId;
      const role = socket.role;
      if (!tunnelId || !role) return;
      const tunnel = tunnels.get(tunnelId);
      if (!tunnel) return;

      if (role === "desktop") {
        if (tunnel.desktop !== socket) return;
        tunnel.desktop = null;
        tunnel.requestRoutes.clear();
        for (const mobile of tunnel.mobiles.values()) {
          sendJson(mobile, { version: 1, type: "tunnel.peer_offline", role });
        }
      } else {
        if (tunnel.mobiles.get(socket.connectionId) !== socket) return;
        tunnel.mobiles.delete(socket.connectionId);
        for (const [requestId, connectionId] of tunnel.requestRoutes.entries()) {
          if (connectionId === socket.connectionId) tunnel.requestRoutes.delete(requestId);
        }
        if (tunnel.mobiles.size === 0) {
          sendJson(tunnel.desktop, { version: 1, type: "tunnel.peer_offline", role });
        }
      }
      if (!tunnel.desktop && tunnel.mobiles.size === 0) tunnels.delete(tunnelId);
      logger.info?.("tunnel connection closed", {
        tunnel_id: tunnelId,
        role,
        connection_id: socket.connectionId,
      });
    });
  });

  const heartbeatTimer = setInterval(() => {
    for (const socket of websocketServer.clients) {
      if (!socket.isAlive) {
        socket.terminate();
        continue;
      }
      socket.isAlive = false;
      socket.ping();
    }
  }, heartbeatIntervalMs);
  heartbeatTimer.unref();

  async function start() {
    await ensureAttachmentDirectory();
    await cleanupExpiredAttachments();
    await new Promise((resolve, reject) => {
      httpServer.once("error", reject);
      httpServer.listen(port, host, () => {
        httpServer.off("error", reject);
        resolve();
      });
    });
    if (!attachmentCleanupTimer) {
      attachmentCleanupTimer = setInterval(() => {
        void cleanupExpiredAttachments().catch((error) => {
          logger.warn?.("attachment cleanup failed", {
            message: error instanceof Error ? error.message : String(error),
          });
        });
      }, attachmentCleanupIntervalMs);
      attachmentCleanupTimer.unref();
    }
    const address = httpServer.address();
    logger.info?.("AuraCoder Tunnel Relay listening", { address });
    return address;
  }

  async function stop() {
    clearInterval(heartbeatTimer);
    if (attachmentCleanupTimer) {
      clearInterval(attachmentCleanupTimer);
      attachmentCleanupTimer = null;
    }
    for (const socket of websocketServer.clients) socket.close(1001, "server shutdown");
    await new Promise((resolve) => websocketServer.close(() => resolve()));
    if (httpServer.listening) {
      await new Promise((resolve, reject) => {
        httpServer.close((error) => (error ? reject(error) : resolve()));
      });
    }
  }

  return {
    // HTTP 服务实例，供测试和健康检查使用。
    httpServer,
    // WebSocket 服务实例，供关闭流程使用。
    websocketServer,
    // 当前活动隧道映射。
    tunnels,
    // 启动 Relay 并初始化附件目录。
    start,
    // 停止 Relay，同时停止清理定时器。
    stop,
    // 暴露清理入口，便于运维和测试主动清理过期附件。
    cleanupExpiredAttachments,
  };
}
