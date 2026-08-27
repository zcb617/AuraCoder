import { create } from "zustand";
import type {
  GitBranch,
  GitBranchScope,
  GitCommit,
  GitDiffPreview,
  GitRemote,
  GitStash,
  GitStatus,
  GitWorktree,
  WorkspaceGitContext,
} from "../types";
import { ipc } from "../lib/ipc";
import { recordPerfMetric } from "../lib/perfTelemetry";

const BRANCH_PAGE_SIZE = 200;
const COMMIT_PAGE_SIZE = 100;
const GIT_STATUS_CACHE_TTL_MS = 1_000;
const GIT_DIFF_CACHE_TTL_MS = 1_200;
const GIT_ACTIVE_VIEW_REFRESH_MIN_INTERVAL_MS = 1_500;
const GIT_STATUS_CACHE_MAX_ENTRIES = 32;
const GIT_DIFF_CACHE_MAX_ENTRIES = 320;
const GIT_STATUS_CACHE_MAX_BYTES = 3 * 1024 * 1024;
const GIT_DIFF_CACHE_MAX_BYTES = 24 * 1024 * 1024;
const DRAFT_HISTORY_MAX = 3;

export interface GitDraftsPayload {
  commitMessage: string;
  branchName: string;
  commitHistory: string[];
  branchHistory: string[];
}

const EMPTY_DRAFTS: GitDraftsPayload = {
  commitMessage: "",
  branchName: "",
  commitHistory: [],
  branchHistory: [],
};

function draftStorageKey(workspaceId: string): string {
  return `auracoder:git.drafts:${workspaceId}`;
}

function loadDraftsFromStorage(workspaceId: string): GitDraftsPayload {
  try {
    const raw = localStorage.getItem(draftStorageKey(workspaceId));
    if (!raw) return { ...EMPTY_DRAFTS };
    const parsed = JSON.parse(raw) as Partial<GitDraftsPayload>;
    return {
      commitMessage: typeof parsed.commitMessage === "string" ? parsed.commitMessage : "",
      branchName: typeof parsed.branchName === "string" ? parsed.branchName : "",
      commitHistory: Array.isArray(parsed.commitHistory)
        ? parsed.commitHistory.filter((v): v is string => typeof v === "string").slice(0, DRAFT_HISTORY_MAX)
        : [],
      branchHistory: Array.isArray(parsed.branchHistory)
        ? parsed.branchHistory.filter((v): v is string => typeof v === "string").slice(0, DRAFT_HISTORY_MAX)
        : [],
    };
  } catch {
    return { ...EMPTY_DRAFTS };
  }
}

function saveDraftsToStorage(workspaceId: string, payload: GitDraftsPayload): void {
  try {
    localStorage.setItem(draftStorageKey(workspaceId), JSON.stringify(payload));
  } catch {
    // localStorage full or unavailable — silently ignore
  }
}

function addToHistory(history: string[], entry: string): string[] {
  const trimmed = entry.trim();
  if (!trimmed) return history;
  const deduped = history.filter((h) => h !== trimmed);
  return [trimmed, ...deduped].slice(0, DRAFT_HISTORY_MAX);
}

export type GitPanelView = "changes" | "branches" | "commits" | "stash" | "worktrees";
export type GitRemoteSyncAction = "fetch" | "pull" | "push";

interface GitStatusCacheEntry {
  status: GitStatus;
  revision: number;
  updatedAt: number;
}

interface GitDiffCacheEntry {
  diff: GitDiffPreview;
  revision: number;
  updatedAt: number;
}

const repoRevisionByWorkspace = new Map<string, number>();
const statusCacheByWorkspace = new Map<string, GitStatusCacheEntry>();
const statusInFlightByWorkspace = new Map<string, Promise<GitStatus>>();
const diffCacheByKey = new Map<string, GitDiffCacheEntry>();
const diffInFlightByKey = new Map<string, Promise<GitDiffPreview>>();
const activeViewRefreshedAtByKey = new Map<string, number>();
let statusCacheBytes = 0;
let diffCacheBytes = 0;

function estimateStatusCacheEntryBytes(workspaceId: string, entry: GitStatusCacheEntry): number {
  let bytes = workspaceId.length * 2 + entry.status.branch.length * 2 + 96;
  for (const file of entry.status.files) {
    bytes += file.path.length * 2;
    bytes += (file.indexStatus?.length ?? 0) * 2;
    bytes += (file.worktreeStatus?.length ?? 0) * 2;
    bytes += 48;
  }
  return bytes;
}

function estimateDiffCacheEntryBytes(key: string, entry: GitDiffCacheEntry): number {
  return (key.length + entry.diff.content.length) * 2 + 128;
}

