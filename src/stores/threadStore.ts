import { create } from "zustand";
import { ipc } from "../lib/ipc";
import {
  NEW_THREAD_FALLBACK_RUNTIME,
  resolveNewThreadRuntime,
  type NewThreadServiceTier,
} from "../lib/newThreadRuntime";
import { resolvePreferredOnboardingChatSelection } from "../lib/onboarding";
import type { Thread } from "../types";
import { useChatComposerStore } from "./chatComposerStore";
import { useEngineStore } from "./engineStore";
import { useOnboardingStore } from "./onboardingStore";

interface EnsureThreadInput {
  workspaceId: string;
  engineId?: string;
  modelId?: string;
  reasoningEffort?: string | null;
  serviceTier?: NewThreadServiceTier | null;
  title?: string;
}

interface CreateThreadInput {
  workspaceId: string;
  engineId?: string;
  modelId?: string;
  reasoningEffort?: string | null;
  serviceTier?: NewThreadServiceTier | null;
  title?: string;
}

interface ThreadState {
  threads: Thread[];
  threadsByWorkspace: Record<string, Thread[]>;
  archivedThreadsByWorkspace: Record<string, Thread[]>;
  activeThreadId: string | null;
  loading: boolean;
  error?: string;
  createThread: (input: CreateThreadInput) => Promise<string | null>;
  renameThread: (threadId: string, title: string) => Promise<void>;
  ensureThreadForScope: (input: EnsureThreadInput) => Promise<string | null>;
  refreshThreads: (workspaceId: string) => Promise<void>;
  /** 仅从本地数据库重载指定 workspace 的线程，不触发远端会话发现。 */
  reloadThreadsFromLocalDatabase: (workspaceId: string) => Promise<void>;
  refreshArchivedThreads: (workspaceId: string) => Promise<void>;
  refreshAllThreads: (workspaceIds: string[]) => Promise<void>;
  removeThread: (threadId: string) => Promise<void>;
  restoreThread: (threadId: string) => Promise<void>;
  forkCodexThread: (threadId: string) => Promise<Thread | null>;
  rollbackCodexThread: (threadId: string, numTurns: number) => Promise<Thread | null>;
  compactCodexThread: (threadId: string) => Promise<Thread | null>;
  attachCodexRemoteThread: (
    workspaceId: string,
    engineThreadId: string,
    // 旧签名由界面传入 modelId，导致导入历史会话时覆盖 Codex 的实际模型；禁止恢复。
    // modelId: string,
  ) => Promise<Thread | null>;
  attachOpenCodeRemoteSession: (
    workspaceId: string,
    engineThreadId: string,
    cwd: string,
    modelId: string,
  ) => Promise<Thread | null>;
  setActiveThread: (threadId: string | null) => void;
  applyThreadUpdateLocal: (thread: Thread) => boolean;
  markThreadAwaitingApproval: (threadId: string) => void;
  setThreadReasoningEffortLocal: (threadId: string, reasoningEffort: string | null) => void;
  setThreadLastModelLocal: (threadId: string, modelId: string | null) => void;
}

const DEFAULT_ENGINE = NEW_THREAD_FALLBACK_RUNTIME.engineId;
const DEFAULT_MODEL = NEW_THREAD_FALLBACK_RUNTIME.modelId;

function mergeWorkspaceThreads(
  current: Record<string, Thread[]>,
  workspaceId: string,
  threads: Thread[],
): Record<string, Thread[]> {
  return {
    ...current,
    [workspaceId]: threads,
  };
}

function flattenThreadsByWorkspace(threadsByWorkspace: Record<string, Thread[]>): Thread[] {
  return Object.values(threadsByWorkspace)
    .flat()
    .sort((a, b) => new Date(b.lastActivityAt).getTime() - new Date(a.lastActivityAt).getTime());
}

