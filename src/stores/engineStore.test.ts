import { beforeEach, describe, expect, it, vi } from "vitest";

const mockIpc = vi.hoisted(() => ({
  getExecutionTarget: vi.fn(),
  listActivedClis: vi.fn(),
  listSshConnections: vi.fn(),
  getEngineInfo: vi.fn(),
  getChatProviderUsage: vi.fn(),
  engineHealth: vi.fn(),
}));

vi.mock("../lib/ipc", () => ({
  ipc: mockIpc,
}));

import { useEngineStore } from "./engineStore";

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((nextResolve) => {
    resolve = nextResolve;
  });
  return { promise, resolve };
}

describe("engineStore", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    mockIpc.getExecutionTarget.mockImplementation((workspaceId?: string | null) =>
      Promise.resolve(
        workspaceId === "workspace-ssh"
          ? {
              targetKey: "ssh:connection-a",
              kind: "ssh",
              displayName: "开发机 A",
              connectionId: "connection-a",
              hostName: "192.168.1.12",
              user: "tester",
              port: 22,
              projectPath: "/var/work/project",
              connectionStatus: "ok",
            }
          : {
              targetKey: "local",
              kind: "local",
              displayName: "本机",
              connectionStatus: "ok",
            },
      ),
    );
    mockIpc.getChatProviderUsage.mockResolvedValue([]);
    mockIpc.listSshConnections.mockResolvedValue([]);
    useEngineStore.setState({
      target: {
        targetKey: "local",
        kind: "local",
        displayName: "本机",
      },
      engines: [],
      enginesByTarget: {},
      health: {},
      // 旧实现：healthByTarget: {},
      usage: {},
      // 旧实现：usageByTarget: {},
      healthLoading: {},
      // 旧实现：healthLoadingByTarget: {},
      engineCatalogLoading: {},
      // 旧实现：engineCatalogLoadingByTarget: {},
      usageLoading: {},
      // 旧实现：usageLoadingByTarget: {},
      // 旧实现：targetGenerations: {},
      loading: false,
      loadedOnce: false,
      activeWorkspaceId: null,
      error: undefined,
    });
  });

  it("loads engines without eagerly probing health", async () => {
    mockIpc.listActivedClis.mockResolvedValue([
      {
        id: "codex",
        name: "Codex",
        models: [],
        capabilities: {
          permissionModes: [],
          sandboxModes: [],
          approvalDecisions: [],
        },
      },
    ]);

    await useEngineStore.getState().load();

    expect(mockIpc.listActivedClis).toHaveBeenCalledTimes(1);
    expect(mockIpc.engineHealth).not.toHaveBeenCalled();
    expect(useEngineStore.getState().engines).toHaveLength(1);
  });

  it("does not pass a connection ID when loading a local workspace", async () => {
    mockIpc.listActivedClis.mockResolvedValue([]);

    await useEngineStore.getState().load("workspace-local");

    expect(mockIpc.listActivedClis.mock.calls[0]).toEqual([]);
  });

  it("reuses the cached CLI list on repeated loads", async () => {
    mockIpc.listActivedClis.mockResolvedValue([
      {
        id: "codex",
        name: "Codex",
        models: [{ id: "gpt-live" }],
        capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
      },
    ]);

    await useEngineStore.getState().load(null);
    await useEngineStore.getState().load(null);

    // 第二次 load 命中缓存，不再重复调用后端；缓存由后端健康检查事件驱动刷新。
    expect(mockIpc.listActivedClis).toHaveBeenCalledTimes(1);
    expect(useEngineStore.getState().enginesByTarget.local).toHaveLength(1);
  });

  it("refreshes the cached CLI list when a backend update event arrives", async () => {
    mockIpc.listActivedClis
      .mockResolvedValueOnce([
        {
          id: "codex",
          name: "Codex",
          models: [{ id: "gpt-old" }],
          capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
        },
      ])
      .mockResolvedValueOnce([
        {
          id: "codex",
          name: "Codex",
          models: [{ id: "gpt-new" }],
          capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
        },
      ]);

    await useEngineStore.getState().load(null);
    await useEngineStore.getState().applyCliServicesUpdated({
      scope: "local",
      connectionId: null,
      revision: 1,
    });

    expect(mockIpc.listActivedClis).toHaveBeenCalledTimes(2);
    expect(useEngineStore.getState().engines[0]?.models[0]?.id).toBe("gpt-new");
  });

  it("caches an empty catalog when the backend reports no activated CLIs", async () => {
    mockIpc.listActivedClis.mockRejectedValue(
      new Error("SSH 远端机器没有已激活的 Codex、OpenCode 或 Claude CLI 工具"),
    );

    await useEngineStore.getState().applyCliServicesUpdated({
      scope: "ssh",
      connectionId: "connection-a",
      revision: 1,
    });

    expect(useEngineStore.getState().enginesByTarget["ssh:connection-a"]).toEqual([]);
  });

  it("keeps the cached CLI list when a refresh fails transiently", async () => {
    mockIpc.listActivedClis.mockResolvedValue([
      {
        id: "codex",
        name: "Codex",
        models: [{ id: "gpt-live" }],
        capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
      },
    ]);
    await useEngineStore.getState().load(null);

    mockIpc.listActivedClis.mockRejectedValue(new Error("network unreachable"));
    await useEngineStore.getState().applyCliServicesUpdated({
      scope: "local",
      connectionId: null,
      revision: 2,
    });

    expect(useEngineStore.getState().engines[0]?.models[0]?.id).toBe("gpt-live");
  });

  it("refetches after the cached CLI list is dropped by invalidation", async () => {
    mockIpc.listActivedClis.mockResolvedValue([
      {
        id: "codex",
        name: "Codex",
        models: [{ id: "remote-model" }],
        capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
      },
    ]);

    await useEngineStore.getState().load("workspace-ssh");
    useEngineStore.getState().invalidateConnection("connection-a");
    expect(useEngineStore.getState().enginesByTarget["ssh:connection-a"]).toBeUndefined();

    await useEngineStore.getState().load("workspace-ssh");

    expect(mockIpc.listActivedClis).toHaveBeenCalledTimes(2);
  });

  it("loads an SSH workspace CLI list with its connection ID", async () => {
    mockIpc.listActivedClis.mockResolvedValue([
      {
        id: "codex",
        name: "Codex",
        models: [{ id: "remote-model" }],
        capabilities: {
          permissionModes: [],
          sandboxModes: [],
          approvalDecisions: [],
        },
      },
    ]);

    await useEngineStore.getState().load("workspace-ssh");

    expect(mockIpc.listActivedClis).toHaveBeenCalledWith("connection-a");
    expect(useEngineStore.getState().activeWorkspaceId).toBe("workspace-ssh");
    expect(useEngineStore.getState().engines[0]?.models[0]?.id).toBe("remote-model");
  });

  it("discards late CLI results after switching targets", async () => {
    const firstCatalog = deferred<Array<{ id: string; name: string; models: Array<{ id: string }>; capabilities: { permissionModes: never[]; sandboxModes: never[]; approvalDecisions: never[] } }>>();
    mockIpc.getExecutionTarget.mockImplementation((workspaceId?: string | null) =>
      Promise.resolve({
        targetKey: `ssh:${workspaceId}`,
        kind: "ssh",
        displayName: String(workspaceId),
        connectionId: String(workspaceId),
        connectionStatus: "ok",
      }),
    );
    mockIpc.listActivedClis.mockImplementation((workspaceId?: string | null) => {
      if (workspaceId === "workspace-a") {
        return firstCatalog.promise;
      }
      return Promise.resolve([
        {
          id: "codex",
          name: "Codex B",
          models: [{ id: "model-b" }],
          capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
        },
      ]);
    });

    const firstLoad = useEngineStore.getState().load("workspace-a");
    await Promise.resolve();
    await useEngineStore.getState().load("workspace-b");
    firstCatalog.resolve([
      {
        id: "codex",
        name: "Codex A",
        models: [{ id: "model-a" }],
        capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
      },
    ]);
    await firstLoad;

    expect(useEngineStore.getState().target?.targetKey).toBe("ssh:workspace-b");
    expect(useEngineStore.getState().engines[0]?.models[0]?.id).toBe("model-b");
    // 迟到结果由加载序号守卫丢弃，不会写入 enginesByTarget 污染其他目标的缓存。
    expect(useEngineStore.getState().enginesByTarget["ssh:workspace-a"]).toBeUndefined();
  });

  it("does not restore invalidated target data from an older request", async () => {
    const request = deferred<{
      id: string;
      name: string;
      models: Array<{ id: string }>;
      capabilities: { permissionModes: never[]; sandboxModes: never[]; approvalDecisions: never[] };
    }>();
    useEngineStore.setState({
      activeWorkspaceId: "workspace-ssh",
      target: {
        targetKey: "ssh:connection-a",
        kind: "ssh",
        displayName: "开发机 A",
        connectionId: "connection-a",
      },
      engines: [],
    });
    mockIpc.getEngineInfo.mockReturnValueOnce(request.promise);

    const refresh = useEngineStore.getState().refreshEngineCatalog("codex");
    useEngineStore.getState().invalidateConnection("connection-a");
    request.resolve({
      id: "codex",
      name: "Codex",
      models: [{ id: "stale-model" }],
      capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
    });
    await refresh;

    // 失效连接的目标缓存同步删除，当前视图清空。
    expect(useEngineStore.getState().engines).toEqual([]);
    expect(useEngineStore.getState().enginesByTarget["ssh:connection-a"]).toBeUndefined();
  });

  it("loads engine health on demand", async () => {
    mockIpc.engineHealth.mockResolvedValue({
      id: "codex",
      available: true,
      details: "ready",
      warnings: [],
      checks: [],
      fixes: [],
    });

    const health = await useEngineStore.getState().ensureHealth("codex");

    expect(mockIpc.engineHealth).toHaveBeenCalledWith("codex");
    expect(health?.available).toBe(true);
    expect(useEngineStore.getState().health.codex?.details).toBe("ready");
  });

  it("reads successful CLI health from the backend on every request", async () => {
    mockIpc.engineHealth.mockResolvedValue({
      id: "codex",
      available: true,
      details: "ready",
      warnings: [],
      checks: [],
      fixes: [],
    });

    await useEngineStore.getState().ensureHealth("codex");
    await useEngineStore.getState().ensureHealth("codex");

    expect(mockIpc.engineHealth).toHaveBeenCalledTimes(2);
  });

  it("consumes a successful health result by restoring an empty model catalog", async () => {
    useEngineStore.setState({
      activeWorkspaceId: "workspace-ssh",
      target: {
        targetKey: "ssh:connection-a",
        kind: "ssh",
        displayName: "开发机 A",
        connectionId: "connection-a",
      },
      engines: [
        {
          id: "claude",
          name: "Claude",
          models: [],
          capabilities: {
            permissionModes: [],
            sandboxModes: [],
            approvalDecisions: [],
          },
        },
      ],
      enginesByTarget: {
        "ssh:connection-a": [
          {
            id: "claude",
            name: "Claude",
            models: [],
            capabilities: {
              permissionModes: [],
              sandboxModes: [],
              approvalDecisions: [],
            },
          },
        ],
      },
    });
    mockIpc.engineHealth.mockResolvedValue({
      id: "claude",
      available: true,
      details: "ready",
      warnings: [],
      checks: [],
      fixes: [],
    });
    mockIpc.getEngineInfo.mockResolvedValue({
      id: "claude",
      name: "Claude",
      models: [
        {
          id: "opus[1m]",
          displayName: "Opus (1M context)",
          description: "Remote Claude model",
          hidden: false,
          isDefault: true,
          inputModalities: ["text"],
          attachmentModalities: [],
          supportsPersonality: false,
          defaultReasoningEffort: "medium",
          supportedReasoningEfforts: [],
        },
      ],
      capabilities: {
        permissionModes: [],
        sandboxModes: [],
        approvalDecisions: [],
      },
    });

    const health = await useEngineStore.getState().ensureHealth("claude");

    expect(health?.available).toBe(true);
    expect(mockIpc.getEngineInfo).toHaveBeenCalledWith("claude", "workspace-ssh");
    expect(useEngineStore.getState().engines[0]?.models[0]?.id).toBe("opus[1m]");
    // 单引擎目录刷新写穿目标缓存，视图与缓存保持一致。
    expect(
      useEngineStore.getState().enginesByTarget["ssh:connection-a"]?.[0]?.models[0]?.id,
    ).toBe("opus[1m]");
    expect(useEngineStore.getState().engineCatalogLoading).toEqual({});
  });

  it("does not cache thrown health errors and allows retries", async () => {
    mockIpc.engineHealth
      .mockRejectedValueOnce(new Error("temporary failure"))
      .mockResolvedValueOnce({
        id: "codex",
        available: true,
        details: "ready",
        warnings: [],
        checks: [],
        fixes: [],
      });

    const first = await useEngineStore.getState().ensureHealth("codex");
    const second = await useEngineStore.getState().ensureHealth("codex");

    expect(first).toBeNull();
    expect(second?.available).toBe(true);
    expect(mockIpc.engineHealth).toHaveBeenCalledTimes(2);
    expect(useEngineStore.getState().health.codex?.details).toBe("ready");
  });

  it("marks Codex available when a runtime update arrives", () => {
    useEngineStore.setState({
      target: {
        targetKey: "local",
        kind: "local",
        displayName: "本机",
      },
      health: {
        codex: {
          id: "codex",
          available: false,
          details: "Engine discovery failed: codex missing",
          warnings: [],
          checks: ["codex --version"],
          fixes: [],
        },
      },
      /* 旧实现还要把同一状态复制到 healthByTarget.local。 */
    });

    useEngineStore.getState().applyRuntimeUpdate({
      engineId: "codex",
      protocolDiagnostics: {
        methodAvailability: [
          {
            method: "app/list",
            status: "available",
          },
        ],
        experimentalFeatures: [],
        collaborationModes: [],
        apps: [],
        skills: [],
        pluginMarketplaces: [],
        mcpServers: [],
        fetchedAt: "2026-03-06T00:00:00Z",
        stale: false,
      },
    });

    const codex = useEngineStore.getState().health.codex;
    expect(codex?.available).toBe(true);
    expect(codex?.details).toBeUndefined();
    expect(codex?.protocolDiagnostics?.fetchedAt).toBe("2026-03-06T00:00:00Z");
  });
});
