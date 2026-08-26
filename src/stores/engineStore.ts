import { create } from "zustand";
import type {
  ChatProviderUsage,
  EngineHealth,
  EngineInfo,
  EngineRuntimeUpdatedEvent,
  ExecutionTarget,
} from "../types";
import { ipc } from "../lib/ipc";
import type { CliServicesUpdatedEvent } from "../lib/ipc";

/*
旧实现按执行目标缓存 CLI、模型、健康状态和用量，并在命中缓存后跳过后端接口。
统一 CLI 生命周期已经由后端负责，前端不能再保存第二套 CLI 状态来源。
旧实现完整保留在本注释中，下面的新实现只保存当前页面最近一次实时请求的结果。

interface EngineState {
  target: ExecutionTarget | null;
  engines: EngineInfo[];
  enginesByTarget: Record<string, EngineInfo[]>;
  health: Record<string, EngineHealth>;
  healthByTarget: Record<string, Record<string, EngineHealth>>;
  usage: Record<string, ChatProviderUsage>;
  usageByTarget: Record<string, Record<string, ChatProviderUsage>>;
  healthLoading: Record<string, boolean>;
  healthLoadingByTarget: Record<string, Record<string, boolean>>;
  engineCatalogLoading: Record<string, boolean>;
  engineCatalogLoadingByTarget: Record<string, Record<string, boolean>>;
  usageLoading: Record<string, boolean>;
  usageLoadingByTarget: Record<string, Record<string, boolean>>;
  targetGenerations: Record<string, number>;
  loading: boolean;
  loadedOnce: boolean;
  activeWorkspaceId: string | null;
  error?: string;
  load: (workspaceId?: string | null) => Promise<void>;
  refreshEngineCatalog: (engineId: string) => Promise<EngineInfo | null>;
  ensureHealth: (
    engineId: string,
    options?: { force?: boolean },
  ) => Promise<EngineHealth | null>;
  ensureUsage: (
    engineId: string,
    options?: { force?: boolean },
  ) => Promise<ChatProviderUsage | null>;
  invalidateConnection: (connectionId: string) => void;
  mergeHealth: (reports: EngineHealth[]) => void;
  applyRuntimeUpdate: (event: EngineRuntimeUpdatedEvent) => void;
}

let pendingHealthRequests: Partial<Record<string, Promise<EngineHealth | null>>> = {};
let pendingEngineCatalogRequests: Partial<Record<string, Promise<EngineInfo | null>>> = {};
let pendingUsageRequests: Partial<Record<string, Promise<ChatProviderUsage | null>>> = {};
let engineLoadSequence = 0;

// 阶段计划 5 前使用 workspaceId 作为请求键；现在统一使用后端返回的稳定 targetKey。
// function targetRequestKey(workspaceId: string | null, engineId: string): string {
//   return `${workspaceId ?? "local"}:${engineId}`;
// }
function targetRequestKey(targetKey: string, engineId: string): string {
  return `${targetKey}:${engineId}`;
}

export const useEngineStore = create<EngineState>((set, get) => ({
  target: null,
  engines: [],
  enginesByTarget: {},
  health: {},
  healthByTarget: {},
  usage: {},
  usageByTarget: {},
  healthLoading: {},
  healthLoadingByTarget: {},
  engineCatalogLoading: {},
  engineCatalogLoadingByTarget: {},
  usageLoading: {},
  usageLoadingByTarget: {},
  targetGenerations: {},
  loading: false,
  loadedOnce: false,
  activeWorkspaceId: null,
  load: async (workspaceId = null) => {
    const normalizedWorkspaceId = workspaceId ?? null;
    const sequence = ++engineLoadSequence;
    const previousTarget = get().target;
    set({
      target: null,
      engines: [],
      health: {},
      usage: {},
      healthLoading: {},
      engineCatalogLoading: {},
      usageLoading: {},
      loading: true,
      activeWorkspaceId: normalizedWorkspaceId,
      error: undefined,
    });
    try {
      const target = await ipc.getExecutionTarget(normalizedWorkspaceId);
      const targetIdentityChanged =
        previousTarget?.targetKey === target.targetKey &&
        (previousTarget.connectionStatus !== target.connectionStatus ||
          previousTarget.hostName !== target.hostName ||
          previousTarget.port !== target.port ||
          previousTarget.user !== target.user);
      if (targetIdentityChanged && target.connectionId) {
        get().invalidateConnection(target.connectionId);
      }

      const targetGeneration = get().targetGenerations[target.targetKey] ?? 0;
      const cachedEngines = get().enginesByTarget[target.targetKey];
      if (
        sequence === engineLoadSequence &&
        get().activeWorkspaceId === normalizedWorkspaceId
      ) {
        set((state) => ({
          target,
          engines: cachedEngines ?? [],
          health: state.healthByTarget[target.targetKey] ?? {},
          usage: state.usageByTarget[target.targetKey] ?? {},
          usageLoading: state.usageLoadingByTarget[target.targetKey] ?? {},
          healthLoading: state.healthLoadingByTarget[target.targetKey] ?? {},
          engineCatalogLoading:
            state.engineCatalogLoadingByTarget[target.targetKey] ?? {},
          loading: cachedEngines === undefined,
          loadedOnce: cachedEngines !== undefined || state.loadedOnce,
          error: undefined,
        }));
      }
      if (cachedEngines !== undefined) {
        return;
      }

      const engines = normalizedWorkspaceId
        ? await ipc.listActivedClis(normalizedWorkspaceId)
        : await ipc.listActivedClis();
      set((state) => {
        if ((state.targetGenerations[target.targetKey] ?? 0) !== targetGeneration) {
          return state;
        }
        const enginesByTarget = {
          ...state.enginesByTarget,
          [target.targetKey]: engines,
        };
        if (
          sequence !== engineLoadSequence ||
          state.activeWorkspaceId !== normalizedWorkspaceId ||
          state.target?.targetKey !== target.targetKey
        ) {
          return { enginesByTarget };
        }
        return {
          engines,
          enginesByTarget,
          health: state.healthByTarget[target.targetKey] ?? {},
          healthLoading: state.healthLoadingByTarget[target.targetKey] ?? {},
          engineCatalogLoading:
            state.engineCatalogLoadingByTarget[target.targetKey] ?? {},
          loading: false,
          loadedOnce: true,
          error: undefined,
        };
      });
    } catch (error) {
      if (
        sequence !== engineLoadSequence ||
        get().activeWorkspaceId !== normalizedWorkspaceId
      ) {
        return;
      }
      const message = String(error);
      set({
        loading: false,
        loadedOnce: true,
        error: message,
        health: {
          codex: {
            id: "codex",
            available: false,
            details: `Engine discovery failed: ${message}`,
            warnings: [],
            checks: [],
            fixes: [],
          },
        },
      });
    }
  },
  refreshEngineCatalog: async (engineId) => {
    const workspaceId = get().activeWorkspaceId;
    const targetKey = get().target?.targetKey ?? "local";
    const targetGeneration = get().targetGenerations[targetKey] ?? 0;
    const requestKey = targetRequestKey(targetKey, engineId);
    const pendingRequest = pendingEngineCatalogRequests[requestKey];
    if (pendingRequest) {
      return pendingRequest;
    }

    set((state) => {
      const loadingForTarget = {
        ...(state.engineCatalogLoadingByTarget[targetKey] ?? {}),
        [engineId]: true,
      };
      return {
        engineCatalogLoadingByTarget: {
          ...state.engineCatalogLoadingByTarget,
          [targetKey]: loadingForTarget,
        },
        engineCatalogLoading:
          state.target?.targetKey === targetKey
            ? loadingForTarget
            : state.engineCatalogLoading,
      };
    });

    const request = (async () => {
      try {
        const engine = workspaceId
          ? await ipc.getEngineInfo(engineId, workspaceId)
          : await ipc.getEngineInfo(engineId);
        set((state) => {
          if ((state.targetGenerations[targetKey] ?? 0) !== targetGeneration) {
            return state;
          }
          const targetEngines = state.enginesByTarget[targetKey] ?? [];
          const engineIndex = targetEngines.findIndex((item) => item.id === engine.id);
          const engines = engineIndex >= 0
            ? targetEngines.map((item) => (item.id === engine.id ? engine : item))
            : [...targetEngines, engine];
          const targetHealth = state.healthByTarget[targetKey] ?? {};
          const health = {
            ...targetHealth,
            [engine.id]: {
              id: engine.id,
              available: true,
              details: targetHealth[engine.id]?.details,
              warnings: targetHealth[engine.id]?.warnings ?? [],
              checks: targetHealth[engine.id]?.checks ?? [],
              fixes: targetHealth[engine.id]?.fixes ?? [],
              protocolDiagnostics: targetHealth[engine.id]?.protocolDiagnostics,
            },
          };
          const activeTarget = state.target?.targetKey === targetKey;
          return {
            enginesByTarget: {
              ...state.enginesByTarget,
              [targetKey]: engines,
            },
            healthByTarget: {
              ...state.healthByTarget,
              [targetKey]: health,
            },
            engines: activeTarget ? engines : state.engines,
            health: activeTarget ? health : state.health,
            error:
              activeTarget && state.error?.startsWith(`${engineId}:`)
                ? undefined
                : state.error,
          };
        });
        return engine;
      } catch (error) {
        const message = String(error);
        set((state) => {
          if ((state.targetGenerations[targetKey] ?? 0) !== targetGeneration) {
            return state;
          }
          const targetHealth = {
            ...(state.healthByTarget[targetKey] ?? {}),
            [engineId]: {
              id: engineId,
              available: false,
              details: message,
              warnings: [],
              checks: [],
              fixes: [],
            },
          };
          const activeTarget = state.target?.targetKey === targetKey;
          return {
            healthByTarget: {
              ...state.healthByTarget,
              [targetKey]: targetHealth,
            },
            error: activeTarget ? `${engineId}: ${message}` : state.error,
            health: activeTarget ? targetHealth : state.health,
          };
        });
        return null;
      } finally {
        if ((get().targetGenerations[targetKey] ?? 0) === targetGeneration) {
          delete pendingEngineCatalogRequests[requestKey];
        }
        set((state) => {
          if ((state.targetGenerations[targetKey] ?? 0) !== targetGeneration) {
            return state;
          }
          const { [engineId]: _ignored, ...rest } =
            state.engineCatalogLoadingByTarget[targetKey] ?? {};
          return {
            engineCatalogLoadingByTarget: {
              ...state.engineCatalogLoadingByTarget,
              [targetKey]: rest,
            },
            engineCatalogLoading:
              state.target?.targetKey === targetKey
                ? rest
                : state.engineCatalogLoading,
          };
        });
      }
    })();

    pendingEngineCatalogRequests[requestKey] = request;
    return request;
  },
  ensureHealth: async (engineId, options) => {
    const workspaceId = get().activeWorkspaceId;
    const targetKey = get().target?.targetKey ?? "local";
    const targetGeneration = get().targetGenerations[targetKey] ?? 0;
    const requestKey = targetRequestKey(targetKey, engineId);
    const existing = get().healthByTarget[targetKey]?.[engineId];
    if (existing && !options?.force) {
      return existing;
    }

    if (pendingHealthRequests[requestKey]) {
      return pendingHealthRequests[requestKey];
    }

    set((state) => {
      const loadingForTarget = {
        ...(state.healthLoadingByTarget[targetKey] ?? {}),
        [engineId]: true,
      };
      return {
        healthLoadingByTarget: {
          ...state.healthLoadingByTarget,
          [targetKey]: loadingForTarget,
        },
        healthLoading:
          state.target?.targetKey === targetKey
            ? loadingForTarget
            : state.healthLoading,
      };
    });

    const request = (async () => {
      try {
        const health = workspaceId
          ? await ipc.engineHealth(engineId, workspaceId)
          : await ipc.engineHealth(engineId);
        set((state) => {
          if ((state.targetGenerations[targetKey] ?? 0) !== targetGeneration) {
            return state;
          }
          const targetHealth = {
            ...(state.healthByTarget[targetKey] ?? {}),
            [health.id]: health,
          };
          const { [engineId]: _ignored, ...rest } =
            state.healthLoadingByTarget[targetKey] ?? {};
          const activeTarget = state.target?.targetKey === targetKey;
          return {
            healthByTarget: {
              ...state.healthByTarget,
              [targetKey]: targetHealth,
            },
            healthLoadingByTarget: {
              ...state.healthLoadingByTarget,
              [targetKey]: rest,
            },
            health: activeTarget ? targetHealth : state.health,
            healthLoading: activeTarget ? rest : state.healthLoading,
          };
        });
        const engine = get().enginesByTarget[targetKey]?.find(
          (item) => item.id === engineId,
        );
        if (
          health.available &&
          engine &&
          engine.models.length === 0 &&
          get().target?.targetKey === targetKey
        ) {
          await get().refreshEngineCatalog(engineId);
        }
        return health;
      } catch (error) {
        const message = String(error);
        set((state) => {
          if ((state.targetGenerations[targetKey] ?? 0) !== targetGeneration) {
            return state;
          }
          const { [engineId]: _ignored, ...rest } =
            state.healthLoadingByTarget[targetKey] ?? {};
          const activeTarget = state.target?.targetKey === targetKey;
          return {
            healthLoadingByTarget: {
              ...state.healthLoadingByTarget,
              [targetKey]: rest,
            },
            healthLoading: activeTarget ? rest : state.healthLoading,
            error: activeTarget ? `${engineId}: ${message}` : state.error,
          };
        });
        return null;
      } finally {
        if ((get().targetGenerations[targetKey] ?? 0) === targetGeneration) {
          delete pendingHealthRequests[requestKey];
        }
      }
    })();

    pendingHealthRequests[requestKey] = request;
    return request;
  },
  ensureUsage: async (engineId, options) => {
    const workspaceId = get().activeWorkspaceId;
    const targetKey = get().target?.targetKey ?? "local";
    const targetGeneration = get().targetGenerations[targetKey] ?? 0;
    const requestKey = targetRequestKey(targetKey, engineId);
    const existing = get().usageByTarget[targetKey]?.[engineId];
    if (existing && !options?.force) {
      return existing;
    }
    if (pendingUsageRequests[requestKey]) {
      return pendingUsageRequests[requestKey];
    }

    set((state) => {
      const loadingForTarget = {
        ...(state.usageLoadingByTarget[targetKey] ?? {}),
        [engineId]: true,
      };
      return {
        usageLoadingByTarget: {
          ...state.usageLoadingByTarget,
          [targetKey]: loadingForTarget,
        },
        usageLoading:
          state.target?.targetKey === targetKey
            ? loadingForTarget
            : state.usageLoading,
      };
    });
    const request = (async () => {
      try {
        const providers = await ipc.getChatProviderUsage(workspaceId, engineId);
        const provider = providers.find((item) => item.engineId === engineId) ?? null;
        if (provider) {
          set((state) => {
            if ((state.targetGenerations[targetKey] ?? 0) !== targetGeneration) {
              return state;
            }
            const targetUsage = {
              ...(state.usageByTarget[targetKey] ?? {}),
              [engineId]: provider,
            };
            return {
              usageByTarget: {
                ...state.usageByTarget,
                [targetKey]: targetUsage,
              },
              usage:
                state.target?.targetKey === targetKey
                  ? targetUsage
                  : state.usage,
            };
          });
        }
        return provider;
      } catch (error) {
        if (
          get().target?.targetKey === targetKey &&
          (get().targetGenerations[targetKey] ?? 0) === targetGeneration
        ) {
          set({ error: `${engineId}: ${String(error)}` });
        }
        return null;
      } finally {
        if ((get().targetGenerations[targetKey] ?? 0) === targetGeneration) {
          delete pendingUsageRequests[requestKey];
        }
        set((state) => {
          if ((state.targetGenerations[targetKey] ?? 0) !== targetGeneration) {
            return state;
          }
          const { [engineId]: _ignored, ...rest } =
            state.usageLoadingByTarget[targetKey] ?? {};
          return {
            usageLoadingByTarget: {
              ...state.usageLoadingByTarget,
              [targetKey]: rest,
            },
            usageLoading:
              state.target?.targetKey === targetKey
                ? rest
                : state.usageLoading,
          };
        });
      }
    })();
    pendingUsageRequests[requestKey] = request;
    return request;
  },
  invalidateConnection: (connectionId) => {
    const targetKey = `ssh:${connectionId}`;
    for (const requestKey of Object.keys(pendingHealthRequests)) {
      if (requestKey.startsWith(`${targetKey}:`)) {
        delete pendingHealthRequests[requestKey];
      }
    }
    for (const requestKey of Object.keys(pendingEngineCatalogRequests)) {
      if (requestKey.startsWith(`${targetKey}:`)) {
        delete pendingEngineCatalogRequests[requestKey];
      }
    }
    for (const requestKey of Object.keys(pendingUsageRequests)) {
      if (requestKey.startsWith(`${targetKey}:`)) {
        delete pendingUsageRequests[requestKey];
      }
    }
    set((state) => {
      const { [targetKey]: _engines, ...enginesByTarget } = state.enginesByTarget;
      const { [targetKey]: _health, ...healthByTarget } = state.healthByTarget;
      const { [targetKey]: _usage, ...usageByTarget } = state.usageByTarget;
      const { [targetKey]: _healthLoading, ...healthLoadingByTarget } =
        state.healthLoadingByTarget;
      const { [targetKey]: _catalogLoading, ...engineCatalogLoadingByTarget } =
        state.engineCatalogLoadingByTarget;
      const { [targetKey]: _usageLoading, ...usageLoadingByTarget } =
        state.usageLoadingByTarget;
      const active = state.target?.targetKey === targetKey;
      return {
        targetGenerations: {
          ...state.targetGenerations,
          [targetKey]: (state.targetGenerations[targetKey] ?? 0) + 1,
        },
        enginesByTarget,
        healthByTarget,
        usageByTarget,
        healthLoadingByTarget,
        engineCatalogLoadingByTarget,
        usageLoadingByTarget,
        engines: active ? [] : state.engines,
        health: active ? {} : state.health,
        usage: active ? {} : state.usage,
        healthLoading: active ? {} : state.healthLoading,
        engineCatalogLoading: active ? {} : state.engineCatalogLoading,
        usageLoading: active ? {} : state.usageLoading,
      };
    });
  },
  mergeHealth: (reports) =>
    set((state) => {
      if (reports.length === 0) {
        return state;
      }

      const targetKey = state.target?.targetKey ?? "local";
      const nextHealth = { ...(state.healthByTarget[targetKey] ?? {}) };
      const nextHealthLoading = {
        ...(state.healthLoadingByTarget[targetKey] ?? {}),
      };
      for (const report of reports) {
        nextHealth[report.id] = report;
        delete nextHealthLoading[report.id];
      }

      return {
        health: nextHealth,
        healthByTarget: {
          ...state.healthByTarget,
          [targetKey]: nextHealth,
        },
        healthLoading: nextHealthLoading,
        healthLoadingByTarget: {
          ...state.healthLoadingByTarget,
          [targetKey]: nextHealthLoading,
        },
      };
    }),
  applyRuntimeUpdate: ({ engineId, protocolDiagnostics }) =>
    set((state) => {
      // 全局 Codex runtime 事件来自本机 engine registry，不能污染当前 SSH 目标。
      const targetKey = "local";
      const localHealth = state.healthByTarget[targetKey] ?? {};
      const current = localHealth[engineId];
      const nextHealth: EngineHealth = current
        ? {
            ...current,
            available: true,
            details: current.available ? current.details : undefined,
            protocolDiagnostics: protocolDiagnostics ?? current.protocolDiagnostics,
          }
        : {
            id: engineId,
            available: true,
            warnings: [],
            checks: [],
            fixes: [],
            protocolDiagnostics,
          };

      const { [engineId]: _ignored, ...rest } =
        state.healthLoadingByTarget[targetKey] ?? {};
      const nextLocalHealth = {
        ...localHealth,
        [engineId]: nextHealth,
      };
      const localActive = state.target?.targetKey === targetKey;

      return {
        healthByTarget: {
          ...state.healthByTarget,
          [targetKey]: nextLocalHealth,
        },
        healthLoadingByTarget: {
          ...state.healthLoadingByTarget,
          [targetKey]: rest,
        },
        health: localActive ? nextLocalHealth : state.health,
        healthLoading: localActive ? rest : state.healthLoading,
      };
    }),
}));
*/