function applyThreadReasoningEffort(
  thread: Thread,
  reasoningEffort: string | null
): Thread {
  return {
    ...thread,
    reasoningEffort,
  };
}

function applyThreadLastModel(
  thread: Thread,
  modelId: string | null
): Thread {
  return {
    ...thread,
    modelId: modelId ?? thread.modelId,
  };
}

function readThreadLastModelId(thread: Thread): string | null {
  const raw = thread.modelId;
  if (typeof raw !== "string") {
    return null;
  }
  const normalized = raw.trim();
  return normalized.length > 0 ? normalized : null;
}

function threadMatchesRequestedModel(thread: Thread, modelId: string): boolean {
  return thread.modelId === modelId;
}

const LAST_THREAD_KEY = "auracoder:lastActiveThreadId";

const CODEX_REMOTE_THREAD_PAGE_SIZE = 100;
const codexRemoteThreadDiscoveryInFlight = new Map<string, Promise<void>>();

// 旧逻辑在导入历史会话前选取一个全局默认模型，并把它写给所有 Codex 会话；禁止恢复。
// function defaultCodexModelId(): string | null {
//   const codexEngine = useEngineStore.getState().engines.find((engine) => engine.id === "codex");
//   if (!codexEngine) {
//     return null;
//   }
//
//   return (
//     codexEngine.models.find((model) => model.isDefault && !model.hidden) ??
//     codexEngine.models.find((model) => !model.hidden) ??
//     codexEngine.models[0]
//   )?.id ?? null;
// }

async function discoverCodexRemoteThreads(
  workspaceId: string,
  localThreads: Thread[],
): Promise<void> {
  const pending = codexRemoteThreadDiscoveryInFlight.get(workspaceId);
  if (pending) {
    return pending;
  }

  // const modelId = defaultCodexModelId();
  // if (!modelId) {
  //   return;
  // }

  const localThreadsById = new Map(localThreads.map((thread) => [thread.id, thread]));
  const discovery = (async () => {
    // 旧实现每条 attach 失败只写一句无上下文的 console.warn，发布版无从排查；
    // 现在逐条记录工作区与完整错误文本，并在结束时输出导入汇总。
    let attachedCount = 0;
    let failedCount = 0;
    try {
      let cursor: string | null = null;
      const seenCursors = new Set<string>();

      while (true) {
        const page = await ipc.listCodexRemoteThreads(workspaceId, {
          cursor,
          limit: CODEX_REMOTE_THREAD_PAGE_SIZE,
          archived: false,
        });

        for (const remoteThread of page.threads) {
          // 旧逻辑：if (remoteThread.localThreadId != null) continue; // 忽略远端更新时间，禁止执行
          const localThread = remoteThread.localThreadId
            ? localThreadsById.get(remoteThread.localThreadId)
            : undefined;
          const isSameActivityTime =
            localThread !== undefined &&
            Date.parse(localThread.lastActivityAt) === Date.parse(remoteThread.updatedAt);
          if (remoteThread.localThreadId !== null && isSameActivityTime) {
            continue;
          }

          try {
            // await ipc.attachCodexRemoteThread(workspaceId, remoteThread.engineThreadId, modelId);
            await ipc.attachCodexRemoteThread(workspaceId, remoteThread.engineThreadId);
            attachedCount += 1;
          } catch (error) {
            failedCount += 1;
            // console.warn(
            //   `Failed to attach discovered Codex thread ${remoteThread.engineThreadId}:`,
            //   error,
            // );
            console.warn(
              `Failed to attach discovered Codex thread ${remoteThread.engineThreadId} in workspace ${workspaceId}: ${String(error)}`,
              error,
            );
          }
        }

        const nextCursor = page.nextCursor ?? null;
        if (!nextCursor || seenCursors.has(nextCursor)) {
          return;
        }
        seenCursors.add(nextCursor);
        cursor = nextCursor;
      }
    } catch (error) {
      // console.warn(`Failed to discover Codex threads for workspace ${workspaceId}:`, error);
      console.warn(
        `Failed to discover Codex threads for workspace ${workspaceId}: ${String(error)}`,
        error,
      );
    } finally {
      if (attachedCount > 0 || failedCount > 0) {
        console.info(
          `Codex thread discovery finished for workspace ${workspaceId}: attached=${attachedCount} failed=${failedCount}`,
        );
      }
      codexRemoteThreadDiscoveryInFlight.delete(workspaceId);
    }
  })();

  codexRemoteThreadDiscoveryInFlight.set(workspaceId, discovery);
  return discovery;
}

