import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Workspace } from "../types";

const mockIpc = vi.hoisted(() => ({
  deleteWorkspace: vi.fn(), listArchivedWorkspaces: vi.fn(), listWorkspaces: vi.fn(), openWorkspace: vi.fn(),
  scheduleExtensionCatalogWorkspaceRefresh: vi.fn(), setWorkspaceTrustLevel: vi.fn(),
}));
const mockTerminal = vi.hoisted(() => ({ prepareWorkspaceActivation: vi.fn() }));
vi.mock("../lib/ipc", () => ({ ipc: mockIpc }));
vi.mock("./terminalStore", () => ({ useTerminalStore: { getState: () => mockTerminal } }));

function workspace(id: string, rootPath = `/workspace/${id}`): Workspace {
  return { id, name: id, rootPath, trustLevel: "standard", createdAt: new Date(0).toISOString(), lastOpenedAt: new Date(0).toISOString() };
}

import { useWorkspaceStore } from "./workspaceStore";

describe("workspaceStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("localStorage", { getItem: vi.fn(() => null), setItem: vi.fn(), removeItem: vi.fn() });
    useWorkspaceStore.setState({ workspaces: [], archivedWorkspaces: [], activeWorkspaceId: null, loading: false, error: undefined });
    mockIpc.listWorkspaces.mockResolvedValue([]); mockIpc.listArchivedWorkspaces.mockResolvedValue([]);
    mockIpc.scheduleExtensionCatalogWorkspaceRefresh.mockResolvedValue(undefined); mockTerminal.prepareWorkspaceActivation.mockResolvedValue(undefined);
  });
  it("loads and switches projects without Git selection state", async () => {
    const first = workspace("one"); const second = workspace("two");
    mockIpc.listWorkspaces.mockResolvedValue([first, second]);
    await useWorkspaceStore.getState().loadWorkspaces();
    await useWorkspaceStore.getState().setActiveWorkspace(second.id);
    expect(useWorkspaceStore.getState().activeWorkspaceId).toBe(second.id);
    expect(mockTerminal.prepareWorkspaceActivation).toHaveBeenCalledWith(second.id);
  });
  it("updates project trust level", async () => {
    const current = workspace("one");
    useWorkspaceStore.setState({ workspaces: [current], archivedWorkspaces: [], activeWorkspaceId: current.id });
    mockIpc.setWorkspaceTrustLevel.mockResolvedValue(undefined);
    await useWorkspaceStore.getState().setWorkspaceTrustLevel(current.id, "trusted");
    expect(mockIpc.setWorkspaceTrustLevel).toHaveBeenCalledWith(current.id, "trusted");
    expect(useWorkspaceStore.getState().workspaces[0].trustLevel).toBe("trusted");
  });
});
