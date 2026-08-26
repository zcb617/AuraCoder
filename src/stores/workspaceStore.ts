import { create } from "zustand";
import type { Repo, TrustLevel, Workspace } from "../types";
import { ipc } from "../lib/ipc";
import { useGitStore } from "./gitStore";
import { useTerminalStore } from "./terminalStore";

interface SetActiveRepoOptions {
  remember?: boolean;
}

interface WorkspaceState {
  workspaces: Workspace[];
  archivedWorkspaces: Workspace[];
  activeWorkspaceId: string | null;
  /** 正在后台同步远端项目历史会话的 SSH workspace 集合。 */
  sshSessionSyncingWorkspaceIds: Record<string, boolean>;
  /** 正在等待后端返回新 SSH workspace 的创建请求数量。 */
  sshWorkspaceCreationInFlight: number;
  /** 后端完成通知先于 workspace 前端登记时暂存的 workspace 集合。 */
  sshSessionSyncCompletedBeforeRegistration: Record<string, boolean>;
  repos: Repo[];
  activeRepoId: string | null;
  reposLoading: boolean;
  loading: boolean;
  error?: string;
  loadWorkspaces: () => Promise<void>;
  refreshArchivedWorkspaces: () => Promise<void>;
  openWorkspace: (path: string, scanDepth?: number) => Promise<Workspace | null>;
  createSshWorkspace: (
    connectionId: string,
    name: string,
    rootPath: string,
    scanDepth?: number,
  ) => Promise<Workspace | null>;
  /** 清除远端项目历史会话同步状态，并保留已有会话数据。 */
  completeSshSessionSync: (workspaceId: string) => void;
  removeWorkspace: (workspaceId: string) => Promise<boolean>;
  restoreWorkspace: (workspaceId: string) => Promise<void>;
  loadRepos: (workspaceId: string) => Promise<void>;
  setActiveWorkspace: (workspaceId: string) => Promise<void>;
  setActiveRepo: (repoId: string | null, options?: SetActiveRepoOptions) => void;
  setRepoGitActive: (repoId: string, isActive: boolean) => Promise<void>;
  setWorkspaceGitActiveRepos: (workspaceId: string, repoIds: string[]) => Promise<void>;
  hasWorkspaceGitSelection: (workspaceId: string) => Promise<boolean>;
  setRepoTrustLevel: (repoId: string, trustLevel: TrustLevel) => Promise<void>;
  setAllReposTrustLevel: (trustLevel: TrustLevel) => Promise<void>;
  rescanWorkspace: (workspaceId: string, scanDepth?: number) => Promise<Workspace | null>;
}

const LAST_WORKSPACE_KEY = "auracoder:lastActiveWorkspaceId";
const LAST_REPO_BY_WORKSPACE_KEY = "auracoder:lastActiveRepoByWorkspace";
let reposLoadSeq = 0;

type LastRepoByWorkspace = Record<string, string>;

function isTransientLinuxAppImageRoot(rootPath: string): boolean {
  return /^\/(?:var\/tmp|tmp)\/\.mount_[^/]+(?:\/|$)/.test(rootPath);
}

function resolveStartupWorkspaceId(
  workspaces: Workspace[],
  savedId: string | null,
): string | null {
  const savedWorkspace = savedId
    ? workspaces.find((workspace) => workspace.id === savedId) ?? null
    : null;

  if (savedWorkspace && !isTransientLinuxAppImageRoot(savedWorkspace.rootPath)) {
    return savedWorkspace.id;
  }

  if (!savedId) {
    return null;
  }

  return (
    workspaces.find((workspace) => !isTransientLinuxAppImageRoot(workspace.rootPath))?.id ?? null
  );
}

function readLastRepoByWorkspace(): LastRepoByWorkspace {
  try {
    const raw = localStorage.getItem(LAST_REPO_BY_WORKSPACE_KEY);
    if (!raw) {
      return {};
    }

    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {};
    }

    const next: LastRepoByWorkspace = {};
    for (const [workspaceId, repoId] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof workspaceId !== "string" || typeof repoId !== "string") {
        continue;
      }
      const normalizedRepoId = repoId.trim();
      if (!normalizedRepoId) {
        continue;
      }
      next[workspaceId] = normalizedRepoId;
    }
    return next;
  } catch {
    return {};
  }
}

