import { afterEach, describe, expect, it } from "vitest";
import { ChildProcessWithoutNullStreams, spawn } from "node:child_process";
import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { fileURLToPath, pathToFileURL } from "node:url";
import path from "node:path";
import { createInterface } from "node:readline";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";

type SidecarEvent = Record<string, unknown>;

const testFilePath = fileURLToPath(import.meta.url);
const testDir = path.dirname(testFilePath);
const repoRoot = path.resolve(testDir, "..");
const itWithUnixSignals = process.platform === "win32" ? it.skip : it;
const sidecarScriptPath = path.join(
  repoRoot,
  "src-tauri",
  "sidecar",
  "claude-agent-sdk-server.mjs",
);
const mockSdkModulePath = pathToFileURL(
  path.join(repoRoot, "tests", "fixtures", "claude-agent-sdk-mock.mjs"),
).href;
void [mkdtemp, rm, writeFile, tmpdir];

class SidecarHarness {
  readonly child: ChildProcessWithoutNullStreams;
  readonly events: SidecarEvent[] = [];

  private stderr = "";
  private waiters: Array<{
    predicate: (event: SidecarEvent) => boolean;
    resolve: (event: SidecarEvent) => void;
    reject: (error: Error) => void;
    timer: ReturnType<typeof setTimeout>;
  }> = [];

  constructor(scenario: unknown, env: Record<string, string> = {}) {
    this.child = spawn(process.execPath, [sidecarScriptPath], {
      cwd: repoRoot,
      env: {
        ...process.env,
        CLAUDE_AGENT_SDK_MODULE: mockSdkModulePath,
        CLAUDE_AGENT_SDK_MOCK_SCENARIO: JSON.stringify(scenario),
        PANES_DISABLE_CLAUDE_USAGE_FETCH: "1",
        ...env,
      },
      stdio: ["pipe", "pipe", "pipe"],
    });

    createInterface({
      input: this.child.stdout,
      crlfDelay: Infinity,
    }).on("line", (line) => {
      const event = JSON.parse(line) as SidecarEvent;
      this.events.push(event);
      this.resolveWaiters(event);
    });

    createInterface({
      input: this.child.stderr,
      crlfDelay: Infinity,
    }).on("line", (line) => {
      this.stderr += `${line}\n`;
    });

    this.child.once("exit", (code, signal) => {
      const error = new Error(
        `Claude sidecar exited before the test finished (code=${code}, signal=${signal}). stderr:\n${this.stderr}`,
      );
      for (const waiter of this.waiters.splice(0)) {
        clearTimeout(waiter.timer);
        waiter.reject(error);
      }
    });
  }

  send(payload: Record<string, unknown>) {
    this.child.stdin.write(`${JSON.stringify(payload)}\n`);
  }

  waitFor(
    predicate: (event: SidecarEvent) => boolean,
    timeoutMs = 5_000,
  ): Promise<SidecarEvent> {
    const existing = this.events.find(predicate);
    if (existing) {
      return Promise.resolve(existing);
    }

    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.waiters = this.waiters.filter((waiter) => waiter.timer !== timer);
        reject(
          new Error(
            `Timed out waiting for sidecar event.\nCaptured events:\n${JSON.stringify(this.events, null, 2)}\nStderr:\n${this.stderr}`,
          ),
        );
      }, timeoutMs);

      this.waiters.push({
        predicate,
        resolve,
        reject,
        timer,
      });
    });
  }

  async close() {
    if (this.child.exitCode != null || this.child.killed) {
      return;
    }

    this.child.kill();
    await new Promise<void>((resolve) => {
      this.child.once("exit", () => resolve());
      setTimeout(resolve, 1_000);
    });
  }

  private resolveWaiters(event: SidecarEvent) {
    const remainingWaiters = [];
    for (const waiter of this.waiters) {
      if (!waiter.predicate(event)) {
        remainingWaiters.push(waiter);
        continue;
      }

      clearTimeout(waiter.timer);
      waiter.resolve(event);
    }
    this.waiters = remainingWaiters;
  }
}

function makeSuccessResult(
  partial: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    type: "result",
    subtype: "success",
    is_error: false,
    duration_ms: 0,
    duration_api_ms: 0,
    num_turns: 1,
    result: "",
    stop_reason: null,
    total_cost_usd: 0,
    usage: {},
    modelUsage: {},
    session_id: "mock-session",
    ...partial,
  };
}

function makeErrorResult(
  partial: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    type: "result",
    subtype: "error_during_execution",
    is_error: true,
    duration_ms: 0,
    duration_api_ms: 0,
    num_turns: 1,
    stop_reason: null,
    total_cost_usd: 0,
    usage: {},
    modelUsage: {},
    permission_denials: [],
    errors: ["Claude query failed."],
    session_id: "mock-session",
    ...partial,
  };
}

let activeHarness: SidecarHarness | null = null;

async function spawnHarness(scenario: unknown, env: Record<string, string> = {}) {
  activeHarness = new SidecarHarness(scenario, env);
  await activeHarness.waitFor((event) => event.type === "ready");
  return activeHarness;
}

