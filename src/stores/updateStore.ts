import { create } from "zustand";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { relaunch } from "@tauri-apps/plugin-process";
import { ipc } from "../lib/ipc";
import type { UpdateErrorStage, UpdateProcessState } from "../types";

export const DEFAULT_AUTO_UPDATE_INTERVAL_MINUTES = 30;

const AUTO_UPDATE_INTERVAL_STORAGE_KEY = "auracoder:auto-update-interval-minutes";

type UpdateStatus = UpdateProcessState["phase"] | "ready";
type DownloadPhase = "idle" | "downloading" | "installing";
type UpdateCheckMode = "manual" | "automatic";
type UpdateDownloadSource = UpdateCheckMode | null;
type UpdateCheckResult = UpdateProcessState | null;

function readAutoUpdateIntervalMinutes(): number {
  try {
    const stored = localStorage.getItem(AUTO_UPDATE_INTERVAL_STORAGE_KEY);
    if (stored === null) return DEFAULT_AUTO_UPDATE_INTERVAL_MINUTES;

    const value = Number.parseInt(stored, 10);
    return Number.isFinite(value) && value >= 0
      ? value
      : DEFAULT_AUTO_UPDATE_INTERVAL_MINUTES;
  } catch {
    return DEFAULT_AUTO_UPDATE_INTERVAL_MINUTES;
  }
}

function normalizeAutoUpdateIntervalMinutes(value: number): number {
  if (!Number.isFinite(value)) return DEFAULT_AUTO_UPDATE_INTERVAL_MINUTES;
  return Math.max(0, Math.floor(value));
}

function saveAutoUpdateIntervalMinutes(value: number): void {
  try {
    localStorage.setItem(AUTO_UPDATE_INTERVAL_STORAGE_KEY, String(value));
  } catch {
    // Ignore storage failures; the setting remains available for this session.
  }
}

export function isUpdateDownloaded(state: Pick<UpdateState, "status">): boolean {
  return state.status === "downloaded";
}

function mapUpdateState(state: UpdateProcessState): Partial<UpdateState> {
  return {
    status: state.phase,
    version: state.version,
    error: state.error,
    errorStage: state.errorStage ?? null,
    downloadPhase: state.phase === "installing" ? "installing" : state.phase === "downloading" ? "downloading" : "idle",
    downloadedBytes: state.downloadedBytes,
    totalBytes: state.totalBytes,
    downloadSource: state.source,
  };
}

interface UpdateState {
  status: UpdateStatus;
  version: string | null;
  error: string | null;
  /** 当前错误所属的业务阶段，供自动更新决定是否重试。 */
  errorStage: UpdateErrorStage | null;
  lastCheckedAt: number | null;
  downloadPhase: DownloadPhase;
  downloadedBytes: number;
  totalBytes: number | null;
  downloadSource: UpdateDownloadSource;
  autoUpdateIntervalMinutes: number;
  /** True after user clicks "Not now" — hides dot until next app launch */
  snoozed: boolean;

  restoreUpdateState: () => Promise<void>;
  runAutomaticUpdate: () => Promise<void>;
  checkForUpdate: (mode?: UpdateCheckMode) => Promise<UpdateCheckResult>;
  downloadUpdate: (mode?: UpdateCheckMode) => Promise<void>;
  downloadAndInstall: () => Promise<void>;
  isUpdateDownloaded: () => boolean;
  installDownloadedUpdate: () => Promise<void>;
  /** 选择并安装开发版 macOS 的本地更新包。 */
  installLocalUpdateForDevelopment: (archivePath: string) => Promise<void>;
  setAutoUpdateIntervalMinutes: (minutes: number) => void;
  resetToIdle: () => void;
  snooze: () => void;
}

let progressUnlisten: UnlistenFn | null = null;
let progressListenerPromise: Promise<void> | null = null;
let restorePromise: Promise<void> | null = null;
let checkPromise: Promise<UpdateCheckResult> | null = null;
let downloadPromise: Promise<void> | null = null;
let installPromise: Promise<void> | null = null;
let localInstallPromise: Promise<void> | null = null;
let localInstallCallingInstall = false;
let automaticUpdatePromise: Promise<void> | null = null;

async function ensureProgressListener(set: (state: Partial<UpdateState>) => void): Promise<void> {
  if (progressListenerPromise) return progressListenerPromise;

  progressListenerPromise = listen<UpdateProcessState>("update-download-progress", ({ payload }) => {
    set(mapUpdateState(payload));
  }).then((unlisten) => {
    progressUnlisten = unlisten;
  });
  await progressListenerPromise;
}

