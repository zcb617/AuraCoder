import assert from "node:assert/strict";
import { afterEach, test } from "node:test";
import { WebSocket } from "ws";
import { mkdtemp, rm } from "fs/promises";
import { tmpdir } from "os";
import { join } from "path";

import { createRelayServer } from "../src/relay.js";

const activeRelays = [];
const temporaryDirectories = [];

afterEach(async () => {
  while (activeRelays.length > 0) await activeRelays.pop().stop();
  while (temporaryDirectories.length > 0) {
    await rm(temporaryDirectories.pop(), { recursive: true, force: true });
  }
});

async function startRelay(options = {}) {
  const attachmentDirectory = await mkdtemp(join(tmpdir(), "auracoder-relay-"));
  temporaryDirectories.push(attachmentDirectory);
  const relay = createRelayServer({
    host: "127.0.0.1",
    port: 0,
    heartbeatIntervalMs: 60_000,
    logger: { info() {}, warn() {} },
    attachmentDirectory,
    ...options,
  });
  const address = await relay.start();
  activeRelays.push(relay);
  return {
    relay,
    url: `ws://127.0.0.1:${address.port}/ws/tunnel`,
    httpUrl: `http://127.0.0.1:${address.port}`,
    attachmentDirectory,
  };
}

function connect(url, role, tunnelId = "test-tunnel-0001", credential = "a".repeat(32)) {
  return new Promise((resolve, reject) => {
    const socket = new WebSocket(url);
    socket.once("error", reject);
    socket.once("open", () => {
      socket.send(JSON.stringify({
        version: 1,
        type: "tunnel.hello",
        role,
        tunnel_id: tunnelId,
        credential,
      }));
    });
    socket.once("message", (data) => {
      const ready = JSON.parse(data.toString());
      assert.equal(ready.type, "tunnel.ready");
      resolve(socket);
    });
  });
}

function nextMessage(socket) {
  return new Promise((resolve, reject) => {
    socket.once("error", reject);
    socket.once("message", (data, isBinary) => resolve({ data, isBinary }));
  });
}

async function buildAttachmentForm(tunnelId, content, {
  deviceId = "device-test-1",
  batchId = "batch-test-1",
  fileName = "hello.txt",
  mimeType = "text/plain",
  attachmentKind = "file",
} = {}) {
  const form = new FormData();
  form.set("tunnel_id", tunnelId);
  form.set("device_id", deviceId);
  form.set("batch_id", batchId);
  form.set("file_name", fileName);
  form.set("mime_type", mimeType);
  form.set("attachment_kind", attachmentKind);
  form.set("file", new Blob([content], { type: mimeType }), fileName);
  return form;
}

test("health endpoint reports relay state", async () => {
  const { url } = await startRelay();
  const healthUrl = url.replace("ws://", "http://").replace("/ws/tunnel", "/healthz");
  const response = await fetch(healthUrl);
  assert.equal(response.status, 200);
  assert.deepEqual(await response.json(), {
    status: "ok",
    tunnels: 0,
    desktop_connections: 0,
    mobile_connections: 0,
  });
});

test("forwards text frames between desktop and mobile", async () => {
  const { url } = await startRelay();
  const desktop = await connect(url, "desktop");
  const desktopPeerOnline = nextMessage(desktop);
  const mobile = await connect(url, "mobile");
  assert.equal(JSON.parse((await desktopPeerOnline).data.toString()).type, "tunnel.peer_online");

  const received = nextMessage(desktop);
  mobile.send(JSON.stringify({ kind: "request", id: "request-1" }));
  assert.deepEqual(JSON.parse((await received).data.toString()), {
    kind: "request",
    id: "request-1",
  });

  desktop.close();
  mobile.close();
});

test("rejects a mismatched credential", async () => {
  const { url } = await startRelay();
  const desktop = await connect(url, "desktop");
  const mobile = new WebSocket(url);

  const closeCode = await new Promise((resolve, reject) => {
    mobile.once("error", reject);
    mobile.once("open", () => {
      mobile.send(JSON.stringify({
        version: 1,
        type: "tunnel.hello",
        role: "mobile",
        tunnel_id: "test-tunnel-0001",
        credential: "b".repeat(32),
      }));
    });
    mobile.once("close", (code) => resolve(code));
  });

  assert.equal(closeCode, 4403);
  desktop.close();
});