function removeStatusCacheEntry(workspaceId: string) {
  const existing = statusCacheByWorkspace.get(workspaceId);
  if (!existing) {
    return;
  }
  statusCacheBytes = Math.max(
    0,
    statusCacheBytes - estimateStatusCacheEntryBytes(workspaceId, existing),
  );
  statusCacheByWorkspace.delete(workspaceId);
}

function removeDiffCacheEntry(key: string) {
  const existing = diffCacheByKey.get(key);
  if (!existing) {
    return;
  }
  diffCacheBytes = Math.max(0, diffCacheBytes - estimateDiffCacheEntryBytes(key, existing));
  diffCacheByKey.delete(key);
}

function trimStatusCacheToLimits() {
  while (
    statusCacheByWorkspace.size > GIT_STATUS_CACHE_MAX_ENTRIES ||
    statusCacheBytes > GIT_STATUS_CACHE_MAX_BYTES
  ) {
    let oldestKey: string | null = null;
    let oldestUpdatedAt = Number.POSITIVE_INFINITY;

    for (const [key, entry] of statusCacheByWorkspace.entries()) {
      if (entry.updatedAt < oldestUpdatedAt) {
        oldestUpdatedAt = entry.updatedAt;
        oldestKey = key;
      }
    }

    if (!oldestKey) {
      break;
    }
    removeStatusCacheEntry(oldestKey);
  }
}

function trimDiffCacheToLimits() {
  while (
    diffCacheByKey.size > GIT_DIFF_CACHE_MAX_ENTRIES ||
    diffCacheBytes > GIT_DIFF_CACHE_MAX_BYTES
  ) {
    let oldestKey: string | null = null;
    let oldestUpdatedAt = Number.POSITIVE_INFINITY;

    for (const [key, entry] of diffCacheByKey.entries()) {
      if (entry.updatedAt < oldestUpdatedAt) {
        oldestUpdatedAt = entry.updatedAt;
        oldestKey = key;
      }
    }

    if (!oldestKey) {
      break;
    }
    removeDiffCacheEntry(oldestKey);
  }
}

function setStatusCacheEntry(workspaceId: string, entry: GitStatusCacheEntry) {
  removeStatusCacheEntry(workspaceId);
  statusCacheByWorkspace.set(workspaceId, entry);
  statusCacheBytes += estimateStatusCacheEntryBytes(workspaceId, entry);
  trimStatusCacheToLimits();
}

function setDiffCacheEntry(key: string, entry: GitDiffCacheEntry) {
  removeDiffCacheEntry(key);
  diffCacheByKey.set(key, entry);
  diffCacheBytes += estimateDiffCacheEntryBytes(key, entry);
  trimDiffCacheToLimits();
}

function getWorkspaceRevision(workspaceId: string): number {
  return repoRevisionByWorkspace.get(workspaceId) ?? 0;
}

function incrementWorkspaceRevision(workspaceId: string): number {
  const next = getWorkspaceRevision(workspaceId) + 1;
  repoRevisionByWorkspace.set(workspaceId, next);
  return next;
}

function buildDiffCacheKey(workspaceId: string, filePath: string, staged: boolean): string {
  return `${workspaceId}::${staged ? "staged" : "worktree"}::${filePath}`;
}

function invalidateWorkspaceCaches(workspaceId: string) {
  incrementWorkspaceRevision(workspaceId);
  removeStatusCacheEntry(workspaceId);
  statusInFlightByWorkspace.delete(workspaceId);
  for (const key of [...diffCacheByKey.keys()]) {
    if (key.startsWith(`${workspaceId}::`)) {
      removeDiffCacheEntry(key);
    }
  }
  for (const key of diffInFlightByKey.keys()) {
    if (key.startsWith(`${workspaceId}::`)) {
      diffInFlightByKey.delete(key);
    }
  }
  for (const key of activeViewRefreshedAtByKey.keys()) {
    if (key.startsWith(`${workspaceId}::`)) {
      activeViewRefreshedAtByKey.delete(key);
    }
  }
}

function shouldRefreshActiveView(
  workspaceId: string,
  view: GitPanelView,
  force: boolean,
): boolean {
  if (view === "changes") {
    return false;
  }
  if (force) {
    return true;
  }
  const key = `${workspaceId}::${view}`;
  const now = performance.now();
  const last = activeViewRefreshedAtByKey.get(key);
  if (last !== undefined && now - last < GIT_ACTIVE_VIEW_REFRESH_MIN_INTERVAL_MS) {
    return false;
  }
  return true;
}

function markActiveViewRefreshed(workspaceId: string, view: GitPanelView) {
  if (view === "changes") {
    return;
  }
  activeViewRefreshedAtByKey.set(`${workspaceId}::${view}`, performance.now());
}

