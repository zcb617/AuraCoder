import { create } from "zustand";
import type { TrustLevel, Workspace } from "../types";
import { ipc } from "../lib/ipc";
import { useTerminalStore } from "./terminalStore";

interface WorkspaceState {
  /** 允许旧测试夹具在迁移期间携带未使用字段；生产状态不保存这些字段。 */
  [key: string]: any;
  /** 当前可用的项目列表。 */
  workspaces: Workspace[];
  /** 已归档的项目列表。 */
  archivedWorkspaces: Workspace[];
  /** 当前项目身份。 */
  activeWorkspaceId: string | null;
  /** 正在同步远程项目历史会话的项目集合。 */
  sshSessionSyncingWorkspaceIds: Record<string, boolean>;
  /** 正在创建远程项目的请求数量。 */
  sshWorkspaceCreationInFlight: number;
  /** 远程会话同步完成通知先到达时暂存的项目集合。 */
  sshSessionSyncCompletedBeforeRegistration: Record<string, boolean>;
  /** 项目列表加载状态。 */
  loading: boolean;
  /** 最近一次项目操作错误。 */
  error?: string;
  /** 加载项目列表并恢复最近项目。 */
  loadWorkspaces: () => Promise<void>;
  /** 刷新归档项目列表。 */
  refreshArchivedWorkspaces: () => Promise<void>;
  /** 打开本地项目。 */
  openWorkspace: (path: string) => Promise<Workspace | null>;
  /** 创建 SSH 远程项目。 */
  createSshWorkspace: (connectionId: string, name: string, rootPath: string) => Promise<Workspace | null>;
  /** 清除远程项目历史会话同步状态。 */
  completeSshSessionSync: (workspaceId: string) => void;
  /** 永久删除项目。 */
  removeWorkspace: (workspaceId: string) => Promise<boolean>;
  /** 恢复归档项目。 */
  restoreWorkspace: (workspaceId: string) => Promise<void>;
  /** 切换当前项目。 */
  setActiveWorkspace: (workspaceId: string) => Promise<void>;
  /** 更新项目级信任等级。 */
  setWorkspaceTrustLevel: (workspaceId: string, trustLevel: TrustLevel) => Promise<void>;
  /** 重新读取项目记录。 */
  rescanWorkspace: (workspaceId: string) => Promise<Workspace | null>;
}

const LAST_WORKSPACE_KEY = "auracoder:lastActiveWorkspaceId";

/** 判断项目是否位于 Linux 临时挂载目录。 */
function isTransientLinuxAppImageRoot(rootPath: string): boolean {
  return /^\/(?:var\/tmp|tmp)\/\.mount_[^/]+(?:\/|$)/.test(rootPath);
}

/** 根据持久化记录选择启动时的项目。 */
function resolveStartupWorkspaceId(workspaces: Workspace[], savedId: string | null): string | null {
  const savedWorkspace = savedId ? workspaces.find((workspace) => workspace.id === savedId) ?? null : null;
  if (savedWorkspace && !isTransientLinuxAppImageRoot(savedWorkspace.rootPath)) return savedWorkspace.id;
  if (!savedId) return null;
  return workspaces.find((workspace) => !isTransientLinuxAppImageRoot(workspace.rootPath))?.id ?? null;
}

/** 在项目切换后异步刷新项目扩展目录。 */
function scheduleActiveWorkspaceExtensionRefresh(workspaceId: string): void {
  void Promise.resolve(ipc.scheduleExtensionCatalogWorkspaceRefresh(workspaceId)).catch(() => {
    // 扩展目录刷新失败不阻断项目启动。
  });
}