function resolveImplicitNewThreadRuntime(
  state: Pick<ThreadState, "threads" | "activeThreadId">,
  workspaceId: string,
) {
  const engines = useEngineStore.getState().engines;
  const onboardingSelection = resolvePreferredOnboardingChatSelection(
    useOnboardingStore.getState().selectedChatEngines,
    engines,
  );
  const composerRuntime =
    useChatComposerStore.getState().runtimeByWorkspace[workspaceId] ?? null;
  const activeThread =
    state.threads.find(
      (thread) =>
        thread.id === state.activeThreadId &&
        thread.workspaceId === workspaceId,
    ) ?? null;

  return resolveNewThreadRuntime({
    engines,
    composerRuntime,
    activeThread,
    onboardingSelection,
  });
}

export const useThreadStore = create<ThreadState>((set, get) => ({
  threads: [],
  threadsByWorkspace: {},
  archivedThreadsByWorkspace: {},
  activeThreadId: null,
  loading: false,
  // 创建会话并更新线程集合；失败时保留错误状态并向公共动作传播原始异常。
  createThread: async ({
    workspaceId,
    engineId,
    modelId,
    reasoningEffort,
    serviceTier,
    title,
  }) => {
    const effectiveRuntime =
      engineId || modelId || reasoningEffort || serviceTier
        ? {
            engineId: engineId ?? DEFAULT_ENGINE,
            modelId: modelId ?? DEFAULT_MODEL,
            reasoningEffort: reasoningEffort ?? null,
            serviceTier: serviceTier ?? null,
          }
        : resolveImplicitNewThreadRuntime(get(), workspaceId);

    set({ loading: true, error: undefined });

    try {
      const created = await ipc.createThread(
        workspaceId,
        effectiveRuntime.engineId,
        effectiveRuntime.modelId,
        title ?? "Workspace Chat",
        effectiveRuntime.reasoningEffort,
        effectiveRuntime.serviceTier,
      );

      const existingWorkspaceThreads = get().threadsByWorkspace[workspaceId] ?? [];
      const workspaceThreads = [created, ...existingWorkspaceThreads.filter((thread) => thread.id !== created.id)];
      const threadsByWorkspace = mergeWorkspaceThreads(get().threadsByWorkspace, workspaceId, workspaceThreads);
      const threads = flattenThreadsByWorkspace(threadsByWorkspace);

      localStorage.setItem(LAST_THREAD_KEY, created.id);
      set({
        threadsByWorkspace,
        threads,
        activeThreadId: created.id,
        loading: false,
      });

      return created.id;
    } catch (error) {
      set({ loading: false, error: String(error) });
      /* return null; */
      throw error;
    }
  },
  renameThread: async (threadId, title) => {
    set({ loading: true, error: undefined });
    try {
      const updated = await ipc.renameThread(threadId, title);
      set((state) => {
        const updateThread = (thread: Thread) => (thread.id === updated.id ? updated : thread);
        const threadsByWorkspace = Object.entries(state.threadsByWorkspace).reduce<
          Record<string, Thread[]>
        >((acc, [workspaceId, threads]) => {
          acc[workspaceId] = threads.map(updateThread);
          return acc;
        }, {});
        const threads = flattenThreadsByWorkspace(threadsByWorkspace);

        return {
          threadsByWorkspace,
          threads,
          loading: false,
        };
      });
    } catch (error) {
      set({ loading: false, error: String(error) });
    }
  },
  ensureThreadForScope: async ({
    workspaceId,
    engineId,
    modelId,
    reasoningEffort,
    serviceTier,
    title,
  }) => {
    const fallbackRuntime = resolveImplicitNewThreadRuntime(get(), workspaceId);
    const effectiveEngine = engineId ?? fallbackRuntime.engineId;
    const effectiveModel = modelId ?? fallbackRuntime.modelId;
    const effectiveReasoningEffort =
      reasoningEffort ?? fallbackRuntime.reasoningEffort;
    const effectiveServiceTier = serviceTier ?? fallbackRuntime.serviceTier;

    set({ loading: true, error: undefined });

    try {
      const all = await ipc.listThreads(workspaceId);
      const scoped = all.filter(
        (thread) =>
          thread.engineId === effectiveEngine
      );
      const scopedForModel = scoped
        .filter((thread) => threadMatchesRequestedModel(thread, effectiveModel))
        .sort(
          (a, b) =>
            new Date(b.lastActivityAt).getTime() - new Date(a.lastActivityAt).getTime(),
        );

      const activeId = get().activeThreadId;
      let selected =
        scopedForModel.find((thread) => thread.id === activeId) ?? scopedForModel[0];
      if (!selected) {
        selected = await ipc.createThread(
          workspaceId,
          effectiveEngine,
          effectiveModel,
          title ?? "General",
          effectiveReasoningEffort,
          effectiveServiceTier,
        );
      }

      const workspaceThreads = [selected, ...all.filter((thread) => thread.id !== selected.id)];
      const threadsByWorkspace = mergeWorkspaceThreads(get().threadsByWorkspace, workspaceId, workspaceThreads);
      const threads = flattenThreadsByWorkspace(threadsByWorkspace);
      set({
        threadsByWorkspace,
        threads,
        activeThreadId: selected.id,
        loading: false
      });
      return selected.id;
    } catch (error) {
      set({ loading: false, error: String(error) });
      return null;
    }
  },
  refreshThreads: async (workspaceId) => {
    set({ loading: true, error: undefined });
    try {
      const localThreads = await ipc.listThreads(workspaceId);
      // 旧逻辑先发现、后读取本地会话，无法按刷新前快照判断远端更新时间；顺序错误，禁止执行。
      // await discoverCodexRemoteThreads(workspaceId);
      await discoverCodexRemoteThreads(workspaceId, localThreads);
      const workspaceThreads = await ipc.listThreads(workspaceId);
      const threadsByWorkspace = mergeWorkspaceThreads(get().threadsByWorkspace, workspaceId, workspaceThreads);
      const threads = flattenThreadsByWorkspace(threadsByWorkspace);
      const active = get().activeThreadId;
      set({
        threadsByWorkspace,
        threads,
        // 旧逻辑会在 activeThreadId 为 null 时自动选中刷新结果的第一条会话：
        // activeThreadId:
        //   active && threads.some((item) => item.id === active)
        //     ? active
        //     : workspaceThreads[0]?.id ?? null,
        // null 表示用户明确停留在“新对话”，后台同步不得把输入目标切换到旧会话。
        activeThreadId:
          active && threads.some((item) => item.id === active) ? active : null,
        loading: false
      });
    } catch (error) {
      set({ loading: false, error: String(error) });
    }
  },
  reloadThreadsFromLocalDatabase: async (workspaceId) => {
    try {
      // 同步通知只代表本地数据库已更新；这里禁止触发任何远端 CLI 会话发现。
      const workspaceThreads = await ipc.listThreads(workspaceId);
      const threadsByWorkspace = mergeWorkspaceThreads(
        get().threadsByWorkspace,
        workspaceId,
        workspaceThreads,
      );
      set({
        threadsByWorkspace,
        threads: flattenThreadsByWorkspace(threadsByWorkspace),
      });
    } catch (error) {
      // 不修改线程集合，保留通知到达前的内存数据，并让调用方决定如何提示用户。
      throw error;
    }
  },
  refreshArchivedThreads: async (workspaceId) => {
    try {
      const archivedThreads = await ipc.listArchivedThreads(workspaceId);
      set((state) => ({
        archivedThreadsByWorkspace: {
          ...state.archivedThreadsByWorkspace,
          [workspaceId]: archivedThreads,
        },
      }));
    } catch (error) {
      set({ error: String(error) });
    }
  },
  refreshAllThreads: async (workspaceIds) => {
    if (!workspaceIds.length) {
      set({
        threads: [],
        threadsByWorkspace: {},
        archivedThreadsByWorkspace: {},
        activeThreadId: null,
        loading: false,
        error: undefined,
      });
      return;
    }

    set({ loading: true, error: undefined });
    try {
      /*
      旧启动执行逻辑通过 Promise.all 并行发现远端 Codex，会话边界不再符合本地项目逐项同步要求，保留但禁止执行：
      const results = await Promise.all(
        workspaceIds.map(async (workspaceId) => {
          const localThreads = await ipc.listThreads(workspaceId);
          await discoverCodexRemoteThreads(workspaceId, localThreads);
          return {
            workspaceId,
            threads: await ipc.listThreads(workspaceId),
          };
        }),
      );
      */
      const results: Array<{ workspaceId: string; threads: Thread[] }> = [];
      for (const workspaceId of workspaceIds) {
        try {
          await ipc.refreshLocalProjectSessions(workspaceId);
        } catch (error) {
          console.warn(
            `Failed to refresh local project sessions for workspace ${workspaceId}: ${String(error)}`,
            error,
          );
        }
        // 旧启动路径通过 listThreads + discoverCodexRemoteThreads 发现远端会话；
        // await discoverCodexRemoteThreads(workspaceId, await ipc.listThreads(workspaceId));
        results.push({
          workspaceId,
          threads: await ipc.listThreads(workspaceId),
        });
      }

      // const threadsByWorkspace = results.reduce<Record<string, Thread[]>>((acc, item) => {
      //   acc[item.workspaceId] = item.threads;
      //   return acc;
      // }, {});
      // const threads = flattenThreadsByWorkspace(threadsByWorkspace);
      // const active = get().activeThreadId;
      // const savedId = localStorage.getItem(LAST_THREAD_KEY);
      // const restoredId =
      //   (active && threads.some((item) => item.id === active)) ? active
      //   : (savedId && threads.some((item) => item.id === savedId)) ? savedId
      //   : null;
      //
      // set({
      //   threadsByWorkspace,
      //   threads,
      //   activeThreadId: restoredId,
      //   loading: false,
      // });
      set((state) => {
        const threadsByWorkspace = results.reduce<Record<string, Thread[]>>((acc, item) => {
          acc[item.workspaceId] = item.threads;
          return acc;
        }, { ...state.threadsByWorkspace });
        const threads = flattenThreadsByWorkspace(threadsByWorkspace);
        const active = state.activeThreadId;
        const savedId = localStorage.getItem(LAST_THREAD_KEY);
        const restoredId =
          (active && threads.some((item) => item.id === active)) ? active
          : (savedId && threads.some((item) => item.id === savedId)) ? savedId
          : null;

        return {
          threadsByWorkspace,
          threads,
          activeThreadId: restoredId,
          loading: false,
        };
      });
    } catch (error) {
      set({ loading: false, error: String(error) });
    }
  },
  removeThread: async (threadId) => {
    set({ loading: true, error: undefined });
    try {
      await ipc.archiveThread(threadId);
      let archivedThread: Thread | null = null;
      let archivedWorkspaceId: string | null = null;
      const nextThreadsByWorkspace = Object.entries(get().threadsByWorkspace).reduce<
        Record<string, Thread[]>
      >((acc, [workspaceId, threads]) => {
        const target = threads.find((thread) => thread.id === threadId);
        if (target) {
          archivedThread = target;
          archivedWorkspaceId = workspaceId;
        }
        const remaining = threads.filter((thread) => thread.id !== threadId);
        acc[workspaceId] = remaining;
        return acc;
      }, {});
      const threads = flattenThreadsByWorkspace(nextThreadsByWorkspace);
      const active = get().activeThreadId;

      set((state) => {
        const archivedThreadsByWorkspace = { ...state.archivedThreadsByWorkspace };
        if (archivedThread && archivedWorkspaceId) {
          const currentArchived = archivedThreadsByWorkspace[archivedWorkspaceId] ?? [];
          archivedThreadsByWorkspace[archivedWorkspaceId] = [
            archivedThread,
            ...currentArchived.filter((thread) => thread.id !== threadId),
          ];
        }

        return {
          threadsByWorkspace: nextThreadsByWorkspace,
          archivedThreadsByWorkspace,
          threads,
          activeThreadId: active === threadId ? null : active,
          loading: false,
        };
      });
    } catch (error) {
      set({ loading: false, error: String(error) });
      throw error;
    }
  },
  restoreThread: async (threadId) => {
    set({ loading: true, error: undefined });
    try {
      const restored = await ipc.restoreThread(threadId);
      set((state) => {
        const workspaceId = restored.workspaceId;
        const workspaceThreads = state.threadsByWorkspace[workspaceId] ?? [];
        const nextWorkspaceThreads = [
          restored,
          ...workspaceThreads.filter((thread) => thread.id !== threadId),
        ];
        const threadsByWorkspace = mergeWorkspaceThreads(
          state.threadsByWorkspace,
          workspaceId,
          nextWorkspaceThreads,
        );
        const archivedThreads = state.archivedThreadsByWorkspace[workspaceId] ?? [];
        const archivedThreadsByWorkspace = {
          ...state.archivedThreadsByWorkspace,
          [workspaceId]: archivedThreads.filter((thread) => thread.id !== threadId),
        };

        return {
          threadsByWorkspace,
          archivedThreadsByWorkspace,
          threads: flattenThreadsByWorkspace(threadsByWorkspace),
          loading: false,
        };
      });
    } catch (error) {
      set({ loading: false, error: String(error) });
      // 继续把后端业务提示交给侧边栏展示，失败时归档列表保持不变。
      throw error;
    }
  },
  forkCodexThread: async (threadId) => {
    set({ loading: true, error: undefined });
    try {
      const forked = await ipc.forkCodexThread(threadId);
      localStorage.setItem(LAST_THREAD_KEY, forked.id);
      set((state) => {
        const workspaceId = forked.workspaceId;
        const workspaceThreads = state.threadsByWorkspace[workspaceId] ?? [];
        const nextWorkspaceThreads = [
          forked,
          ...workspaceThreads.filter((thread) => thread.id !== forked.id),
        ];
        const threadsByWorkspace = mergeWorkspaceThreads(
          state.threadsByWorkspace,
          workspaceId,
          nextWorkspaceThreads,
        );

        return {
          threadsByWorkspace,
          threads: flattenThreadsByWorkspace(threadsByWorkspace),
          activeThreadId: forked.id,
          loading: false,
        };
      });
      return forked;
    } catch (error) {
      set({ loading: false, error: String(error) });
      return null;
    }
  },
  rollbackCodexThread: async (threadId, numTurns) => {
    set({ loading: true, error: undefined });
    try {
      const rolledBack = await ipc.rollbackCodexThread(threadId, numTurns);
      localStorage.setItem(LAST_THREAD_KEY, rolledBack.id);
      set((state) => {
        const workspaceId = rolledBack.workspaceId;
        const workspaceThreads = state.threadsByWorkspace[workspaceId] ?? [];
        const nextWorkspaceThreads = [
          rolledBack,
          ...workspaceThreads.filter((thread) => thread.id !== rolledBack.id),
        ];
        const threadsByWorkspace = mergeWorkspaceThreads(
          state.threadsByWorkspace,
          workspaceId,
          nextWorkspaceThreads,
        );

        return {
          threadsByWorkspace,
          threads: flattenThreadsByWorkspace(threadsByWorkspace),
          activeThreadId: rolledBack.id,
          loading: false,
        };
      });
      return rolledBack;
    } catch (error) {
      set({ loading: false, error: String(error) });
      return null;
    }
  },
  compactCodexThread: async (threadId) => {
    set({ loading: true, error: undefined });
    try {
      const compacted = await ipc.compactCodexThread(threadId);
      set((state) => {
        const workspaceId = compacted.workspaceId;
        const workspaceThreads = state.threadsByWorkspace[workspaceId] ?? [];
        const nextWorkspaceThreads = workspaceThreads.map((thread) =>
          thread.id === compacted.id ? compacted : thread,
        );
        const threadsByWorkspace = mergeWorkspaceThreads(
          state.threadsByWorkspace,
          workspaceId,
          nextWorkspaceThreads,
        );

        return {
          threadsByWorkspace,
          threads: flattenThreadsByWorkspace(threadsByWorkspace),
          loading: false,
        };
      });
      return compacted;
    } catch (error) {
      set({ loading: false, error: String(error) });
      return null;
    }
  },
  attachCodexRemoteThread: async (workspaceId, engineThreadId) => {
    set({ loading: true, error: undefined });
    try {
      // const attached = await ipc.attachCodexRemoteThread(workspaceId, engineThreadId, modelId);
      const attached = await ipc.attachCodexRemoteThread(workspaceId, engineThreadId);
      localStorage.setItem(LAST_THREAD_KEY, attached.id);
      set((state) => {
        const workspaceThreads = state.threadsByWorkspace[workspaceId] ?? [];
        const nextWorkspaceThreads = [
          attached,
          ...workspaceThreads.filter((thread) => thread.id !== attached.id),
        ];
        const threadsByWorkspace = mergeWorkspaceThreads(
          state.threadsByWorkspace,
          workspaceId,
          nextWorkspaceThreads,
        );
        const archivedThreads = state.archivedThreadsByWorkspace[workspaceId] ?? [];
        const archivedThreadsByWorkspace = {
          ...state.archivedThreadsByWorkspace,
          [workspaceId]: archivedThreads.filter((thread) => thread.id !== attached.id),
        };

        return {
          threadsByWorkspace,
          archivedThreadsByWorkspace,
          threads: flattenThreadsByWorkspace(threadsByWorkspace),
          activeThreadId: attached.id,
          loading: false,
        };
      });
      return attached;
    } catch (error) {
      set({ loading: false, error: String(error) });
      return null;
    }
  },
  attachOpenCodeRemoteSession: async (workspaceId, engineThreadId, cwd, modelId) => {
    set({ loading: true, error: undefined });
    try {
      const attached = await ipc.attachOpenCodeRemoteSession(
        workspaceId,
        engineThreadId,
        cwd,
        modelId,
      );
      localStorage.setItem(LAST_THREAD_KEY, attached.id);
      set((state) => {
        const workspaceThreads = state.threadsByWorkspace[workspaceId] ?? [];
        const nextWorkspaceThreads = [
          attached,
          ...workspaceThreads.filter((thread) => thread.id !== attached.id),
        ];
        const threadsByWorkspace = mergeWorkspaceThreads(
          state.threadsByWorkspace,
          workspaceId,
          nextWorkspaceThreads,
        );
        const archivedThreads = state.archivedThreadsByWorkspace[workspaceId] ?? [];
        const archivedThreadsByWorkspace = {
          ...state.archivedThreadsByWorkspace,
          [workspaceId]: archivedThreads.filter((thread) => thread.id !== attached.id),
        };

        return {
          threadsByWorkspace,
          archivedThreadsByWorkspace,
          threads: flattenThreadsByWorkspace(threadsByWorkspace),
          activeThreadId: attached.id,
          loading: false,
        };
      });
      return attached;
    } catch (error) {
      set({ loading: false, error: String(error) });
      return null;
    }
  },
  setActiveThread: (threadId) => {
    if (threadId) {
      localStorage.setItem(LAST_THREAD_KEY, threadId);
    } else {
      localStorage.removeItem(LAST_THREAD_KEY);
    }
    set({ activeThreadId: threadId });
  },
  applyThreadUpdateLocal: (updatedThread) => {
    let applied = false;

    set((state) => {
      const workspaceThreads = state.threadsByWorkspace[updatedThread.workspaceId];
      if (!workspaceThreads?.some((thread) => thread.id === updatedThread.id)) {
        return state;
      }

      applied = true;
      const nextWorkspaceThreads = workspaceThreads.map((thread) =>
        thread.id === updatedThread.id ? updatedThread : thread,
      );
      const threadsByWorkspace = mergeWorkspaceThreads(
        state.threadsByWorkspace,
        updatedThread.workspaceId,
        nextWorkspaceThreads,
      );
      const archivedThreads = state.archivedThreadsByWorkspace[updatedThread.workspaceId] ?? [];
      const archivedThreadsByWorkspace = archivedThreads.some(
        (thread) => thread.id === updatedThread.id,
      )
        ? {
            ...state.archivedThreadsByWorkspace,
            [updatedThread.workspaceId]: archivedThreads.map((thread) =>
              thread.id === updatedThread.id ? updatedThread : thread,
            ),
          }
        : state.archivedThreadsByWorkspace;

      return {
        threadsByWorkspace,
        archivedThreadsByWorkspace,
        threads: flattenThreadsByWorkspace(threadsByWorkspace),
      };
    });

    return applied;
  },
  markThreadAwaitingApproval: (threadId) =>
    set((state) => {
      const updateThread = (thread: Thread) =>
        thread.id === threadId && thread.status !== "awaiting_approval"
          ? { ...thread, status: "awaiting_approval" as const }
          : thread;

      const threadsByWorkspace = Object.entries(state.threadsByWorkspace).reduce<
        Record<string, Thread[]>
      >((acc, [workspaceId, threads]) => {
        acc[workspaceId] = threads.map(updateThread);
        return acc;
      }, {});

      return {
        threadsByWorkspace,
        threads: flattenThreadsByWorkspace(threadsByWorkspace),
      };
    }),
  setThreadReasoningEffortLocal: (threadId, reasoningEffort) =>
    set((state) => {
      const updateThread = (thread: Thread) =>
        thread.id === threadId
          ? applyThreadReasoningEffort(thread, reasoningEffort)
          : thread;

      const threadsByWorkspace = Object.entries(state.threadsByWorkspace).reduce<
        Record<string, Thread[]>
      >((acc, [workspaceId, threads]) => {
        acc[workspaceId] = threads.map(updateThread);
        return acc;
      }, {});

      return {
        threadsByWorkspace,
        threads: state.threads.map(updateThread),
      };
    }),
  setThreadLastModelLocal: (threadId, modelId) =>
    set((state) => {
      const updateThread = (thread: Thread) =>
        thread.id === threadId
          ? applyThreadLastModel(thread, modelId)
          : thread;

      const threadsByWorkspace = Object.entries(state.threadsByWorkspace).reduce<
        Record<string, Thread[]>
      >((acc, [workspaceId, threads]) => {
        acc[workspaceId] = threads.map(updateThread);
        return acc;
      }, {});

      return {
        threadsByWorkspace,
        threads: state.threads.map(updateThread),
      };
    }),
}));