function writeLastRepoByWorkspace(next: LastRepoByWorkspace): void {
  try {
    localStorage.setItem(LAST_REPO_BY_WORKSPACE_KEY, JSON.stringify(next));
  } catch {
    // localStorage unavailable or full; ignore persistence failure.
  }
}

function rememberLastRepo(workspaceId: string, repoId: string): void {
  const current = readLastRepoByWorkspace();
  if (current[workspaceId] === repoId) {
    return;
  }
  current[workspaceId] = repoId;
  writeLastRepoByWorkspace(current);
}

function scheduleActiveWorkspaceExtensionRefresh(workspaceId: string): void {
  void Promise.resolve(ipc.scheduleExtensionCatalogWorkspaceRefresh(workspaceId)).catch(() => {
    // Catalog refresh is background work. It must not block workspace startup
    // or turn an unavailable provider into a workspace-loading failure.
  });
}

export function resolveActiveRepoId(
  workspaceId: string,
  repos: Repo[],
  currentActiveRepoId: string | null,
): string | null {
  if (!repos.length) {
    return null;
  }

  if (currentActiveRepoId && repos.some((repo) => repo.id === currentActiveRepoId)) {
    return currentActiveRepoId;
  }

  const persisted = readLastRepoByWorkspace()[workspaceId];
  if (persisted && repos.some((repo) => repo.id === persisted && repo.isActive)) {
    return persisted;
  }

  return repos.find((repo) => repo.isActive)?.id ?? repos[0]?.id ?? null;
}