async function getGitStatusCached(workspaceId: string, force = false): Promise<GitStatus> {
  const revision = getWorkspaceRevision(workspaceId);
  const now = performance.now();
  const cached = statusCacheByWorkspace.get(workspaceId);
  if (
    !force &&
    cached &&
    cached.revision === revision &&
    now - cached.updatedAt <= GIT_STATUS_CACHE_TTL_MS
  ) {
    setStatusCacheEntry(workspaceId, {
      ...cached,
      updatedAt: now,
    });
    return cached.status;
  }

  const inFlight = statusInFlightByWorkspace.get(workspaceId);
  if (inFlight) {
    return inFlight;
  }

  const requestRevision = revision;
  const requestPromise = ipc
    .getGitStatus(workspaceId)
    .then((status) => {
      if (getWorkspaceRevision(workspaceId) === requestRevision) {
        setStatusCacheEntry(workspaceId, {
          status,
          revision: requestRevision,
          updatedAt: performance.now(),
        });
      }
      return status;
    })
    .finally(() => {
      statusInFlightByWorkspace.delete(workspaceId);
    });

  statusInFlightByWorkspace.set(workspaceId, requestPromise);
  return requestPromise;
}

async function getGitDiffCached(
  workspaceId: string,
  filePath: string,
  staged: boolean,
  force = false,
): Promise<GitDiffPreview> {
  const key = buildDiffCacheKey(workspaceId, filePath, staged);
  const revision = getWorkspaceRevision(workspaceId);
  const now = performance.now();
  const cached = diffCacheByKey.get(key);
  if (
    !force &&
    cached &&
    cached.revision === revision &&
    now - cached.updatedAt <= GIT_DIFF_CACHE_TTL_MS
  ) {
    setDiffCacheEntry(key, {
      ...cached,
      updatedAt: now,
    });
    return cached.diff;
  }

  const inFlight = diffInFlightByKey.get(key);
  if (inFlight) {
    return inFlight;
  }

  const requestRevision = revision;
  const requestPromise = ipc
    .getFileDiff(workspaceId, filePath, staged)
    .then((diff) => {
      if (getWorkspaceRevision(workspaceId) === requestRevision) {
        setDiffCacheEntry(key, {
          diff,
          revision: requestRevision,
          updatedAt: performance.now(),
        });
      }
      return diff;
    })
    .finally(() => {
      diffInFlightByKey.delete(key);
    });

  diffInFlightByKey.set(key, requestPromise);
  return requestPromise;
}

interface GitState {
  /** 当前项目根目录的 Git 上下文。 */
  workspaceId: string | null;
  /** 当前项目是否为 Git 仓库及其根目录信息。 */
  gitContext: WorkspaceGitContext | null;
  status?: GitStatus;
  selectedFile?: string;
  selectedFileStaged?: boolean;
  diff?: GitDiffPreview;
  loading: boolean;
  error?: string;
  remoteSyncAction: GitRemoteSyncAction | null;
  remoteSyncWorkspaceId: string | null;
  activeView: GitPanelView;
  branchScope: GitBranchScope;
  branches: GitBranch[];
  branchesTotal: number;
  branchesHasMore: boolean;
  branchesOffset: number;
  branchSearch: string;
  commits: GitCommit[];
  commitsOffset: number;
  commitsHasMore: boolean;
  commitsTotal: number;
  stashes: GitStash[];
  worktrees: GitWorktree[];
  remotes: GitRemote[];
  remotesWorkspaceId: string | null;
  remotesLoading: boolean;
  remotesError?: string;
  selectedCommitHash?: string;
  commitDiff?: GitDiffPreview;
  refresh: (workspaceId: string, options?: { force?: boolean }) => Promise<void>;
  invalidateWorkspaceCache: (workspaceId: string) => void;
  setActiveView: (view: GitPanelView) => void;
  setBranchScope: (scope: GitBranchScope) => void;
  selectFile: (workspaceId: string, filePath: string, staged?: boolean) => Promise<void>;
  stage: (workspaceId: string, filePath: string) => Promise<void>;
  stageMany: (workspaceId: string, files: string[]) => Promise<void>;
  unstage: (workspaceId: string, filePath: string) => Promise<void>;
  unstageMany: (workspaceId: string, files: string[]) => Promise<void>;
  discardFiles: (workspaceId: string, files: string[]) => Promise<void>;
  commit: (workspaceId: string, message: string) => Promise<string>;
  softResetLastCommit: (workspaceId: string) => Promise<void>;
  fetchRemote: (workspaceId: string) => Promise<void>;
  pullRemote: (workspaceId: string) => Promise<void>;
  pushRemote: (workspaceId: string) => Promise<void>;
  loadBranches: (workspaceId: string, scope?: GitBranchScope, search?: string) => Promise<void>;
  loadMoreBranches: (workspaceId: string) => Promise<void>;
  setBranchSearch: (workspaceId: string, query: string) => Promise<void>;
  checkoutBranch: (workspaceId: string, branchName: string, isRemote: boolean) => Promise<void>;
  createBranch: (workspaceId: string, branchName: string, fromRef?: string | null) => Promise<void>;
  renameBranch: (workspaceId: string, oldName: string, newName: string) => Promise<void>;
  deleteBranch: (workspaceId: string, branchName: string, force: boolean) => Promise<void>;
  loadCommits: (workspaceId: string, append?: boolean) => Promise<void>;
  loadMoreCommits: (workspaceId: string) => Promise<void>;
  loadWorktrees: (workspaceId: string) => Promise<void>;
  addWorktree: (workspaceId: string, worktreePath: string, branchName: string, baseRef?: string | null) => Promise<GitWorktree>;
  removeWorktree: (workspaceId: string, worktreePath: string, force: boolean, branchName?: string | null, deleteBranch?: boolean) => Promise<void>;
  pruneWorktrees: (workspaceId: string) => Promise<void>;
  loadStashes: (workspaceId: string) => Promise<void>;
  pushStash: (workspaceId: string, message?: string) => Promise<void>;
  applyStash: (workspaceId: string, stashIndex: number) => Promise<void>;
  popStash: (workspaceId: string, stashIndex: number) => Promise<void>;
  selectCommit: (workspaceId: string, commitHash: string) => Promise<void>;
  clearCommitSelection: () => void;
  loadRemotes: (workspaceId: string) => Promise<void>;
  addRemote: (workspaceId: string, name: string, url: string) => Promise<void>;
  removeRemote: (workspaceId: string, name: string) => Promise<void>;
  renameRemote: (workspaceId: string, oldName: string, newName: string) => Promise<void>;
  getStatusForWorkspace: (workspaceId: string) => Promise<GitStatus>;
  clearError: () => void;
  drafts: GitDraftsPayload;
  loadDraftsForWorkspace: (workspaceId: string) => void;
  setCommitMessageDraft: (workspaceId: string, message: string) => void;
  setBranchNameDraft: (workspaceId: string, name: string) => void;
  pushCommitHistory: (workspaceId: string, message: string) => void;
  pushBranchHistory: (workspaceId: string, name: string) => void;
  flushDrafts: (workspaceId: string) => void;
  /** 加载项目根目录 Git 上下文并清理旧项目视图。 */
  loadWorkspaceContext: (workspaceId: string) => Promise<void>;
}

