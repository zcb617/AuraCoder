import { beforeEach, describe, expect, it, vi } from "vitest";
import type { EngineInfo, Thread } from "../types";

const mockIpc = vi.hoisted(() => ({
  attachCodexRemoteThread: vi.fn(),
  archiveThread: vi.fn(),
  restoreThread: vi.fn(),
  listCodexRemoteThreads: vi.fn(),
  listThreads: vi.fn(),
}));

const mockEngineState = vi.hoisted(() => ({
  engines: [] as EngineInfo[],
}));

vi.mock("../lib/ipc", () => ({
  ipc: mockIpc,
}));

vi.mock("./engineStore", () => ({
  useEngineStore: {
    getState: () => mockEngineState,
  },
}));

import { useThreadStore } from "./threadStore";

function makeThread(id: string, lastActivityAt = new Date(0).toISOString()): Thread {
  return {
    id,
    workspaceId: "workspace-1",
    repoId: null,
    engineId: "codex",
    modelId: "gpt-5.6",
    engineThreadId: `engine-${id}`,
    title: id,
    status: "idle",
    messageCount: 0,
    totalTokens: 0,
    createdAt: new Date(0).toISOString(),
    lastActivityAt,
  };
}

function makeCodexEngine(): EngineInfo {
  return {
    id: "codex",
    name: "Codex",
    models: [
      {
        id: "gpt-5.6",
        displayName: "GPT-5.6",
        description: "",
        hidden: false,
        isDefault: true,
        inputModalities: [],
        attachmentModalities: [],
        supportsPersonality: false,
        defaultReasoningEffort: "medium",
        supportedReasoningEfforts: [],
      },
    ],
    capabilities: {
      permissionModes: [],
      sandboxModes: [],
      approvalDecisions: [],
    },
  };
}

