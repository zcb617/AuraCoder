import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type { SshConfigHost, SshConnection, SshConnectionInput, SshConnectionTest } from "../types";
import { useEngineStore } from "./engineStore";
import { useWorkspaceStore } from "./workspaceStore";

interface SshConnectionState {
  /** 当前可用的 SSH 连接配置。 */
  connections: SshConnection[];
  /** 已软删除且可恢复的 SSH 连接配置。 */
  deletedConnections: SshConnection[];
  /** 本机 SSH 配置文件中的可导入主机。 */
  scanResults: SshConfigHost[];
  /** 最近一次成功执行的连接检测结果。 */
  tests: Record<string, SshConnectionTest | undefined>;
  /** 页面首次加载连接数据时的状态。 */
  loading: boolean;
  /** 扫描 SSH 配置文件时的状态。 */
  scanning: boolean;
  /** 当前 SSH 连接业务操作的错误提示。 */
  error: string | null;
  /** 加载当前连接与已删除连接列表。 */
  refresh: (silent?: boolean) => Promise<void>;
  /** 扫描本机 SSH 配置文件。 */
  scan: () => Promise<void>;
  /** 导入所选 SSH 配置主机并刷新实际连接状态。 */
  importHosts: (aliases: string[]) => Promise<void>;
  /** 新建手动 SSH 连接并返回已保存的连接信息。 */
  createManual: (input: SshConnectionInput) => Promise<SshConnection>;
  /** 更新 SSH 连接配置并刷新关联项目状态。 */
  update: (id: string, input: SshConnectionInput) => Promise<SshConnection>;
  /** 检测 SSH 连接并返回本次检测结果。 */
  test: (id: string) => Promise<SshConnectionTest>;
  /** 更新连接启用状态并刷新关联项目状态。 */
  setEnabled: (id: string, enabled: boolean) => Promise<void>;
  /** 删除 SSH 连接并刷新关联项目状态。 */
  remove: (id: string) => Promise<void>;
  /** 恢复 SSH 连接并刷新关联项目状态。 */
  restore: (id: string) => Promise<void>;
}

/** 在连接变更结束后尽力刷新引擎、连接列表和关联工作区状态。 */
async function refreshAfterConnectionChange(
  getState: () => SshConnectionState,
  id: string,
): Promise<void> {
  // 连接状态变化后失效引擎缓存，并分别刷新连接与工作区真实数据。
  try {
    useEngineStore.getState().invalidateConnection(id);
  } catch (error) {
    console.error("刷新 SSH 连接引擎状态失败", error);
  }
  try {
    await getState().refresh(true);
  } catch (error) {
    console.error("刷新 SSH 连接列表失败", error);
  }
  try {
    await useWorkspaceStore.getState().loadWorkspaces();
  } catch (error) {
    console.error("刷新 SSH 关联工作区失败", error);
  }
}

export const useSshConnectionStore = create<SshConnectionState>((set, get) => ({
  connections: [], deletedConnections: [], scanResults: [], tests: {}, loading: false, scanning: false, error: null,
  refresh: async (silent = false) => {
    if (!silent) set({ loading: true, error: null });
    try {
      const [connections, deletedConnections] = await Promise.all([ipc.listSshConnections(), ipc.listDeletedSshConnections()]);
      set({ connections, deletedConnections });
    } catch (error) {
      if (silent) console.error("静默刷新 SSH 连接列表失败", error);
      else set({ error: String(error) });
    } finally { if (!silent) set({ loading: false }); }
  },
  scan: async () => {
    set({ scanning: true, error: null });
    try { set({ scanResults: await ipc.scanSshConfigHosts() }); } catch (error) { set({ error: String(error), scanResults: [] }); } finally { set({ scanning: false }); }
  },
  importHosts: async (aliases) => {
    let importError: string | null = null;
    set({ error: null });
    try {
      const results = await ipc.importSshConfigHosts(aliases);
      const errors = results
        .filter((result) => result.error)
        .map((result) => `${result.alias}: ${result.error}`);
      if (errors.length > 0) importError = errors.join("\n");
    } catch (error) {
      importError = String(error);
    } finally {
      // 异常也先刷新真实连接和扫描结果，再恢复导入业务错误供页面展示。
      await get().refresh(true);
      await get().scan();
      if (importError) {
        const scanError = get().error;
        set({ error: scanError ? `${importError}\n扫描 SSH 配置失败：${scanError}` : importError });
      }
    }
  },
  createManual: async (input) => {
    set({ error: null });
    try {
      return await ipc.createManualSshConnection(input);
    } catch (error) {
      set({ error: String(error) });
      throw error;
    } finally {
      // 创建可能在保存后因监控异常失败，始终刷新后端实际状态。
      await get().refresh(true);
    }
  },
  update: async (id, input) => {
    set({ error: null });
    try {
      return await ipc.updateSshConnection(id, input);
    } catch (error) {
      set({ error: String(error) });
      throw error;
    } finally {
      await refreshAfterConnectionChange(get, id);
    }
  },
  test: async (id) => {
    set({ error: null });
    try {
      const value = await ipc.testSshConnection(id);
      set((state) => ({ tests: { ...state.tests, [id]: value } }));
      return value;
    } catch (error) {
      set({ error: String(error) });
      throw error;
    } finally {
      await refreshAfterConnectionChange(get, id);
    }
  },
  setEnabled: async (id, enabled) => {
    set({ error: null });
    try {
      await ipc.setSshConnectionEnabled(id, enabled);
    } catch (error) {
      set({ error: String(error) });
      throw error;
    } finally {
      await refreshAfterConnectionChange(get, id);
    }
  },
  remove: async (id) => {
    set({ error: null });
    try {
      await ipc.deleteSshConnection(id);
    } catch (error) {
      set({ error: String(error) });
      throw error;
    } finally {
      await refreshAfterConnectionChange(get, id);
    }
  },
  restore: async (id) => {
    set({ error: null });
    try {
      await ipc.restoreSshConnection(id);
    } catch (error) {
      set({ error: String(error) });
      throw error;
    } finally {
      await refreshAfterConnectionChange(get, id);
    }
  },
}));
