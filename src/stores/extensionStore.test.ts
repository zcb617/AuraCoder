import { describe, expect, it, vi } from "vitest";
import type { ExtensionCatalog, ExtensionItem } from "../types";
import { ipc } from "../lib/ipc";
import {
  buildExtensionCacheKey,
  getEffectiveExtensionItems,
  isAvailableExtension,
  isInstalledExtension,
  resolveExtensionProvider,
  useExtensionStore,
} from "./extensionStore";

function item(overrides: Partial<ExtensionItem>): ExtensionItem {
  return {
    id: "example",
    providerId: "codex",
    kind: "plugin",
    name: "Example",
    scope: "user",
    officiallyAvailable: false,
    catalogAuthority: null,
    installed: null,
    configured: null,
    enabled: null,
    health: "unknown",
    availableActions: [],
    requiresNewSession: false,
    ...overrides,
  };
}

function catalog(name: string): ExtensionCatalog {
  return {
    providerId: "codex",
    cwd: "D:/work/project",
    items: [item({ id: name, name, installed: true, enabled: true })],
    sources: [],
    capabilities: {
      hasOfficialSkillCatalog: false,
      canToggleSkills: false,
      hasOfficialPluginCatalog: true,
      canInstallPlugins: true,
      canTogglePlugins: false,
      hasOfficialMcpCatalog: false,
      canManageMcp: true,
      canAuthenticateMcp: true,
    },
    fetchedAt: new Date(0).toISOString(),
    hasSnapshot: true,
  };
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

describe("extensionStore helpers", () => {
  it("uses provider, workspace and normalized cwd in cache keys", () => {
    expect(
      buildExtensionCacheKey({
        providerId: "claude",
        workspaceId: "workspace",
        cwd: "D:\\Work\\Project\\",
      }),
    ).toBe("claude::workspace::d:/work/project");
  });

  it("does not reuse the same path across different SSH workspaces", () => {
    const baseContext = {
      providerId: "claude" as const,
      cwd: "/srv/project",
    };
    expect(
      buildExtensionCacheKey({ ...baseContext, workspaceId: "ssh-target-a" }),
    ).not.toBe(buildExtensionCacheKey({ ...baseContext, workspaceId: "ssh-target-b" }));
  });

  it("prefers the active thread provider over persisted and default providers", () => {
    expect(resolveExtensionProvider("workspace", { workspace: "claude" }, "opencode", "codex"))
      .toBe("opencode");
    expect(resolveExtensionProvider("workspace", { workspace: "claude" }, null, "codex"))
      .toBe("claude");
  });

  it("separates local installed items from provider-official available items", () => {
    const local = item({ installed: true });
    const available = item({
      id: "official",
      officiallyAvailable: true,
      catalogAuthority: "provider_official",
      installed: false,
    });
    const thirdPartyAvailable = item({
      id: "third-party",
      officiallyAvailable: true,
      installed: false,
    });
    expect(isInstalledExtension(local)).toBe(true);
    expect(isAvailableExtension(available)).toBe(true);
    expect(isAvailableExtension(thirdPartyAvailable)).toBe(false);
  });

  it("only exposes installed or configured and enabled items to chat", () => {
    const catalog = {
      items: [
        item({ id: "enabled", installed: true, enabled: true }),
        item({ id: "disabled", installed: true, enabled: false }),
        item({ id: "available", officiallyAvailable: true, catalogAuthority: "provider_official", installed: false }),
        item({ id: "mcp", kind: "mcp", configured: true, health: "disconnected" }),
      ],
    } as ExtensionCatalog;
    expect(getEffectiveExtensionItems(catalog).map((candidate) => candidate.id)).toEqual([
      "enabled",
      "mcp",
    ]);
  });

  it("does not let an older provider request overwrite a newer result", async () => {
    const first = deferred<ExtensionCatalog>();
    const second = deferred<ExtensionCatalog>();
    const getCatalog = vi
      .spyOn(ipc, "getExtensionCatalog")
      .mockImplementationOnce(() => first.promise)
      .mockImplementationOnce(() => second.promise);
    useExtensionStore.setState({ entries: {} });
    const context = {
      providerId: "codex" as const,
      workspaceId: "workspace",
      cwd: "D:/work/project",
    };

    const olderRequest = useExtensionStore.getState().loadCatalog(context);
    const newerRequest = useExtensionStore.getState().loadCatalog(context);
    second.resolve(catalog("newer"));
    await newerRequest;
    first.resolve(catalog("older"));
    await olderRequest;

    const key = buildExtensionCacheKey(context);
    expect(useExtensionStore.getState().entries[key]?.catalog?.items[0]?.id).toBe("newer");
    getCatalog.mockRestore();
  });

  it("queues a manual refresh without replacing the cached catalog", async () => {
    const context = {
      providerId: "codex" as const,
      workspaceId: "workspace",
      cwd: "D:/work/project",
    };
    const cached = catalog("cached");
    const requestRefresh = vi
      .spyOn(ipc, "requestExtensionCatalogRefresh")
      .mockResolvedValue({ ...cached, refreshing: true });
    const getCatalog = vi.spyOn(ipc, "getExtensionCatalog");
    const key = buildExtensionCacheKey(context);
    useExtensionStore.setState({
      entries: {
        [key]: {
          context,
          phase: "ready",
          catalog: cached,
          stale: false,
          requestSequence: 0,
        },
      },
    });

    await useExtensionStore.getState().requestRefresh(context);

    expect(requestRefresh).toHaveBeenCalledWith("codex", "workspace", "D:/work/project");
    expect(getCatalog).not.toHaveBeenCalled();
    expect(useExtensionStore.getState().entries[key]?.catalog?.items[0]?.id).toBe("cached");
    expect(useExtensionStore.getState().entries[key]?.refreshRequested).toBe(true);
    requestRefresh.mockRestore();
    getCatalog.mockRestore();
  });
});
