import { afterEach, describe, expect, it } from "vitest";
const { once } = await import("no" + "de:events");
const { mkdtemp, mkdir, rm, utimes, writeFile } = await import("no" + "de:fs/promises");
const { createServer } = await import("no" + "de:net");
const { tmpdir } = await import("no" + "de:os");
const { default: path } = await import("no" + "de:path");
const { spawn } = await import("no" + "de:child_process");
const { fileURLToPath, pathToFileURL } = await import("no" + "de:url");

const testPath = fileURLToPath(import.meta.url);
const repoRoot = path.resolve(path.dirname(testPath), "..");
const serverScript = path.join(
  repoRoot,
  "src-tauri",
  "sidecar",
  "claude-remote-session-server.mjs",
);
const mockSdkModulePath = pathToFileURL(
  path.join(repoRoot, "tests", "fixtures", "claude-agent-sdk-mock.mjs"),
).href;
const tempRoots = [];
const children = [];

async function availablePort() {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  server.close();
  return address.port;
}

async function startServer(home, port, env = {}) {
  const child = spawn(process.execPath, [serverScript, "--port", String(port)], {
    env: { ...process.env, HOME: home, USERPROFILE: home, ...env },
    stdio: "ignore",
  });
  children.push(child);
  for (let attempt = 0; attempt < 30; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/health`);
      if (response.ok) {
        return;
      }
    } catch {
      // 服务尚未开始监听。
    }
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error("Claude SSH remote session server did not become healthy.");
}

afterEach(async () => {
  for (const child of children.splice(0)) {
    child.kill();
  }
  await Promise.all(tempRoots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

describe("Claude SSH remote session server", () => {
  it("rejects non-loopback listening addresses", async () => {
    const port = await availablePort();
    const child = spawn(
      process.execPath,
      [serverScript, "--host", "0.0.0.0", "--port", String(port)],
      { stdio: "ignore" },
    );
    children.push(child);
    const [exitCode] = await once(child, "exit");
    expect(exitCode).not.toBe(0);
  });

  it("proxies command responses through the shared event stream", async () => {
    const home = await mkdtemp(path.join(tmpdir(), "auracoder-claude-remote-protocol-"));
    tempRoots.push(home);
    const port = await availablePort();
    await startServer(home, port);

    const controller = new AbortController();
    const eventsResponse = await fetch(`http://127.0.0.1:${port}/events`, {
      signal: controller.signal,
    });
    expect(eventsResponse.ok).toBe(true);
    const reader = eventsResponse.body.getReader();
    const commandResponse = await fetch(`http://127.0.0.1:${port}/command`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ id: "version-1", method: "version" }),
    });
    expect(commandResponse.status).toBe(202);

    const decoder = new TextDecoder();
    let buffered = "";
    let versionEvent;
    for (let attempt = 0; attempt < 20 && !versionEvent; attempt += 1) {
      const { done, value } = await reader.read();
      if (done) break;
      buffered += decoder.decode(value, { stream: true });
      const lines = buffered.split("\n");
      buffered = lines.pop() ?? "";
      versionEvent = lines
        .filter(Boolean)
        .map((line) => JSON.parse(line))
        .find((event) => event.id === "version-1");
    }
    expect(versionEvent).toMatchObject({
      id: "version-1",
      type: "version",
      version: "1.0.0",
    });
    const staleApprovalResponse = await fetch(`http://127.0.0.1:${port}/command`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        method: "approval_response",
        params: { approvalId: "missing", response: { decision: "accept" } },
      }),
    });
    expect(staleApprovalResponse.status).toBe(409);
    controller.abort();
  });

  it("creates and destroys persistent Claude session handles through HTTP", async () => {
    const home = await mkdtemp(path.join(tmpdir(), "auracoder-claude-session-handle-"));
    tempRoots.push(home);
    const port = await availablePort();
    await startServer(home, port, {
      CLAUDE_AGENT_SDK_MODULE: mockSdkModulePath,
      CLAUDE_AGENT_SDK_MOCK_SCENARIO: JSON.stringify({
        persistentInput: true,
        sessionId: "http-persistent-session",
      }),
      PANES_DISABLE_CLAUDE_USAGE_FETCH: "1",
    });

    const createResponse = await fetch(`http://127.0.0.1:${port}/session-handles`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        threadId: "thread-http-persistent",
        prompt: "first message",
        cwd: "/work/project-http",
      }),
    });
    expect(createResponse.status).toBe(201);
    const created = await createResponse.json();
    expect(created).toMatchObject({
      threadId: "thread-http-persistent",
      reused: false,
    });
    expect(created.handleId).toEqual(expect.any(String));

    const messageResponse = await fetch(
      `http://127.0.0.1:${port}/session-handles/${encodeURIComponent("thread-http-persistent")}/messages`,
      {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({
          prompt: "second message",
          cwd: "/work/project-http",
        }),
      },
    );
    expect(messageResponse.status).toBe(202);
    await expect(messageResponse.json()).resolves.toMatchObject({
      threadId: "thread-http-persistent",
      handleId: created.handleId,
      accepted: true,
    });

    const interruptResponse = await fetch(
      `http://127.0.0.1:${port}/session-handles/${encodeURIComponent("thread-http-persistent")}/interrupt`,
      { method: "POST" },
    );
    expect(interruptResponse.status).toBe(200);
    await expect(interruptResponse.json()).resolves.toMatchObject({
      threadId: "thread-http-persistent",
      handleId: created.handleId,
      interrupted: true,
    });

    const destroyResponse = await fetch(
      `http://127.0.0.1:${port}/session-handles/${encodeURIComponent("thread-http-persistent")}`,
      { method: "DELETE" },
    );
    expect(destroyResponse.status).toBe(200);
    await expect(destroyResponse.json()).resolves.toMatchObject({
      threadId: "thread-http-persistent",
      handleId: created.handleId,
      success: true,
    });
  });

  it("only returns the requested project's remote Claude sessions", async () => {
    const home = await mkdtemp(path.join(tmpdir(), "auracoder-claude-remote-"));
    tempRoots.push(home);
    const cwd = "/work/project_a";
    const otherCwd = "/work/project-b";
    const projectDirectory = cwd.replace(/[^a-zA-Z0-9-]/g, "-");
    const transcriptDirectory = path.join(home, ".claude", "projects", projectDirectory);
    await mkdir(transcriptDirectory, { recursive: true });
    const transcriptPath = path.join(transcriptDirectory, "session-a.jsonl");
    await writeFile(
      transcriptPath,
      [
        JSON.stringify({
          type: "user",
          cwd,
          timestamp: "2026-08-14T08:00:00.000Z",
          message: { content: "请检查远端项目" },
        }),
        JSON.stringify({
          type: "assistant",
          cwd,
          timestamp: "2026-08-14T09:00:00.000Z",
          message: { content: "好的" },
        }),
      ].join("\n"),
    );
    await utimes(
      transcriptPath,
      new Date("2026-08-14T09:00:00.000Z"),
      new Date("2026-08-14T09:00:00.000Z"),
    );
    const port = await availablePort();
    await startServer(home, port);

    const response = await fetch(
      `http://127.0.0.1:${port}/sessions?cwd=${encodeURIComponent(cwd)}`,
    );
    expect(response.ok).toBe(true);
    await expect(response.json()).resolves.toEqual([
      {
        id: "session-a",
        cwd,
        title: "请检查远端项目",
        updatedAt: "2026-08-14T09:00:00.000Z",
      },
    ]);

    const otherResponse = await fetch(
      `http://127.0.0.1:${port}/sessions?cwd=${encodeURIComponent(otherCwd)}`,
    );
    await expect(otherResponse.json()).resolves.toEqual([]);
  });
});