test("keeps different tunnel ids isolated", async () => {
  const { url } = await startRelay();
  const firstDesktop = await connect(url, "desktop", "test-tunnel-0001");
  const secondDesktop = await connect(url, "desktop", "test-tunnel-0002");
  const firstPeerOnline = nextMessage(firstDesktop);
  const firstMobile = await connect(url, "mobile", "test-tunnel-0001");
  await firstPeerOnline;

  let leaked = false;
  secondDesktop.once("message", () => {
    leaked = true;
  });
  firstMobile.send("only-first-tunnel");
  await new Promise((resolve) => setTimeout(resolve, 50));
  assert.equal(leaked, false);

  firstDesktop.close();
  secondDesktop.close();
  firstMobile.close();
});

test("uploads, downloads, and deletes a mobile attachment", async () => {
  const tunnelId = "test-tunnel-0001";
  const credential = "a".repeat(32);
  const { url, httpUrl } = await startRelay();
  const desktop = await connect(url, "desktop", tunnelId, credential);
  const content = Buffer.from("hello relay attachment");
  const form = await buildAttachmentForm(tunnelId, content, {
    fileName: "你好.txt",
  });
  const upload = await fetch(httpUrl + "/api/mobile/attachments", {
    method: "POST",
    headers: { "x-auracoder-relay-credential": credential },
    body: form,
  });
  assert.equal(upload.status, 201);
  const uploaded = await upload.json();
  assert.deepEqual(uploaded, {
    attachment_key: uploaded.attachment_key,
    file_name: "你好.txt",
    size_bytes: content.length,
    mime_type: "text/plain",
    device_id: "device-test-1",
    batch_id: "batch-test-1",
  });
  assert.match(uploaded.attachment_key, /^[0-9a-f-]{36}$/i);

  const attachmentUrl = httpUrl + "/api/mobile/attachments/"
    + uploaded.attachment_key + "?tunnel_id=" + encodeURIComponent(tunnelId);
  const download = await fetch(attachmentUrl, {
    headers: { "x-auracoder-relay-credential": credential },
  });
  assert.equal(download.status, 200);
  assert.deepEqual(Buffer.from(await download.arrayBuffer()), content);
  assert.equal(download.headers.get("content-type"), "text/plain");
  assert.equal(download.headers.get("content-length"), String(content.length));
  assert.equal(download.headers.get("x-auracoder-device-id"), "device-test-1");
  assert.equal(download.headers.get("x-auracoder-batch-id"), "batch-test-1");
  assert.equal(decodeURIComponent(download.headers.get("x-auracoder-file-name")), "你好.txt");

  const removal = await fetch(attachmentUrl, {
    method: "DELETE",
    headers: { "x-auracoder-relay-credential": credential },
  });
  assert.equal(removal.status, 204);
  const missing = await fetch(attachmentUrl, {
    headers: { "x-auracoder-relay-credential": credential },
  });
  assert.equal(missing.status, 404);
  desktop.close();
});

test("rejects attachment requests with an invalid relay credential", async () => {
  const tunnelId = "test-tunnel-0001";
  const credential = "a".repeat(32);
  const { url, httpUrl } = await startRelay();
  const desktop = await connect(url, "desktop", tunnelId, credential);
  const form = await buildAttachmentForm(tunnelId, Buffer.from("secret"));
  const response = await fetch(httpUrl + "/api/mobile/attachments", {
    method: "POST",
    headers: { "x-auracoder-relay-credential": "b".repeat(32) },
    body: form,
  });
  assert.equal(response.status, 403);
  desktop.close();
});

test("rejects an attachment larger than 10 MiB", async () => {
  const tunnelId = "test-tunnel-0001";
  const credential = "a".repeat(32);
  const { url, httpUrl } = await startRelay();
  const desktop = await connect(url, "desktop", tunnelId, credential);
  const form = await buildAttachmentForm(
    tunnelId,
    Buffer.alloc(10 * 1024 * 1024 + 1, 1),
  );
  const response = await fetch(httpUrl + "/api/mobile/attachments", {
    method: "POST",
    headers: { "x-auracoder-relay-credential": credential },
    body: form,
  });
  assert.equal(response.status, 413);
  desktop.close();
});
