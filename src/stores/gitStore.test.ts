import { beforeEach, describe, expect, it, vi } from "vitest";
import type { GitDiffPreview, GitStatus, GitWorktree } from "../types";

const mockIpc = vi.hoisted(() => ({
  getGitStatus: vi.fn(),
  getFileDiff: vi.fn(),
  stageFiles: vi.fn(),
  unstageFiles: vi.fn(),
  discardFiles: vi.fn(),
  commit: vi.fn(),
  softResetLastCommit: vi.fn(),
  fetchGit: vi.fn(),
  pullGit: vi.fn(),
  pushGit: vi.fn(),
  listGitBranches: vi.fn(),
  checkoutGitBranch: vi.fn(),
  createGitBranch: vi.fn(),
  renameGitBranch: vi.fn(),
  deleteGitBranch: vi.fn(),
  listGitCommits: vi.fn(),
  listGitStashes: vi.fn(),
  pushGitStash: vi.fn(),
  applyGitStash: vi.fn(),
  popGitStash: vi.fn(),
  addGitWorktree: vi.fn(),
  listGitWorktrees: vi.fn(),
  removeGitWorktree: vi.fn(),
  pruneGitWorktrees: vi.fn(),
  getCommitDiff: vi.fn(),
}));

vi.mock("../lib/ipc", () => ({
  ipc: mockIpc,
}));

import { useGitStore } from "./gitStore";

function makeStatus(branch: string, files: GitStatus["files"] = []): GitStatus {
  return {
    branch,
    files,
    ahead: 0,
    behind: 0,
  };
}

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function makeDiffPreview(content = ""): GitDiffPreview {
  return {
    content,
    truncated: false,
    originalBytes: content.length,
    returnedBytes: content.length,
  };
}