export const useWorkspaceStore = create<WorkspaceState>((set, get) => ({
  workspaces: [], archivedWorkspaces: [], activeWorkspaceId: null,
  sshSessionSyncingWorkspaceIds: {}, sshWorkspaceCreationInFlight: 0,
  sshSessionSyncCompletedBeforeRegistration: {}, loading: false, error: undefined,
  loadWorkspaces: async () => {
    set({ loading: true, error: undefined });
    try {
      const workspaces = await ipc.listWorkspaces();
      const activeWorkspaceId = resolveStartupWorkspaceId(workspaces, localStorage.getItem(LAST_WORKSPACE_KEY));
      set({ workspaces, activeWorkspaceId, loading: false });
      if (activeWorkspaceId) {
        const workspace = workspaces.find((item) => item.id === activeWorkspaceId);
        if (workspace?.locationKind !== "ssh") {
          scheduleActiveWorkspaceExtensionRefresh(activeWorkspaceId);
          await useTerminalStore.getState().prepareWorkspaceActivation(activeWorkspaceId);
        }
      }
      await get().refreshArchivedWorkspaces();
    } catch (error) { set({ loading: false, error: String(error) }); }
  },
  refreshArchivedWorkspaces: async () => {
    try { set({ archivedWorkspaces: await ipc.listArchivedWorkspaces() }); }
    catch (error) { set({ error: String(error) }); }
  },
  openWorkspace: async (path) => {
    set({ loading: true, error: undefined });
    try {
      const workspace = await ipc.openWorkspace(path);
      set((state) => ({ workspaces: [workspace, ...state.workspaces.filter((item) => item.id !== workspace.id)], archivedWorkspaces: state.archivedWorkspaces.filter((item) => item.id !== workspace.id), activeWorkspaceId: workspace.id, loading: false }));
      localStorage.setItem(LAST_WORKSPACE_KEY, workspace.id);
      scheduleActiveWorkspaceExtensionRefresh(workspace.id);
      await useTerminalStore.getState().prepareWorkspaceActivation(workspace.id);
      return workspace;
    } catch (error) { set({ loading: false, error: String(error) }); return null; }
  },
  createSshWorkspace: async (connectionId, name, rootPath) => {
    set((state) => ({ loading: true, error: undefined, sshWorkspaceCreationInFlight: state.sshWorkspaceCreationInFlight + 1 }));
    try {
      const workspace = await ipc.createSshWorkspace(connectionId, name, rootPath);
      localStorage.setItem(LAST_WORKSPACE_KEY, workspace.id);
      set((state) => ({ workspaces: [workspace, ...state.workspaces.filter((item) => item.id !== workspace.id)], archivedWorkspaces: state.archivedWorkspaces.filter((item) => item.id !== workspace.id), activeWorkspaceId: workspace.id, loading: false, sshWorkspaceCreationInFlight: Math.max(0, state.sshWorkspaceCreationInFlight - 1) }));
      await useTerminalStore.getState().prepareWorkspaceActivation(workspace.id);
      return workspace;
    } catch (error) { set((state) => ({ loading: false, error: String(error), sshWorkspaceCreationInFlight: Math.max(0, state.sshWorkspaceCreationInFlight - 1) })); return null; }
  },
  completeSshSessionSync: (workspaceId) => set((state) => {
    if (!state.sshSessionSyncingWorkspaceIds[workspaceId]) {
      if (state.sshWorkspaceCreationInFlight === 0) return state;
      return { sshSessionSyncCompletedBeforeRegistration: { ...state.sshSessionSyncCompletedBeforeRegistration, [workspaceId]: true } };
    }
    const next = { ...state.sshSessionSyncingWorkspaceIds }; delete next[workspaceId];
    return { sshSessionSyncingWorkspaceIds: next };
  }),
  removeWorkspace: async (workspaceId) => {
    set({ loading: true, error: undefined });
    try {
      await ipc.deleteWorkspace(workspaceId);
      const remaining = get().workspaces.filter((workspace) => workspace.id !== workspaceId);
      const nextActive = get().activeWorkspaceId === workspaceId ? remaining[0]?.id ?? null : get().activeWorkspaceId;
      set((state) => ({ workspaces: remaining, archivedWorkspaces: state.archivedWorkspaces.filter((workspace) => workspace.id !== workspaceId), activeWorkspaceId: nextActive, loading: false }));
      if (nextActive) { localStorage.setItem(LAST_WORKSPACE_KEY, nextActive); await useTerminalStore.getState().prepareWorkspaceActivation(nextActive); }
      else localStorage.removeItem(LAST_WORKSPACE_KEY);
      return true;
    } catch (error) { set({ loading: false, error: String(error) }); return false; }
  },
  restoreWorkspace: async (workspaceId) => {
    set({ loading: true, error: undefined });
    try {
      const restored = await ipc.restoreWorkspace(workspaceId);
      set((state) => ({ workspaces: [restored, ...state.workspaces.filter((workspace) => workspace.id !== workspaceId)], archivedWorkspaces: state.archivedWorkspaces.filter((workspace) => workspace.id !== workspaceId), activeWorkspaceId: state.activeWorkspaceId ?? restored.id, loading: false }));
      if (get().activeWorkspaceId === restored.id) await useTerminalStore.getState().prepareWorkspaceActivation(restored.id);
    } catch (error) { set({ loading: false, error: String(error) }); }
  },
  setActiveWorkspace: async (workspaceId) => {
    localStorage.setItem(LAST_WORKSPACE_KEY, workspaceId);
    set({ activeWorkspaceId: workspaceId, error: undefined });
    const workspace = get().workspaces.find((item) => item.id === workspaceId);
    if (workspace) { scheduleActiveWorkspaceExtensionRefresh(workspaceId); await useTerminalStore.getState().prepareWorkspaceActivation(workspaceId); }
  },
  setWorkspaceTrustLevel: async (workspaceId, trustLevel) => {
    try {
      await ipc.setWorkspaceTrustLevel(workspaceId, trustLevel);
      set((state) => ({ workspaces: state.workspaces.map((workspace) => workspace.id === workspaceId ? { ...workspace, trustLevel } : workspace), archivedWorkspaces: state.archivedWorkspaces.map((workspace) => workspace.id === workspaceId ? { ...workspace, trustLevel } : workspace) }));
    } catch (error) { set({ error: String(error) }); throw error; }
  },
  rescanWorkspace: async (workspaceId) => {
    const workspace = get().workspaces.find((item) => item.id === workspaceId);
    if (!workspace) return null;
    try {
      const updated = await ipc.openWorkspace(workspace.rootPath);
      set((state) => ({ workspaces: [updated, ...state.workspaces.filter((item) => item.id !== updated.id)] }));
      return updated;
    } catch (error) { set({ error: String(error) }); throw error; }
  },
}));