describe("threadStore remote Codex discovery", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("localStorage", {
      getItem: vi.fn(() => null),
      setItem: vi.fn(),
      removeItem: vi.fn(),
      clear: vi.fn(),
    });
    mockEngineState.engines = [makeCodexEngine()];
    mockIpc.attachCodexRemoteThread.mockResolvedValue(makeThread("attached"));
    mockIpc.archiveThread.mockResolvedValue(undefined);
    mockIpc.listThreads.mockResolvedValue([makeThread("local")]);
    useThreadStore.setState({
      threads: [],
      threadsByWorkspace: {},
      archivedThreadsByWorkspace: {},
      activeThreadId: null,
      loading: false,
      error: undefined,
    });
  });

  it("attaches every unlinked remote Codex thread before refreshing a workspace", async () => {
    mockIpc.listCodexRemoteThreads
      .mockResolvedValueOnce({
        threads: [
          {
            engineThreadId: "remote-1",
            title: "Remote one",
            preview: "",
            cwd: "/workspace",
            createdAt: new Date(0).toISOString(),
            updatedAt: new Date(0).toISOString(),
            modelId: "gpt-5.6-terra",
            reasoningEffort: "high",
            modelProvider: "openai",
            sourceKind: "appServer",
            statusType: "idle",
            activeFlags: [],
            archived: false,
            localThreadId: null,
          },
          {
            engineThreadId: "already-linked",
            title: "Existing",
            preview: "",
            cwd: "/workspace",
            createdAt: new Date(0).toISOString(),
            updatedAt: new Date(0).toISOString(),
            modelId: "gpt-5.6-sol",
            reasoningEffort: "medium",
            modelProvider: "openai",
            sourceKind: "appServer",
            statusType: "idle",
            activeFlags: [],
            archived: false,
            localThreadId: "local",
          },
        ],
        nextCursor: "page-2",
      })
      .mockResolvedValueOnce({
        threads: [
          {
            engineThreadId: "remote-2",
            title: "Remote two",
            preview: "",
            cwd: "/workspace",
            createdAt: new Date(0).toISOString(),
            updatedAt: new Date(0).toISOString(),
            modelId: "gpt-5.6-terra",
            reasoningEffort: "xhigh",
            modelProvider: "openai",
            sourceKind: "appServer",
            statusType: "idle",
            activeFlags: [],
            archived: false,
            localThreadId: null,
          },
        ],
        nextCursor: null,
      });

    await useThreadStore.getState().refreshThreads("workspace-1");

    expect(mockIpc.attachCodexRemoteThread).toHaveBeenNthCalledWith(
      1,
      "workspace-1",
      "remote-1",
    );
    expect(mockIpc.attachCodexRemoteThread).toHaveBeenNthCalledWith(
      2,
      "workspace-1",
      "remote-2",
    );
    expect(mockIpc.attachCodexRemoteThread).toHaveBeenCalledTimes(2);
    expect(mockIpc.listThreads).toHaveBeenCalledWith("workspace-1");
    expect(useThreadStore.getState().threads).toEqual([makeThread("local")]);
  });

  it("attaches an existing local Codex thread when the remote activity time changes", async () => {
    mockIpc.listCodexRemoteThreads.mockResolvedValueOnce({
      threads: [
        {
          engineThreadId: "remote-updated",
          title: "Remote updated",
          preview: "",
          cwd: "/workspace",
          createdAt: new Date(0).toISOString(),
          updatedAt: new Date(1000).toISOString(),
          modelId: "gpt-5.6-terra",
          reasoningEffort: "high",
          modelProvider: "openai",
          sourceKind: "appServer",
          statusType: "idle",
          activeFlags: [],
          archived: false,
          localThreadId: "local",
        },
      ],
      nextCursor: null,
    });

    await useThreadStore.getState().refreshThreads("workspace-1");

    expect(mockIpc.attachCodexRemoteThread).toHaveBeenCalledTimes(1);
    expect(mockIpc.attachCodexRemoteThread).toHaveBeenCalledWith(
      "workspace-1",
      "remote-updated",
    );
  });

  it("keeps the explicit new-conversation selection during a workspace refresh", async () => {
    mockIpc.listCodexRemoteThreads.mockResolvedValueOnce({
      threads: [],
      nextCursor: null,
    });
    useThreadStore.setState({ activeThreadId: null });

    await useThreadStore.getState().refreshThreads("workspace-1");

    expect(useThreadStore.getState().threads).toEqual([makeThread("local")]);
    expect(useThreadStore.getState().activeThreadId).toBeNull();
  });

  it("keeps local threads visible when remote Codex discovery fails", async () => {
    const warning = vi.spyOn(console, "warn").mockImplementation(() => {});
    mockIpc.listCodexRemoteThreads.mockRejectedValueOnce(new Error("Codex unavailable"));

    await useThreadStore.getState().refreshThreads("workspace-1");

    expect(mockIpc.listThreads).toHaveBeenCalledWith("workspace-1");
    expect(useThreadStore.getState().threads).toEqual([makeThread("local")]);
    expect(useThreadStore.getState().error).toBeUndefined();
    warning.mockRestore();
  });

  it("rethrows archive failures so the sidebar can show an engine-specific prompt", async () => {
    const thread = makeThread("local");
    const error = new Error("thread already has an active writer");
    mockIpc.archiveThread.mockRejectedValueOnce(error);
    useThreadStore.setState({
      threads: [thread],
      threadsByWorkspace: { "workspace-1": [thread] },
      archivedThreadsByWorkspace: {},
      activeThreadId: thread.id,
      loading: false,
      error: undefined,
    });

    await expect(useThreadStore.getState().removeThread(thread.id)).rejects.toBe(error);
    expect(useThreadStore.getState().threads).toEqual([thread]);
    expect(useThreadStore.getState().error).toBe(String(error));
  });

  it("keeps archived thread state when restore is rejected", async () => {
    const archivedThread = makeThread("archived");
    const error = new Error("restore rejected");
    mockIpc.restoreThread.mockRejectedValueOnce(error);
    useThreadStore.setState({
      threads: [],
      threadsByWorkspace: { "workspace-1": [] },
      archivedThreadsByWorkspace: { "workspace-1": [archivedThread] },
      activeThreadId: null,
      loading: false,
      error: undefined,
    });

    await expect(useThreadStore.getState().restoreThread(archivedThread.id)).rejects.toBe(error);

    expect(mockIpc.restoreThread).toHaveBeenCalledWith(archivedThread.id);
    expect(useThreadStore.getState().archivedThreadsByWorkspace).toEqual({
      "workspace-1": [archivedThread],
    });
    expect(useThreadStore.getState().threads).toEqual([]);
    expect(useThreadStore.getState().loading).toBe(false);
    expect(useThreadStore.getState().error).toBe(String(error));
  });
});
