import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type { SshConfigHost, SshConnection, SshConnectionInput, SshConnectionTest } from "../types";
import { useEngineStore } from "./engineStore";
import { useWorkspaceStore } from "./workspaceStore";

interface SshConnectionState {
  connections: SshConnection[];
  deletedConnections: SshConnection[];
  scanResults: SshConfigHost[];
  tests: Record<string, SshConnectionTest | undefined>;
  loading: boolean;
  scanning: boolean;
  error: string | null;
  refresh: (silent?: boolean) => Promise<void>;
  scan: () => Promise<void>;
  importHosts: (aliases: string[]) => Promise<void>;
  createManual: (input: SshConnectionInput) => Promise<SshConnection>;
  update: (id: string, input: SshConnectionInput) => Promise<SshConnection>;
  test: (id: string) => Promise<SshConnectionTest>;
  setEnabled: (id: string, enabled: boolean) => Promise<void>;
  remove: (id: string) => Promise<void>;
  restore: (id: string) => Promise<void>;
}

export const useSshConnectionStore = create<SshConnectionState>((set, get) => ({
  connections: [], deletedConnections: [], scanResults: [], tests: {}, loading: false, scanning: false, error: null,
  refresh: async (silent = false) => {
    if (!silent) set({ loading: true, error: null });
    try {
      const [connections, deletedConnections] = await Promise.all([ipc.listSshConnections(), ipc.listDeletedSshConnections()]);
      set({ connections, deletedConnections });
    } catch (error) { set({ error: String(error) }); } finally { if (!silent) set({ loading: false }); }
  },
  scan: async () => {
    set({ scanning: true, error: null });
    try { set({ scanResults: await ipc.scanSshConfigHosts() }); } catch (error) { set({ error: String(error), scanResults: [] }); } finally { set({ scanning: false }); }
  },
  importHosts: async (aliases) => { await ipc.importSshConfigHosts(aliases); await get().refresh(); await get().scan(); },
  createManual: async (input) => { const value = await ipc.createManualSshConnection(input); await get().refresh(); return value; },
  update: async (id, input) => { const value = await ipc.updateSshConnection(id, input); useEngineStore.getState().invalidateConnection(id); await get().refresh(); await useWorkspaceStore.getState().loadWorkspaces(); return value; },
  test: async (id) => { const value = await ipc.testSshConnection(id); useEngineStore.getState().invalidateConnection(id); set((state) => ({ tests: { ...state.tests, [id]: value } })); await get().refresh(); await useWorkspaceStore.getState().loadWorkspaces(); return value; },
  setEnabled: async (id, enabled) => { await ipc.setSshConnectionEnabled(id, enabled); useEngineStore.getState().invalidateConnection(id); await get().refresh(); await useWorkspaceStore.getState().loadWorkspaces(); },
  remove: async (id) => { await ipc.deleteSshConnection(id); useEngineStore.getState().invalidateConnection(id); await get().refresh(); await useWorkspaceStore.getState().loadWorkspaces(); },
  restore: async (id) => { await ipc.restoreSshConnection(id); useEngineStore.getState().invalidateConnection(id); await get().refresh(); await useWorkspaceStore.getState().loadWorkspaces(); },
}));
