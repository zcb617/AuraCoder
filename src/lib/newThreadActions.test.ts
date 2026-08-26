import { beforeEach, describe, expect, it, vi } from "vitest";

const mockToast = vi.hoisted(() => ({
  // 向用户展示新建会话失败的固定业务提示。
  error: vi.fn(),
}));

const mockWorkspaceState = vi.hoisted(() => ({
  // 当前已激活的工作区标识，用于决定是否切换工作区。
  activeWorkspaceId: "workspace-1" as string | null,
  // 激活目标工作区并完成其上下文准备。
  setActiveWorkspace: vi.fn(),
  // 清除当前工作区的活动仓库选择。
  setActiveRepo: vi.fn(),
}));

const mockTerminalState = vi.hoisted(() => ({
  // 工作区的终端布局，用于计算新会话目标布局。
  workspaces: {} as Record<string, { layoutMode?: string }>,
}));

const mockThreadState = vi.hoisted(() => ({
  // 创建会话并返回本地会话标识。
  createThread: vi.fn(),
}));

const mockUiState = vi.hoisted(() => ({
  // 切换到对话主视图。
  setActiveView: vi.fn(),
}));

const mockChatState = vi.hoisted(() => ({
  // 激活新创建的会话。
  setActiveThread: vi.fn(),
}));

const mockWorkspacePaneNavigation = vi.hoisted(() => ({
  // 应用新会话所需的工作区窗格布局。
  applyWorkspaceLayoutMode: vi.fn(),
  // 读取当前工作区的窗格布局模式。
  getWorkspacePaneLayoutMode: vi.fn(() => null),
}));

const mockT = vi.hoisted(() => vi.fn((key: string) => {
  if (key === "app:sidebar.newThreadFailed") {
    return "Failed to create a new conversation";
  }
  return "New Thread";
}));

vi.mock("../i18n", () => ({
  t: mockT,
}));

vi.mock("../stores/chatStore", () => ({
  useChatStore: {
    getState: () => mockChatState,
  },
}));

vi.mock("../stores/terminalStore", () => ({
  useTerminalStore: {
    getState: () => mockTerminalState,
  },
}));

vi.mock("../stores/threadStore", () => ({
  useThreadStore: {
    getState: () => mockThreadState,
  },
}));

vi.mock("../stores/toastStore", () => ({
  toast: mockToast,
}));

vi.mock("../stores/uiStore", () => ({
  useUiStore: {
    getState: () => mockUiState,
  },
}));

vi.mock("../stores/workspaceStore", () => ({
  useWorkspaceStore: {
    getState: () => mockWorkspaceState,
  },
}));

vi.mock("./workspacePaneNavigation", () => mockWorkspacePaneNavigation);

import { createAndActivateWorkspaceThread } from "./newThreadActions";

describe("createAndActivateWorkspaceThread", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockWorkspaceState.activeWorkspaceId = "workspace-1";
    mockWorkspaceState.setActiveWorkspace.mockResolvedValue(undefined);
    mockWorkspaceState.setActiveRepo.mockImplementation(() => undefined);
    mockTerminalState.workspaces = {};
    mockThreadState.createThread.mockResolvedValue("thread-1");
    mockUiState.setActiveView.mockImplementation(() => undefined);
    mockChatState.setActiveThread.mockResolvedValue(undefined);
    mockWorkspacePaneNavigation.getWorkspacePaneLayoutMode.mockReturnValue(null);
    mockT.mockImplementation((key: string) =>
      key === "app:sidebar.newThreadFailed"
        ? "Failed to create a new conversation"
        : "New Thread",
    );
  });

  // 创建会话失败时仅展示固定业务文案，不泄露原始错误，也不激活不存在的会话。
  it("shows a fixed toast and does not activate a thread when creation fails", async () => {
    const error = new Error("database failure at /private/internal/aura.sqlite");
    mockThreadState.createThread.mockRejectedValueOnce(error);

    const result = await createAndActivateWorkspaceThread("workspace-1");

    expect(result).toBeNull();
    expect(mockToast.error).toHaveBeenCalledTimes(1);
    expect(mockToast.error).toHaveBeenCalledWith("Failed to create a new conversation");
    expect(mockToast.error.mock.calls[0]?.[0]).not.toContain(error.message);
    expect(mockChatState.setActiveThread).not.toHaveBeenCalled();
  });

  // 创建会话成功时返回会话标识，并保持新建后的会话激活行为。
  it("returns and activates the newly created thread on success", async () => {
    const result = await createAndActivateWorkspaceThread("workspace-1");

    expect(result).toBe("thread-1");
    expect(mockToast.error).not.toHaveBeenCalled();
    expect(mockChatState.setActiveThread).toHaveBeenCalledWith("thread-1");
  });
});