export const useWorkspaceStore = create<WorkspaceState>((set, get) => ({
  workspaces: [],
  archivedWorkspaces: [],
  activeWorkspaceId: null,
  sshSessionSyncingWorkspaceIds: {},
  sshWorkspaceCreationInFlight: 0,
  sshSessionSyncCompletedBeforeRegistration: {},
  repos: [],
  activeRepoId: null,
  reposLoading: false,
  loading: false,
  loadWorkspaces: async () => {
    set({ loading: true, error: undefined });
    try {
      const workspaces = await ipc.listWorkspaces();
      const savedId = localStorage.getItem(LAST_WORKSPACE_KEY);
      const activeWorkspaceId = resolveStartupWorkspaceId(workspaces, savedId);
      set({ workspaces, activeWorkspaceId, loading: false });
      if (activeWorkspaceId) {
        const activeWorkspace = workspaces.find((workspace) => workspace.id === activeWorkspaceId);
        if (activeWorkspace?.locationKind !== "ssh") {
          scheduleActiveWorkspaceExtensionRefresh(activeWorkspaceId);
          await useTerminalStore.getState().prepareWorkspaceActivation(activeWorkspaceId);
          useGitStore.getState().loadDraftsForWorkspace(activeWorkspaceId);
        }
        await get().loadRepos(activeWorkspaceId);
      }
      await get().refreshArchivedWorkspaces();
    } catch (error) {
      set({ loading: false, error: String(error) });
    }
  },
  refreshArchivedWorkspaces: async () => {
    try {
      const archivedWorkspaces = await ipc.listArchivedWorkspaces();
      set({ archivedWorkspaces });
    } catch (error) {
      set({ error: String(error) });
    }
  },
  openWorkspace: async (path, scanDepth) => {
    set({ loading: true, error: undefined });
    try {
      const workspace = await ipc.openWorkspace(path, scanDepth);
      const current = get().workspaces.filter((item) => item.id !== workspace.id);
      const workspaces = [workspace, ...current];
      set((state) => ({
        workspaces,
        archivedWorkspaces: state.archivedWorkspaces.filter((item) => item.id !== workspace.id),
        activeWorkspaceId: workspace.id,
        loading: false,
      }));
      scheduleActiveWorkspaceExtensionRefresh(workspace.id);
      await useTerminalStore.getState().prepareWorkspaceActivation(workspace.id);
      await get().loadRepos(workspace.id);
      return workspace;
    } catch (error) {
      set({ loading: false, error: String(error) });
      return null;
    }
  },
  createSshWorkspace: async (connectionId, name, rootPath, scanDepth) => {
    set((state) => ({
      loading: true,
      error: undefined,
      sshWorkspaceCreationInFlight: state.sshWorkspaceCreationInFlight + 1,
    }));
    try {
      const workspace = await ipc.createSshWorkspace(connectionId, name, rootPath, scanDepth);
      localStorage.setItem(LAST_WORKSPACE_KEY, workspace.id);
      set((state) => {
        const completedBeforeRegistration =
          state.sshSessionSyncCompletedBeforeRegistration[workspace.id] === true;
        const nextCompletedBeforeRegistration = {
          ...state.sshSessionSyncCompletedBeforeRegistration,
        };
        delete nextCompletedBeforeRegistration[workspace.id];
        return {
          sshSessionSyncingWorkspaceIds: completedBeforeRegistration
            ? state.sshSessionSyncingWorkspaceIds
            : {
                ...state.sshSessionSyncingWorkspaceIds,
                [workspace.id]: true,
              },
          sshSessionSyncCompletedBeforeRegistration: nextCompletedBeforeRegistration,
          workspaces: [workspace, ...state.workspaces.filter((item) => item.id !== workspace.id)],
          archivedWorkspaces: state.archivedWorkspaces.filter((item) => item.id !== workspace.id),
          activeWorkspaceId: workspace.id,
          repos: [],
          activeRepoId: null,
          reposLoading: false,
          loading: false,
          sshWorkspaceCreationInFlight: Math.max(0, state.sshWorkspaceCreationInFlight - 1),
        };
      });
      await useTerminalStore.getState().prepareWorkspaceActivation(workspace.id);
      useGitStore.getState().loadDraftsForWorkspace(workspace.id);
      await get().loadRepos(workspace.id);
      return workspace;
    } catch (error) {
      set((state) => ({
        loading: false,
        error: String(error),
        sshWorkspaceCreationInFlight: Math.max(0, state.sshWorkspaceCreationInFlight - 1),
      }));
      return null;
    }
  },
  completeSshSessionSync: (workspaceId) => {
    set((state) => {
      if (!state.sshSessionSyncingWorkspaceIds[workspaceId]) {
        if (state.sshWorkspaceCreationInFlight === 0) {
          return state;
        }
        return {
          sshSessionSyncCompletedBeforeRegistration: {
            ...state.sshSessionSyncCompletedBeforeRegistration,
            [workspaceId]: true,
          },
        };
      }
      const next = { ...state.sshSessionSyncingWorkspaceIds };
      delete next[workspaceId];
      return { sshSessionSyncingWorkspaceIds: next };
    });
  },
  removeWorkspace: async (workspaceId) => {
    set({ loading: true, error: undefined });
    let removedFromSystem = false;
    try {
      // 历史实现只归档项目；移除项目必须永久删除本地保存的工作区记录。
      // await ipc.archiveWorkspace(workspaceId);
      await ipc.deleteWorkspace(workspaceId);
      removedFromSystem = true;
      const wasActiveWorkspace = get().activeWorkspaceId === workspaceId;
      // const removed = get().workspaces.find((workspace) => workspace.id === workspaceId) ?? null;
      const remaining = get().workspaces.filter((workspace) => workspace.id !== workspaceId);
      const nextActive =
        wasActiveWorkspace
          ? remaining[0]?.id ?? null
          : get().activeWorkspaceId;

      set((state) => ({
        workspaces: remaining,
        /*
        archivedWorkspaces: removed
          ? [
              removed,
              ...state.archivedWorkspaces.filter((workspace) => workspace.id !== workspaceId),
            ]
          : state.archivedWorkspaces,
        */
        archivedWorkspaces: state.archivedWorkspaces.filter((workspace) => workspace.id !== workspaceId),
        activeWorkspaceId: nextActive,
        sshSessionSyncingWorkspaceIds: Object.fromEntries(
          Object.entries(state.sshSessionSyncingWorkspaceIds).filter(
            ([id]) => id !== workspaceId,
          ),
        ),
        sshSessionSyncCompletedBeforeRegistration: Object.fromEntries(
          Object.entries(state.sshSessionSyncCompletedBeforeRegistration).filter(
            ([id]) => id !== workspaceId,
          ),
        ),
        loading: false,
      }));

      const lastRepoByWorkspace = readLastRepoByWorkspace();
      if (lastRepoByWorkspace[workspaceId]) {
        delete lastRepoByWorkspace[workspaceId];
        writeLastRepoByWorkspace(lastRepoByWorkspace);
      }
      if (nextActive) {
        localStorage.setItem(LAST_WORKSPACE_KEY, nextActive);
      } else {
        localStorage.removeItem(LAST_WORKSPACE_KEY);
      }

      if (nextActive) {
        const nextWorkspace = remaining.find((workspace) => workspace.id === nextActive);
      if (wasActiveWorkspace && nextWorkspace) {
          await useTerminalStore.getState().prepareWorkspaceActivation(nextActive);
        }
        await get().loadRepos(nextActive);
      } else {
        set({ repos: [], activeRepoId: null, reposLoading: false });
      }
      return true;
    } catch (error) {
      set({ loading: false, error: String(error) });
      return removedFromSystem;
    }
  },
  restoreWorkspace: async (workspaceId) => {
    set({ loading: true, error: undefined });
    try {
      const restored = await ipc.restoreWorkspace(workspaceId);
      set((state) => {
        const workspaces = [
          restored,
          ...state.workspaces.filter((workspace) => workspace.id !== workspaceId),
        ];
        const nextActiveWorkspaceId = state.activeWorkspaceId ?? restored.id;
        return {
          workspaces,
          archivedWorkspaces: state.archivedWorkspaces.filter(
            (workspace) => workspace.id !== workspaceId,
          ),
          activeWorkspaceId: nextActiveWorkspaceId,
          loading: false,
        };
      });

      if (!get().activeWorkspaceId || get().activeWorkspaceId === restored.id) {
 await useTerminalStore.getState().prepareWorkspaceActivation(restored.id);
        await get().loadRepos(restored.id);
      }
    } catch (error) {
      set({ loading: false, error: String(error) });
    }
  },
  loadRepos: async (workspaceId) => {
    const requestSeq = ++reposLoadSeq;
    const workspace = get().workspaces.find((item) => item.id === workspaceId);
    if (!workspace) {
      if (get().activeWorkspaceId !== workspaceId) return;
      set({ repos: [], activeRepoId: null, reposLoading: false });
      return;
    }
    set({ reposLoading: true });
    try {
      const repos = await ipc.getRepos(workspaceId);
      if (requestSeq !== reposLoadSeq) {
        return;
      }
      if (get().activeWorkspaceId !== workspaceId) {
        set({ reposLoading: false });
        return;
      }
      const fallbackActiveRepoId = resolveActiveRepoId(
        workspaceId,
        repos,
        get().activeRepoId,
      );
      if (fallbackActiveRepoId) {
        rememberLastRepo(workspaceId, fallbackActiveRepoId);
      }
      set({
        repos,
        activeRepoId: fallbackActiveRepoId,
        reposLoading: false,
      });
    } catch (error) {
      if (requestSeq !== reposLoadSeq) {
        return;
      }
      if (get().activeWorkspaceId !== workspaceId) {
        set({ reposLoading: false });
        return;
      }
      set({ error: String(error), reposLoading: false });
    }
  },
  setActiveWorkspace: async (workspaceId) => {
    const prevWorkspaceId = get().activeWorkspaceId;
    if (prevWorkspaceId) {
      useGitStore.getState().flushDrafts(prevWorkspaceId);
    }
    localStorage.setItem(LAST_WORKSPACE_KEY, workspaceId);
    set({ activeWorkspaceId: workspaceId, activeRepoId: null, repos: [], error: undefined });
    const workspace = get().workspaces.find((item) => item.id === workspaceId);
    if (workspace) {
      scheduleActiveWorkspaceExtensionRefresh(workspaceId);
      await useTerminalStore.getState().prepareWorkspaceActivation(workspaceId);
    }
    await get().loadRepos(workspaceId);
    if (workspace) {
      useGitStore.getState().loadDraftsForWorkspace(workspaceId);
    }
  },
  setActiveRepo: (repoId, options) => {
    set({ activeRepoId: repoId });

    if (options?.remember === false || !repoId) {
      return;
    }

    const workspaceId = get().activeWorkspaceId;
    if (!workspaceId) {
      return;
    }

    const repoExistsInWorkspace = get().repos.some(
      (repo) => repo.id === repoId && repo.workspaceId === workspaceId,
    );
    if (!repoExistsInWorkspace) {
      return;
    }

    rememberLastRepo(workspaceId, repoId);
  },
  setRepoGitActive: async (repoId, isActive) => {
    try {
      await ipc.setRepoGitActive(repoId, isActive);
      set((state) => ({
        repos: state.repos.map((repo) =>
          repo.id === repoId
            ? {
                ...repo,
                isActive,
              }
            : repo,
        ),
      }));
    } catch (error) {
      set({ error: String(error) });
      throw error;
    }
  },
  setWorkspaceGitActiveRepos: async (workspaceId, repoIds) => {
    try {
      await ipc.setWorkspaceGitActiveRepos(workspaceId, repoIds);
      set((state) => {
        const selected = new Set(repoIds);
        return {
          repos: state.repos.map((repo) =>
            repo.workspaceId === workspaceId
              ? {
                  ...repo,
                  isActive: selected.has(repo.id),
                }
              : repo,
          ),
        };
      });
    } catch (error) {
      set({ error: String(error) });
      throw error;
    }
  },
  hasWorkspaceGitSelection: async (workspaceId) => {
    const status = await ipc.hasWorkspaceGitSelection(workspaceId);
    return status.configured;
  },
  setRepoTrustLevel: async (repoId, trustLevel) => {
    try {
      await ipc.setRepoTrustLevel(repoId, trustLevel);
      set((state) => ({
        repos: state.repos.map((repo) =>
          repo.id === repoId
            ? {
                ...repo,
                trustLevel
              }
            : repo
        )
      }));
    } catch (error) {
      set({ error: String(error) });
    }
  },
  rescanWorkspace: async (workspaceId, scanDepth) => {
    const workspace = get().workspaces.find((w) => w.id === workspaceId);
    if (!workspace) return null;
    if (workspace.locationKind === "ssh") {
      await get().loadRepos(workspaceId);
      return workspace;
    }

    try {
      const updatedWorkspace = await ipc.openWorkspace(
        workspace.rootPath,
        scanDepth ?? workspace.scanDepth,
      );
      set((state) => ({
        workspaces: [
          updatedWorkspace,
          ...state.workspaces.filter((item) => item.id !== updatedWorkspace.id),
        ],
        archivedWorkspaces: state.archivedWorkspaces.filter(
          (item) => item.id !== updatedWorkspace.id,
        ),
      }));

      if (get().activeWorkspaceId === workspaceId) {
        await get().loadRepos(workspaceId);
      }

      return updatedWorkspace;
    } catch (error) {
      set({ error: String(error) });
      throw error;
    }
  },
  setAllReposTrustLevel: async (trustLevel) => {
    const repos = get().repos;
    if (!repos.length) {
      return;
    }

    try {
      await Promise.all(
        repos.map((repo) => ipc.setRepoTrustLevel(repo.id, trustLevel))
      );
      set((state) => ({
        repos: state.repos.map((repo) => ({
          ...repo,
          trustLevel
        }))
      }));
    } catch (error) {
      set({ error: String(error) });
    }
  }
}));
