const WebSocket = require("ws");

const endpoint = process.env.RELAY_URL || "ws://host.docker.internal:18080/ws/tunnel";
const tunnelId = process.env.TUNNEL_ID || "auracoder_mobile_qa_1";
const credential = process.env.TUNNEL_CREDENTIAL || "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const pairingToken = process.env.PAIRING_TOKEN || "1111111111111111111111111111111111111111111111111111111111111111";
const deviceCredential = process.env.DEVICE_CREDENTIAL || "2222222222222222222222222222222222222222222222222222222222222222";
let pairingAvailable = true;
const workspace = {
  id: "qa-workspace",
  name: "AuraCoder Mobile 联调项目",
  rootPath: "D:/work/auracoder_zh",
  lastOpenedAt: "2026-08-10T08:00:00Z",
};
let thread = {
  id: "qa-thread",
  workspaceId: workspace.id,
  engineId: "codex",
  modelId: "gpt-5",
  title: "验证手机远程对话",
  status: "idle",
  messageCount: 2,
  lastActivityAt: "2026-08-10T08:10:00Z",
};
let messages = [
  {
    id: "qa-user-1",
    threadId: thread.id,
    role: "user",
    content: "请说明当前联调状态。",
    status: "completed",
    createdAt: "2026-08-10T08:09:00Z",
  },
  {
    id: "qa-assistant-1",
    threadId: thread.id,
    role: "assistant",
    content: "手机端已通过 **Tunnel Relay** 连接。\n\n```text\nWSS tunnel ready\n```",
    status: "completed",
    createdAt: "2026-08-10T08:10:00Z",
  },
];

const socket = new WebSocket(endpoint);

socket.on("open", () => {
  socket.send(JSON.stringify({
    version: 1,
    type: "tunnel.hello",
    role: "desktop",
    tunnel_id: tunnelId,
    credential,
  }));
});

socket.on("message", (data) => {
  let request;
  try {
    request = JSON.parse(data.toString());
  } catch {
    return;
  }
  if (request.type || request.kind !== "request") return;
  if (request.method === "device.pair") {
    const pairingAccepted = pairingAvailable && request.auth === pairingToken;
    if (pairingAccepted) pairingAvailable = false;
    socket.send(JSON.stringify(pairingAccepted
      ? { version: 1, kind: "response", id: request.id, ok: true, payload: { device_credential: deviceCredential } }
      : { version: 1, kind: "response", id: request.id, ok: false, error: { code: "unauthorized", message: "Pairing token is invalid or already used" } }));
    return;
  }
  if (request.auth !== deviceCredential) {
    socket.send(JSON.stringify({
      version: 1,
      kind: "response",
      id: request.id,
      ok: false,
      error: { code: "unauthorized", message: "Device credential is invalid" },
    }));
    return;
  }

  let payload = {};
  if (request.method === "desktop.get_status") payload = { version: "0.65.0-qa", online: true };
  else if (request.method === "workspace.list") payload = [workspace];
  else if (request.method === "thread.list") payload = [thread];
  else if (request.method === "message.list") payload = { messages, nextCursor: null };
  else if (request.method === "message.send") {
    messages = [...messages, {
      id: `qa-user-${Date.now()}`,
      threadId: thread.id,
      role: "user",
      content: request.payload.message,
      status: "completed",
      createdAt: new Date().toISOString(),
    }, {
      id: `qa-assistant-${Date.now()}`,
      threadId: thread.id,
      role: "assistant",
      content: "已收到手机消息，正在通过远程隧道回复…",
      status: "streaming",
      createdAt: new Date().toISOString(),
    }];
    thread = { ...thread, status: "streaming", messageCount: messages.length, lastActivityAt: new Date().toISOString() };
  } else if (request.method === "turn.stop") {
    thread = { ...thread, status: "idle" };
    messages = messages.map((message) => message.status === "streaming" ? { ...message, status: "interrupted" } : message);
  }

  socket.send(JSON.stringify({ version: 1, kind: "response", id: request.id, ok: true, payload }));

  if (request.method === "thread.subscribe" || request.method === "message.send" || request.method === "turn.stop") {
    setTimeout(() => {
      if (socket.readyState !== WebSocket.OPEN) return;
      socket.send(JSON.stringify({
        version: 1,
        kind: "event",
        event: "thread.snapshot",
        payload: { thread, messages: { messages, nextCursor: null } },
      }));
    }, 120);
  }
});

socket.on("error", (error) => {
  process.stderr.write(`${error.message}\n`);
  process.exitCode = 1;
});

process.on("SIGTERM", () => socket.close(1000, "fixture shutdown"));
