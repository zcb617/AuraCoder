import { beforeEach, describe, expect, it, vi } from "vitest";

const mockWorkspaceState = vi.hoisted(() => ({
  /** 当前已激活的工作区标识，用于计算目标布局。 */
  activeWorkspaceId: "workspace-1" as string | null,
  /** 仅切换前端当前工作区。 */
  setActiveWorkspaceLocal: vi.fn(),
}));

const mockTerminalState = vi.hoisted(() => ({
  /** 当前工作区的终端布局，用于计算新草稿目标布局。 */
  workspaces: {} as Record<string, { layoutMode?: string }>,
}));

const mockThreadState = vi.hoisted(() => ({
  /** 清除前端选中的正式会话。 */
  setActiveThread: vi.fn(),
  /** 创建正式会话，仅用于确认新建草稿时不调用。 */
  createThread: vi.fn(),
}));

const mockChatState = vi.hoisted(() => ({
  /** 清除聊天展示并解绑当前正式会话。 */
  setActiveThread: vi.fn(),
}));

const mockComposerState = vi.hoisted(() => ({
  /** 重置工作区的新草稿输入和临时表单状态。 */
  initializeWorkspaceDraft: vi.fn(),
}));

const mockUiState = vi.hoisted(() => ({
  /** 切换到对话主视图。 */
  setActiveView: vi.fn(),
}));

const mockWorkspacePaneState = vi.hoisted(() => ({
  /** 直接应用前端窗格布局，不启动终端。 */
  applyLegacyLayoutMode: vi.fn(),
}));

const mockWorkspacePaneNavigation = vi.hoisted(() => ({
  /** 读取当前工作区的窗格布局模式。 */
  getWorkspacePaneLayoutMode: vi.fn(() => null),
}));

vi.mock("../stores/chatStore", () => ({
  useChatStore: {
    getState: () => mockChatState,
  },
}));

vi.mock("../stores/chatComposerStore", () => ({
  useChatComposerStore: {
    getState: () => mockComposerState,
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

vi.mock("../stores/uiStore", () => ({
  useUiStore: {
    getState: () => mockUiState,
  },
}));

vi.mock("../stores/workspacePaneStore", () => ({
  useWorkspacePaneStore: {
    getState: () => mockWorkspacePaneState,
  },
}));

vi.mock("../stores/workspaceStore", () => ({
  useWorkspaceStore: {
    getState: () => mockWorkspaceState,
  },
}));

vi.mock("./workspacePaneNavigation", () => mockWorkspacePaneNavigation);

import { initializeWorkspaceChat } from "./newThreadActions";

describe("initializeWorkspaceChat", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockWorkspaceState.activeWorkspaceId = "workspace-1";
    mockTerminalState.workspaces = {};
    mockWorkspacePaneNavigation.getWorkspacePaneLayoutMode.mockReturnValue(null);
  });

  // 新建操作只初始化前端草稿，不创建正式线程或触发持久化流程。
  it("initializes an empty workspace chat draft", async () => {
    const result = await initializeWorkspaceChat("workspace-1");

    expect(result).toBe(true);
    expect(mockUiState.setActiveView).toHaveBeenCalledWith("chat");
    expect(mockThreadState.setActiveThread).toHaveBeenCalledWith(null);
    expect(mockChatState.setActiveThread).toHaveBeenCalledWith(null);
    expect(mockComposerState.initializeWorkspaceDraft).toHaveBeenCalledWith("workspace-1");
    expect(mockWorkspaceState.setActiveWorkspaceLocal).toHaveBeenCalledWith("workspace-1");
    expect(mockWorkspacePaneState.applyLegacyLayoutMode).toHaveBeenCalledWith("workspace-1", "chat");
    expect(mockThreadState.createThread).not.toHaveBeenCalled();
  });

  // 缺少工作区标识时不初始化空草稿或修改前端状态。
  it("returns false when the workspace is unavailable", async () => {
    const result = await initializeWorkspaceChat(null);

    expect(result).toBe(false);
    expect(mockUiState.setActiveView).not.toHaveBeenCalled();
    expect(mockThreadState.setActiveThread).not.toHaveBeenCalled();
    expect(mockComposerState.initializeWorkspaceDraft).not.toHaveBeenCalled();
  });
});

/*
 * 旧契约依赖数据库创建、失败 toast 和 threadId 返回，按前端草稿初始化要求停用：
 * it("shows a fixed toast and does not activate a thread when creation fails", async () => {
 *   mockThreadState.createThread.mockRejectedValueOnce(new Error("database failure"));
 *   const result = await createAndActivateWorkspaceThread("workspace-1");
 *   expect(result).toBeNull();
 *   expect(mockToast.error).toHaveBeenCalledWith("Failed to create a new conversation");
 *   expect(mockChatState.setActiveThread).not.toHaveBeenCalled();
 * });
 * it("returns and activates the newly created thread on success", async () => {
 *   mockThreadState.createThread.mockResolvedValue("thread-1");
 *   const result = await createAndActivateWorkspaceThread("workspace-1");
 *   expect(result).toBe("thread-1");
 *   expect(mockChatState.setActiveThread).toHaveBeenCalledWith("thread-1");
 * });
 */