async function flushPromises() {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("gitStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();

    mockIpc.getGitStatus.mockResolvedValue(makeStatus("main"));
    mockIpc.getFileDiff.mockResolvedValue(makeDiffPreview());
    mockIpc.stageFiles.mockResolvedValue(undefined);
    mockIpc.unstageFiles.mockResolvedValue(undefined);
    mockIpc.discardFiles.mockResolvedValue(undefined);
    mockIpc.commit.mockResolvedValue("abc123");
    mockIpc.softResetLastCommit.mockResolvedValue(undefined);
    mockIpc.fetchGit.mockResolvedValue(undefined);
    mockIpc.pullGit.mockResolvedValue(undefined);
    mockIpc.pushGit.mockResolvedValue(undefined);
    mockIpc.listGitBranches.mockResolvedValue({ entries: [] });
    mockIpc.checkoutGitBranch.mockResolvedValue(undefined);
    mockIpc.createGitBranch.mockResolvedValue(undefined);
    mockIpc.renameGitBranch.mockResolvedValue(undefined);
    mockIpc.deleteGitBranch.mockResolvedValue(undefined);
    mockIpc.listGitCommits.mockResolvedValue({
      entries: [],
      offset: 0,
      limit: 100,
      total: 0,
      hasMore: false,
    });
    mockIpc.listGitStashes.mockResolvedValue([]);
    mockIpc.pushGitStash.mockResolvedValue(undefined);
    mockIpc.applyGitStash.mockResolvedValue(undefined);
    mockIpc.popGitStash.mockResolvedValue(undefined);
    mockIpc.addGitWorktree.mockResolvedValue({
      path: "/workspace/main/.auracoder/worktrees/feature",
      headSha: null,
      branch: "feature",
      isMain: false,
      isLocked: false,
      isPrunable: false,
    } satisfies GitWorktree);
    mockIpc.listGitWorktrees.mockResolvedValue([]);
    mockIpc.removeGitWorktree.mockResolvedValue(undefined);
    mockIpc.pruneGitWorktrees.mockResolvedValue(undefined);
    mockIpc.getCommitDiff.mockResolvedValue(makeDiffPreview());

    useGitStore.setState({
      status: undefined,
      selectedFile: undefined,
      selectedFileStaged: undefined,
      diff: undefined,
      loading: false,
      error: undefined,
      workspaceId: null,
      gitContext: null,
      remoteSyncAction: null,
      remoteSyncWorkspaceId: null,
      activeView: "changes",
      branchScope: "local",
      branches: [],
      commits: [],
      commitsOffset: 0,
      commitsHasMore: false,
      commitsTotal: 0,
      stashes: [],
      worktrees: [],
      remotesWorkspaceId: null,
      selectedCommitHash: undefined,
      commitDiff: undefined,
    });
  });

  it("keeps loading true until all overlapping operations settle", async () => {
    const fetchDeferred = deferred<void>();
    const pullDeferred = deferred<void>();
    mockIpc.fetchGit.mockReturnValueOnce(fetchDeferred.promise);
    mockIpc.pullGit.mockReturnValueOnce(pullDeferred.promise);

    const fetchPromise = useGitStore.getState().fetchRemote("ws-1");
    const pullPromise = useGitStore.getState().pullRemote("ws-1");
    await flushPromises();
    expect(useGitStore.getState().loading).toBe(true);

    fetchDeferred.resolve(undefined);
    await flushPromises();
    expect(useGitStore.getState().loading).toBe(true);

    pullDeferred.resolve(undefined);
    await Promise.all([fetchPromise, pullPromise]);
    expect(useGitStore.getState().loading).toBe(false);
  });

  it("tracks remote sync state only for remote operations", async () => {
    const pushDeferred = deferred<void>();
    mockIpc.pushGit.mockReturnValueOnce(pushDeferred.promise);

    const pushPromise = useGitStore.getState().pushRemote("ws-1");
    await flushPromises();
    expect(useGitStore.getState().remoteSyncAction).toBe("push");
    expect(useGitStore.getState().remoteSyncWorkspaceId).toBe("ws-1");

    pushDeferred.resolve(undefined);
    await pushPromise;
    expect(useGitStore.getState().remoteSyncAction).toBeNull();
    expect(useGitStore.getState().remoteSyncWorkspaceId).toBeNull();
  });

  it("ignores stale refresh responses after workspace switch", async () => {
    const repoAStatus = deferred<GitStatus>();
    mockIpc.getGitStatus.mockImplementation((workspaceId: string) => {
      if (workspaceId === "ws-a") {
        return repoAStatus.promise;
      }
      return Promise.resolve(makeStatus("repo-b-branch"));
    });

    useGitStore.setState({ workspaceId: "ws-a" });
    const repoARefresh = useGitStore.getState().refresh("ws-a");
    await flushPromises();

    useGitStore.setState({ workspaceId: "ws-b" });
    await useGitStore.getState().refresh("ws-b");
    expect(useGitStore.getState().status?.branch).toBe("repo-b-branch");

    repoAStatus.resolve(makeStatus("repo-a-branch"));
    await repoARefresh;
    expect(useGitStore.getState().status?.branch).toBe("repo-b-branch");
  });

  it("refreshes status after bulk stage mutation", async () => {
    const workspaceId = "ws-stage";
    mockIpc.getGitStatus
      .mockResolvedValueOnce(makeStatus("main", []))
      .mockResolvedValueOnce(makeStatus("main", [{ path: "a.ts", indexStatus: "added" }]));

    useGitStore.setState({ workspaceId });
    await useGitStore.getState().refresh(workspaceId);
    expect(useGitStore.getState().status?.files).toHaveLength(0);

    await useGitStore.getState().stageMany(workspaceId, ["a.ts"]);
    expect(mockIpc.stageFiles).toHaveBeenCalledWith(workspaceId, ["a.ts"]);
    expect(useGitStore.getState().status?.files).toHaveLength(1);
    expect(useGitStore.getState().status?.files[0]?.path).toBe("a.ts");
  });

  it("refreshes the same workspace after removing a worktree", async () => {
    const workspaceId = "ws-main";
    const worktreePath = "/workspace/main/.auracoder/worktrees/feature";
    const remainingWorktrees: GitWorktree[] = [
      {
        path: "/workspace/main",
        headSha: null,
        branch: "main",
        isMain: true,
        isLocked: false,
        isPrunable: false,
      },
    ];

    mockIpc.listGitWorktrees.mockResolvedValue(remainingWorktrees);
    mockIpc.getGitStatus.mockResolvedValue(makeStatus("main"));

    useGitStore.setState({ workspaceId, activeView: "worktrees" });

    await useGitStore
      .getState()
      .removeWorktree(workspaceId, worktreePath, false, "feature", false);

    expect(mockIpc.removeGitWorktree).toHaveBeenCalledWith(
      workspaceId,
      worktreePath,
      false,
      "feature",
      false,
    );
    expect(useGitStore.getState().workspaceId).toBe(workspaceId);
    expect(mockIpc.getGitStatus).toHaveBeenLastCalledWith(workspaceId);
  });
});