interface EngineState {
  target: ExecutionTarget | null;
  engines: EngineInfo[];
  /**
   * 按执行目标缓存的 CLI 目录（local 或 ssh:{connectionId}）。
   * 缓存在启动完成后一次性预热，之后由后端健康检查事件驱动刷新；
   * 页面只读缓存，不再每次打开都实时拉取。
   */
  enginesByTarget: Record<string, EngineInfo[]>;
  health: Record<string, EngineHealth>;
  usage: Record<string, ChatProviderUsage>;
  healthLoading: Record<string, boolean>;
  engineCatalogLoading: Record<string, boolean>;
  usageLoading: Record<string, boolean>;
  loading: boolean;
  loadedOnce: boolean;
  activeWorkspaceId: string | null;
  error?: string;
  load: (workspaceId?: string | null) => Promise<void>;
  preloadCatalogs: () => Promise<void>;
  applyCliServicesUpdated: (
    event: Pick<CliServicesUpdatedEvent, "scope" | "connectionId" | "revision">,
  ) => Promise<void>;
  refreshEngineCatalog: (engineId: string) => Promise<EngineInfo | null>;
  ensureHealth: (
    engineId: string,
    options?: { force?: boolean },
  ) => Promise<EngineHealth | null>;
  ensureUsage: (
    engineId: string,
    options?: { force?: boolean },
  ) => Promise<ChatProviderUsage | null>;
  invalidateConnection: (connectionId: string) => void;
  mergeHealth: (reports: EngineHealth[]) => void;
  applyRuntimeUpdate: (event: EngineRuntimeUpdatedEvent) => void;
}

