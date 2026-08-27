import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GitFileCompare, ReadFileResult } from "../types";

const mockIpc = vi.hoisted(() => ({ readFile: vi.fn(), writeFile: vi.fn(), getGitFileCompare: vi.fn() }));
const mockGitStore = vi.hoisted(() => ({ invalidateWorkspaceCache: vi.fn(), refresh: vi.fn() }));
const mockWorkspace = vi.hoisted(() => ({ activeWorkspaceId: "ws-1", workspaces: [{ id: "ws-1", locationKind: "local", rootPath: "/workspace" }] }));
vi.mock("../lib/ipc", () => ({ ipc: mockIpc }));
vi.mock("./gitStore", () => ({ useGitStore: { getState: () => mockGitStore } }));
vi.mock("./workspaceStore", () => ({ useWorkspaceStore: { getState: () => mockWorkspace } }));
vi.mock("./terminalStore", () => ({ useTerminalStore: { getState: () => ({}) } }));
vi.mock("./toastStore", () => ({ toast: { success: vi.fn(), error: vi.fn(), warning: vi.fn() } }));
vi.mock("../i18n", () => ({ t: (key: string) => key }));
vi.mock("../components/editor/CodeMirrorEditor", () => ({ destroyCachedEditor: vi.fn() }));
import { useFileStore } from "./fileStore";

const compare: GitFileCompare = { source: "changes", baseContent: "before\n", modifiedContent: "after\n", baseLabel: "Index", modifiedLabel: "Working Tree", changeType: "modified", hasStagedChanges: false, hasUnstagedChanges: true, isBinary: false, isEditable: true, fallbackReason: null };
const readResult: ReadFileResult = { content: "plain\n", sizeBytes: 6, isBinary: false, version: "v1" };

describe("fileStore workspace root", () => {
  beforeEach(() => { vi.clearAllMocks(); mockIpc.readFile.mockResolvedValue(readResult); mockIpc.writeFile.mockResolvedValue({ version: "v2" }); mockIpc.getGitFileCompare.mockResolvedValue(compare); mockGitStore.refresh.mockResolvedValue(undefined); useFileStore.setState({ tabs: [], activeTabId: null, pendingCloseTabId: null }); });
  it("opens a Git diff with workspace identity and relative file path", async () => {
    await useFileStore.getState().openGitDiffFile("ws-1", "apps/app/source/page.tsx", { source: "changes" });
    expect(mockIpc.getGitFileCompare).toHaveBeenCalledWith("ws-1", "apps/app/source/page.tsx", "changes");
    expect(useFileStore.getState().tabs[0]).toMatchObject({ workspaceId: "ws-1", rootPath: "/workspace", filePath: "apps/app/source/page.tsx", gitFilePath: "apps/app/source/page.tsx" });
  });
  it("opens and saves ordinary files under the project root", async () => {
    await useFileStore.getState().openFile("/workspace", "README.md");
    const tab = useFileStore.getState().tabs[0]!;
    useFileStore.getState().setTabContent(tab.id, "updated\n");
    await useFileStore.getState().saveTab(tab.id);
    expect(mockIpc.readFile).toHaveBeenCalledWith("/workspace", "README.md");
    expect(mockIpc.writeFile).toHaveBeenCalledWith("/workspace", "README.md", "updated\n", "v1");
  });
});