export const useUpdateStore = create<UpdateState>((set, get) => ({
  status: "idle",
  version: null,
  error: null,
  errorStage: null,
  lastCheckedAt: null,
  downloadPhase: "idle",
  downloadedBytes: 0,
  totalBytes: null,
  downloadSource: null,
  autoUpdateIntervalMinutes: readAutoUpdateIntervalMinutes(),
  snoozed: false,

  restoreUpdateState: () => {
    if (restorePromise) return restorePromise;
    restorePromise = (async () => {
      try {
        const state = await ipc.getUpdateState();
        set({ ...mapUpdateState(state), lastCheckedAt: state.phase === "idle" ? Date.now() : get().lastCheckedAt });
      } catch (error) {
        set({
          status: "error",
          error: error instanceof Error ? error.message : String(error),
          errorStage: "check",
        });
      }
    })().finally(() => {
      restorePromise = null;
    });
    return restorePromise;
  },

  runAutomaticUpdate: () => {
    if (automaticUpdatePromise) return automaticUpdatePromise;
    automaticUpdatePromise = (async () => {
      const currentStatus = get().status;
      if (currentStatus === "error" && get().errorStage === "install") {
        return;
      }
      if (["checking", "downloading", "downloaded", "installing", "ready"].includes(currentStatus)) {
        return;
      }
      if (currentStatus === "idle" || currentStatus === "error") {
        await get().restoreUpdateState();
      }
      if (get().status === "error" && get().errorStage === "install") return;
      if (get().isUpdateDownloaded()) return;

      if (get().status !== "available") {
        await get().checkForUpdate("automatic");
      }
      if (get().status !== "available") return;

      await get().downloadUpdate("automatic");
    })().finally(() => {
      automaticUpdatePromise = null;
    });
    return automaticUpdatePromise;
  },

  checkForUpdate: (mode = "manual") => {
    if (checkPromise) return checkPromise;
    checkPromise = (async () => {
      const currentStatus = get().status;
      if (["checking", "downloading", "downloaded", "installing", "ready"].includes(currentStatus)) {
        return null;
      }

      set({
        status: "checking",
        error: null,
        errorStage: null,
        downloadPhase: "idle",
        downloadSource: mode,
      });

      try {
        const state = await ipc.checkForUpdate(mode);
        set({ ...mapUpdateState(state), lastCheckedAt: Date.now() });
        return state;
      } catch (error) {
        const errorMessage = error instanceof Error ? error.message : String(error);
        const errorState: UpdateProcessState = {
          phase: "error",
          version: get().version,
          source: mode,
          downloadedBytes: get().downloadedBytes,
          totalBytes: get().totalBytes,
          error: errorMessage,
          errorStage: "check",
        };
        set({ ...mapUpdateState(errorState), lastCheckedAt: Date.now() });
        return errorState;
      }
    })().finally(() => {
      checkPromise = null;
    });
    return checkPromise;
  },

  downloadUpdate: (mode = "manual") => {
    if (downloadPromise) return downloadPromise;
    downloadPromise = (async () => {
      const currentStatus = get().status;
      if (["downloading", "downloaded", "installing", "ready"].includes(currentStatus)) return;

      set({ error: null, errorStage: null });
      try {
        await ensureProgressListener(set);
        const state = await ipc.downloadUpdate(mode);
        set(mapUpdateState(state));
      } catch (error) {
        set({
          status: "error",
          error: error instanceof Error ? error.message : String(error),
          errorStage: "download",
          downloadPhase: "idle",
        });
      }
    })().finally(() => {
      downloadPromise = null;
    });
    return downloadPromise;
  },

  downloadAndInstall: async () => {
    if (get().isUpdateDownloaded()) {
      await get().installDownloadedUpdate();
      return;
    }

    if (get().status !== "available") {
      await get().checkForUpdate("manual");
    }
    if (get().status !== "available") return;

    await get().downloadUpdate("manual");
    if (get().isUpdateDownloaded()) {
      await get().installDownloadedUpdate();
    }
  },

  isUpdateDownloaded: () => isUpdateDownloaded(get()),

  installDownloadedUpdate: () => {
    if (installPromise) return installPromise;
    if (localInstallPromise && !localInstallCallingInstall) return localInstallPromise;
    installPromise = (async () => {
      if (!get().isUpdateDownloaded()) return;

      set({ status: "installing", downloadPhase: "installing", error: null, errorStage: null });
      try {
        const result = await ipc.installDownloadedUpdate();
        set({ status: "ready", error: null, errorStage: null });
        if (result.restartMode === "tauriRelaunch") {
          await relaunch();
        }
      } catch (error) {
        set({
          status: "error",
          error: error instanceof Error ? error.message : String(error),
          errorStage: "install",
          downloadPhase: "idle",
        });
      }
    })().finally(() => {
      installPromise = null;
    });
    return installPromise;
  },

  installLocalUpdateForDevelopment: (archivePath) => {
    if (installPromise) return installPromise;
    if (localInstallPromise) return localInstallPromise;
    localInstallPromise = (async () => {
      try {
        const state = await ipc.prepareLocalUpdateForDevelopment(archivePath);
        set(mapUpdateState(state));
        localInstallCallingInstall = true;
        let installTask: Promise<void>;
        try {
          installTask = get().installDownloadedUpdate();
        } finally {
          localInstallCallingInstall = false;
        }
        await installTask;
      } catch (error) {
        set({
          status: "error",
          error: error instanceof Error ? error.message : String(error),
          errorStage: "install",
          downloadPhase: "idle",
        });
      }
    })().finally(() => {
      localInstallPromise = null;
    });
    return localInstallPromise;
  },

  setAutoUpdateIntervalMinutes: (minutes) => {
    const normalized = normalizeAutoUpdateIntervalMinutes(minutes);
    saveAutoUpdateIntervalMinutes(normalized);
    set({ autoUpdateIntervalMinutes: normalized });
  },

  resetToIdle: () => {
    set({
      status: "idle",
      version: null,
      error: null,
      errorStage: null,
      downloadSource: null,
      downloadPhase: "idle",
      downloadedBytes: 0,
      totalBytes: null,
    });
  },

  snooze: () => {
    set({ snoozed: true });
  },
}));

export function disposeUpdateProgressListener(): void {
  progressUnlisten?.();
  progressUnlisten = null;
  progressListenerPromise = null;
}
