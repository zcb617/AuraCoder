import { afterEach, describe, expect, it } from "vitest";

const { once } = await import("no" + "de:events");
const { mkdtemp, mkdir, readFile, rm, writeFile } = await import("no" + "de:fs/promises");
const { createServer } = await import("no" + "de:net");
const { tmpdir } = await import("no" + "de:os");
const { default: path } = await import("no" + "de:path");
const { spawn } = await import("no" + "de:child_process");
const { fileURLToPath } = await import("no" + "de:url");

const testPath = fileURLToPath(import.meta.url);
const repoRoot = path.resolve(path.dirname(testPath), "..");
const serverScript = path.join(
  repoRoot,
  "src-tauri",
  "sidecar",
  "claude-remote-session-server.mjs",
);
const distScript = path.join(
  repoRoot,
  "src-tauri",
  "sidecar-dist",
  "claude-remote-session-server.mjs",
);
const children = [];
const tempRoots = [];

async function availablePort() {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const address = server.address();
  server.close();
  return address.port;
}

async function startServer(home) {
  const port = await availablePort();
  const child = spawn(process.execPath, [serverScript, "--port", String(port)], {
    env: {
      ...process.env,
      HOME: home,
      USERPROFILE: home,
    },
    stdio: "ignore",
  });
  children.push(child);
  for (let attempt = 0; attempt < 40; attempt += 1) {
    try {
      await fetch(`http://127.0.0.1:${port}/health`);
      return { child, port };
    } catch {
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
  throw new Error("Claude SSH remote session server did not become available");
}

function projectDirectoryName(cwd) {
  return path.posix.resolve(cwd).replace(/[^a-zA-Z0-9-]/g, "-");
}

async function createSessionFile(home, cwd, sessionId, content = "检查远端项目") {
  const directory = path.join(home, ".claude", "projects", projectDirectoryName(cwd));
  await mkdir(directory, { recursive: true });
  await writeFile(
    path.join(directory, `${sessionId}.jsonl`),
    [
      JSON.stringify({ cwd, type: "system" }),
      JSON.stringify({ type: "user", message: { content } }),
    ].join("\n"),
    "utf8",
  );
}

afterEach(async () => {
  for (const child of children.splice(0)) {
    child.kill();
  }
  await Promise.all(tempRoots.splice(0).map((root) => rm(root, { recursive: true, force: true })));
});

describe("Claude remote session lookup by ID", () => {
  it("returns the target session summary without a cwd parameter", async () => {
    const home = await mkdtemp(path.join(tmpdir(), "auracoder-claude-session-by-id-"));
    tempRoots.push(home);
    const cwd = "/work/project-by-id";
    const sessionId = "11111111-1111-4111-8111-111111111111";
    await createSessionFile(home, cwd, sessionId);
    const { port } = await startServer(home);

    const response = await fetch(`http://127.0.0.1:${port}/sessions/${sessionId}`);
    expect(response.status).toBe(200);
    await expect(response.json()).resolves.toMatchObject({
      id: sessionId,
      sessionId,
      cwd,
      title: "检查远端项目",
    });
  });

  it("returns 404 for a valid but missing session ID", async () => {
    const home = await mkdtemp(path.join(tmpdir(), "auracoder-claude-session-by-id-"));
    tempRoots.push(home);
    const { port } = await startServer(home);
    const sessionId = "22222222-2222-4222-8222-222222222222";

    const response = await fetch(`http://127.0.0.1:${port}/sessions/${sessionId}`);
    expect(response.status).toBe(404);
    await expect(response.json()).resolves.toMatchObject({ error: expect.stringContaining(sessionId) });
  });

  it.each(["", "not-a-session-id", "../secret", "%2Fsecret", "%E0%A4%A"]) (
    "returns 400 for an invalid session ID: %s",
    async (sessionId) => {
      const home = await mkdtemp(path.join(tmpdir(), "auracoder-claude-session-by-id-"));
      tempRoots.push(home);
      const { port } = await startServer(home);
      const encoded = encodeURIComponent(sessionId);
      const response = await fetch(`http://127.0.0.1:${port}/sessions/${encoded}`);
      expect(response.status).toBe(400);
    },
  );

  it("returns 409 when the same ID exists in multiple Claude project directories", async () => {
    const home = await mkdtemp(path.join(tmpdir(), "auracoder-claude-session-by-id-"));
    tempRoots.push(home);
    const sessionId = "33333333-3333-4333-8333-333333333333";
    await createSessionFile(home, "/work/project-one", sessionId);
    await createSessionFile(home, "/work/project-two", sessionId);
    const { port } = await startServer(home);

    const response = await fetch(`http://127.0.0.1:${port}/sessions/${sessionId}`);
    expect(response.status).toBe(409);
    await expect(response.json()).resolves.toMatchObject({ error: expect.stringContaining(sessionId) });
  });

  it("keeps the cwd-filtered list response unchanged", async () => {
    const home = await mkdtemp(path.join(tmpdir(), "auracoder-claude-session-by-id-"));
    tempRoots.push(home);
    const cwd = "/work/project-list";
    const sessionId = "44444444-4444-4444-8444-444444444444";
    await createSessionFile(home, cwd, sessionId);
    const { port } = await startServer(home);

    const response = await fetch(
      `http://127.0.0.1:${port}/sessions?cwd=${encodeURIComponent(cwd)}`,
    );
    expect(response.status).toBe(200);
    const sessions = await response.json();
    expect(sessions).toHaveLength(1);
    expect(sessions[0]).toMatchObject({ id: sessionId, cwd, title: "检查远端项目" });
  });

  it("keeps the sidecar distribution copy byte-identical", async () => {
    await expect(readFile(serverScript)).resolves.toEqual(await readFile(distScript));
  });
});
