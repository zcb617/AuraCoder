import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const mockUseEffect = vi.hoisted(() => vi.fn());
const mockUseWorkspaceStore = vi.hoisted(() => vi.fn());
const mockUseGitStore = vi.hoisted(() => vi.fn());
const mockLoadWorkspaceContext = vi.hoisted(() => vi.fn());
const mockRefresh = vi.hoisted(() => vi.fn());
const mockInvalidateWorkspaceCache = vi.hoisted(() => vi.fn());
const mockWatchGitRepo = vi.hoisted(() => vi.fn());
const mockListenGitRepoChanged = vi.hoisted(() => vi.fn());
const mockUnlisten = vi.hoisted(() => vi.fn());

vi.mock("react", async () => {
  const actual = await vi.importActual<typeof import("react")>("react");
  return { ...actual, useEffect: mockUseEffect };
});

vi.mock("react-i18next", () => ({
  useTranslation: () => ({ t: (key: string) => key }),
}));

vi.mock("lucide-react", () => ({
  GitBranch: () => null,
}));

vi.mock("../../stores/workspaceStore", () => ({
  useWorkspaceStore: mockUseWorkspaceStore,
}));

vi.mock("../../stores/gitStore", () => ({
  useGitStore: mockUseGitStore,
}));

vi.mock("../../lib/ipc", () => ({
  ipc: { watchGitRepo: mockWatchGitRepo },
  listenGitRepoChanged: mockListenGitRepoChanged,
}));

vi.mock("./GitChangesView", () => ({ GitChangesView: () => null }));
vi.mock("./GitBranchesView", () => ({ GitBranchesView: () => null }));
vi.mock("./GitCommitsView", () => ({ GitCommitsView: () => null }));
vi.mock("./GitStashView", () => ({ GitStashView: () => null }));
vi.mock("./GitWorktreesView", () => ({ GitWorktreesView: () => null }));

import { GitPanel } from "./GitPanel";

interface GitRepoEvent {
  /** 发生 Git 仓库变化的项目标识。 */
  workspaceId: string;
}

type EffectCleanup = (() => void) | undefined;
type CapturedEffect = () => void | EffectCleanup;

let capturedEffects: CapturedEffect[] = [];
let eventCallback: ((event: GitRepoEvent) => void) | undefined;

/** 等待 GitPanel 异步监听注册和刷新流程完成。 */
async function flushMicrotasks(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

describe("GitPanel Git 仓库实时刷新", () => {
  beforeEach(() => {
    capturedEffects = [];
    eventCallback = undefined;
    mockUseEffect.mockImplementation((effect: CapturedEffect) => {
      capturedEffects.push(effect);
    });
    mockUseWorkspaceStore.mockImplementation((selector: (state: object) => unknown) =>
      selector({ activeWorkspaceId: "workspace-a" }),
    );
    mockUseGitStore.mockImplementation((selector: (state: object) => unknown) =>
      selector({
        gitContext: { kind: "repository", name: "repo-a" },
        activeView: "changes",
        loadWorkspaceContext: mockLoadWorkspaceContext,
        refresh: mockRefresh,
        invalidateWorkspaceCache: mockInvalidateWorkspaceCache,
        error: undefined,
      }),
    );
    mockLoadWorkspaceContext.mockResolvedValue(undefined);
    mockRefresh.mockResolvedValue(undefined);
    mockInvalidateWorkspaceCache.mockImplementation(() => undefined);
    mockWatchGitRepo.mockResolvedValue(undefined);
    mockListenGitRepoChanged.mockImplementation(
      async (callback: (event: GitRepoEvent) => void) => {
        eventCallback = callback;
        return mockUnlisten;
      },
    );
    mockUnlisten.mockImplementation(() => undefined);
    GitPanel({ visible: true });
  });

  afterEach(() => {
    vi.restoreAllMocks();
    vi.clearAllMocks();
  });

  it("当前 workspace 事件先失效缓存再强制刷新，其他 workspace 不刷新", async () => {
    const calls: string[] = [];
    mockInvalidateWorkspaceCache.mockImplementation(() => calls.push("invalidate"));
    mockRefresh.mockImplementation(async () => calls.push("refresh"));

    const cleanup = capturedEffects[1]?.();
    await flushMicrotasks();
    eventCallback?.({ workspaceId: "workspace-other" });
    expect(mockInvalidateWorkspaceCache).not.toHaveBeenCalled();
    expect(mockRefresh).not.toHaveBeenCalled();

    eventCallback?.({ workspaceId: "workspace-a" });
    await flushMicrotasks();
    expect(mockInvalidateWorkspaceCache).toHaveBeenCalledWith("workspace-a");
    expect(mockRefresh).toHaveBeenCalledWith("workspace-a", { force: true });
    expect(calls).toEqual(["invalidate", "refresh"]);

    cleanup?.();
    expect(mockUnlisten).toHaveBeenCalledTimes(1);
  });

  it("监听注册完成前卸载时立即停止迟到的 listener", async () => {
    let resolveListen: ((stop: () => void) => void) | undefined;
    mockListenGitRepoChanged.mockImplementation(
      (callback: (event: GitRepoEvent) => void) => {
        eventCallback = callback;
        return new Promise((resolve: (stop: () => void) => void) => {
          resolveListen = resolve;
        });
      },
    );

    const cleanup = capturedEffects[1]?.();
    await flushMicrotasks();
    cleanup?.();
    resolveListen?.(mockUnlisten);
    await flushMicrotasks();

    expect(mockUnlisten).toHaveBeenCalledTimes(1);
  });

  it("watch 失败后仍注册 listener，并记录原始异常和 workspaceId", async () => {
    const watchError = new Error("watch failed");
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    mockWatchGitRepo.mockRejectedValue(watchError);

    const cleanup = capturedEffects[1]?.();
    await flushMicrotasks();

    expect(mockListenGitRepoChanged).toHaveBeenCalledTimes(1);
    expect(consoleError).toHaveBeenCalledWith(
      "Git 仓库监听启动失败，项目状态仍继续注册事件监听",
      "workspace-a",
      watchError,
    );
    cleanup?.();
  });

  it("listen 失败和 refresh 失败均被捕获且记录原始异常", async () => {
    const listenError = new Error("listen failed");
    const refreshError = new Error("refresh failed");
    const consoleError = vi.spyOn(console, "error").mockImplementation(() => undefined);
    mockListenGitRepoChanged.mockRejectedValue(listenError);

    const cleanup = capturedEffects[1]?.();
    await flushMicrotasks();
    expect(consoleError).toHaveBeenCalledWith(
      "Git 仓库变化事件监听注册失败",
      "workspace-a",
      listenError,
    );
    cleanup?.();

    mockListenGitRepoChanged.mockImplementation(
      async (callback: (event: GitRepoEvent) => void) => {
        eventCallback = callback;
        return mockUnlisten;
      },
    );
    mockRefresh.mockRejectedValue(refreshError);
    const secondCleanup = capturedEffects[1]?.();
    await flushMicrotasks();
    eventCallback?.({ workspaceId: "workspace-a" });
    await flushMicrotasks();

    expect(consoleError).toHaveBeenCalledWith(
      "Git 状态强制刷新失败",
      "workspace-a",
      refreshError,
    );
    secondCleanup?.();
  });
});