async function runStartupProbe(scriptPath: string, env: Record<string, string>) {
  const child = spawn(process.execPath, [scriptPath], {
    cwd: path.dirname(scriptPath),
    env: {
      ...process.env,
      PANES_DISABLE_CLAUDE_USAGE_FETCH: "1",
      ...env,
    },
    stdio: ["pipe", "pipe", "pipe"],
  });
  const output = createInterface({ input: child.stdout, crlfDelay: Infinity });
  let resolveFirstEvent: (event: SidecarEvent) => void = () => undefined;
  let rejectFirstEvent: (error: Error) => void = () => undefined;
  const firstEvent = new Promise<SidecarEvent>((resolve, reject) => {
    resolveFirstEvent = resolve;
    rejectFirstEvent = reject;
  });
  const exit = new Promise<{ code: number | null; signal: string | null }>((resolve) => {
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  output.once("line", (line) => {
    try {
      resolveFirstEvent(JSON.parse(line) as SidecarEvent);
    } catch (error) {
      rejectFirstEvent(error instanceof Error ? error : new Error(String(error)));
    }
  });
  child.once("error", rejectFirstEvent);

  const timeout = setTimeout(() => {
    rejectFirstEvent(new Error("Timed out waiting for startup dependency error."));
  }, 5_000);
  try {
    const event = await firstEvent;
    const status = await exit;
    return { event, ...status };
  } finally {
    clearTimeout(timeout);
    output.close();
    if (child.exitCode == null) {
      child.kill();
      await exit;
    }
  }
}

afterEach(async () => {
  await activeHarness?.close();
  activeHarness = null;
});

function parseObservationResults(harness: SidecarHarness, queryId: string) {
  const textEvent = harness.events.find(
    (event) => event.id === queryId && event.type === "text_delta",
  );
  return JSON.parse(String(textEvent?.content ?? "[]")) as Array<{
    type: string;
    result: Record<string, unknown>;
  }>;
}

describe("claude-agent-sdk-server sidecar", () => {
  it("emits a structured zod/v4 startup error before exiting", async () => {
    const tempDir = await mkdtemp(path.join(tmpdir(), "auracoder-claude-startup-"));
    try {
      const source = await (await import("fs/promises")).readFile(sidecarScriptPath, "utf8");
      const copiedScriptPath = path.join(tempDir, "claude-agent-sdk-server.mjs");
      await writeFile(copiedScriptPath, source, "utf8");

      const result = await runStartupProbe(copiedScriptPath, {
        CLAUDE_AGENT_SDK_MODULE: "",
      });

      expect(result.code).not.toBe(0);
      expect(result.event).toMatchObject({
        type: "error",
        recoverable: false,
        errorType: "startup_dependency_load_failed",
        isAuthError: false,
      });
      expect(String(result.event.message)).toContain("zod/v4");
    } finally {
      await rm(tempDir, { recursive: true, force: true });
    }
  });

  it("emits a structured SDK startup error with the requested module identifier", async () => {
    const missingModule = "@anthropic-ai/claude-agent-sdk-module-missing-for-test";
    const result = await runStartupProbe(sidecarScriptPath, {
      CLAUDE_AGENT_SDK_MODULE: missingModule,
    });

    expect(result.code).not.toBe(0);
    expect(result.event).toMatchObject({
      type: "error",
      recoverable: false,
      errorType: "startup_dependency_load_failed",
      isAuthError: false,
    });
    expect(String(result.event.message)).toContain(missingModule);
  });

  it("discovers the model catalog from the selected Claude runtime", async () => {
    const harness = await spawnHarness(
      {
        models: [
          {
            value: "claude-fable-5[1m]",
            resolvedModel: "claude-fable-5",
            displayName: "Fable",
            description: "Fable 5",
            supportsEffort: true,
            supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"],
          },
        ],
        expectedSupportedModelsSettingSources: ["user"],
      },
      { PANES_CLAUDE_CODE_EXECUTABLE: "/tmp/claude-current" },
    );

    harness.send({
      id: "models-current",
      method: "list_models",
      params: { cwd: repoRoot },
    });

    const event = await harness.waitFor(
      (candidate) => candidate.id === "models-current" && candidate.type === "models",
    );

    expect(event).toMatchObject({
      runtimeSource: "system",
      runtimeExecutable: "/tmp/claude-current",
      models: [
        {
          value: "claude-fable-5[1m]",
          displayName: "Fable",
          supportedEffortLevels: ["low", "medium", "high", "xhigh", "max"],
        },
      ],
    });
  });

  it("keeps a Claude session handle alive until the remote component destroys it", async () => {
    const harness = await spawnHarness({
      persistentInput: true,
      sessionId: "persistent-session",
    });

    harness.send({
      id: "create-persistent-session",
      method: "create_session_handle",
      params: {
        threadId: "thread-persistent",
        handleId: "handle-persistent",
        prompt: "first message",
        cwd: repoRoot,
      },
    });

    await expect(
      harness.waitFor(
        (event) =>
          event.id === "create-persistent-session" &&
          event.type === "session_handle_created",
      ),
    ).resolves.toMatchObject({
      threadId: "thread-persistent",
      handleId: "handle-persistent",
      reused: false,
    });
    await harness.waitFor(
      (event) =>
        event.id === "create-persistent-session" && event.type === "turn_completed",
    );

    harness.send({
      id: "reuse-persistent-session",
      method: "create_session_handle",
      params: {
        threadId: "thread-persistent",
        handleId: "handle-must-not-replace-existing",
        prompt: "unused message",
        cwd: repoRoot,
      },
    });
    await expect(
      harness.waitFor(
        (event) =>
          event.id === "reuse-persistent-session" &&
          event.type === "session_handle_created",
      ),
    ).resolves.toMatchObject({
      threadId: "thread-persistent",
      handleId: "handle-persistent",
      reused: true,
    });

    harness.send({
      id: "send-persistent-session-message",
      method: "send_session_message",
      params: {
        threadId: "thread-persistent",
        prompt: "second message",
        cwd: repoRoot,
      },
    });
    await expect(
      harness.waitFor(
        (event) =>
          event.id === "send-persistent-session-message" &&
          event.type === "session_message_accepted",
      ),
    ).resolves.toMatchObject({
      threadId: "thread-persistent",
      handleId: "handle-persistent",
      accepted: true,
    });
    await expect(
      harness.waitFor(
        (event) =>
          event.id === "create-persistent-session" &&
          event.type === "text_delta" &&
          event.content === "second message",
      ),
    ).resolves.toMatchObject({ content: "second message" });

    harness.send({
      id: "destroy-persistent-session",
      method: "destroy_session_handle",
      params: { threadId: "thread-persistent" },
    });

    await expect(
      harness.waitFor(
        (event) =>
          event.id === "destroy-persistent-session" &&
          event.type === "session_handle_destroyed",
      ),
    ).resolves.toMatchObject({
      threadId: "thread-persistent",
      handleId: "handle-persistent",
      success: true,
    });
  });

  it("updates the model and effort before sending the next persistent-session message", async () => {
    const harness = await spawnHarness({
      persistentInput: true,
      emitPersistentRuntimeState: true,
      sessionId: "persistent-runtime-session",
    });

    harness.send({
      id: "create-runtime-session",
      method: "create_session_handle",
      params: {
        threadId: "thread-runtime",
        handleId: "handle-runtime",
        prompt: "first message",
        cwd: repoRoot,
        model: "model-a",
        reasoningEffort: "high",
      },
    });
    await harness.waitFor(
      (event) => event.id === "create-runtime-session" && event.type === "session_handle_created",
    );
    await harness.waitFor(
      (event) => event.id === "create-runtime-session" && event.type === "turn_completed",
    );

    harness.send({
      id: "send-runtime-session",
      method: "send_session_message",
      params: {
        threadId: "thread-runtime",
        prompt: "second message",
        cwd: repoRoot,
        model: "model-b",
        reasoningEffort: "low",
      },
    });
    await harness.waitFor(
      (event) => event.id === "send-runtime-session" && event.type === "session_message_accepted",
    );
    const secondText = await harness.waitFor(
      (event) => {
        if (event.id !== "create-runtime-session" || event.type !== "text_delta") {
          return false;
        }
        try {
          const state = JSON.parse(String(event.content)) as { text?: string };
          return state.text === "second message";
        } catch {
          return false;
        }
      },
    );
    const state = JSON.parse(String(secondText.content)) as {
      text: string;
      currentModel: string | null;
      currentEffort: string | null;
      runtimeControlCalls: Array<{ type: string; value: string | null }>;
    };

    expect(state).toMatchObject({
      text: "second message",
      currentModel: "model-b",
      currentEffort: "low",
    });
    expect(state.runtimeControlCalls).toEqual([
      { type: "set_model", value: "model-b" },
      { type: "apply_flag_settings", value: "low" },
    ]);
  });

  it("clears the previous persistent-session effort when the next value is None", async () => {
    const harness = await spawnHarness({
      persistentInput: true,
      emitPersistentRuntimeState: true,
      sessionId: "persistent-clear-effort-session",
    });

    harness.send({
      id: "create-clear-effort-session",
      method: "create_session_handle",
      params: {
        threadId: "thread-clear-effort",
        handleId: "handle-clear-effort",
        prompt: "first message",
        cwd: repoRoot,
        model: "model-a",
        reasoningEffort: "high",
      },
    });
    await harness.waitFor(
      (event) => event.id === "create-clear-effort-session" && event.type === "session_handle_created",
    );
    await harness.waitFor(
      (event) => event.id === "create-clear-effort-session" && event.type === "turn_completed",
    );

    harness.send({
      id: "send-clear-effort-session",
      method: "send_session_message",
      params: {
        threadId: "thread-clear-effort",
        prompt: "second message",
        cwd: repoRoot,
        model: "model-a",
        reasoningEffort: null,
      },
    });
    await harness.waitFor(
      (event) => event.id === "send-clear-effort-session" && event.type === "session_message_accepted",
    );
    const secondText = await harness.waitFor(
      (event) => {
        if (event.id !== "create-clear-effort-session" || event.type !== "text_delta") {
          return false;
        }
        try {
          const state = JSON.parse(String(event.content)) as { text?: string };
          return state.text === "second message";
        } catch {
          return false;
        }
      },
    );
    const state = JSON.parse(String(secondText.content)) as {
      currentEffort: string | null;
      runtimeControlCalls: Array<{ type: string; value: string | null }>;
    };

    expect(state.currentEffort).toBeNull();
    expect(state.runtimeControlCalls).toEqual([
      { type: "set_model", value: "model-a" },
      { type: "apply_flag_settings", value: null },
    ]);
  });

  it("does not accept a persistent message when setModel fails", async () => {
    const harness = await spawnHarness({
      persistentInput: true,
      failSetModel: true,
      sessionId: "persistent-set-model-failure-session",
    });

    harness.send({
      id: "create-set-model-failure-session",
      method: "create_session_handle",
      params: {
        threadId: "thread-set-model-failure",
        handleId: "handle-set-model-failure",
        prompt: "first message",
        cwd: repoRoot,
      },
    });
    await harness.waitFor(
      (event) => event.id === "create-set-model-failure-session" && event.type === "session_handle_created",
    );
    await harness.waitFor(
      (event) => event.id === "create-set-model-failure-session" && event.type === "turn_completed",
    );
    const eventCountBeforeSend = harness.events.length;

    harness.send({
      id: "send-set-model-failure-session",
      method: "send_session_message",
      params: {
        threadId: "thread-set-model-failure",
        prompt: "second message",
        cwd: repoRoot,
        model: "model-b",
        reasoningEffort: "low",
      },
    });
    await expect(
      harness.waitFor(
        (event) => event.id === "send-set-model-failure-session" && event.type === "error",
      ),
    ).resolves.toMatchObject({ message: "Mock Claude query setModel failed." });

    const newEvents = harness.events.slice(eventCountBeforeSend);
    expect(newEvents.some((event) => event.type === "session_message_accepted")).toBe(false);
    expect(newEvents.some((event) => event.type === "text_delta")).toBe(false);
  });

  it("does not accept a persistent message when applyFlagSettings fails", async () => {
    const harness = await spawnHarness({
      persistentInput: true,
      failApplyFlagSettings: true,
      sessionId: "persistent-effort-failure-session",
    });

    harness.send({
      id: "create-effort-failure-session",
      method: "create_session_handle",
      params: {
        threadId: "thread-effort-failure",
        handleId: "handle-effort-failure",
        prompt: "first message",
        cwd: repoRoot,
      },
    });
    await harness.waitFor(
      (event) => event.id === "create-effort-failure-session" && event.type === "session_handle_created",
    );
    await harness.waitFor(
      (event) => event.id === "create-effort-failure-session" && event.type === "turn_completed",
    );
    const eventCountBeforeSend = harness.events.length;

    harness.send({
      id: "send-effort-failure-session",
      method: "send_session_message",
      params: {
        threadId: "thread-effort-failure",
        prompt: "second message",
        cwd: repoRoot,
        model: "model-b",
        reasoningEffort: "low",
      },
    });
    await expect(
      harness.waitFor(
        (event) => event.id === "send-effort-failure-session" && event.type === "error",
      ),
    ).resolves.toMatchObject({ message: "Mock Claude query applyFlagSettings failed." });

    const newEvents = harness.events.slice(eventCountBeforeSend);
    expect(newEvents.some((event) => event.type === "session_message_accepted")).toBe(false);
    expect(newEvents.some((event) => event.type === "text_delta")).toBe(false);
  });

  it("denies Write in read-only mode even when writableRoots are present", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "permission",
          toolName: "Write",
          input: { file_path: path.join(repoRoot, "allowed.txt") },
          toolUseID: "write-read-only",
        },
      ],
      emitObservationResult: true,
      sessionId: "session-read-only",
    });

    harness.send({
      id: "query-read-only",
      method: "query",
      params: {
        prompt: "attempt write",
        cwd: repoRoot,
        sandboxMode: "read-only",
        writableRoots: [repoRoot],
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-read-only" && event.type === "turn_completed",
    );

    const observations = parseObservationResults(harness, "query-read-only");
    expect(observations).toHaveLength(1);
    expect(observations[0]?.result.behavior).toBe("deny");
    expect(observations[0]?.result.message).toBe("File writes are disabled for this Claude thread.");
  });

  it("workspace-write allows approved roots and denies paths outside them", async () => {
    const outsidePath = path.join(path.dirname(repoRoot), "outside.txt");
    const harness = await spawnHarness({
      steps: [
        {
          type: "permission",
          toolName: "Write",
          input: { file_path: path.join(repoRoot, "inside.txt") },
          toolUseID: "write-inside",
        },
        {
          type: "permission",
          toolName: "Write",
          input: { file_path: outsidePath },
          toolUseID: "write-outside",
        },
      ],
      emitObservationResult: true,
      sessionId: "session-workspace-write",
    });

    harness.send({
      id: "query-workspace-write",
      method: "query",
      params: {
        prompt: "attempt writes",
        cwd: repoRoot,
        approvalPolicy: "trusted",
        sandboxMode: "workspace-write",
        writableRoots: [repoRoot],
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-workspace-write" && event.type === "turn_completed",
    );

    const observations = parseObservationResults(harness, "query-workspace-write");
    expect(observations).toHaveLength(2);
    expect(observations[0]?.result.behavior).toBe("allow");
    expect(observations[1]?.result.behavior).toBe("deny");
    expect(observations[1]?.result.message).toBe(
      "This file path is outside the approved writable roots for the thread.",
    );
  });

  it("defaults workspace-write roots to cwd when writableRoots are omitted", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "permission",
          toolName: "Write",
          input: { file_path: path.join(repoRoot, "inside-default-root.txt") },
          toolUseID: "write-default-root",
        },
      ],
      emitObservationResult: true,
      sessionId: "session-default-root",
    });

    harness.send({
      id: "query-default-root",
      method: "query",
      params: {
        prompt: "attempt write",
        cwd: repoRoot,
        approvalPolicy: "trusted",
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-default-root" && event.type === "turn_completed",
    );

    const observations = parseObservationResults(harness, "query-default-root");
    expect(observations).toHaveLength(1);
    expect(observations[0]?.result.behavior).toBe("allow");
  });

  it("uses interactive default permission mode for non-plan queries", async () => {
    const harness = await spawnHarness({
      steps: [],
      emitObservationResult: true,
      emitQueryOptions: true,
      sessionId: "session-permission-mode",
    });

    harness.send({
      id: "query-permission-mode",
      method: "query",
      params: {
        prompt: "inspect options",
        cwd: repoRoot,
      },
    });

    await harness.waitFor(
      (event) =>
        event.id === "query-permission-mode" && event.type === "turn_completed",
    );

    const observations = parseObservationResults(harness, "query-permission-mode");
    expect(observations).toHaveLength(1);
    expect(observations[0]?.type).toBe("query_options");
    expect(observations[0]?.result.permissionMode).toBe("default");
    expect(observations[0]?.result.settings).toEqual({
      permissions: {
        defaultMode: "default",
        disableBypassPermissionsMode: "disable",
      },
    });
    expect(observations[0]?.result.settingSources).toEqual(["user", "project"]);
    expect(observations[0]?.result.sandbox).toEqual({
      enabled: true,
      failIfUnavailable: process.platform !== "win32",
      autoAllowBashIfSandboxed: true,
      allowUnsandboxedCommands: false,
      filesystem: {
        allowWrite: [repoRoot],
      },
      network: {
        allowedDomains: [],
        allowLocalBinding: false,
        allowUnixSockets: [],
      },
    });
  });

  it("keeps only supported values when settingSources is explicit", async () => {
    const harness = await spawnHarness({
      steps: [],
      emitObservationResult: true,
      emitQueryOptions: true,
    });

    harness.send({
      id: "query-explicit-setting-sources",
      method: "query",
      params: {
        prompt: "inspect explicit setting sources",
        cwd: repoRoot,
        settingSources: ["local", "invalid", "user", "project-invalid"],
      },
    });

    await harness.waitFor(
      (event) =>
        event.id === "query-explicit-setting-sources" && event.type === "turn_completed",
    );

    const observations = parseObservationResults(harness, "query-explicit-setting-sources");
    expect(observations).toHaveLength(1);
    expect(observations[0]?.type).toBe("query_options");
    expect(observations[0]?.result.settingSources).toEqual(["local", "user"]);
  });

  it("registers AuraCoder computer control as an in-process SDK tool server", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "computer_control_tool",
          toolName: "click",
          input: { pid: 1234, x: 10, y: 20 },
          callId: "claude-computer-call-1",
        },
      ],
      emitObservationResult: true,
      emitQueryOptions: true,
    });

    harness.send({
      id: "query-computer-control-sdk",
      method: "query",
      params: {
        prompt: "use computer control",
        cwd: repoRoot,
        threadId: "thread-computer-control",
        computerControlTools: [
          {
            name: "click",
            description: "点击指定应用窗口",
            inputSchema: {
              type: "object",
              properties: {
                pid: { type: "integer" },
                x: { type: "integer" },
                y: { type: "integer" },
              },
              required: ["pid", "x", "y"],
            },
          },
        ],
      },
    });

    const call = await harness.waitFor(
      (event) => event.type === "computer_control_tool_call",
    );
    expect(call).toMatchObject({
      id: "query-computer-control-sdk",
      callId: "claude-computer-call-1",
      toolName: "click",
      threadId: "thread-computer-control",
      turnId: "query-computer-control-sdk",
      arguments: { pid: 1234, x: 10, y: 20 },
    });

    harness.send({
      method: "computer_control_tool_result",
      params: {
        requestId: "query-computer-control-sdk",
        callId: "claude-computer-call-1",
        result: { content: [{ type: "text", text: "click completed" }] },
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-computer-control-sdk" && event.type === "turn_completed",
    );

    const observations = parseObservationResults(harness, "query-computer-control-sdk");
    expect(observations.some((item) => item.type === "computer_control_result")).toBe(true);
    const options = observations.find((item) => item.type === "query_options");
    expect(options?.result.allowedTools).toContain("mcp__auracoder-computer-control__*");
  });

  it("registers AuraCoder thread tools as an in-process SDK tool server", async () => {
    const harness = await spawnHarness({
      steps: [],
      emitObservationResult: true,
      emitQueryOptions: true,
    });

    harness.send({
      id: "query-auracoder-thread-sdk",
      method: "query",
      params: {
        prompt: "read auracoder thread",
        cwd: repoRoot,
        threadId: "thread-auracoder-thread",
        auracoderThreadTools: [
          {
            name: "get_auracoder_thread_message_count",
            description: "获取指定 AuraCoder 会话的消息总行数。回答前必须先使用此工具确定分页范围。",
            inputSchema: {
              type: "object",
              properties: {
                thread_id: { type: "string", description: "AuraCoder 会话 ID" },
              },
              required: ["thread_id"],
              additionalProperties: false,
            },
          },
          {
            name: "get_auracoder_thread_messages_page",
            description: "按创建时间倒序分页读取指定 AuraCoder 会话消息。page 和 page_size 从 1 开始。",
            inputSchema: {
              type: "object",
              properties: {
                thread_id: { type: "string", description: "AuraCoder 会话 ID" },
                page: { type: "integer", minimum: 1, description: "页码，从 1 开始" },
                page_size: { type: "integer", minimum: 1, description: "每页条数，从 1 开始" },
              },
              required: ["thread_id", "page", "page_size"],
              additionalProperties: false,
            },
          },
        ],
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-auracoder-thread-sdk" && event.type === "turn_completed",
    );

    const observations = parseObservationResults(harness, "query-auracoder-thread-sdk");
    const options = observations.find((item) => item.type === "query_options");
    expect(options?.result.allowedTools).toContain("mcp__auracoder-thread__*");
    expect(options?.result.mcpServers).toEqual({
      "auracoder-thread": {
        name: "auracoder-thread",
        version: "1.0.0",
        tools: [
          "get_auracoder_thread_message_count",
          "get_auracoder_thread_messages_page",
        ],
      },
    });
  });

  it("rejects danger-full-access explicitly for Claude", async () => {
    const harness = await spawnHarness({ steps: [] });

    harness.send({
      id: "query-full-access",
      method: "query",
      params: {
        prompt: "invalid sandbox",
        cwd: repoRoot,
        sandboxMode: "danger-full-access",
      },
    });

    const errorEvent = await harness.waitFor(
      (event) => event.id === "query-full-access" && event.type === "error",
    );
    const completed = await harness.waitFor(
      (event) => event.id === "query-full-access" && event.type === "turn_completed",
    );

    expect(errorEvent.message).toContain("does not support sandboxMode=danger-full-access");
    expect(completed.status).toBe("failed");
  });

  it("marks terminal SDK errors as failed turns", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "yield",
          message: {
            type: "system",
            subtype: "init",
            session_id: "session-error",
          },
        },
        {
          type: "yield",
          message: makeErrorResult({
            session_id: "session-error",
            errors: ["tool execution exploded", "budget exceeded"],
          }),
        },
      ],
    });

    harness.send({
      id: "query-error",
      method: "query",
      params: {
        prompt: "run failing scenario",
        cwd: repoRoot,
      },
    });

    const completed = await harness.waitFor(
      (event) => event.id === "query-error" && event.type === "turn_completed",
    );
    const errorEvent = harness.events.find(
      (event) => event.id === "query-error" && event.type === "error",
    );

    expect(errorEvent?.message).toBe("tool execution exploded\nbudget exceeded");
    expect(completed.status).toBe("failed");
    expect(completed.sessionId).toBe("session-error");
  });

  it("surfaces assistant errors, status notices, rate limits, and token usage", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "yield",
          message: {
            type: "system",
            subtype: "init",
            session_id: "session-events",
          },
        },
        {
          type: "yield",
          message: {
            type: "assistant",
            error: "authentication_failed",
            session_id: "session-events",
          },
        },
        {
          type: "yield",
          message: {
            type: "system",
            subtype: "status",
            status: "compacting",
            session_id: "session-events",
          },
        },
        {
          type: "yield",
          message: {
            type: "rate_limit_event",
            session_id: "session-events",
            rate_limit_info: {
              rateLimitType: "five_hour",
              utilization: 0.87,
              resetsAt: 1_740_000_000,
            },
          },
        },
        {
          type: "yield",
          message: {
            type: "stream_event",
            session_id: "session-events",
            event: {
              type: "message_start",
              message: {
                usage: {
                  input_tokens: 11,
                  output_tokens: 2,
                },
              },
            },
          },
        },
        {
          type: "yield",
          message: {
            type: "stream_event",
            session_id: "session-events",
            event: {
              type: "message_delta",
              delta: {
                stop_reason: "end_turn",
              },
              usage: {
                output_tokens: 7,
              },
            },
          },
        },
        {
          type: "yield",
          message: makeSuccessResult({
            session_id: "session-events",
            usage: {
              input_tokens: 11,
              output_tokens: 7,
            },
          }),
        },
      ],
    });

    harness.send({
      id: "query-events",
      method: "query",
      params: {
        prompt: "surface events",
        cwd: repoRoot,
      },
    });

    const completed = await harness.waitFor(
      (event) => event.id === "query-events" && event.type === "turn_completed",
    );
    const errorEvent = harness.events.find(
      (event) => event.id === "query-events" && event.type === "error",
    );
    const noticeEvent = harness.events.find(
      (event) => event.id === "query-events" && event.type === "notice",
    );
    const usageEvent = harness.events.find(
      (event) => event.id === "query-events" && event.type === "usage_limits_updated",
    );

    expect(errorEvent).toMatchObject({
      message: "Claude authentication failed. Sign in again or refresh your credentials.",
      errorType: "authentication_failed",
      isAuthError: true,
      recoverable: false,
    });
    expect(noticeEvent).toMatchObject({
      kind: "claude_status",
      title: "Claude status",
      message: "Claude is compacting context.",
    });
    expect(usageEvent).toMatchObject({
      usage: {
        fiveHourPercent: 87,
        fiveHourResetsAt: 1_740_000_000,
      },
    });
    expect(completed).toMatchObject({
      status: "failed",
      sessionId: "session-events",
      tokenUsage: {
        input: 11,
        output: 7,
      },
      stopReason: "end_turn",
    });
  });

  it("keeps the Fable weekly limit separate and reports Fable context", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "yield",
          message: {
            type: "rate_limit_event",
            rate_limit_info: {
              rateLimitType: "seven_day",
              utilization: 0.25,
              resetsAt: 1_740_000_000,
            },
          },
        },
        {
          type: "yield",
          message: {
            type: "rate_limit_event",
            rate_limit_info: {
              rateLimitType: "seven_day_overage_included",
              utilization: 0.4,
              resetsAt: 1_740_100_000,
            },
          },
        },
        {
          type: "yield",
          message: {
            type: "stream_event",
            event: {
              type: "message_start",
              message: {
                usage: {
                  input_tokens: 25_000,
                  cache_creation_input_tokens: 5_000,
                  cache_read_input_tokens: 20_000,
                  output_tokens: 0,
                },
              },
            },
          },
        },
        {
          type: "yield",
          message: makeSuccessResult({
            session_id: "session-fable-usage",
            usage: { input_tokens: 25_000, output_tokens: 10 },
          }),
        },
      ],
    });

    harness.send({
      id: "query-fable-usage",
      method: "query",
      params: {
        prompt: "surface Fable usage",
        cwd: repoRoot,
        model: "claude-fable-5[1m]",
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-fable-usage" && event.type === "turn_completed",
    );

    const usageEvents = harness.events.filter(
      (event) => event.id === "query-fable-usage" && event.type === "usage_limits_updated",
    );
    expect(usageEvents).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          usage: expect.objectContaining({
            weeklyPercent: 25,
            fableWeeklyPercent: null,
          }),
        }),
        expect.objectContaining({
          usage: expect.objectContaining({
            weeklyPercent: null,
            fableWeeklyPercent: 40,
            fableWeeklyResetsAt: 1_740_100_000,
          }),
        }),
        expect.objectContaining({
          usage: expect.objectContaining({ contextWindowPercent: 95 }),
        }),
      ]),
    );
  });

  it("loads current Claude usage including the scoped Fable weekly limit", async () => {
    let authorizationHeader = "";
    const usageServer = createServer((request, response) => {
      authorizationHeader = String(request.headers.authorization || "");
      response.writeHead(200, { "content-type": "application/json" });
      response.end(
        JSON.stringify({
          five_hour: {
            utilization: 12,
            resets_at: "2026-07-12T07:30:00Z",
          },
          seven_day: {
            utilization: 46,
            resets_at: "2026-07-13T12:00:00Z",
          },
          limits: [
            {
              kind: "weekly_scoped",
              percent: 76,
              resets_at: "2026-07-13T12:00:00Z",
              scope: { model: { display_name: "Fable" } },
            },
          ],
        }),
      );
    });
    await new Promise<void>((resolve) => usageServer.listen(0, "127.0.0.1", resolve));
    const address = usageServer.address() as AddressInfo;

    try {
      const harness = await spawnHarness(
        { steps: [] },
        {
          CLAUDE_CODE_OAUTH_TOKEN: "test-oauth-token",
          PANES_DISABLE_CLAUDE_USAGE_FETCH: "0",
          PANES_CLAUDE_USAGE_URL: `http://127.0.0.1:${address.port}/api/oauth/usage`,
        },
      );

      harness.send({
        id: "current-usage",
        method: "get_usage_limits",
      });

      const usageEvent = await harness.waitFor(
        (event) =>
          event.id === "current-usage" &&
          event.type === "usage_limits_updated" &&
          (event.usage as Record<string, unknown>)?.fableWeeklyPercent === 76,
      );

      expect(authorizationHeader).toBe("Bearer test-oauth-token");
      expect(usageEvent).toMatchObject({
        usage: {
          fiveHourPercent: 12,
          weeklyPercent: 46,
          fableWeeklyPercent: 76,
          fableWeeklyResetsAt: 1_783_944_000,
        },
      });
    } finally {
      await activeHarness?.close();
      await new Promise<void>((resolve, reject) =>
        usageServer.close((error) => (error ? reject(error) : resolve())),
      );
    }
  });

  it("uses tool_response and emits action output deltas", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "hook",
          hook: "PreToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "printf ok" },
            tool_use_id: "tool-1",
          },
        },
        {
          type: "hook",
          hook: "PostToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "printf ok" },
            tool_use_id: "tool-1",
            tool_response: "stdout: ok",
          },
        },
        {
          type: "yield",
          message: makeSuccessResult({ session_id: "session-tool-output" }),
        },
      ],
    });

    harness.send({
      id: "query-tool-output",
      method: "query",
      params: {
        prompt: "run tool output scenario",
        cwd: repoRoot,
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-tool-output" && event.type === "turn_completed",
    );

    const started = harness.events.find(
      (event) =>
        event.id === "query-tool-output" &&
        event.type === "action_started" &&
        (event.details as Record<string, unknown> | undefined)?.command === "printf ok",
    );
    const outputDelta = harness.events.find(
      (event) =>
        event.id === "query-tool-output" &&
        event.type === "action_output_delta" &&
        event.content === "stdout: ok",
    );
    const completed = harness.events.find(
      (event) =>
        event.id === "query-tool-output" &&
        event.type === "action_completed",
    );

    expect(started?.actionId).toBeDefined();
    expect(outputDelta?.actionId).toBe(started?.actionId);
    expect(outputDelta?.stream).toBe("stdout");
    expect(completed?.actionId).toBe(started?.actionId);
    expect(completed?.output).toBe("stdout: ok");
  });

  it("streams long tool output in chunks without truncation", async () => {
    const longOutput = "x".repeat(10_500);
    const harness = await spawnHarness({
      steps: [
        {
          type: "hook",
          hook: "PreToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "python - <<'PY'" },
            tool_use_id: "tool-long-output",
          },
        },
        {
          type: "hook",
          hook: "PostToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "python - <<'PY'" },
            tool_use_id: "tool-long-output",
            tool_response: longOutput,
          },
        },
        {
          type: "yield",
          message: makeSuccessResult({ session_id: "session-long-output" }),
        },
      ],
    });

    harness.send({
      id: "query-long-output",
      method: "query",
      params: {
        prompt: "stream long output",
        cwd: repoRoot,
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-long-output" && event.type === "turn_completed",
    );

    const chunks = harness.events.filter(
      (event) =>
        event.id === "query-long-output" && event.type === "action_output_delta",
    );
    const completed = harness.events.find(
      (event) =>
        event.id === "query-long-output" && event.type === "action_completed",
    );

    expect(chunks.length).toBeGreaterThan(1);
    expect(chunks.map((event) => String(event.content ?? "")).join("")).toBe(longOutput);
    expect(completed?.output).toBe(longOutput);
  });

  it("returns updatedPermissions for accept_for_session approvals", async () => {
    const suggestions = [
      {
        type: "addRules",
        rules: [{ toolName: "Bash", ruleContent: "npm test" }],
        behavior: "allow",
        destination: "session",
      },
    ];
    const harness = await spawnHarness({
      steps: [
        {
          type: "permission",
          toolName: "Bash",
          input: { command: "npm test" },
          toolUseID: "permission-tool-1",
          options: { suggestions },
        },
      ],
      emitObservationResult: true,
      sessionId: "session-approval",
    });

    harness.send({
      id: "query-approval",
      method: "query",
      params: {
        prompt: "request approval",
        cwd: repoRoot,
        approvalPolicy: "untrusted",
      },
    });

    const approvalEvent = await harness.waitFor(
      (event) => event.id === "query-approval" && event.type === "approval_requested",
    );
    harness.send({
      method: "approval_response",
      params: {
        approvalId: approvalEvent.approvalId,
        response: { decision: "accept_for_session" },
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-approval" && event.type === "turn_completed",
    );

    const textEvent = harness.events.find(
      (event) => event.id === "query-approval" && event.type === "text_delta",
    );
    const observations = JSON.parse(String(textEvent?.content ?? "[]")) as Array<{
      type: string;
      result: Record<string, unknown>;
    }>;

    expect(observations).toHaveLength(1);
    expect(observations[0]?.type).toBe("permission_result");
    expect(observations[0]?.result.behavior).toBe("allow");
    expect(observations[0]?.result.updatedPermissions).toEqual(suggestions);
  });

  it("routes AskUserQuestion approvals through updatedInput answers", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "permission",
          toolName: "AskUserQuestion",
          input: {
            questions: [
              {
                id: "stack",
                header: "Stack",
                question: "Which package manager should we use?",
                options: [
                  { label: "pnpm", description: "Recommended" },
                  { label: "npm", description: "Fallback" },
                ],
                multiSelect: false,
              },
            ],
          },
          toolUseID: "ask-user-question-1",
        },
      ],
      emitObservationResult: true,
      sessionId: "session-ask-user-question",
    });

    harness.send({
      id: "query-ask-user-question",
      method: "query",
      params: {
        prompt: "ask the user a question",
        cwd: repoRoot,
      },
    });

    const approvalEvent = await harness.waitFor(
      (event) =>
        event.id === "query-ask-user-question" &&
        event.type === "approval_requested",
    );
    expect(approvalEvent.details).toEqual({
      _serverMethod: "item/tool/requestuserinput",
      questions: [
        {
          id: "stack",
          header: "Stack",
          question: "Which package manager should we use?",
          options: [
            { label: "pnpm", description: "Recommended" },
            { label: "npm", description: "Fallback" },
          ],
          multiSelect: false,
        },
      ],
    });

    harness.send({
      method: "approval_response",
      params: {
        approvalId: approvalEvent.approvalId,
        response: {
          answers: {
            stack: {
              answers: ["pnpm"],
            },
          },
        },
      },
    });

    await harness.waitFor(
      (event) =>
        event.id === "query-ask-user-question" && event.type === "turn_completed",
    );

    const observations = parseObservationResults(harness, "query-ask-user-question");
    expect(observations).toHaveLength(1);
    expect(observations[0]?.result).toEqual({
      behavior: "allow",
      updatedInput: {
        questions: [
          {
            id: "stack",
            header: "Stack",
            question: "Which package manager should we use?",
            options: [
              { label: "pnpm", description: "Recommended" },
              { label: "npm", description: "Fallback" },
            ],
            multiSelect: false,
          },
        ],
        answers: {
          "Which package manager should we use?": "pnpm",
        },
      },
    });
  });

  it("denies malformed approval payloads instead of hanging the query", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "permission",
          toolName: "Bash",
          input: { command: "npm test" },
          toolUseID: "permission-invalid-approval",
        },
      ],
      emitObservationResult: true,
      sessionId: "session-invalid-approval",
    });

    harness.send({
      id: "query-invalid-approval",
      method: "query",
      params: {
        prompt: "request approval",
        cwd: repoRoot,
        approvalPolicy: "restricted",
      },
    });

    const approvalEvent = await harness.waitFor(
      (event) => event.id === "query-invalid-approval" && event.type === "approval_requested",
    );
    harness.send({
      method: "approval_response",
      params: {
        approvalId: approvalEvent.approvalId,
        response: {},
      },
    });

    const errorEvent = await harness.waitFor(
      (event) => event.id === "query-invalid-approval" && event.type === "error",
    );
    const completed = await harness.waitFor(
      (event) => event.id === "query-invalid-approval" && event.type === "turn_completed",
    );

    expect(errorEvent.message).toContain("explicit decision field");
    expect(completed.status).toBe("completed");

    const observations = parseObservationResults(harness, "query-invalid-approval");
    expect(observations).toHaveLength(1);
    expect(observations[0]?.result).toEqual({
      behavior: "deny",
      message: "Claude approval response was invalid and was denied.",
    });
  });

  it("emits synthetic action completion when a prestarted tool is denied", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "hook",
          hook: "PreToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "npm publish" },
            tool_use_id: "tool-denied",
          },
        },
        {
          type: "permission",
          toolName: "Bash",
          input: { command: "npm publish" },
          toolUseID: "tool-denied",
        },
      ],
      sessionId: "session-denied-tool",
    });

    harness.send({
      id: "query-denied-tool",
      method: "query",
      params: {
        prompt: "deny the tool",
        cwd: repoRoot,
        approvalPolicy: "restricted",
      },
    });

    const approvalEvent = await harness.waitFor(
      (event) =>
        event.id === "query-denied-tool" && event.type === "approval_requested",
    );
    const started = await harness.waitFor(
      (event) =>
        event.id === "query-denied-tool" && event.type === "action_started",
    );

    harness.send({
      method: "approval_response",
      params: {
        approvalId: approvalEvent.approvalId,
        response: { decision: "decline" },
      },
    });

    const completed = await harness.waitFor(
      (event) =>
        event.id === "query-denied-tool" && event.type === "action_completed",
    );

    expect(completed).toMatchObject({
      actionId: started.actionId,
      success: false,
      error: "Tool usage denied by the user.",
    });
  });

  itWithUnixSignals("emits interrupted turn completion before exiting on SIGTERM", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "permission",
          toolName: "Bash",
          input: { command: "npm test" },
          toolUseID: "tool-sigterm",
        },
      ],
      sessionId: "session-sigterm",
    });

    harness.send({
      id: "query-sigterm",
      method: "query",
      params: {
        prompt: "wait for approval",
        cwd: repoRoot,
        approvalPolicy: "restricted",
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-sigterm" && event.type === "approval_requested",
    );

    harness.child.kill("SIGTERM");

    const completed = await harness.waitFor(
      (event) => event.id === "query-sigterm" && event.type === "turn_completed",
    );

    expect(completed.status).toBe("interrupted");
  });

  it("matches tool completions by tool_use_id when hooks interleave", async () => {
    const harness = await spawnHarness({
      steps: [
        {
          type: "hook",
          hook: "PreToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "echo first" },
            tool_use_id: "tool-first",
          },
        },
        {
          type: "hook",
          hook: "PreToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "echo second" },
            tool_use_id: "tool-second",
          },
        },
        {
          type: "hook",
          hook: "PostToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "echo first" },
            tool_use_id: "tool-first",
            tool_response: "first output",
          },
        },
        {
          type: "hook",
          hook: "PostToolUse",
          input: {
            tool_name: "Bash",
            tool_input: { command: "echo second" },
            tool_use_id: "tool-second",
            tool_response: "second output",
          },
        },
        {
          type: "yield",
          message: makeSuccessResult({ session_id: "session-interleaving" }),
        },
      ],
    });

    harness.send({
      id: "query-interleaving",
      method: "query",
      params: {
        prompt: "run interleaved hooks",
        cwd: repoRoot,
      },
    });

    await harness.waitFor(
      (event) => event.id === "query-interleaving" && event.type === "turn_completed",
    );

    const firstStart = harness.events.find(
      (event) =>
        event.id === "query-interleaving" &&
        event.type === "action_started" &&
        (event.details as Record<string, unknown> | undefined)?.command === "echo first",
    );
    const secondStart = harness.events.find(
      (event) =>
        event.id === "query-interleaving" &&
        event.type === "action_started" &&
        (event.details as Record<string, unknown> | undefined)?.command === "echo second",
    );
    const completions = harness.events.filter(
      (event) =>
        event.id === "query-interleaving" && event.type === "action_completed",
    );
    const firstCompletion = completions[0];
    const secondCompletion = completions[1];

    expect(firstCompletion?.actionId).toBe(firstStart?.actionId);
    expect(secondCompletion?.actionId).toBe(secondStart?.actionId);
    expect(firstCompletion?.actionId).not.toBe(secondStart?.actionId);
    expect(secondCompletion?.actionId).not.toBe(firstStart?.actionId);
  });
});