let pendingHealthRequests: Partial<Record<string, Promise<EngineHealth | null>>> = {};
let pendingEngineCatalogRequests: Partial<Record<string, Promise<EngineInfo | null>>> = {};
let pendingUsageRequests: Partial<Record<string, Promise<ChatProviderUsage | null>>> = {};
let engineLoadSequence = 0;
const requestGenerations: Record<string, number> = {};

function targetRequestKey(targetKey: string, engineId: string): string {
  return `${targetKey}:${engineId}`;
}

function isCurrentTarget(
  state: EngineState,
  workspaceId: string | null,
  targetKey: string,
  requestGeneration: number,
): boolean {
  return (
    state.activeWorkspaceId === workspaceId &&
    state.target?.targetKey === targetKey &&
    (requestGenerations[targetKey] ?? 0) === requestGeneration
  );
}

/**
 * 拉取指定执行目标的 CLI 目录并写入缓存；命中当前激活目标时同步更新页面视图。
 * 启动预热和后端健康检查事件刷新共用本条取数路径（与启动时首次取数是同一个
 * listActivedClis 接口）。
 */
async function fetchAndCacheCatalog(
  targetKey: string,
  connectionId: string | null,
): Promise<void> {
  const requestGeneration = requestGenerations[targetKey] ?? 0;
  const applyCatalog = (engines: EngineInfo[]) => {
    if ((requestGenerations[targetKey] ?? 0) !== requestGeneration) {
      return;
    }
    useEngineStore.setState((state) => {
      const enginesByTarget = { ...state.enginesByTarget, [targetKey]: engines };
      return state.target?.targetKey === targetKey
        ? { enginesByTarget, engines, error: undefined }
        : { enginesByTarget };
    });
  };

  try {
    const engines = connectionId
      ? await ipc.listActivedClis(connectionId)
      : await ipc.listActivedClis();
    applyCatalog(engines);
  } catch (error) {
    // 后端在目标没有任何已激活 CLI 时返回错误；健康检查 reconcile 后这代表
    // 目录已被清空，缓存同步置空。其他错误保留旧缓存，避免瞬时故障清空界面。
    if (String(error).includes("没有已激活")) {
      applyCatalog([]);
      return;
    }
    console.warn(`Failed to refresh engine catalog for ${targetKey}:`, error);
  }
}