async function refreshActiveView(workspaceId: string, state: Pick<GitState, "activeView" | "branchScope" | "branchSearch">) {
  if (state.activeView === "branches") {
    const branchesPage = await ipc.listGitBranches(
      workspaceId,
      state.branchScope,
      0,
      BRANCH_PAGE_SIZE,
      state.branchSearch || undefined,
    );
    return {
      branches: branchesPage.entries,
      branchesTotal: branchesPage.total,
      branchesHasMore: branchesPage.hasMore,
      branchesOffset: branchesPage.offset + branchesPage.entries.length,
    } satisfies Partial<GitState>;
  }

  if (state.activeView === "commits") {
    const commitsPage = await ipc.listGitCommits(workspaceId, 0, COMMIT_PAGE_SIZE);
    return {
      commits: commitsPage.entries,
      commitsOffset: commitsPage.offset + commitsPage.entries.length,
      commitsHasMore: commitsPage.hasMore,
      commitsTotal: commitsPage.total,
    } satisfies Partial<GitState>;
  }

  if (state.activeView === "stash") {
    const stashes = await ipc.listGitStashes(workspaceId);
    return {
      stashes,
    } satisfies Partial<GitState>;
  }

  if (state.activeView === "worktrees") {
    const worktrees = await ipc.listGitWorktrees(workspaceId);
    return {
      worktrees,
    } satisfies Partial<GitState>;
  }

  return {};
}

