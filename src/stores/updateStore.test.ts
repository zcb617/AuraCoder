import { beforeEach, describe, expect, it, vi } from "vitest";

const ipcMocks = vi.hoisted(() => ({
  getUpdateState: vi.fn(),
  checkForUpdate: vi.fn(),
  downloadUpdate: vi.fn(),
  installDownloadedUpdate: vi.fn(),
}));

const relaunchMock = vi.hoisted(() => vi.fn());
const storageMock = vi.hoisted(() => ({
  getItem: vi.fn(),
  setItem: vi.fn(),
}));

vi.mock("../lib/ipc", () => ({
  ipc: ipcMocks,
}));

vi.mock("@tauri-apps/plugin-process", () => ({
  relaunch: relaunchMock,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async () => vi.fn()),
}));

import { isUpdateDownloaded, useUpdateStore } from "./updateStore";

const idleState = {
  phase: "idle" as const,
  version: null,
  source: null,
  downloadedBytes: 0,
  totalBytes: null,
  error: null,
};

describe("updateStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.stubGlobal("localStorage", storageMock);
    useUpdateStore.setState({
      status: "idle",
      version: null,
      error: null,
      lastCheckedAt: null,
      downloadPhase: "idle",
      downloadedBytes: 0,
      totalBytes: null,
      downloadSource: null,
      autoUpdateIntervalMinutes: 30,
      snoozed: false,
    });
    ipcMocks.getUpdateState.mockResolvedValue(idleState);
    ipcMocks.installDownloadedUpdate.mockResolvedValue({ restartMode: "tauriRelaunch" });
    relaunchMock.mockResolvedValue(undefined);
  });

  it("restores a downloaded update without checking again", async () => {
    const downloadedState = {
      phase: "downloaded" as const,
      version: "0.67.0",
      source: "automatic" as const,
      downloadedBytes: 1000,
      totalBytes: 1000,
      error: null,
    };
    ipcMocks.getUpdateState.mockResolvedValue(downloadedState);

    await useUpdateStore.getState().runAutomaticUpdate();

    expect(ipcMocks.getUpdateState).toHaveBeenCalledOnce();
    expect(ipcMocks.checkForUpdate).not.toHaveBeenCalled();
    expect(ipcMocks.downloadUpdate).not.toHaveBeenCalled();
    expect(useUpdateStore.getState()).toMatchObject({
      status: "downloaded",
      version: "0.67.0",
      downloadSource: "automatic",
    });
    expect(isUpdateDownloaded(useUpdateStore.getState())).toBe(true);
  });

  it("composes automatic check and download as one process", async () => {
    ipcMocks.checkForUpdate.mockResolvedValue({
      phase: "available",
      version: "0.67.0",
      source: "automatic",
      downloadedBytes: 0,
      totalBytes: 1000,
      error: null,
    });
    ipcMocks.downloadUpdate.mockResolvedValue({
      phase: "downloaded",
      version: "0.67.0",
      source: "automatic",
      downloadedBytes: 1000,
      totalBytes: 1000,
      error: null,
    });

    await useUpdateStore.getState().runAutomaticUpdate();

    expect(ipcMocks.checkForUpdate).toHaveBeenCalledWith("automatic");
    expect(ipcMocks.downloadUpdate).toHaveBeenCalledWith("automatic");
    expect(useUpdateStore.getState().status).toBe("downloaded");
  });

  it("rechecks and redownloads on the next automatic run after download failure", async () => {
    const availableState = {
      phase: "available" as const,
      version: "0.67.0",
      source: "automatic" as const,
      downloadedBytes: 0,
      totalBytes: 1000,
      error: null,
    };
    const downloadedState = {
      phase: "downloaded" as const,
      version: "0.67.0",
      source: "automatic" as const,
      downloadedBytes: 1000,
      totalBytes: 1000,
      error: null,
    };
    ipcMocks.checkForUpdate
      .mockResolvedValueOnce(availableState)
      .mockResolvedValueOnce(availableState);
    ipcMocks.downloadUpdate
      .mockRejectedValueOnce(new Error("下载连接已无进度"))
      .mockResolvedValueOnce(downloadedState);

    await useUpdateStore.getState().runAutomaticUpdate();

    expect(useUpdateStore.getState().status).toBe("error");

    await useUpdateStore.getState().runAutomaticUpdate();

    expect(ipcMocks.getUpdateState).toHaveBeenCalledTimes(2);
    expect(ipcMocks.checkForUpdate).toHaveBeenCalledTimes(2);
    expect(ipcMocks.checkForUpdate).toHaveBeenNthCalledWith(1, "automatic");
    expect(ipcMocks.checkForUpdate).toHaveBeenNthCalledWith(2, "automatic");
    expect(ipcMocks.downloadUpdate).toHaveBeenCalledTimes(2);
    expect(ipcMocks.downloadUpdate).toHaveBeenNthCalledWith(1, "automatic");
    expect(ipcMocks.downloadUpdate).toHaveBeenNthCalledWith(2, "automatic");
    expect(useUpdateStore.getState()).toMatchObject({
      status: "downloaded",
      downloadSource: "automatic",
    });
  });

  it("installs a restored downloaded update through the single operation", async () => {
    ipcMocks.getUpdateState.mockResolvedValue({
      phase: "downloaded",
      version: "0.67.0",
      source: "automatic",
      downloadedBytes: 1000,
      totalBytes: 1000,
      error: null,
    });
    await useUpdateStore.getState().restoreUpdateState();

    await useUpdateStore.getState().installDownloadedUpdate();

    expect(ipcMocks.installDownloadedUpdate).toHaveBeenCalledOnce();
    expect(relaunchMock).toHaveBeenCalledOnce();
    expect(useUpdateStore.getState().status).toBe("ready");
  });

  // externalUpdater 已接管关闭、替换和启动时，前端只更新为 ready，不再次调用 relaunch。
  it("does not relaunch when an external updater takes over", async () => {
    useUpdateStore.setState({
      status: "downloaded",
      version: "0.67.0",
      error: null,
      downloadPhase: "idle",
      downloadedBytes: 1000,
      totalBytes: 1000,
      downloadSource: "automatic",
    });
    ipcMocks.installDownloadedUpdate.mockResolvedValue({ restartMode: "externalUpdater" });

    await useUpdateStore.getState().installDownloadedUpdate();

    expect(ipcMocks.installDownloadedUpdate).toHaveBeenCalledOnce();
    expect(relaunchMock).not.toHaveBeenCalled();
    expect(useUpdateStore.getState().status).toBe("ready");
  });

  it("persists a zero interval to disable automatic checks", () => {
    useUpdateStore.getState().setAutoUpdateIntervalMinutes(0);

    expect(useUpdateStore.getState().autoUpdateIntervalMinutes).toBe(0);
    expect(storageMock.setItem).toHaveBeenCalledWith("auracoder:auto-update-interval-minutes", "0");
  });

  it("shows checking before the check request returns", async () => {
    let resolveCheck!: (state: typeof idleState) => void;
    ipcMocks.checkForUpdate.mockReturnValue(
      new Promise<typeof idleState>((resolve) => {
        resolveCheck = resolve;
      }),
    );

    const pendingCheck = useUpdateStore.getState().checkForUpdate();

    expect(useUpdateStore.getState().status).toBe("checking");
    expect(useUpdateStore.getState().error).toBeNull();

    resolveCheck(idleState);
    await pendingCheck;

    expect(useUpdateStore.getState().status).toBe("idle");
  });
});