export const useEngineStore = create<EngineState>((set, get) => ({
  target: null,
  engines: [],
  enginesByTarget: {},
  health: {},
  usage: {},
  healthLoading: {},
  engineCatalogLoading: {},
  usageLoading: {},
  loading: false,
  loadedOnce: false,
  activeWorkspaceId: null,

  load: async (workspaceId = null) => {
    const normalizedWorkspaceId = workspaceId ?? null;
    const sequence = ++engineLoadSequence;
    // 不再同步清空已有目录：缓存由启动预热和后端健康检查事件维护，重复加载
    // 直接复用缓存，避免每次使用都经历“清空 → 远端拉取”的转圈窗口。
    set({
      activeWorkspaceId: normalizedWorkspaceId,
      error: undefined,
    });

    try {
      const target = await ipc.getExecutionTarget(normalizedWorkspaceId);
      if (
        sequence !== engineLoadSequence ||
        get().activeWorkspaceId !== normalizedWorkspaceId
      ) {
        return;
      }

      const targetChanged = get().target?.targetKey !== target.targetKey;
      const cachedEngines = get().enginesByTarget[target.targetKey];
      set((state) => ({
        target,
        engines: cachedEngines ?? (targetChanged ? [] : state.engines),
        health: targetChanged ? {} : state.health,
        usage: targetChanged ? {} : state.usage,
        healthLoading: targetChanged ? {} : state.healthLoading,
        engineCatalogLoading: targetChanged ? {} : state.engineCatalogLoading,
        usageLoading: targetChanged ? {} : state.usageLoading,
        loading: cachedEngines === undefined,
        loadedOnce: cachedEngines !== undefined || state.loadedOnce,
        error: undefined,
      }));
      if (cachedEngines !== undefined) {
        return;
      }

      const targetRequestGeneration = requestGenerations[target.targetKey] ?? 0;

      let engines: EngineInfo[];
      if (target.kind === "ssh") {
        if (!target.connectionId) {
          throw new Error("远端项目未绑定 SSH 连接");
        }
        engines = await ipc.listActivedClis(target.connectionId);
      } else {
        engines = await ipc.listActivedClis();
      }
      if (
        sequence !== engineLoadSequence ||
        !isCurrentTarget(
          get(),
          normalizedWorkspaceId,
          target.targetKey,
          targetRequestGeneration,
        )
      ) {
        return;
      }
      set((state) => ({
        engines,
        enginesByTarget: { ...state.enginesByTarget, [target.targetKey]: engines },
        loading: false,
        loadedOnce: true,
        error: undefined,
      }));
    } catch (error) {
      if (
        sequence !== engineLoadSequence ||
        get().activeWorkspaceId !== normalizedWorkspaceId
      ) {
        return;
      }
      set({
        loading: false,
        loadedOnce: true,
        error: String(error),
      });
    }
  },

  preloadCatalogs: async () => {
    // 启动完成后一次性预热本机和全部已启用 SSH 连接的 CLI 目录缓存。
    const tasks: Promise<void>[] = [fetchAndCacheCatalog("local", null)];
    try {
      const connections = await ipc.listSshConnections();
      for (const connection of connections) {
        if (!connection.enabled) {
          continue;
        }
        tasks.push(fetchAndCacheCatalog(`ssh:${connection.id}`, connection.id));
      }
    } catch (error) {
      console.warn("Failed to list SSH connections for engine catalog preload:", error);
    }
    await Promise.all(tasks);
  },

  applyCliServicesUpdated: async (event) => {
    const targetKey =
      event.scope === "ssh" && event.connectionId
        ? `ssh:${event.connectionId}`
        : "local";
    await fetchAndCacheCatalog(
      targetKey,
      event.scope === "ssh" ? event.connectionId : null,
    );
  },

  refreshEngineCatalog: async (engineId) => {
    const workspaceId = get().activeWorkspaceId;
    const targetKey = get().target?.targetKey ?? "local";
    const requestGeneration = requestGenerations[targetKey] ?? 0;
    const requestKey = targetRequestKey(targetKey, engineId);
    const pendingRequest = pendingEngineCatalogRequests[requestKey];
    if (pendingRequest) {
      return pendingRequest;
    }

    set((state) => ({
      engineCatalogLoading: {
        ...state.engineCatalogLoading,
        [engineId]: true,
      },
    }));

    const request = (async () => {
      try {
        const engine = workspaceId
          ? await ipc.getEngineInfo(engineId, workspaceId)
          : await ipc.getEngineInfo(engineId);
        if (!isCurrentTarget(get(), workspaceId, targetKey, requestGeneration)) {
          return engine;
        }
        set((state) => {
          const engines = state.engines.some((item) => item.id === engine.id)
            ? state.engines.map((item) => (item.id === engine.id ? engine : item))
            : [...state.engines, engine];
          return {
            engines,
            // 单引擎目录刷新同步写穿目标缓存，避免视图与缓存分叉。
            enginesByTarget: { ...state.enginesByTarget, [targetKey]: engines },
            health: {
              ...state.health,
              [engine.id]: {
                id: engine.id,
                available: true,
                details: state.health[engine.id]?.details,
                warnings: state.health[engine.id]?.warnings ?? [],
                checks: state.health[engine.id]?.checks ?? [],
                fixes: state.health[engine.id]?.fixes ?? [],
                protocolDiagnostics: state.health[engine.id]?.protocolDiagnostics,
              },
            },
            error: state.error?.startsWith(`${engineId}:`) ? undefined : state.error,
          };
        });
        return engine;
      } catch (error) {
        const message = String(error);
        if (isCurrentTarget(get(), workspaceId, targetKey, requestGeneration)) {
          set((state) => ({
            health: {
              ...state.health,
              [engineId]: {
                id: engineId,
                available: false,
                details: message,
                warnings: [],
                checks: [],
                fixes: [],
              },
            },
            error: `${engineId}: ${message}`,
          }));
        }
        return null;
      } finally {
        if ((requestGenerations[targetKey] ?? 0) === requestGeneration) {
          delete pendingEngineCatalogRequests[requestKey];
        }
        if (isCurrentTarget(get(), workspaceId, targetKey, requestGeneration)) {
          set((state) => {
            const { [engineId]: _ignored, ...rest } = state.engineCatalogLoading;
            return { engineCatalogLoading: rest };
          });
        }
      }
    })();

    pendingEngineCatalogRequests[requestKey] = request;
    return request;
  },

  ensureHealth: async (engineId, _options) => {
    const workspaceId = get().activeWorkspaceId;
    const targetKey = get().target?.targetKey ?? "local";
    const requestGeneration = requestGenerations[targetKey] ?? 0;
    const requestKey = targetRequestKey(targetKey, engineId);
    const pendingRequest = pendingHealthRequests[requestKey];
    if (pendingRequest) {
      return pendingRequest;
    }

    set((state) => ({
      healthLoading: {
        ...state.healthLoading,
        [engineId]: true,
      },
    }));

    const request = (async () => {
      try {
        const health = workspaceId
          ? await ipc.engineHealth(engineId, workspaceId)
          : await ipc.engineHealth(engineId);
        if (!isCurrentTarget(get(), workspaceId, targetKey, requestGeneration)) {
          return health;
        }
        set((state) => {
          const { [engineId]: _ignored, ...rest } = state.healthLoading;
          return {
            health: { ...state.health, [health.id]: health },
            healthLoading: rest,
          };
        });

        const engine = get().engines.find((item) => item.id === engineId);
        if (health.available && engine && engine.models.length === 0) {
          await get().refreshEngineCatalog(engineId);
        }
        return health;
      } catch (error) {
        if (isCurrentTarget(get(), workspaceId, targetKey, requestGeneration)) {
          set((state) => {
            const { [engineId]: _ignored, ...rest } = state.healthLoading;
            return {
              healthLoading: rest,
              error: `${engineId}: ${String(error)}`,
            };
          });
        }
        return null;
      } finally {
        if ((requestGenerations[targetKey] ?? 0) === requestGeneration) {
          delete pendingHealthRequests[requestKey];
        }
      }
    })();

    pendingHealthRequests[requestKey] = request;
    return request;
  },

  ensureUsage: async (engineId, _options) => {
    const workspaceId = get().activeWorkspaceId;
    const targetKey = get().target?.targetKey ?? "local";
    const requestGeneration = requestGenerations[targetKey] ?? 0;
    const requestKey = targetRequestKey(targetKey, engineId);
    const pendingRequest = pendingUsageRequests[requestKey];
    if (pendingRequest) {
      return pendingRequest;
    }

    set((state) => ({
      usageLoading: {
        ...state.usageLoading,
        [engineId]: true,
      },
    }));

    const request = (async () => {
      try {
        const providers = await ipc.getChatProviderUsage(workspaceId, engineId);
        const provider = providers.find((item) => item.engineId === engineId) ?? null;
        if (
          provider &&
          isCurrentTarget(get(), workspaceId, targetKey, requestGeneration)
        ) {
          set((state) => ({
            usage: { ...state.usage, [engineId]: provider },
          }));
        }
        return provider;
      } catch (error) {
        if (isCurrentTarget(get(), workspaceId, targetKey, requestGeneration)) {
          set({ error: `${engineId}: ${String(error)}` });
        }
        return null;
      } finally {
        if ((requestGenerations[targetKey] ?? 0) === requestGeneration) {
          delete pendingUsageRequests[requestKey];
        }
        if (isCurrentTarget(get(), workspaceId, targetKey, requestGeneration)) {
          set((state) => {
            const { [engineId]: _ignored, ...rest } = state.usageLoading;
            return { usageLoading: rest };
          });
        }
      }
    })();

    pendingUsageRequests[requestKey] = request;
    return request;
  },

  invalidateConnection: (connectionId) => {
    const targetKey = `ssh:${connectionId}`;
    requestGenerations[targetKey] = (requestGenerations[targetKey] ?? 0) + 1;
    for (const requestKey of Object.keys(pendingHealthRequests)) {
      if (requestKey.startsWith(`${targetKey}:`)) delete pendingHealthRequests[requestKey];
    }
    for (const requestKey of Object.keys(pendingEngineCatalogRequests)) {
      if (requestKey.startsWith(`${targetKey}:`)) delete pendingEngineCatalogRequests[requestKey];
    }
    for (const requestKey of Object.keys(pendingUsageRequests)) {
      if (requestKey.startsWith(`${targetKey}:`)) delete pendingUsageRequests[requestKey];
    }
    set((state) => {
      const { [targetKey]: _dropped, ...enginesByTarget } = state.enginesByTarget;
      return state.target?.connectionId === connectionId
        ? {
            enginesByTarget,
            engines: [],
            health: {},
            usage: {},
            healthLoading: {},
            engineCatalogLoading: {},
            usageLoading: {},
            error: undefined,
          }
        : { enginesByTarget };
    });
  },

  mergeHealth: (reports) =>
    set((state) => {
      if (reports.length === 0) return state;
      const nextHealth = { ...state.health };
      const nextHealthLoading = { ...state.healthLoading };
      for (const report of reports) {
        nextHealth[report.id] = report;
        delete nextHealthLoading[report.id];
      }
      return { health: nextHealth, healthLoading: nextHealthLoading };
    }),

  applyRuntimeUpdate: ({ engineId, protocolDiagnostics }) =>
    set((state) => {
      if (state.target?.targetKey !== "local") return state;
      const current = state.health[engineId];
      const nextHealth: EngineHealth = current
        ? {
            ...current,
            available: true,
            details: current.available ? current.details : undefined,
            protocolDiagnostics: protocolDiagnostics ?? current.protocolDiagnostics,
          }
        : {
            id: engineId,
            available: true,
            warnings: [],
            checks: [],
            fixes: [],
            protocolDiagnostics,
          };
      const { [engineId]: _ignored, ...rest } = state.healthLoading;
      return {
        health: { ...state.health, [engineId]: nextHealth },
        healthLoading: rest,
      };
    }),
}));
