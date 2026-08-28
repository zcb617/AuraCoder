import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { COMMAND_PALETTE_DEFAULT_LAUNCH } from "../lib/commandPalette";

type UiStoreModule = typeof import("./uiStore");

function createStorageStub() {
  const storage = new Map<string, string>();
  return {
    getItem: vi.fn((key: string) => storage.get(key) ?? null),
    setItem: vi.fn((key: string, value: string) => {
      storage.set(key, value);
    }),
    removeItem: vi.fn((key: string) => {
      storage.delete(key);
    }),
    clear: vi.fn(() => {
      storage.clear();
    }),
  };
}

describe("uiStore focus mode", () => {
  let useUiStore: UiStoreModule["useUiStore"];

  beforeEach(async () => {
    vi.resetModules();
    vi.stubGlobal("localStorage", createStorageStub());
    ({ useUiStore } = await import("./uiStore"));
    useUiStore.setState({
      showSidebar: true,
      sidebarPinned: true,
      showGitPanel: true,
      gitPanelPinned: true,
      showExplorer: true,
      focusMode: false,
      focusModeSnapshot: null,
      activeView: "chat",
      settingsSection: "overview",
      settingsWorkspaceId: null,
      usageLimitsModalOpen: false,
      commandPaletteOpen: false,
      commandPaletteLaunch: COMMAND_PALETTE_DEFAULT_LAUNCH,
      messageFocusTarget: null,
      imageAttachmentPreview: null,
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("captures the current shell state and hides the left sidebar on entry", () => {
    useUiStore.getState().setFocusMode(true);

    expect(useUiStore.getState()).toMatchObject({
      focusMode: true,
      showSidebar: false,
      showGitPanel: true,
      focusModeSnapshot: {
        showSidebar: true,
        showGitPanel: true,
      },
    });
  });

  it("keeps sidebar and git toggles working while focus mode is active", () => {
    const state = useUiStore.getState();

    state.setFocusMode(true);
    state.toggleSidebar();
    state.toggleGitPanel();

    expect(useUiStore.getState()).toMatchObject({
      focusMode: true,
      showSidebar: true,
      showGitPanel: false,
    });
  });

  it("restores the pre-focus shell state when leaving focus mode", () => {
    useUiStore.setState({
      showSidebar: true,
      showGitPanel: false,
      gitPanelPinned: true,
      focusMode: false,
      focusModeSnapshot: null,
    });

    const state = useUiStore.getState();
    state.setFocusMode(true);
    state.toggleSidebar();
    state.toggleGitPanel();
    state.toggleFocusMode();

    expect(useUiStore.getState()).toMatchObject({
      focusMode: false,
      showSidebar: true,
      showGitPanel: false,
      focusModeSnapshot: null,
    });
  });

  it("does not overwrite the original snapshot on repeated activation", () => {
    useUiStore.setState({
      showSidebar: false,
      showGitPanel: true,
      gitPanelPinned: false,
      focusMode: false,
      focusModeSnapshot: null,
    });

    const state = useUiStore.getState();
    state.setFocusMode(true);
    state.toggleGitPanel();
    state.setFocusMode(true);
    state.setFocusMode(false);

    expect(useUiStore.getState()).toMatchObject({
      focusMode: false,
      showSidebar: false,
      showGitPanel: true,
      gitPanelPinned: false,
      focusModeSnapshot: null,
    });
  });

  it("keeps git pin state separate from visibility toggles", () => {
    const state = useUiStore.getState();

    state.setGitPanelPinned(false);
    state.toggleGitPanel();
    state.toggleGitPanel();

    expect(useUiStore.getState()).toMatchObject({
      showGitPanel: true,
      gitPanelPinned: false,
    });
  });

  it("persists explicit Git panel visibility changes", () => {
    const storage = globalThis.localStorage as unknown as ReturnType<typeof createStorageStub>;

    useUiStore.getState().setGitPanelVisible(false);

    expect(storage.setItem).toHaveBeenCalledWith("auracoder:gitPanelVisible", "false");
    expect(useUiStore.getState().showGitPanel).toBe(false);
  });

  it("restores saved Git panel visibility when the app starts", async () => {
    const storage = globalThis.localStorage as unknown as ReturnType<typeof createStorageStub>;
    storage.setItem("auracoder:gitPanelVisible", "false");

    vi.resetModules();
    const reloadedModule = await import("./uiStore");

    expect(reloadedModule.useUiStore.getState().showGitPanel).toBe(false);
  });

  it("persists git pin state changes and forces the panel visible", () => {
    const storage = globalThis.localStorage as unknown as ReturnType<typeof createStorageStub>;
    const state = useUiStore.getState();

    useUiStore.setState({ showGitPanel: false, gitPanelPinned: true });
    state.toggleGitPanelPin();

    expect(storage.setItem).toHaveBeenCalledWith("auracoder:gitPanelPinned", "false");
    expect(storage.setItem).toHaveBeenCalledWith("auracoder:gitPanelVisible", "true");
    expect(useUiStore.getState()).toMatchObject({
      showGitPanel: true,
      gitPanelPinned: false,
    });
  });

  it("opens a draft image in the visible user attachment tool", () => {
    useUiStore.setState({ showGitPanel: false, activeRightTool: "git" });

    useUiStore.getState().openImageAttachmentPreview("workspace-a", "image-1");

    expect(useUiStore.getState()).toMatchObject({
      showGitPanel: true,
      activeRightTool: "attachments",
      imageAttachmentPreview: {
        source: "draft",
        workspaceId: "workspace-a",
        attachmentId: "image-1",
      },
    });
  });

  it("opens a sent image in the visible user attachment tool", () => {
    useUiStore.setState({ showGitPanel: false, activeRightTool: "git" });

    useUiStore.getState().openMessageImageAttachmentPreview("workspace-a", {
      type: "attachment",
      fileName: "result.png",
      filePath: "/home/tester/.cache/auracoder/attachments/result.png",
      previewFilePath: "C:\\AuraCoder\\attachments\\message-images\\result.png",
      sizeBytes: 128,
      mimeType: "image/png",
      isRemote: true,
    });

    expect(useUiStore.getState()).toMatchObject({
      showGitPanel: true,
      activeRightTool: "attachments",
      imageAttachmentPreview: {
        source: "message",
        workspaceId: "workspace-a",
        attachment: {
          type: "attachment",
          fileName: "result.png",
          previewFilePath: "C:\\AuraCoder\\attachments\\message-images\\result.png",
          isRemote: true,
        },
      },
    });
  });

  it("persists explicit explorer visibility changes", () => {
    const storage = globalThis.localStorage as unknown as ReturnType<typeof createStorageStub>;

    useUiStore.getState().setExplorerOpen(false);

    expect(storage.setItem).toHaveBeenCalledWith("auracoder:explorerOpen", "false");
    expect(useUiStore.getState().showExplorer).toBe(false);
  });

  it("opens the command palette with structured launch defaults", () => {
    useUiStore.getState().openCommandPalette({ variant: "search", initialQuery: "?", searchScope: "threads" });

    expect(useUiStore.getState()).toMatchObject({
      commandPaletteOpen: true,
      commandPaletteLaunch: {
        variant: "search",
        initialQuery: "?",
        searchScope: "threads",
      },
    });
  });

  it("resets command palette launch state when closing", () => {
    const state = useUiStore.getState();
    state.openCommandPalette({ variant: "search", initialQuery: "?", searchScope: "files" });
    state.closeCommandPalette();

    expect(useUiStore.getState()).toMatchObject({
      commandPaletteOpen: false,
      commandPaletteLaunch: COMMAND_PALETTE_DEFAULT_LAUNCH,
    });
  });

  it("opens global settings without discarding the selected workspace", () => {
    useUiStore.setState({ settingsWorkspaceId: "workspace-1" });

    useUiStore.getState().openSettings();

    expect(useUiStore.getState()).toMatchObject({
      activeView: "settings",
      settingsSection: "overview",
      settingsWorkspaceId: "workspace-1",
    });
  });

  it("opens workspace settings at the general section", () => {
    useUiStore.getState().openWorkspaceSettings("workspace-2");

    expect(useUiStore.getState()).toMatchObject({
      activeView: "settings",
      settingsSection: "workspace-general",
      settingsWorkspaceId: "workspace-2",
    });
  });

  it("opens and closes usage limits without changing the active view", () => {
    useUiStore.setState({ activeView: "chat" });

    useUiStore.getState().openUsageLimitsModal();

    expect(useUiStore.getState()).toMatchObject({
      activeView: "chat",
      usageLimitsModalOpen: true,
    });

    useUiStore.getState().closeUsageLimitsModal();

    expect(useUiStore.getState()).toMatchObject({
      activeView: "chat",
      usageLimitsModalOpen: false,
    });
  });
});
