import { beforeEach, describe, expect, it, vi } from "vitest";

const mockOpenExternal = vi.hoisted(() => vi.fn());
const mockOpenFileAtLocation = vi.hoisted(() => vi.fn());
const mockSetLayoutMode = vi.hoisted(() => vi.fn());
const mockShowSurface = vi.hoisted(() => vi.fn());
const mockWorkspaceState = vi.hoisted(() => ({ activeWorkspaceId: "ws-1", workspaces: [{ id: "ws-1", name: "Workspace", rootPath: "/workspace", trustLevel: "standard", createdAt: "", lastOpenedAt: "" }] }));

vi.mock("@tauri-apps/plugin-shell", () => ({ open: mockOpenExternal }));
vi.mock("../stores/fileStore", () => ({ useFileStore: { getState: () => ({ openFileAtLocation: mockOpenFileAtLocation }) } }));
vi.mock("../stores/terminalStore", () => ({ useTerminalStore: { getState: () => ({ setLayoutMode: mockSetLayoutMode, workspaces: {} }) } }));
vi.mock("../stores/workspacePaneStore", () => ({ collectWorkspacePaneLeaves: vi.fn(() => []), getWorkspacePaneActiveTab: vi.fn(() => null), useWorkspacePaneStore: { getState: () => ({ ensureWorkspace: vi.fn(), showSurface: mockShowSurface, workspaces: {} }) } }));
vi.mock("../stores/uiStore", () => ({ useUiStore: { getState: () => ({ setExplorerOpen: vi.fn(), setActiveView: vi.fn() }) } }));
vi.mock("../stores/workspaceStore", () => ({ useWorkspaceStore: { getState: () => mockWorkspaceState } }));

import { classifyLinkTarget, extractTextLinkMatches, navigateLinkTarget, resolveActiveWorkspaceLocalFileLinkTarget, resolveLocalFileLinkTarget } from "./fileLinkNavigation";
import { DEFAULT_LINK_OPEN_GESTURE } from "./linkOpenSettings";
import { useChatComposerStore } from "../stores/chatComposerStore";

describe("fileLinkNavigation project root", () => {
  beforeEach(() => { vi.clearAllMocks(); useChatComposerStore.setState({ linkOpenGesture: DEFAULT_LINK_OPEN_GESTURE }); });
  it("resolves absolute paths relative to workspace root", () => {
    expect(resolveLocalFileLinkTarget("/workspace/apps/app/src/main.ts#L12", { workspaceRoot: "/workspace" })).toMatchObject({ rootPath: "/workspace", filePath: "apps/app/src/main.ts", line: 12 });
  });
  it("resolves encoded files and Windows workspace paths", () => {
    expect(resolveLocalFileLinkTarget("file:///workspace/docs/My%20File.md#L9", { workspaceRoot: "/workspace" })).toMatchObject({ rootPath: "/workspace", filePath: "docs/My File.md", line: 9 });
    expect(resolveLocalFileLinkTarget("C:\\Work\\project\\src\\app.ts:7:3", { workspaceRoot: "C:/Work/project" })).toMatchObject({ rootPath: "C:/Work/project", filePath: "src/app.ts", line: 7, column: 3 });
  });
  it("resolves relative links only from workspace root", () => {
    expect(resolveLocalFileLinkTarget("src/main.ts:44:7", { workspaceRoot: "/workspace" })).toMatchObject({ rootPath: "/workspace", filePath: "src/main.ts", absolutePath: "/workspace/src/main.ts" });
    expect(resolveActiveWorkspaceLocalFileLinkTarget("src/main.ts")).toMatchObject({ rootPath: "/workspace", filePath: "src/main.ts" });
  });
  it("rejects project-external absolute paths", () => expect(resolveLocalFileLinkTarget("/outside/file.ts", { workspaceRoot: "/workspace" })).toBeNull());
  it("keeps link classification and extraction behavior", () => {
    expect(classifyLinkTarget("https://example.com")).toBe("external");
    expect(extractTextLinkMatches("see /workspace/src/main.ts#L1 and https://example.com/docs")).toHaveLength(2);
  });
  it("keeps configured click behavior", async () => {
    useChatComposerStore.getState().setLinkOpenGesture("click");
    await expect(navigateLinkTarget("/workspace/src/main.ts#L12C4", { shiftKey: false })).resolves.toBe("internal");
    expect(mockOpenFileAtLocation).toHaveBeenCalledWith("/workspace", "src/main.ts", { line: 12, column: 4 });
    expect(mockShowSurface).toHaveBeenCalledWith("ws-1", "editor");
  });
  it("opens external links only through configured gesture", async () => {
    useChatComposerStore.getState().setLinkOpenGesture("shift-click");
    await expect(navigateLinkTarget("https://example.com", { shiftKey: false })).resolves.toBe("ignored");
    await expect(navigateLinkTarget("https://example.com", { shiftKey: true })).resolves.toBe("external");
    expect(mockOpenExternal).toHaveBeenCalledWith("https://example.com");
  });
});
