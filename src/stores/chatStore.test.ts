import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  ApprovalResponse,
  ChatProviderUsage,
  SteerReceipt,
  StreamEvent,
  Thread,
} from "../types";

const mockIpc = vi.hoisted(() => ({
  sendMessage: vi.fn(),
  updateThreadRuntimeSelection: vi.fn(),
  setThreadPermissions: vi.fn(),
  steerMessage: vi.fn(),
  getThreadMessagesWindow: vi.fn(),
  getChatProviderUsage: vi.fn(),
  getActionOutput: vi.fn(),
  respondApproval: vi.fn(),
  cancelTurn: vi.fn(),
  syncThreadFromEngine: vi.fn(),
}));

const mockListenThreadEvents = vi.hoisted(() => vi.fn());
const mockRecordPerfMetric = vi.hoisted(() => vi.fn());

vi.mock("../lib/ipc", () => ({
  ipc: mockIpc,
  listenThreadEvents: mockListenThreadEvents,
}));

vi.mock("../lib/perfTelemetry", () => ({
  recordPerfMetric: mockRecordPerfMetric,
}));

import { useChatStore } from "./chatStore";
import { useThreadStore } from "./threadStore";

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

describe("chatStore send", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockIpc.getThreadMessagesWindow.mockResolvedValue({
      messages: [],
      nextCursor: null,
    });
    mockIpc.getChatProviderUsage.mockResolvedValue([]);
    mockIpc.getActionOutput.mockResolvedValue({
      found: true,
      outputChunks: [],
      truncated: false,
    });
    mockIpc.steerMessage.mockImplementation((...args: unknown[]) => {
      const clientSteerId = String(args[5]);
      return Promise.resolve<SteerReceipt>({
        clientSteerId,
        expectedTurnId: "turn-1",
        acceptedAt: "2026-08-11T15:26:57.608Z",
      });
    });
    mockIpc.cancelTurn.mockResolvedValue(undefined);
    mockIpc.syncThreadFromEngine.mockResolvedValue({
      id: "thread-1",
      workspaceId: "workspace-1",
      engineId: "codex",
      modelId: "gpt-5.3-codex",
      engineThreadId: "engine-thread-1",
      engineMetadata: {
        codexSyncRequired: false,
      },
      title: "Thread 1",
      status: "idle",
      messageCount: 0,
      totalTokens: 0,
      createdAt: new Date().toISOString(),
      lastActivityAt: new Date().toISOString(),
    });
    mockListenThreadEvents.mockResolvedValue(() => {});
    useThreadStore.setState({
      threads: [],
      threadsByWorkspace: {},
      archivedThreadsByWorkspace: {},
      activeThreadId: null,
      loading: false,
      error: undefined,
    });
    useChatStore.setState({
      threadId: "thread-1",
      sessionReadyByThread: {},
      messages: [],
      olderCursor: null,
      hasOlderMessages: false,
      loadingOlderMessages: false,
      olderLoadBlockedUntil: 0,
      status: "idle",
      streaming: false,
      preparingEngineId: null,
      preparingAttachments: false,
      usageLimits: null,
      usageLimitsLoading: false,
      error: undefined,
      unlisten: undefined,
    });
  });

  it("adds an assistant placeholder immediately while the turn request is in flight", async () => {
    const pendingRequest = deferred<string>();
    mockIpc.sendMessage.mockReturnValueOnce(pendingRequest.promise);

    const sendPromise = useChatStore.getState().send("hello", {
      engineId: "codex",
      modelId: "gpt-5.3-codex",
      reasoningEffort: "high",
    });

    const state = useChatStore.getState();
    expect(state.streaming).toBe(true);
    expect(state.preparingEngineId).toBe("codex");
    expect(state.sessionReadyByThread).toEqual({});
    expect(state.messages).toHaveLength(2);
    expect(state.messages[0]).toMatchObject({
      role: "user",
      status: "completed",
    });
    expect(state.messages[1]).toMatchObject({
      role: "assistant",
      status: "streaming",
      turnEngineId: "codex",
      turnModelId: "gpt-5.3-codex",
      turnReasoningEffort: "high",
    });

    pendingRequest.resolve("assistant-message-id");
    await expect(sendPromise).resolves.toBe(true);
    expect(useChatStore.getState().preparingEngineId).toBeNull();
  });

  it("keeps a ready thread ready across subsequent sends", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id-1");
    await expect(
      useChatStore.getState().send("first", {
        engineId: "codex",
        modelId: "gpt-5.3-codex",
      }),
    ).resolves.toBe(true);

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "TurnStarted",
      client_turn_id: "client-turn-1",
    });
    streamHandler!({ type: "TurnCompleted", status: "completed" });
    await vi.advanceTimersByTimeAsync(20);
    expect(useChatStore.getState().sessionReadyByThread).toEqual({ "thread-1": true });

    const secondRequest = deferred<string>();
    mockIpc.sendMessage.mockReturnValueOnce(secondRequest.promise);
    const secondSend = useChatStore.getState().send("second", {
      engineId: "codex",
      modelId: "gpt-5.3-codex",
    });

    expect(useChatStore.getState().sessionReadyByThread).toEqual({ "thread-1": true });
    secondRequest.resolve("assistant-message-id-2");
    await expect(secondSend).resolves.toBe(true);
    expect(useChatStore.getState().sessionReadyByThread).toEqual({ "thread-1": true });

    vi.useRealTimers();
  });

  it("shows remote attachment preparation until the send request is accepted", async () => {
    const pendingRequest = deferred<string>();
    mockIpc.sendMessage.mockReturnValueOnce(pendingRequest.promise);

    const sendPromise = useChatStore.getState().send("hello", {
      engineId: "claude",
      modelId: "sonnet",
      remoteAttachmentUpload: true,
    });

    expect(useChatStore.getState().preparingAttachments).toBe(true);
    pendingRequest.resolve("assistant-message-id");
    await expect(sendPromise).resolves.toBe(true);
    expect(useChatStore.getState().preparingAttachments).toBe(false);
  });

  it("removes the optimistic turn if the turn request fails", async () => {
    mockIpc.sendMessage.mockRejectedValueOnce(new Error("send failed"));

    await expect(useChatStore.getState().send("hello")).resolves.toBe(false);

    const state = useChatStore.getState();
    expect(state.streaming).toBe(false);
    expect(state.preparingEngineId).toBeNull();
    expect(state.status).toBe("error");
    expect(state.sessionReadyByThread).toEqual({});
    expect(state.messages).toEqual([]);
  });

  it("keeps a first turn unready when it is interrupted before TurnStarted", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "codex",
        modelId: "gpt-5.3-codex",
      }),
    ).resolves.toBe(true);

    expect(streamHandler).not.toBeNull();
    streamHandler!({ type: "TurnCompleted", status: "interrupted" });
    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().sessionReadyByThread).toEqual({});

    vi.useRealTimers();
  });

  it.each(["failed", "interrupted"] as const)(
    "keeps readiness after TurnStarted and terminal status %s",
    async (terminalStatus) => {
      vi.useFakeTimers();

      let streamHandler: ((event: StreamEvent) => void) | null = null;
      mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
        streamHandler = onEvent;
        return () => {};
      });

      await useChatStore.getState().setActiveThread("thread-1");
      mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
      await expect(
        useChatStore.getState().send("hello", {
          engineId: "codex",
          modelId: "gpt-5.3-codex",
        }),
      ).resolves.toBe(true);

      expect(streamHandler).not.toBeNull();
      streamHandler!({
        type: "TurnStarted",
        client_turn_id: "client-turn-terminal",
      });
      streamHandler!({ type: "TurnCompleted", status: terminalStatus });
      await vi.advanceTimersByTimeAsync(20);

      expect(useChatStore.getState().sessionReadyByThread).toEqual({ "thread-1": true });

      vi.useRealTimers();
    },
  );

  it("persists a complete runtime selection before sending the message", async () => {
    mockIpc.updateThreadRuntimeSelection.mockResolvedValueOnce(undefined);
    mockIpc.setThreadPermissions.mockResolvedValueOnce({
      autonomyPreset: ["automatic"],
      trust: ["automatic"],
      approval: ["automatic"],
      sandbox: ["automatic"],
      network: ["automatic"],
      defaultForNewThreads: [],
    });
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");

    await expect(
      useChatStore.getState().send("hello", {
        engineId: "opencode",
        modelId: "openai/gpt-5",
        reasoningEffort: "high",
        sendMethod: "prompt",
        permissionModeJson: "allow",
        permissionValues: {
          autonomyPreset: ["automatic"],
          trust: ["automatic"],
          approval: ["automatic"],
          sandbox: ["automatic"],
          network: ["automatic"],
          defaultForNewThreads: [],
        },
      }),
    ).resolves.toBe(true);

    expect(mockIpc.updateThreadRuntimeSelection).toHaveBeenCalledTimes(1);
    expect(mockIpc.setThreadPermissions).toHaveBeenCalledTimes(1);
    expect(mockIpc.sendMessage).toHaveBeenCalledTimes(1);
    expect(mockIpc.updateThreadRuntimeSelection.mock.invocationCallOrder[0]).toBeLessThan(
      mockIpc.setThreadPermissions.mock.invocationCallOrder[0],
    );
    expect(mockIpc.setThreadPermissions.mock.invocationCallOrder[0]).toBeLessThan(
      mockIpc.sendMessage.mock.invocationCallOrder[0],
    );
  });

  it("does not send when permission persistence fails and rolls back the optimistic turn", async () => {
    mockIpc.updateThreadRuntimeSelection.mockResolvedValueOnce(undefined);
    mockIpc.setThreadPermissions.mockRejectedValueOnce(new Error("permission failed"));

    await expect(
      useChatStore.getState().send("hello", {
        engineId: "opencode",
        modelId: "openai/gpt-5",
        reasoningEffort: "high",
        sendMethod: "prompt",
        permissionValues: {
          autonomyPreset: ["automatic"],
          trust: ["automatic"],
          approval: ["automatic"],
          sandbox: ["automatic"],
          network: ["automatic"],
          defaultForNewThreads: [],
        },
      }),
    ).resolves.toBe(false);

    expect(mockIpc.updateThreadRuntimeSelection).toHaveBeenCalledTimes(1);
    expect(mockIpc.setThreadPermissions).toHaveBeenCalledTimes(1);
    expect(mockIpc.sendMessage).not.toHaveBeenCalled();
    expect(useChatStore.getState().streaming).toBe(false);
    expect(useChatStore.getState().messages).toEqual([]);
  });

  it("does not send when runtime selection persistence fails and rolls back the optimistic turn", async () => {
    mockIpc.updateThreadRuntimeSelection.mockRejectedValueOnce(new Error("selection failed"));

    await expect(
      useChatStore.getState().send("hello", {
        engineId: "opencode",
        modelId: "openai/gpt-5",
        reasoningEffort: "high",
        sendMethod: "prompt",
        permissionModeJson: "allow",
      }),
    ).resolves.toBe(false);

    expect(mockIpc.sendMessage).not.toHaveBeenCalled();
    expect(useChatStore.getState().streaming).toBe(false);
    expect(useChatStore.getState().messages).toEqual([]);
  });

  it("skips runtime selection persistence for automatic continuation without a complete selection", async () => {
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");

    await expect(
      useChatStore.getState().send("continue", {
        engineId: "opencode",
        modelId: "openai/gpt-5",
        reasoningEffort: "high",
      }),
    ).resolves.toBe(true);

    expect(mockIpc.updateThreadRuntimeSelection).not.toHaveBeenCalled();
    expect(mockIpc.setThreadPermissions).not.toHaveBeenCalled();
    expect(mockIpc.sendMessage).toHaveBeenCalledTimes(1);
  });

  it("routes streamed content to the matching optimistic assistant via clientTurnId", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");

    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "codex",
        modelId: "gpt-5.3-codex",
      }),
    ).resolves.toBe(true);

    const optimisticAssistant = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.clientTurnId);
    expect(optimisticAssistant?.clientTurnId).toBeTruthy();
    expect(streamHandler).not.toBeNull();
    const emitStreamEvent = streamHandler!;

    useChatStore.setState((state) => ({
      ...state,
      messages: [
        ...state.messages,
        {
          id: "assistant-other",
          threadId: "thread-1",
          role: "assistant",
          clientTurnId: "client-turn-other",
          status: "streaming",
          schemaVersion: 1,
          blocks: [],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
    }));

    emitStreamEvent({
      type: "TurnStarted",
      client_turn_id: optimisticAssistant?.clientTurnId ?? null,
    });
    emitStreamEvent({
      type: "TextDelta",
      content: "matched content",
    });

    await vi.advanceTimersByTimeAsync(20);

    const state = useChatStore.getState();
    const matchedAssistant = state.messages.find((message) => message.id === optimisticAssistant?.id);
    const trailingAssistant = state.messages.find((message) => message.id === "assistant-other");

    expect(state.sessionReadyByThread).toEqual({ "thread-1": true });
    expect(matchedAssistant?.blocks).toEqual([{ type: "text", content: "matched content" }]);
    expect(trailingAssistant?.blocks ?? []).toEqual([]);
    expect(mockRecordPerfMetric).toHaveBeenCalledWith(
      "chat.turn.first_text.ms",
      expect.any(Number),
      expect.objectContaining({
        threadId: "thread-1",
        clientTurnId: optimisticAssistant?.clientTurnId,
      }),
    );

    vi.useRealTimers();
  });

  it("keeps a failed turn stopped after the remote interrupt completion notice", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "claude",
        modelId: "sonnet",
      }),
    ).resolves.toBe(true);

    const assistantBeforeFailure = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.clientTurnId);
    expect(assistantBeforeFailure?.clientTurnId).toBeTruthy();
    expect(streamHandler).not.toBeNull();

    streamHandler!({
      type: "TurnStarted",
      client_turn_id: assistantBeforeFailure?.clientTurnId ?? null,
    });
    streamHandler!({
      type: "TextDelta",
      content: "先前已经收到的文本。",
    });
    streamHandler!({
      type: "Error",
      message: "本轮对话执行失败",
      recoverable: false,
    });
    streamHandler!({
      type: "Notice",
      kind: "remote_interrupt_completed",
      level: "info",
      title: "远端任务已终止",
      message: "本轮异常对应的远端执行已经取消。",
    });

    await vi.advanceTimersByTimeAsync(20);

    const state = useChatStore.getState();
    const assistant = state.messages.find((message) => message.id === assistantBeforeFailure?.id);
    expect(state.status).toBe("error");
    expect(state.streaming).toBe(false);
    expect(state.turnStartedAt).toBeNull();
    expect(state.sessionReadyByThread).toEqual({ "thread-1": true });
    expect(assistant?.status).toBe("error");
    expect(assistant?.blocks).toHaveLength(2);
    expect(assistant?.blocks).toEqual(
      expect.arrayContaining([
        { type: "text", content: "先前已经收到的文本。" },
        { type: "error", message: "本轮对话执行失败" },
      ]),
    );
    expect(state.messages).toEqual(
      expect.arrayContaining([
        expect.objectContaining({
          blocks: expect.arrayContaining([
            {
              type: "notice",
              kind: "remote_interrupt_completed",
              level: "info",
              title: "远端任务已终止",
              message: "本轮异常对应的远端执行已经取消。",
            },
          ]),
        }),
      ]),
    );

    vi.useRealTimers();
  });

  it("allows a new turn to start after a failed turn reaches its terminal state", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "claude",
        modelId: "sonnet",
      }),
    ).resolves.toBe(true);

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "Error",
      message: "本轮对话执行失败",
      recoverable: false,
    });
    await vi.advanceTimersByTimeAsync(20);
    expect(useChatStore.getState().status).toBe("error");
    expect(useChatStore.getState().streaming).toBe(false);
    expect(useChatStore.getState().sessionReadyByThread).toEqual({});

    streamHandler!({
      type: "TurnStarted",
      client_turn_id: "client-turn-next",
    });
    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().status).toBe("streaming");
    expect(useChatStore.getState().streaming).toBe(true);

    vi.useRealTimers();
  });

  it("updates the assistant model label and inserts a reroute notice when the model is rerouted", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");

    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "codex",
        modelId: "gpt-5.1-codex-mini",
      }),
    ).resolves.toBe(true);

    const optimisticAssistant = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.clientTurnId);
    expect(streamHandler).not.toBeNull();

    streamHandler!({
      type: "ModelRerouted",
      from_model: "gpt-5.1-codex-mini",
      to_model: "gpt-5.3-codex",
      reason: "highRiskCyberActivity",
    });

    await vi.advanceTimersByTimeAsync(20);

    const reroutedAssistant = useChatStore
      .getState()
      .messages.find((message) => message.id === optimisticAssistant?.id);
    expect(reroutedAssistant?.turnModelId).toBe("gpt-5.3-codex");
    expect(mockRecordPerfMetric).toHaveBeenCalledWith(
      "chat.turn.first_content.ms",
      expect.any(Number),
      expect.objectContaining({
        threadId: "thread-1",
        modelId: "gpt-5.3-codex",
      }),
    );
    expect(reroutedAssistant?.blocks).toEqual([
      {
        type: "notice",
        kind: "model_rerouted",
        level: "info",
        title: "Model rerouted",
        message: "Switched from gpt-5.1-codex-mini to gpt-5.3-codex (highRiskCyberActivity).",
      },
    ]);

    vi.useRealTimers();
  });

  it("stores generic notice events as notice blocks", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");

    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "codex",
        modelId: "gpt-5.3-codex",
      }),
    ).resolves.toBe(true);

    streamHandler!({
      type: "TextDelta",
      content: "Visible assistant text.",
    });
    streamHandler!({
      type: "Notice",
      kind: "deprecation_notice",
      level: "warning",
      title: "Deprecation notice",
      message: "Use the newer approval API.",
    });

    await vi.advanceTimersByTimeAsync(20);

    const assistant = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.blocks?.length);
    expect(assistant?.blocks).toEqual([
      {
        type: "notice",
        kind: "deprecation_notice",
        level: "warning",
        title: "Deprecation notice",
        message: "Use the newer approval API.",
      },
      {
        type: "text",
        content: "Visible assistant text.",
      },
    ]);

    vi.useRealTimers();
  });

  it("stores Claude background task metadata and replaces it on same-kind updates", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("run a background task", {
        engineId: "claude",
        modelId: "claude-sonnet",
      }),
    ).resolves.toBe(true);

    streamHandler!({
      type: "Notice",
      kind: "claude_background_tasks",
      level: "info",
      title: "Claude 后台任务",
      message: "后台任务已启动",
      metadata: {
        backgroundTasks: [
          {
            taskId: "task-1",
            taskType: "bash",
            description: "执行后台命令",
            status: "running",
            startedAt: 1_000,
            summary: "正在执行",
          },
        ],
        activeTaskCount: 1,
      },
    });
    streamHandler!({
      type: "Notice",
      kind: "claude_background_tasks",
      level: "info",
      title: "Claude 后台任务",
      message: "后台任务已结束",
      metadata: {
        backgroundTasks: [
          {
            taskId: "task-1",
            taskType: "bash",
            description: "执行后台命令",
            status: "completed",
            startedAt: 1_000,
            summary: "执行完成",
            finishedAt: 2_000,
          },
        ],
        activeTaskCount: 0,
      },
    });

    await vi.advanceTimersByTimeAsync(20);

    const assistant = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.blocks?.length);
    expect(assistant?.blocks).toEqual([
      {
        type: "notice",
        kind: "claude_background_tasks",
        level: "info",
        title: "Claude 后台任务",
        message: "后台任务已结束",
        metadata: {
          backgroundTasks: [
            {
              taskId: "task-1",
              taskType: "bash",
              description: "执行后台命令",
              status: "completed",
              startedAt: 1_000,
              summary: "执行完成",
              finishedAt: 2_000,
            },
          ],
          activeTaskCount: 0,
        },
      },
    ]);

    vi.useRealTimers();
  });

  it("associates Claude operations with their background task without changing foreground operations", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("run background work", {
        engineId: "claude",
        modelId: "claude-sonnet",
      }),
    ).resolves.toBe(true);

    streamHandler!({
      type: "ActionStarted",
      action_id: "background-action",
      action_type: "command",
      summary: "后台编译检查",
      details: { command: "python -m py_compile src/proxy.py" },
    });
    streamHandler!({
      type: "ActionStarted",
      action_id: "foreground-action",
      action_type: "search",
      summary: "前台搜索",
      details: { query: "proxy" },
    });
    streamHandler!({
      type: "ActionBackgroundTaskAssigned",
      action_id: "background-action",
      task_id: "task-1",
    });

    await vi.advanceTimersByTimeAsync(20);

    const assistant = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.blocks?.length);
    const actions = assistant?.blocks?.filter((block) => block.type === "action") ?? [];
    const backgroundAction = actions.find(
      (block) => block.type === "action" && block.actionId === "background-action",
    );
    const foregroundAction = actions.find(
      (block) => block.type === "action" && block.actionId === "foreground-action",
    );
    expect(backgroundAction).toMatchObject({ backgroundTaskId: "task-1" });
    expect(foregroundAction?.backgroundTaskId).toBeUndefined();

    vi.useRealTimers();
  });

  it("keeps hook notices at their stream positions", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-hook-order",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
    });

    streamHandler!({ type: "TextDelta", content: "before hooks" });
    streamHandler!({
      type: "Notice",
      kind: "hook_started_first",
      level: "info",
      title: "Hook started",
      message: "first hook started",
    });
    streamHandler!({
      type: "ActionStarted",
      action_id: "action-1",
      engine_action_id: "item-1",
      action_type: "other",
      summary: "Tool call",
      details: {},
    });
    streamHandler!({
      type: "Notice",
      kind: "hook_completed_first",
      level: "info",
      title: "Hook completed",
      message: "first hook completed",
    });
    streamHandler!({ type: "TextDelta", content: "after hooks" });

    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      { type: "text", content: "before hooks" },
      { type: "notice", kind: "hook_started_first" },
      { type: "action", actionId: "action-1" },
      { type: "notice", kind: "hook_completed_first" },
      { type: "text", content: "after hooks" },
    ]);

    vi.useRealTimers();
  });

  it("derives context usage from current context tokens instead of cumulative totals", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "UsageLimitsUpdated",
      usage: {
        current_tokens: 30000,
        max_context_tokens: 200000,
        context_window_percent: 45,
        five_hour_percent: 17,
        weekly_percent: 42,
      },
    });

    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().usageLimits).toEqual({
      currentTokens: 30000,
      maxContextTokens: 200000,
      contextPercent: 90,
      windowFiveHourPercent: 83,
      windowWeeklyPercent: 58,
      windowFableWeeklyPercent: null,
      windowOpusWeeklyPercent: null,
      windowSonnetWeeklyPercent: null,
      windowFiveHourResetsAt: null,
      windowWeeklyResetsAt: null,
      windowFableWeeklyResetsAt: null,
      windowOpusWeeklyResetsAt: null,
      windowSonnetWeeklyResetsAt: null,
    });

    vi.useRealTimers();
  });

  it("merges generic and model-specific Claude usage windows", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");

    streamHandler!({
      type: "UsageLimitsUpdated",
      usage: { five_hour_percent: 10, five_hour_resets_at: 1_740_000_000 },
    });
    streamHandler!({
      type: "UsageLimitsUpdated",
      usage: { weekly_percent: 20, weekly_resets_at: 1_740_100_000 },
    });
    streamHandler!({
      type: "UsageLimitsUpdated",
      usage: {
        fable_weekly_percent: 35,
        fable_weekly_resets_at: 1_740_200_000,
      },
    });

    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().usageLimits).toMatchObject({
      windowFiveHourPercent: 90,
      windowWeeklyPercent: 80,
      windowFableWeeklyPercent: 65,
      windowFiveHourResetsAt: "2025-02-19T21:20:00.000Z",
      windowWeeklyResetsAt: "2025-02-21T01:06:40.000Z",
      windowFableWeeklyResetsAt: "2025-02-22T04:53:20.000Z",
    });

    vi.useRealTimers();
  });

  it("refreshes provider limits when binding a restored conversation", async () => {
    const usageRequest = deferred<ChatProviderUsage[]>();
    mockIpc.getChatProviderUsage.mockReturnValueOnce(usageRequest.promise);
    mockIpc.getThreadMessagesWindow.mockResolvedValueOnce({
      messages: [
        {
          id: "user-restored",
          threadId: "thread-1",
          role: "user",
          content: "continue the work",
          blocks: [{ type: "text", content: "continue the work" }],
          schemaVersion: 1,
          status: "completed",
          tokenUsage: null,
          createdAt: new Date().toISOString(),
        },
      ],
      nextCursor: null,
    });
    useThreadStore.setState({
      threads: [
        {
          id: "thread-1",
          workspaceId: "workspace-1",
          engineId: "claude",
          modelId: "fable",
          engineThreadId: "engine-thread-1",
          engineMetadata: {},
          title: "Restored thread",
          status: "idle",
          messageCount: 1,
          totalTokens: 0,
          createdAt: new Date().toISOString(),
          lastActivityAt: new Date().toISOString(),
        },
      ],
    });

    await useChatStore.getState().setActiveThread("thread-1");

    expect(useChatStore.getState().sessionReadyByThread).toEqual({ "thread-1": true });
    expect(useChatStore.getState()).toMatchObject({
      usageLimits: null,
      usageLimitsLoading: true,
    });

    usageRequest.resolve([
      {
        engineId: "claude",
        name: "Claude",
        available: true,
        windows: [
          { kind: "five_hour", usedPercent: 20, resetsAt: 1_740_000_000 },
          { kind: "weekly", usedPercent: 35, resetsAt: 1_740_100_000 },
          { kind: "fable_weekly", usedPercent: 45, resetsAt: 1_740_200_000 },
        ],
      },
    ]);

    await vi.waitFor(() => {
      expect(useChatStore.getState().usageLimitsLoading).toBe(false);
    });
    expect(useChatStore.getState().usageLimits).toMatchObject({
      contextPercent: null,
      windowFiveHourPercent: 80,
      windowWeeklyPercent: 65,
      windowFableWeeklyPercent: 55,
      windowFiveHourResetsAt: "2025-02-19T21:20:00.000Z",
    });
  });

  it("does not restore readiness for blank or missing engine thread ids", async () => {
    const blankEngineThread: Thread = {
      id: "thread-blank",
      workspaceId: "workspace-1",
      engineId: "claude",
      modelId: "sonnet",
      engineThreadId: "   ",
      engineMetadata: {},
      title: "Blank engine thread",
      status: "idle",
      messageCount: 0,
      totalTokens: 0,
      createdAt: new Date().toISOString(),
      lastActivityAt: new Date().toISOString(),
    };
    const missingEngineThread: Thread = {
      ...blankEngineThread,
      id: "thread-null",
      engineThreadId: null,
      title: "Missing engine thread",
    };
    useThreadStore.setState({
      threads: [blankEngineThread, missingEngineThread],
      threadsByWorkspace: {
        "workspace-1": [blankEngineThread, missingEngineThread],
      },
      archivedThreadsByWorkspace: {},
      activeThreadId: null,
      loading: false,
      error: undefined,
    });

    await useChatStore.getState().setActiveThread("thread-blank");
    expect(useChatStore.getState().sessionReadyByThread).toEqual({});

    await useChatStore.getState().setActiveThread("thread-null");
    expect(useChatStore.getState().sessionReadyByThread).toEqual({});
  });

  it("preserves stdin action output chunks from streamed events", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");

    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "codex",
        modelId: "gpt-5.3-codex",
      }),
    ).resolves.toBe(true);

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "ActionStarted",
      action_id: "action-stdin",
      engine_action_id: "cmd-stdin",
      action_type: "command",
      summary: "pnpm test",
      details: {},
    });
    streamHandler!({
      type: "ActionOutputDelta",
      action_id: "action-stdin",
      stream: "stdin",
      content: "pnpm test\n",
    });

    await vi.advanceTimersByTimeAsync(20);

    const assistant = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.blocks?.length);
    expect(assistant?.blocks).toEqual([
      {
        type: "action",
        actionId: "action-stdin",
        engineActionId: "cmd-stdin",
        actionType: "command",
        summary: "pnpm test",
        details: {},
        outputChunks: [
          {
            stream: "stdin",
            content: "pnpm test\n",
          },
        ],
        outputDeferred: false,
        outputDeferredLoaded: true,
        status: "running",
      },
    ]);

    vi.useRealTimers();
  });

  it("finishes a running action when the turn completes without an action completion event", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "codex",
        modelId: "gpt-5.3-codex",
      }),
    ).resolves.toBe(true);

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "ActionStarted",
      action_id: "web-search-1",
      engine_action_id: "item-1",
      action_type: "search",
      summary: "Web search",
      details: {},
    });
    streamHandler!({
      type: "TurnCompleted",
      status: "completed",
    });

    await vi.advanceTimersByTimeAsync(20);

    const assistant = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.blocks?.length);
    expect(assistant).toMatchObject({ status: "completed" });
    expect(assistant?.blocks).toMatchObject([
      {
        type: "action",
        actionId: "web-search-1",
        status: "done",
      },
    ]);

    vi.useRealTimers();
  });

  it("settles accepted steer blocks when the active turn completes", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-steer",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            {
              type: "steer",
              steerId: "steer-1",
              content: "follow up",
              deliveryStatus: "accepted",
              expectedTurnId: "turn-1",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
    });

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "TurnCompleted",
      status: "completed",
    });
    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().messages[0]).toMatchObject({
      status: "completed",
      blocks: [
        {
          type: "steer",
          steerId: "steer-1",
          deliveryStatus: "settled",
        },
      ],
    });

    vi.useRealTimers();
  });

  it("fixes a steer at the Codex applied boundary after earlier text and tool output", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-steer-order",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            {
              type: "steer",
              steerId: "steer-1",
              content: "那就是整个功能还没开发好",
              deliveryStatus: "accepted",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
    });

    streamHandler!({
      type: "TextDelta",
      content: "不是能力不行，是当前驱动连接还没恢复。",
    });
    streamHandler!({
      type: "ActionStarted",
      action_id: "tool-1",
      engine_action_id: "item-1",
      action_type: "other",
      summary: "Tool call",
      details: {},
    });
    streamHandler!({
      type: "SteerApplied",
      client_steer_id: "steer-1",
      content: "那就是整个功能还没开发好",
      plan_mode: false,
      attachments: [],
      input_items: [],
    });
    streamHandler!({
      type: "TextDelta",
      content: "不能直接判断为整个功能没开发好。",
    });
    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      { type: "text", content: "不是能力不行，是当前驱动连接还没恢复。" },
      { type: "action", actionId: "tool-1" },
      {
        type: "steer",
        steerId: "steer-1",
        content: "那就是整个功能还没开发好",
        deliveryStatus: "applied",
      },
      { type: "text", content: "不能直接判断为整个功能没开发好。" },
    ]);

    vi.useRealTimers();
  });

  it("keeps multiple pending steers at the tail and fixes them in FIFO order", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-multiple-steers",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            { type: "steer", steerId: "steer-1", content: "第一条", deliveryStatus: "accepted" },
            { type: "steer", steerId: "steer-2", content: "第二条", deliveryStatus: "accepted" },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
    });

    streamHandler!({ type: "TextDelta", content: "第一条之前" });
    streamHandler!({
      type: "SteerApplied",
      client_steer_id: "steer-1",
      content: "第一条",
      plan_mode: false,
      attachments: [],
      input_items: [],
    });
    streamHandler!({ type: "TextDelta", content: "两条之间" });
    streamHandler!({
      type: "SteerApplied",
      client_steer_id: "steer-2",
      content: "第二条",
      plan_mode: false,
      attachments: [],
      input_items: [],
    });
    streamHandler!({ type: "TextDelta", content: "第二条之后" });
    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      { type: "text", content: "第一条之前" },
      { type: "steer", steerId: "steer-1", deliveryStatus: "applied" },
      { type: "text", content: "两条之间" },
      { type: "steer", steerId: "steer-2", deliveryStatus: "applied" },
      { type: "text", content: "第二条之后" },
    ]);

    vi.useRealTimers();
  });

  it("collapses existing duplicate diff blocks for same-scope stream updates", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-diff",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          content: "",
          blocks: [
            { type: "diff", diff: "old diff 1", scope: "turn" },
            { type: "text", content: "kept" },
            { type: "diff", diff: "old diff 2", scope: "turn" },
            {
              type: "action",
              actionId: "action-1",
              engineActionId: "cmd-1",
              actionType: "command",
              summary: "pnpm test",
              details: {},
              outputChunks: [],
              status: "done",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
    });

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "DiffUpdated",
      diff: "new diff",
      scope: "turn",
    });

    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().messages[0]?.blocks).toEqual([
      { type: "text", content: "kept" },
      { type: "diff", diff: "new diff", scope: "turn" },
      {
        type: "action",
        actionId: "action-1",
        engineActionId: "cmd-1",
        actionType: "command",
        summary: "pnpm test",
        details: {},
        outputChunks: [],
        status: "done",
      },
    ]);

    vi.useRealTimers();
  });

  it("marks approvals as answered when the runtime resolves them externally", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    mockIpc.getThreadMessagesWindow.mockResolvedValueOnce({
      messages: [
        {
          id: "assistant-approval",
          threadId: "thread-1",
          role: "assistant",
          status: "completed",
          schemaVersion: 1,
          blocks: [
            {
              type: "approval",
              approvalId: "approval-runtime-1",
              actionType: "command",
              summary: "Run command",
              details: {},
              status: "pending",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      nextCursor: null,
    });

    await useChatStore.getState().setActiveThread("thread-1");

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "ApprovalResolved",
      approval_id: "approval-runtime-1",
    });

    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState().messages[0]?.blocks).toEqual([
      {
        type: "approval",
        approvalId: "approval-runtime-1",
        actionType: "command",
        summary: "Run command",
        details: {},
        status: "answered",
      },
    ]);

    vi.useRealTimers();
  });

  it("preserves stdin chunks when hydrating deferred action output", async () => {
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-action",
          threadId: "thread-1",
          role: "assistant",
          status: "completed",
          schemaVersion: 1,
          blocks: [
            {
              type: "action",
              actionId: "action-hydrate",
              engineActionId: "cmd-hydrate",
              actionType: "command",
              summary: "pnpm test",
              details: {},
              outputChunks: [],
              outputDeferred: true,
              outputDeferredLoaded: false,
              status: "done",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: true,
        },
      ],
      olderCursor: null,
      hasOlderMessages: false,
      loadingOlderMessages: false,
      olderLoadBlockedUntil: 0,
      status: "idle",
      streaming: false,
      usageLimits: null,
      error: undefined,
      unlisten: undefined,
    });
    mockIpc.getActionOutput.mockResolvedValueOnce({
      found: true,
      outputChunks: [
        {
          stream: "stdin",
          content: "pnpm test\n",
        },
      ],
      truncated: false,
    });

    await useChatStore.getState().hydrateActionOutput("assistant-action", "action-hydrate");

    expect(useChatStore.getState().messages[0]?.blocks).toEqual([
      {
        type: "action",
        actionId: "action-hydrate",
        engineActionId: "cmd-hydrate",
        actionType: "command",
        summary: "pnpm test",
        details: {},
        outputChunks: [
          {
            stream: "stdin",
            content: "pnpm test\n",
          },
        ],
        outputDeferred: false,
        outputDeferredLoaded: true,
        status: "done",
      },
    ]);
  });

  it("infers accept_for_session for permission approval responses", async () => {
    mockIpc.respondApproval.mockResolvedValueOnce(undefined);
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-approval",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            {
              type: "approval",
              approvalId: "approval-1",
              actionType: "other",
              summary: "Codex requested network access",
              details: {},
              status: "pending",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      olderCursor: null,
      hasOlderMessages: false,
      loadingOlderMessages: false,
      olderLoadBlockedUntil: 0,
      status: "streaming",
      streaming: true,
      usageLimits: null,
      error: undefined,
      unlisten: undefined,
    });

    await useChatStore.getState().respondApproval("approval-1", {
      permissions: {
        network: {
          enabled: true,
        },
      },
      scope: "session",
    });

    expect(mockIpc.respondApproval).toHaveBeenCalledWith("thread-1", "approval-1", {
      permissions: {
        network: {
          enabled: true,
        },
      },
      scope: "session",
    });
    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      {
        type: "approval",
        approvalId: "approval-1",
        actionType: "other",
        summary: "Codex requested network access",
        details: {},
        status: "answered",
        decision: "accept_for_session",
      },
    ]);
    expect(useChatStore.getState().status).toBe("streaming");
    expect(useChatStore.getState().streaming).toBe(true);
  });

  it("treats 'none' permission values as a decline", async () => {
    mockIpc.respondApproval.mockResolvedValueOnce(undefined);
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-approval-none",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            {
              type: "approval",
              approvalId: "approval-none",
              actionType: "other",
              summary: "Network access",
              details: {},
              status: "pending",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      olderCursor: null,
      hasOlderMessages: false,
      loadingOlderMessages: false,
      olderLoadBlockedUntil: 0,
      status: "streaming",
      streaming: true,
      usageLimits: null,
      error: undefined,
      unlisten: undefined,
    });

    await useChatStore.getState().respondApproval("approval-none", {
      permissions: {
        network: "none",
      },
      scope: "turn",
    });

    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      {
        type: "approval",
        approvalId: "approval-none",
        actionType: "other",
        summary: "Network access",
        details: {},
        status: "answered",
        decision: "decline",
      },
    ]);
  });

  it("infers MCP elicitation decisions from action responses", async () => {
    mockIpc.respondApproval.mockResolvedValueOnce(undefined);
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-approval-2",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            {
              type: "approval",
              approvalId: "approval-2",
              actionType: "other",
              summary: "docs requested input",
              details: {},
              status: "pending",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      olderCursor: null,
      hasOlderMessages: false,
      loadingOlderMessages: false,
      olderLoadBlockedUntil: 0,
      status: "streaming",
      streaming: true,
      usageLimits: null,
      error: undefined,
      unlisten: undefined,
    });

    await useChatStore.getState().respondApproval("approval-2", {
      action: "decline",
    });

    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      {
        type: "approval",
        approvalId: "approval-2",
        actionType: "other",
        summary: "docs requested input",
        details: {},
        status: "answered",
        decision: "decline",
      },
    ]);
  });

  it("stores only the latest MCP progress message on the matching action block", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");

    mockIpc.sendMessage.mockResolvedValueOnce("assistant-message-id");
    await expect(
      useChatStore.getState().send("hello", {
        engineId: "codex",
        modelId: "gpt-5.3-codex",
      }),
    ).resolves.toBe(true);

    expect(streamHandler).not.toBeNull();
    streamHandler!({
      type: "ActionStarted",
      action_id: "action-1",
      engine_action_id: "item-1",
      action_type: "other",
      summary: "search_docs",
      details: {},
    });
    streamHandler!({
      type: "ActionProgressUpdated",
      action_id: "action-1",
      message: "Connecting",
    });
    streamHandler!({
      type: "ActionProgressUpdated",
      action_id: "action-1",
      message: "Fetching results",
    });

    await vi.advanceTimersByTimeAsync(20);

    const assistant = useChatStore
      .getState()
      .messages.find((message) => message.role === "assistant" && message.blocks?.length);
    expect(assistant?.blocks).toEqual([
      {
        type: "action",
        actionId: "action-1",
        engineActionId: "item-1",
        actionType: "other",
        summary: "search_docs",
        details: {
          progressKind: "mcp",
          progressMessage: "Fetching results",
        },
        outputChunks: [],
        outputDeferred: false,
        outputDeferredLoaded: true,
        status: "running",
      },
    ]);

    vi.useRealTimers();
  });

  it("adds a steer block to the active assistant while steering an active turn", async () => {
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-1",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      olderCursor: null,
      hasOlderMessages: false,
      loadingOlderMessages: false,
      olderLoadBlockedUntil: 0,
      status: "streaming",
      streaming: true,
      usageLimits: null,
      error: undefined,
      unlisten: undefined,
    });

    await expect(
      useChatStore.getState().steer("follow up", {
        inputItems: [{ type: "mention", name: "Docs", path: "app://docs" }],
      }),
    ).resolves.toBe(true);

    expect(mockIpc.steerMessage).toHaveBeenCalledWith(
      "thread-1",
      "follow up",
      null,
      [{ type: "mention", name: "Docs", path: "app://docs" }],
      false,
      expect.any(String),
      null,
    );
    expect(useChatStore.getState().messages).toHaveLength(1);
    expect(useChatStore.getState().messages[0]).toMatchObject({
      role: "assistant",
      blocks: [
        {
          type: "steer",
          content: "follow up",
          deliveryStatus: "accepted",
          expectedTurnId: "turn-1",
          acceptedAt: "2026-08-11T15:26:57.608Z",
          mentions: [{ type: "mention", name: "Docs", path: "app://docs" }],
        },
      ],
    });
  });

  it("keeps the steer block in sending state until the backend receipt arrives", async () => {
    const pendingReceipt = deferred<SteerReceipt>();
    mockIpc.steerMessage.mockReturnValueOnce(pendingReceipt.promise);
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-1",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
    });

    const steerPromise = useChatStore.getState().steer("follow up");
    const steerBlock = useChatStore.getState().messages[0]?.blocks?.find(
      (block) => block.type === "steer",
    );
    expect(steerBlock).toMatchObject({ deliveryStatus: "sending" });
    expect(steerBlock?.type).toBe("steer");

    pendingReceipt.resolve({
      clientSteerId: steerBlock?.type === "steer" ? steerBlock.steerId : "",
      expectedTurnId: "turn-1",
      acceptedAt: "2026-08-11T15:26:57.608Z",
    });
    await expect(steerPromise).resolves.toBe(true);
  });

  it("does not downgrade an applied steer when the RPC receipt arrives afterward", async () => {
    vi.useFakeTimers();
    const pendingReceipt = deferred<SteerReceipt>();
    mockIpc.steerMessage.mockReturnValueOnce(pendingReceipt.promise);
    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-race",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
    });

    const steerPromise = useChatStore.getState().steer("follow up");
    const optimisticBlock = useChatStore.getState().messages[0]?.blocks?.find(
      (block) => block.type === "steer",
    );
    const clientSteerId = optimisticBlock?.type === "steer" ? optimisticBlock.steerId : "";

    streamHandler!({
      type: "SteerApplied",
      client_steer_id: clientSteerId,
      content: "follow up",
      plan_mode: false,
      attachments: [],
      input_items: [],
    });
    await vi.advanceTimersByTimeAsync(20);

    pendingReceipt.resolve({
      clientSteerId,
      expectedTurnId: "turn-1",
      acceptedAt: "2026-08-11T15:26:57.608Z",
    });
    await expect(steerPromise).resolves.toBe(true);

    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      { type: "steer", steerId: clientSteerId, deliveryStatus: "applied" },
    ]);

    vi.useRealTimers();
  });

  it("retains the optimistic steer block as failed when the steer request fails", async () => {
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-1",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      olderCursor: null,
      hasOlderMessages: false,
      loadingOlderMessages: false,
      olderLoadBlockedUntil: 0,
      status: "streaming",
      streaming: true,
      usageLimits: null,
      error: undefined,
      unlisten: undefined,
    });
    mockIpc.steerMessage.mockRejectedValueOnce(new Error("steer failed"));

    await expect(useChatStore.getState().steer("follow up")).resolves.toBe(false);

    expect(useChatStore.getState().messages).toEqual([
      expect.objectContaining({
        role: "assistant",
        blocks: [
          expect.objectContaining({
            type: "steer",
            content: "follow up",
            deliveryStatus: "failed",
            failureReason: expect.stringContaining("steer failed"),
          }),
        ],
      }),
    ]);
    expect(useChatStore.getState().error).toContain("steer failed");
  });

  it("settles and retains an accepted steer block when the turn is canceled", async () => {
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-1",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            {
              type: "steer",
              steerId: "steer-1",
              content: "follow up",
              deliveryStatus: "accepted",
              expectedTurnId: "turn-1",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
    });

    await useChatStore.getState().cancel();

    expect(mockIpc.cancelTurn).toHaveBeenCalledWith("thread-1");
    expect(useChatStore.getState().messages).toMatchObject([
      {
        id: "assistant-1",
        status: "interrupted",
        blocks: [
          {
            type: "steer",
            steerId: "steer-1",
            deliveryStatus: "settled",
          },
        ],
      },
    ]);
    expect(useChatStore.getState()).toMatchObject({
      status: "idle",
      streaming: false,
      turnStartedAt: null,
    });
  });

  it("interrupts the active assistant and finalizes a running action when canceled", async () => {
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-running-action",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            { type: "text", content: "Working on it" },
            {
              type: "notice",
              kind: "hook_completed_first",
              level: "info",
              title: "Hook completed",
              message: "first hook completed",
            },
            {
              type: "action",
              actionId: "web-search-1",
              engineActionId: "item-1",
              actionType: "search",
              summary: "Web search",
              details: {},
              outputChunks: [],
              status: "running",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
      turnStartedAt: Date.now(),
    });

    await useChatStore.getState().cancel();

    expect(useChatStore.getState()).toMatchObject({
      status: "idle",
      streaming: false,
      turnStartedAt: null,
      messages: [
        {
          id: "assistant-running-action",
          status: "interrupted",
          blocks: [
            { type: "text", content: "Working on it" },
            { type: "notice", kind: "hook_completed_first" },
            {
              type: "action",
              actionId: "web-search-1",
              status: "error",
              result: {
                success: false,
                error: "Action did not report completion before the turn was interrupted.",
              },
            },
          ],
        },
      ],
    });
  });

  it("does not overwrite a completed turn when completion wins the cancel race", async () => {
    const cancelRequest = deferred<void>();
    mockIpc.cancelTurn.mockReturnValueOnce(cancelRequest.promise);
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-completed-race",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [{ type: "text", content: "Almost done" }],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      status: "streaming",
      streaming: true,
      turnStartedAt: Date.now(),
    });

    const cancelPromise = useChatStore.getState().cancel();
    useChatStore.setState((current) => ({
      messages: current.messages.map((message) =>
        message.id === "assistant-completed-race"
          ? { ...message, status: "completed" }
          : message,
      ),
      status: "completed",
      streaming: false,
      turnStartedAt: null,
    }));
    cancelRequest.resolve(undefined);
    await cancelPromise;

    expect(useChatStore.getState()).toMatchObject({
      status: "completed",
      streaming: false,
      messages: [{ id: "assistant-completed-race", status: "completed" }],
    });
  });

  it("folds persisted steer messages into the preceding completed assistant when binding", async () => {
    mockIpc.getThreadMessagesWindow.mockResolvedValueOnce({
      messages: [
        {
          id: "assistant-1",
          threadId: "thread-1",
          role: "assistant",
          content: null,
          blocks: [{ type: "text", content: "Working on it" }],
          turnEngineId: "codex",
          turnModelId: "gpt-5.3-codex",
          turnReasoningEffort: "medium",
          schemaVersion: 1,
          status: "completed",
          tokenUsage: null,
          createdAt: new Date().toISOString(),
        },
        {
          id: "steer-user-1",
          threadId: "thread-1",
          role: "user",
          content: "focus on the failing test",
          blocks: [{ type: "text", content: "focus on the failing test", isSteer: true }],
          turnEngineId: "codex",
          turnModelId: "gpt-5.3-codex",
          turnReasoningEffort: "medium",
          schemaVersion: 1,
          status: "completed",
          tokenUsage: null,
          createdAt: new Date().toISOString(),
        },
      ],
      nextCursor: null,
    });

    await useChatStore.getState().setActiveThread("thread-1");

    expect(useChatStore.getState().messages).toHaveLength(1);
    expect(useChatStore.getState().messages[0]).toMatchObject({
      role: "assistant",
      status: "completed",
      blocks: [
        {
          type: "text",
          content: "Working on it",
        },
        {
          type: "steer",
          steerId: "steer-user-1",
          content: "focus on the failing test",
          deliveryStatus: "settled",
        },
      ],
    });
  });

  it("keeps a persisted steer at its applied boundary without duplicating it on reload", async () => {
    mockIpc.getThreadMessagesWindow.mockResolvedValueOnce({
      messages: [
        {
          id: "assistant-applied-steer",
          threadId: "thread-1",
          role: "assistant",
          content: null,
          blocks: [
            { type: "text", content: "插话之前" },
            {
              type: "steer",
              steerId: "client-steer-1",
              content: "插话内容",
              deliveryStatus: "settled",
            },
            { type: "text", content: "插话之后" },
          ],
          schemaVersion: 1,
          status: "completed",
          tokenUsage: null,
          createdAt: new Date().toISOString(),
        },
        {
          id: "persisted-user-steer-1",
          threadId: "thread-1",
          role: "user",
          content: "插话内容",
          blocks: [
            {
              type: "text",
              content: "插话内容",
              isSteer: true,
              clientSteerId: "client-steer-1",
            },
          ],
          schemaVersion: 1,
          status: "completed",
          tokenUsage: null,
          createdAt: new Date().toISOString(),
        },
      ],
      nextCursor: null,
    });

    await useChatStore.getState().setActiveThread("thread-1");

    expect(useChatStore.getState().messages).toHaveLength(1);
    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      { type: "text", content: "插话之前" },
      { type: "steer", steerId: "client-steer-1", content: "插话内容" },
      { type: "text", content: "插话之后" },
    ]);
  });

  it("keeps regular user turns intact when loading older history", async () => {
    mockIpc.getThreadMessagesWindow
      .mockResolvedValueOnce({
        messages: [
          {
            id: "assistant-latest",
            threadId: "thread-1",
            role: "assistant",
            content: null,
            blocks: [{ type: "text", content: "Latest reply" }],
            turnEngineId: "codex",
            turnModelId: "gpt-5.3-codex",
            turnReasoningEffort: "medium",
            schemaVersion: 1,
            status: "completed",
            tokenUsage: null,
            createdAt: new Date().toISOString(),
          },
        ],
        nextCursor: {
          createdAt: "2026-03-13T00:00:00.000Z",
          id: "cursor-1",
          rowId: 1,
        },
      })
      .mockResolvedValueOnce({
        messages: [
          {
            id: "assistant-earlier",
            threadId: "thread-1",
            role: "assistant",
            content: null,
            blocks: [{ type: "text", content: "Earlier reply" }],
            turnEngineId: "codex",
            turnModelId: "gpt-5.3-codex",
            turnReasoningEffort: "medium",
            schemaVersion: 1,
            status: "completed",
            tokenUsage: null,
            createdAt: new Date().toISOString(),
          },
          {
            id: "user-regular",
            threadId: "thread-1",
            role: "user",
            content: "A normal next turn",
            blocks: [{ type: "text", content: "A normal next turn" }],
            turnEngineId: "codex",
            turnModelId: "gpt-5.3-codex",
            turnReasoningEffort: "medium",
            schemaVersion: 1,
            status: "completed",
            tokenUsage: null,
            createdAt: new Date().toISOString(),
          },
        ],
        nextCursor: null,
      });

    await useChatStore.getState().setActiveThread("thread-1");
    await useChatStore.getState().loadOlderMessages();

    expect(useChatStore.getState().messages).toHaveLength(3);
    expect(useChatStore.getState().messages.map((message) => message.id)).toEqual([
      "assistant-earlier",
      "user-regular",
      "assistant-latest",
    ]);
  });

  it.each([
    { status: "streaming" as const, expectedStreaming: true },
    { status: "awaiting_approval" as const, expectedStreaming: true },
  ])(
    "syncs and preserves the bound thread runtime status when loading a $status thread",
    async ({ status, expectedStreaming }) => {
      const thread = {
        id: "thread-1",
        workspaceId: "workspace-1",
        engineId: "codex" as const,
        modelId: "gpt-5.3-codex",
        engineThreadId: "engine-thread-1",
        engineMetadata: {
          codexSyncRequired: false,
        },
        title: "Thread 1",
        status,
        messageCount: 0,
        totalTokens: 0,
        createdAt: new Date().toISOString(),
        lastActivityAt: new Date().toISOString(),
      };

      useThreadStore.setState({
        threads: [thread],
        threadsByWorkspace: {
          "workspace-1": [thread],
        },
        archivedThreadsByWorkspace: {},
        activeThreadId: "thread-1",
        loading: false,
        error: undefined,
      });

      mockIpc.syncThreadFromEngine.mockResolvedValueOnce(thread);
      await useChatStore.getState().setActiveThread("thread-1");
      expect(mockIpc.syncThreadFromEngine).toHaveBeenCalledWith("thread-1");

      expect(useChatStore.getState()).toMatchObject({
        status,
        streaming: expectedStreaming,
      });
    },
  );

  it("keeps thread list order after activating an attached Codex thread", async () => {
    const threadNewer: Thread = {
      // 列表中较新的 Codex 会话标识。
      id: "thread-newer",
      // 三个测试会话属于同一工作区。
      workspaceId: "workspace-1",
      // 测试会话未绑定具体仓库。
      // 通过 Codex 引擎判断激活时需要同步。
      engineId: "codex",
      // 已附加的远端 Codex 会话标识。
      engineThreadId: "engine-thread-newer",
      // 非同步中的附加会话元数据。
      engineMetadata: { codexSyncRequired: false },
      // 会话使用的模型标识。
      modelId: "gpt-5.3-codex",
      // 会话标题。
      title: "Newer thread",
      // 会话当前运行状态。
      status: "idle",
      // 本地消息数量统计。
      messageCount: 2,
      // 本地 Token 数量统计。
      totalTokens: 20,
      // 会话创建时间。
      createdAt: "2026-08-20T00:00:00.000Z",
      // 用于列表排序的远端真实活动时间。
      lastActivityAt: "2026-08-24T12:00:00.000Z",
    };
    const threadMiddle: Thread = {
      // 列表中间 Codex 会话标识。
      id: "thread-middle",
      // 三个测试会话属于同一工作区。
      workspaceId: "workspace-1",
      // 测试会话未绑定具体仓库。
      // 通过 Codex 引擎判断激活时需要同步。
      engineId: "codex",
      // 已附加的远端 Codex 会话标识。
      engineThreadId: "engine-thread-middle",
      // 非同步中的附加会话元数据。
      engineMetadata: { codexSyncRequired: false },
      // 会话使用的模型标识。
      modelId: "gpt-5.3-codex",
      // 会话标题。
      title: "Middle thread",
      // 会话当前运行状态。
      status: "idle",
      // 本地消息数量统计。
      messageCount: 1,
      // 本地 Token 数量统计。
      totalTokens: 10,
      // 会话创建时间。
      createdAt: "2026-08-19T00:00:00.000Z",
      // 用于列表排序的远端真实活动时间。
      lastActivityAt: "2026-08-24T11:00:00.000Z",
    };
    const threadOlder: Thread = {
      // 列表中较旧 Codex 会话标识。
      id: "thread-older",
      // 三个测试会话属于同一工作区。
      workspaceId: "workspace-1",
      // 测试会话未绑定具体仓库。
      // 通过 Codex 引擎判断激活时需要同步。
      engineId: "codex",
      // 已附加的远端 Codex 会话标识。
      engineThreadId: "engine-thread-older",
      // 非同步中的附加会话元数据。
      engineMetadata: { codexSyncRequired: false },
      // 会话使用的模型标识。
      modelId: "gpt-5.3-codex",
      // 会话标题。
      title: "Older thread",
      // 会话当前运行状态。
      status: "idle",
      // 本地消息数量统计。
      messageCount: 0,
      // 本地 Token 数量统计。
      totalTokens: 0,
      // 会话创建时间。
      createdAt: "2026-08-18T00:00:00.000Z",
      // 用于列表排序的远端真实活动时间。
      lastActivityAt: "2026-08-24T10:00:00.000Z",
    };
    const updatedMiddle: Thread = {
      ...threadMiddle,
      title: "Middle thread updated",
      status: "completed",
      messageCount: 3,
      totalTokens: 42,
      engineMetadata: {
        codexSyncRequired: false,
        codexTranscriptImported: true,
      },
      lastActivityAt: threadMiddle.lastActivityAt,
    };

    useThreadStore.setState({
      threads: [threadNewer, threadMiddle, threadOlder],
      threadsByWorkspace: {
        "workspace-1": [threadNewer, threadMiddle, threadOlder],
      },
      archivedThreadsByWorkspace: {},
      activeThreadId: null,
      loading: false,
      error: undefined,
    });
    mockIpc.getThreadMessagesWindow.mockResolvedValueOnce({
      messages: [],
      nextCursor: null,
    });
    mockIpc.syncThreadFromEngine.mockResolvedValueOnce(updatedMiddle);

    await useChatStore.getState().setActiveThread("thread-middle");

    expect(mockIpc.syncThreadFromEngine).toHaveBeenCalledWith("thread-middle");
    expect(mockIpc.getThreadMessagesWindow).toHaveBeenCalledWith(
      "thread-middle",
      null,
      80,
    );
    expect(useThreadStore.getState().threads.map((thread) => thread.id)).toEqual([
      "thread-newer",
      "thread-middle",
      "thread-older",
    ]);
    expect(
      useThreadStore
        .getState()
        .threadsByWorkspace["workspace-1"]?.map((thread) => thread.id),
    ).toEqual(["thread-newer", "thread-middle", "thread-older"]);
    expect(useThreadStore.getState().threads[1]).toMatchObject({
      title: "Middle thread updated",
      status: "completed",
      messageCount: 3,
      totalTokens: 42,
    });
    expect(useThreadStore.getState().threads[1]?.lastActivityAt).toBe(
      threadMiddle.lastActivityAt,
    );
    expect(useChatStore.getState().threadId).toBe("thread-middle");
  });

  it("does not let a late bind replace an active optimistic turn", async () => {
    const existingUnlisten = vi.fn();
    const lateUnlisten = vi.fn();
    mockListenThreadEvents.mockImplementationOnce(async () => {
      useChatStore.setState({
        threadId: "thread-1",
        messages: [
          {
            id: "optimistic-user",
            threadId: "thread-1",
            role: "user",
            status: "completed",
            schemaVersion: 1,
            blocks: [{ type: "text", content: "hello" }],
            createdAt: new Date().toISOString(),
            hydration: "full",
            hasDeferredContent: false,
          },
          {
            id: "optimistic-assistant",
            threadId: "thread-1",
            role: "assistant",
            status: "streaming",
            schemaVersion: 1,
            blocks: [],
            createdAt: new Date().toISOString(),
            hydration: "full",
            hasDeferredContent: false,
          },
        ],
        status: "streaming",
        streaming: true,
        unlisten: existingUnlisten,
      });
      return lateUnlisten;
    });

    await useChatStore.getState().setActiveThread("thread-1");

    const state = useChatStore.getState();
    expect(state.streaming).toBe(true);
    expect(state.status).toBe("streaming");
    expect(state.messages.map((message) => message.id)).toEqual([
      "optimistic-user",
      "optimistic-assistant",
    ]);
    expect(lateUnlisten).toHaveBeenCalledTimes(1);
    expect(existingUnlisten).not.toHaveBeenCalled();
  });

  it("marks the thread as awaiting approval while a streamed approval is pending", async () => {
    vi.useFakeTimers();

    let streamHandler: ((event: StreamEvent) => void) | null = null;
    mockListenThreadEvents.mockImplementationOnce(async (_threadId, onEvent) => {
      streamHandler = onEvent;
      return () => {};
    });

    await useChatStore.getState().setActiveThread("thread-1");

    streamHandler!({
      type: "ApprovalRequested",
      approval_id: "approval-runtime-2",
      action_type: "command",
      summary: "Run command",
      details: {},
    });

    await vi.advanceTimersByTimeAsync(20);

    expect(useChatStore.getState()).toMatchObject({
      status: "awaiting_approval",
      streaming: true,
    });

    vi.useRealTimers();
  });

  it("syncs an attached Codex thread before binding the message window after a prior sync", async () => {
    const thread = {
      id: "thread-1",
      workspaceId: "workspace-1",
      engineId: "codex" as const,
      modelId: "gpt-5.3-codex",
      engineThreadId: "engine-thread-1",
      engineMetadata: {
        codexSyncRequired: false,
      },
      title: "Thread 1",
      status: "idle" as const,
      messageCount: 0,
      totalTokens: 0,
      createdAt: new Date().toISOString(),
      lastActivityAt: new Date().toISOString(),
    };

    useThreadStore.setState({
      threads: [thread],
      threadsByWorkspace: {
        "workspace-1": [thread],
      },
      archivedThreadsByWorkspace: {},
      activeThreadId: "thread-1",
      loading: false,
      error: undefined,
    });

    await useChatStore.getState().setActiveThread("thread-1");

    expect(mockIpc.syncThreadFromEngine).toHaveBeenCalledWith("thread-1");
    expect(mockIpc.getThreadMessagesWindow).toHaveBeenCalledWith("thread-1", null, 80);
  });

  it("re-syncs an already selected attached Codex thread", async () => {
    const thread = {
      id: "thread-1",
      workspaceId: "workspace-1",
      engineId: "codex" as const,
      modelId: "gpt-5.3-codex",
      engineThreadId: "engine-thread-1",
      engineMetadata: {},
      title: "Thread 1",
      status: "idle" as const,
      messageCount: 1,
      totalTokens: 0,
      createdAt: new Date().toISOString(),
      lastActivityAt: new Date().toISOString(),
    };
    const previousUnlisten = vi.fn();
    useThreadStore.setState({
      threads: [thread],
      threadsByWorkspace: { "workspace-1": [thread] },
      archivedThreadsByWorkspace: {},
      activeThreadId: "thread-1",
      loading: false,
      error: undefined,
    });
    useChatStore.setState({
      threadId: "thread-1",
      unlisten: previousUnlisten,
      status: "idle",
      streaming: false,
    });

    await useChatStore.getState().setActiveThread("thread-1");

    expect(previousUnlisten).toHaveBeenCalledOnce();
    expect(mockIpc.syncThreadFromEngine).toHaveBeenCalledWith("thread-1");
    expect(mockIpc.getThreadMessagesWindow).toHaveBeenCalledWith("thread-1", null, 80);
  });

  it("normalizes deny approvals to decline after IPC success", async () => {
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-1",
          threadId: "thread-1",
          role: "assistant",
          status: "completed",
          schemaVersion: 1,
          blocks: [
            {
              type: "approval",
              approvalId: "approval-1",
              actionType: "command",
              summary: "Run command",
              details: {},
              status: "pending",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      olderCursor: null,
      hasOlderMessages: false,
      loadingOlderMessages: false,
      olderLoadBlockedUntil: 0,
      status: "awaiting_approval",
      streaming: false,
      usageLimits: null,
      error: undefined,
      unlisten: undefined,
    });

    await useChatStore
      .getState()
      .respondApproval("approval-1", { decision: "deny" } as ApprovalResponse);

    expect(mockIpc.respondApproval).toHaveBeenCalledWith("thread-1", "approval-1", {
      decision: "deny",
    });
    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      {
        type: "approval",
        approvalId: "approval-1",
        actionType: "command",
        summary: "Run command",
        details: {},
        status: "answered",
        decision: "decline",
      },
    ]);
    expect(useChatStore.getState().status).toBe("streaming");
    expect(useChatStore.getState().streaming).toBe(true);
  });

  it("returns failure and rolls back when an approval response is rejected", async () => {
    mockIpc.respondApproval.mockRejectedValueOnce(new Error("approval failed"));
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-1",
          threadId: "thread-1",
          role: "assistant",
          status: "completed",
          schemaVersion: 1,
          blocks: [
            {
              type: "approval",
              approvalId: "approval-1",
              actionType: "command",
              summary: "Run command",
              details: {},
              status: "pending",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      error: undefined,
    });

    const accepted = await useChatStore
      .getState()
      .respondApproval("approval-1", { decision: "accept" }, "thread-1");

    expect(accepted).toBe(false);
    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      { approvalId: "approval-1", status: "pending" },
    ]);
    expect(useChatStore.getState().error).toContain("approval failed");
  });

  it("keeps approval pending during delayed IPC and preserves concurrent messages", async () => {
    const response = deferred<void>();
    mockIpc.respondApproval.mockReturnValueOnce(response.promise);
    useChatStore.setState({
      threadId: "thread-1",
      messages: [
        {
          id: "assistant-1",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [
            {
              type: "approval",
              approvalId: "approval-1",
              actionType: "command",
              summary: "Run command",
              details: {},
              status: "pending",
            },
          ],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
      error: undefined,
    });

    const pendingResponse = useChatStore
      .getState()
      .respondApproval("approval-1", { decision: "accept" }, "thread-1");

    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      { approvalId: "approval-1", status: "pending" },
    ]);

    useChatStore.setState((state) => ({
      messages: [
        ...state.messages,
        {
          id: "assistant-2",
          threadId: "thread-1",
          role: "assistant",
          status: "streaming",
          schemaVersion: 1,
          blocks: [{ type: "text", content: "Still working" }],
          createdAt: new Date().toISOString(),
          hydration: "full",
          hasDeferredContent: false,
        },
      ],
    }));
    response.resolve();

    await expect(pendingResponse).resolves.toBe(true);
    expect(useChatStore.getState().messages).toHaveLength(2);
    expect(useChatStore.getState().messages[0]?.blocks).toMatchObject([
      { approvalId: "approval-1", status: "answered" },
    ]);
    expect(useChatStore.getState().messages[1]?.blocks).toMatchObject([
      { type: "text", content: "Still working" },
    ]);
    expect(useChatStore.getState().status).toBe("streaming");
    expect(useChatStore.getState().streaming).toBe(true);
  });

  it("targets an explicit thread without mutating another visible transcript", async () => {
    mockIpc.respondApproval.mockResolvedValueOnce(undefined);
    const visibleMessages = [
      {
        id: "assistant-2",
        threadId: "thread-2",
        role: "assistant" as const,
        status: "completed" as const,
        schemaVersion: 1,
        blocks: [],
        createdAt: new Date().toISOString(),
        hydration: "full" as const,
        hasDeferredContent: false,
      },
    ];
    useChatStore.setState({
      threadId: "thread-2",
      messages: visibleMessages,
      error: undefined,
    });

    const accepted = await useChatStore
      .getState()
      .respondApproval("approval-1", { decision: "accept" }, "thread-1");

    expect(accepted).toBe(true);
    expect(mockIpc.respondApproval).toHaveBeenCalledWith("thread-1", "approval-1", {
      decision: "accept",
    });
    expect(useChatStore.getState().messages).toBe(visibleMessages);
  });

});
