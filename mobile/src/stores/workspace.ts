import { reactive } from "vue";
import type { EngineInfo, Workspace } from "../types";
import { auracoderConnectionManager } from "./auracoder-connection";

const itemsByAuraCoderId = reactive<Record<string, Workspace[]>>({});
const enginesByAuraCoderId = reactive<Record<string, EngineInfo[]>>({});
const loadingByAuraCoderId = reactive<Record<string, boolean>>({});

export const workspaceStore = {
  itemsByAuraCoderId,
  enginesByAuraCoderId,
  loadingByAuraCoderId,
  getItems(auracoderId: string) {
    return itemsByAuraCoderId[auracoderId] || [];
  },
  getEngines(auracoderId: string) {
    return enginesByAuraCoderId[auracoderId] || [];
  },
  async load(auracoderId: string, force = false) {
    if (!force && itemsByAuraCoderId[auracoderId]) return itemsByAuraCoderId[auracoderId];
    loadingByAuraCoderId[auracoderId] = true;
    try {
      // 保留原有并行请求实现，便于追溯此次真机故障的根因。
      /*
      const [workspaces, engines] = await Promise.all([
        auracoderConnectionManager.request<Workspace[]>(auracoderId, "workspace.list"),
        auracoderConnectionManager.request<EngineInfo[]>(auracoderId, "engine.list"),
      ]);
      itemsByAuraCoderId[auracoderId] = workspaces;
      enginesByAuraCoderId[auracoderId] = engines;
      return workspaces;
      */
      const workspaces = await auracoderConnectionManager.request<Workspace[]>(auracoderId, "workspace.list");
      itemsByAuraCoderId[auracoderId] = workspaces;
      try {
        const engines = await auracoderConnectionManager.request<EngineInfo[]>(auracoderId, "engine.list");
        enginesByAuraCoderId[auracoderId] = engines;
      } catch (error) {
        // 项目列表与引擎列表互不依赖；后者失败时保留已经成功加载的项目。
        console.warn("加载引擎列表失败", error);
      }
      return workspaces;
    } finally {
      loadingByAuraCoderId[auracoderId] = false;
    }
  },
  async loadEngines(auracoderId: string) {
    if (enginesByAuraCoderId[auracoderId]) return enginesByAuraCoderId[auracoderId];
    const engines = await auracoderConnectionManager.request<EngineInfo[]>(auracoderId, "engine.list");
    enginesByAuraCoderId[auracoderId] = engines;
    return engines;
  },
  clear(auracoderId: string) {
    delete itemsByAuraCoderId[auracoderId];
    delete enginesByAuraCoderId[auracoderId];
    delete loadingByAuraCoderId[auracoderId];
  },
};