export const useGitStore = create<GitState>((set, get) => {
  let loadingOps = 0;
  let refreshSeq = 0;
  let selectFileSeq = 0;
  let branchesSeq = 0;
  let commitsSeq = 0;
  let stashesSeq = 0;
  let worktreesSeq = 0;
  let commitDiffSeq = 0;
  let remotesSeq = 0;

  const beginLoading = () => {
    loadingOps += 1;
    if (loadingOps === 1) {
      set({ loading: true });
    }
  };

  const endLoading = () => {
    loadingOps = Math.max(0, loadingOps - 1);
    if (loadingOps === 0) {
      set({ loading: false });
    }
  };

  /** 判断异步 Git 结果是否仍属于当前项目，避免切换项目后覆盖新状态。 */
  const isWorkspaceActive = (workspaceId: string): boolean => get().workspaceId === workspaceId;

  const runRefresh = async (workspaceId: string, options?: { force?: boolean }) => {
    const requestSeq = ++refreshSeq;
    const startedAt = performance.now();

    try {
      const status = await getGitStatusCached(workspaceId, options?.force ?? false);
      const currentState = get();
      const selectedFile = currentState.selectedFile;
      const selectedFileStaged = currentState.selectedFileStaged ?? false;
      let selectedDiff: GitDiffPreview | undefined = currentState.diff;
      let nextSelectedFile = selectedFile;
      let nextSelectedFileStaged = currentState.selectedFileStaged;
      const shouldRefreshSelectedDiff = currentState.activeView === "changes";
      let selectedDiffRefreshed = false;

      if (selectedFile) {
        const selectedStatus = status.files.find((file) => file.path === selectedFile);
        const sameStateExists = selectedStatus
          ? (selectedFileStaged ? Boolean(selectedStatus.indexStatus) : Boolean(selectedStatus.worktreeStatus))
          : false;
        const oppositeStateExists = selectedStatus
          ? (selectedFileStaged ? Boolean(selectedStatus.worktreeStatus) : Boolean(selectedStatus.indexStatus))
          : false;

        if (!sameStateExists && !oppositeStateExists) {
          selectedDiff = undefined;
          nextSelectedFile = undefined;
          nextSelectedFileStaged = undefined;
        } else if (shouldRefreshSelectedDiff) {
          if (sameStateExists) {
            try {
              selectedDiff = await getGitDiffCached(workspaceId, selectedFile, selectedFileStaged);
              selectedDiffRefreshed = true;
            } catch {
              selectedDiff = undefined;
            }
          } else {
            const flippedStaged = !selectedFileStaged;
            nextSelectedFileStaged = flippedStaged;
            try {
              selectedDiff = await getGitDiffCached(workspaceId, selectedFile, flippedStaged);
              selectedDiffRefreshed = true;
            } catch {
              selectedDiff = undefined;
            }
          }
        } else {
          if (!sameStateExists && oppositeStateExists) {
            nextSelectedFileStaged = !selectedFileStaged;
          }
          selectedDiff = undefined;
        }
      }

      const forceRefresh = options?.force ?? false;
      const refreshView = shouldRefreshActiveView(
        workspaceId,
        currentState.activeView,
        forceRefresh,
      );
      const viewState = refreshView
        ? await refreshActiveView(workspaceId, {
            activeView: currentState.activeView,
            branchScope: currentState.branchScope,
            branchSearch: currentState.branchSearch,
          })
        : {};

      if (requestSeq === refreshSeq && isWorkspaceActive(workspaceId)) {
        set({
          ...viewState,
          status,
          selectedFile: nextSelectedFile,
          selectedFileStaged: nextSelectedFileStaged,
          diff: selectedDiff,
          error: undefined,
        });
        if (refreshView) {
          markActiveViewRefreshed(workspaceId, currentState.activeView);
        }
      }

      recordPerfMetric("git.refresh.ms", performance.now() - startedAt, {
        workspaceId,
        fileCount: status.files.length,
        cached: !forceRefresh,
        viewRefreshed: refreshView,
        selectedDiffRefreshed,
      });
    } catch (error) {
      if (requestSeq === refreshSeq && isWorkspaceActive(workspaceId)) {
        set({ error: String(error) });
      }
      recordPerfMetric("git.refresh.ms", performance.now() - startedAt, {
        workspaceId,
        failed: true,
      });
    }
  };

  const runWorkspaceMutationWithRefresh = async <T>(
    workspaceId: string,
    mutation: () => Promise<T>,
    options?: { remoteSyncAction?: GitRemoteSyncAction },
  ): Promise<T> => {
    beginLoading();
    set({ error: undefined });

    if (options?.remoteSyncAction) {
      set({ remoteSyncAction: options.remoteSyncAction, remoteSyncWorkspaceId: workspaceId });
    }

    try {
      const result = await mutation();
      get().invalidateWorkspaceCache(workspaceId);
      await runRefresh(workspaceId, { force: true });
      return result;
    } catch (error) {
      if (isWorkspaceActive(workspaceId)) {
        set({ error: String(error) });
      }
      throw error;
    } finally {
      if (
        options?.remoteSyncAction &&
        get().remoteSyncAction === options.remoteSyncAction &&
        get().remoteSyncWorkspaceId === workspaceId
      ) {
        set({ remoteSyncAction: null, remoteSyncWorkspaceId: null });
      }
      endLoading();
    }
  };

  return {
    loading: false,
    workspaceId: null,
    gitContext: null,
    remoteSyncAction: null,
    remoteSyncWorkspaceId: null,
    activeView: "changes",
    branchScope: "local",
    branches: [],
    branchesTotal: 0,
    branchesHasMore: false,
    branchesOffset: 0,
    branchSearch: "",
    commits: [],
    commitsOffset: 0,
    commitsHasMore: false,
    commitsTotal: 0,
    stashes: [],
    worktrees: [],
    remotes: [],
    remotesWorkspaceId: null,
    remotesLoading: false,
    remotesError: undefined,
    refresh: async (workspaceId, options) => {
      beginLoading();
      await runRefresh(workspaceId, options);
      endLoading();
    },
    invalidateWorkspaceCache: (workspaceId) => {
      invalidateWorkspaceCaches(workspaceId);
    },
    setActiveView: (view) => {
      set({ activeView: view, error: undefined });
    },
    setBranchScope: (scope) => {
      set({ branchScope: scope, error: undefined });
    },
    selectFile: async (workspaceId, filePath, staged = false) => {
      const requestSeq = ++selectFileSeq;
      const startedAt = performance.now();
      try {
        const diff = await getGitDiffCached(workspaceId, filePath, staged);
        if (requestSeq === selectFileSeq && isWorkspaceActive(workspaceId)) {
          set({ selectedFile: filePath, selectedFileStaged: staged, diff, error: undefined });
        }
        recordPerfMetric("git.file_diff.ms", performance.now() - startedAt, {
          workspaceId,
          filePath,
          staged,
          truncated: diff.truncated,
          returnedBytes: diff.returnedBytes,
          originalBytes: diff.originalBytes,
        });
      } catch (error) {
        if (requestSeq === selectFileSeq && isWorkspaceActive(workspaceId)) {
          set({ error: String(error) });
        }
        recordPerfMetric("git.file_diff.ms", performance.now() - startedAt, {
          workspaceId,
          filePath,
          staged,
          failed: true,
        });
      }
    },
    stage: async (workspaceId, filePath) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.stageFiles(workspaceId, [filePath]));
    },
    stageMany: async (workspaceId, files) => {
      if (files.length === 0) {
        return;
      }
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.stageFiles(workspaceId, files));
    },
    unstage: async (workspaceId, filePath) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.unstageFiles(workspaceId, [filePath]));
    },
    unstageMany: async (workspaceId, files) => {
      if (files.length === 0) {
        return;
      }
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.unstageFiles(workspaceId, files));
    },
    discardFiles: async (workspaceId, files) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.discardFiles(workspaceId, files));
    },
    commit: async (workspaceId, message) => {
      return runWorkspaceMutationWithRefresh(workspaceId, () => ipc.commit(workspaceId, message));
    },
    softResetLastCommit: async (workspaceId) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.softResetLastCommit(workspaceId));
    },
    fetchRemote: async (workspaceId) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.fetchGit(workspaceId), {
        remoteSyncAction: "fetch",
      });
    },
    pullRemote: async (workspaceId) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.pullGit(workspaceId), {
        remoteSyncAction: "pull",
      });
    },
    pushRemote: async (workspaceId) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.pushGit(workspaceId), {
        remoteSyncAction: "push",
      });
    },
    loadBranches: async (workspaceId, scope, search) => {
      const requestSeq = ++branchesSeq;
      const nextScope = scope ?? get().branchScope;
      const searchQuery = search !== undefined ? search : get().branchSearch;
      beginLoading();
      set({ error: undefined, branchScope: nextScope, branchSearch: searchQuery });

      try {
        const page = await ipc.listGitBranches(workspaceId, nextScope, 0, BRANCH_PAGE_SIZE, searchQuery || undefined);
        if (requestSeq === branchesSeq && isWorkspaceActive(workspaceId)) {
          set({
            branches: page.entries,
            branchesTotal: page.total,
            branchesHasMore: page.hasMore,
            branchesOffset: page.offset + page.entries.length,
          });
        }
      } catch (error) {
        if (requestSeq === branchesSeq && isWorkspaceActive(workspaceId)) {
          set({ error: String(error) });
        }
      } finally {
        endLoading();
      }
    },
    loadMoreBranches: async (workspaceId) => {
      if (!get().branchesHasMore) return;
      const requestSeq = ++branchesSeq;
      const { branchScope, branchSearch, branchesOffset, branches } = get();

      beginLoading();
      set({ error: undefined });

      try {
        const page = await ipc.listGitBranches(
          workspaceId,
          branchScope,
          branchesOffset,
          BRANCH_PAGE_SIZE,
          branchSearch || undefined,
        );
        if (requestSeq === branchesSeq && isWorkspaceActive(workspaceId)) {
          set({
            branches: [...branches, ...page.entries],
            branchesTotal: page.total,
            branchesHasMore: page.hasMore,
            branchesOffset: page.offset + page.entries.length,
          });
        }
      } catch (error) {
        if (requestSeq === branchesSeq && isWorkspaceActive(workspaceId)) {
          set({ error: String(error) });
        }
      } finally {
        endLoading();
      }
    },
    setBranchSearch: async (workspaceId, query) => {
      await get().loadBranches(workspaceId, undefined, query);
    },
    checkoutBranch: async (workspaceId, branchName, isRemote) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.checkoutGitBranch(workspaceId, branchName, isRemote));
    },
    createBranch: async (workspaceId, branchName, fromRef) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () =>
        ipc.createGitBranch(workspaceId, branchName, fromRef ?? null),
      );
    },
    renameBranch: async (workspaceId, oldName, newName) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.renameGitBranch(workspaceId, oldName, newName));
    },
    deleteBranch: async (workspaceId, branchName, force) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.deleteGitBranch(workspaceId, branchName, force));
    },
    loadCommits: async (workspaceId, append = false) => {
      const requestSeq = ++commitsSeq;
      const offset = append ? get().commitsOffset : 0;
      const previousEntries = append ? get().commits : [];

      beginLoading();
      set({ error: undefined });

      try {
        const page = await ipc.listGitCommits(workspaceId, offset, COMMIT_PAGE_SIZE);
        if (requestSeq !== commitsSeq || !isWorkspaceActive(workspaceId)) {
          return;
        }

        const entries = append ? [...previousEntries, ...page.entries] : page.entries;
        set({
          commits: entries,
          commitsOffset: page.offset + page.entries.length,
          commitsHasMore: page.hasMore,
          commitsTotal: page.total,
        });
      } catch (error) {
        if (requestSeq === commitsSeq && isWorkspaceActive(workspaceId)) {
          set({ error: String(error) });
        }
      } finally {
        endLoading();
      }
    },
    loadMoreCommits: async (workspaceId) => {
      if (!get().commitsHasMore) {
        return;
      }
      await get().loadCommits(workspaceId, true);
    },
    loadWorktrees: async (workspaceId) => {
      const requestSeq = ++worktreesSeq;
      beginLoading();
      set({ error: undefined });
      try {
        const worktrees = await ipc.listGitWorktrees(workspaceId);
      if (requestSeq === worktreesSeq && isWorkspaceActive(workspaceId)) {
          set({ worktrees });
        }
      } catch (error) {
      if (requestSeq === worktreesSeq && isWorkspaceActive(workspaceId)) {
          set({ error: String(error) });
        }
      } finally {
        endLoading();
      }
    },
    addWorktree: async (workspaceId, worktreePath, branchName, baseRef) => {
      return runWorkspaceMutationWithRefresh(workspaceId, () =>
        ipc.addGitWorktree(workspaceId, worktreePath, branchName, baseRef),
      );
    },
    removeWorktree: async (workspaceId, worktreePath, force, branchName, deleteBranch) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () =>
        ipc.removeGitWorktree(workspaceId, worktreePath, force, branchName, deleteBranch),
      );
    },
    pruneWorktrees: async (workspaceId) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.pruneGitWorktrees(workspaceId));
    },
    loadStashes: async (workspaceId) => {
      const requestSeq = ++stashesSeq;
      beginLoading();
      set({ error: undefined });
      try {
        const stashes = await ipc.listGitStashes(workspaceId);
        if (requestSeq === stashesSeq && isWorkspaceActive(workspaceId)) {
          set({ stashes });
        }
      } catch (error) {
        if (requestSeq === stashesSeq && isWorkspaceActive(workspaceId)) {
          set({ error: String(error) });
        }
      } finally {
        endLoading();
      }
    },
    pushStash: async (workspaceId, message) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.pushGitStash(workspaceId, message));
    },
    applyStash: async (workspaceId, stashIndex) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.applyGitStash(workspaceId, stashIndex));
    },
    popStash: async (workspaceId, stashIndex) => {
      await runWorkspaceMutationWithRefresh(workspaceId, () => ipc.popGitStash(workspaceId, stashIndex));
    },
    selectCommit: async (workspaceId, commitHash) => {
      const current = get().selectedCommitHash;
      if (current === commitHash) {
        set({ selectedCommitHash: undefined, commitDiff: undefined });
        return;
      }

      const requestSeq = ++commitDiffSeq;
      const startedAt = performance.now();
      set({ selectedCommitHash: commitHash, commitDiff: undefined });
      try {
        const diff = await ipc.getCommitDiff(workspaceId, commitHash);
        if (
          requestSeq === commitDiffSeq &&
          isWorkspaceActive(workspaceId) &&
          get().selectedCommitHash === commitHash
        ) {
          set({ commitDiff: diff });
        }
        recordPerfMetric("git.file_diff.ms", performance.now() - startedAt, {
          workspaceId,
          commitHash,
          truncated: diff.truncated,
          returnedBytes: diff.returnedBytes,
          originalBytes: diff.originalBytes,
        });
      } catch (error) {
        if (
          requestSeq === commitDiffSeq &&
          isWorkspaceActive(workspaceId) &&
          get().selectedCommitHash === commitHash
        ) {
          set({ error: String(error), selectedCommitHash: undefined, commitDiff: undefined });
        }
        recordPerfMetric("git.file_diff.ms", performance.now() - startedAt, {
          workspaceId,
          commitHash,
          failed: true,
        });
      }
    },
    clearCommitSelection: () => {
      set({ selectedCommitHash: undefined, commitDiff: undefined });
    },
    loadRemotes: async (workspaceId) => {
      const requestSeq = ++remotesSeq;
      const { remotes, remotesWorkspaceId } = get();
      const shouldClearRemotes = remotesWorkspaceId !== workspaceId;

      set({
        remotes: shouldClearRemotes ? [] : remotes,
        remotesWorkspaceId: workspaceId,
        remotesLoading: true,
        remotesError: undefined,
        error: undefined,
      });
      try {
        const remotes = await ipc.listGitRemotes(workspaceId);
        if (requestSeq === remotesSeq && isWorkspaceActive(workspaceId)) {
          set({ remotes, remotesWorkspaceId: workspaceId, remotesError: undefined });
        }
      } catch (error) {
        if (requestSeq === remotesSeq && isWorkspaceActive(workspaceId)) {
          set({ error: String(error), remotesError: String(error) });
        }
      } finally {
        if (requestSeq === remotesSeq) {
          set({ remotesLoading: false });
        }
      }
    },
    addRemote: async (workspaceId, name, url) => {
      await runWorkspaceMutationWithRefresh(workspaceId, async () => {
        await ipc.addGitRemote(workspaceId, name, url);
      });
      await get().loadRemotes(workspaceId);
      // Auto-fetch from the new remote and refresh cached git state so new refs
      // appear immediately. Swallow network/empty-remote failures.
      try {
        await ipc.fetchGit(workspaceId);
        get().invalidateWorkspaceCache(workspaceId);
        beginLoading();
        try {
          await runRefresh(workspaceId, { force: true });
        } finally {
          endLoading();
        }
      } catch {
        // Swallow: remote may be unreachable or empty
      }
    },
    removeRemote: async (workspaceId, name) => {
      await runWorkspaceMutationWithRefresh(workspaceId, async () => {
        await ipc.removeGitRemote(workspaceId, name);
      });
      await get().loadRemotes(workspaceId);
    },
    renameRemote: async (workspaceId, oldName, newName) => {
      await runWorkspaceMutationWithRefresh(workspaceId, async () => {
        await ipc.renameGitRemote(workspaceId, oldName, newName);
      });
      await get().loadRemotes(workspaceId);
    },
    getStatusForWorkspace: (workspaceId) => getGitStatusCached(workspaceId),
    clearError: () => set({ error: undefined }),
    drafts: { ...EMPTY_DRAFTS },
    loadDraftsForWorkspace: (workspaceId) => {
      set({ drafts: loadDraftsFromStorage(workspaceId) });
    },
    setCommitMessageDraft: (_workspaceId, message) => {
      set((state) => ({ drafts: { ...state.drafts, commitMessage: message } }));
    },
    setBranchNameDraft: (_workspaceId, name) => {
      set((state) => ({ drafts: { ...state.drafts, branchName: name } }));
    },
    pushCommitHistory: (workspaceId, message) => {
      const drafts = get().drafts;
      const next: GitDraftsPayload = {
        ...drafts,
        commitMessage: "",
        commitHistory: addToHistory(drafts.commitHistory, message),
      };
      set({ drafts: next });
      saveDraftsToStorage(workspaceId, next);
    },
    pushBranchHistory: (workspaceId, name) => {
      const drafts = get().drafts;
      const next: GitDraftsPayload = {
        ...drafts,
        branchName: "",
        branchHistory: addToHistory(drafts.branchHistory, name),
      };
      set({ drafts: next });
      saveDraftsToStorage(workspaceId, next);
    },
    flushDrafts: (workspaceId) => {
      saveDraftsToStorage(workspaceId, get().drafts);
    },
    loadWorkspaceContext: async (workspaceId) => {
      set({
        workspaceId,
        gitContext: null,
        status: undefined,
        selectedFile: undefined,
        selectedFileStaged: undefined,
        diff: undefined,
        branches: [],
        branchesTotal: 0,
        branchesHasMore: false,
        branchesOffset: 0,
        commits: [],
        commitsOffset: 0,
        commitsHasMore: false,
        commitsTotal: 0,
        stashes: [],
        worktrees: [],
        remotes: [],
        remotesWorkspaceId: null,
        remotesError: undefined,
        selectedCommitHash: undefined,
        commitDiff: undefined,
        error: undefined,
      });
      try {
        const gitContext = await ipc.getWorkspaceGitContext(workspaceId);
        if (get().workspaceId !== workspaceId) return;
        set({ workspaceId, gitContext });
        if (gitContext.kind !== "repository") return;
        await get().refresh(workspaceId, { force: true });
      } catch (error) {
        set({ error: String(error) });
      }
    },
  };
});
