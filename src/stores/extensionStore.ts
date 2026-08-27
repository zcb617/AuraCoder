import { create } from "zustand";
import { ipc, listenExtensionCatalogUpdated } from "../lib/ipc";
import type {
  ChatEngineId,
  ExtensionAction,
  ExtensionActionResult,
  ExtensionCatalog,
  ExtensionItem,
  ExtensionKind,
  ExtensionProviderId,
} from "../types";

const EXTENSION_PROVIDER_STORAGE_KEY = "auracoder:extension-provider-by-workspace";

export type ExtensionCatalogPhase = "idle" | "loading" | "ready" | "error";

export interface ExtensionCatalogContext {
  providerId: ExtensionProviderId;
  workspaceId?: string | null;
  cwd?: string | null;
}

export interface ExtensionCatalogEntry {
  context: ExtensionCatalogContext;
  phase: ExtensionCatalogPhase;
  catalog?: ExtensionCatalog;
  error?: string;
  fetchedAt?: number;
  stale: boolean;
  requestSequence: number;
  action?: {
    kind: ExtensionKind;
    extensionId: string;
    action: ExtensionAction;
  };
  refreshRequested?: boolean;
}

interface ExtensionStoreState {
  entries: Record<string, ExtensionCatalogEntry>;
  selectedProviderByWorkspace: Record<string, ExtensionProviderId>;
  setSelectedProvider: (workspaceId: string | null, providerId: ExtensionProviderId) => void;
  loadCatalog: (
    context: ExtensionCatalogContext,
  ) => Promise<ExtensionCatalog | undefined>;
  requestRefresh: (context: ExtensionCatalogContext) => Promise<ExtensionCatalog | undefined>;
  performAction: (
    context: ExtensionCatalogContext,
    item: ExtensionItem,
    action: ExtensionAction,
    scope?: string | null,
  ) => Promise<ExtensionActionResult>;
  markStale: (context: ExtensionCatalogContext) => void;
}

let requestSequence = 0;
let catalogEventListenerStarted = false;

function ensureCatalogEventListener(): void {
  if (
    catalogEventListenerStarted ||
    typeof window === "undefined" ||
    !("__TAURI_INTERNALS__" in window)
  ) {
    return;
  }
  catalogEventListenerStarted = true;
  void listenExtensionCatalogUpdated((event) => {
    const eventCwd = normalizeCwd(event.cwd);
    const entries = useExtensionStore.getState().entries;
    for (const entry of Object.values(entries)) {
      if (
        entry.context.providerId === event.providerId &&
        normalizeCwd(entry.context.cwd) === eventCwd
      ) {
        void useExtensionStore.getState().loadCatalog(entry.context);
      }
    }
  }).catch(() => {
    catalogEventListenerStarted = false;
  });
}

function readSelectedProviders(): Record<string, ExtensionProviderId> {
  try {
    const value = JSON.parse(localStorage.getItem(EXTENSION_PROVIDER_STORAGE_KEY) ?? "{}");
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(
      Object.entries(value).filter(
        ([, provider]) => provider === "codex" || provider === "claude" || provider === "opencode",
      ),
    ) as Record<string, ExtensionProviderId>;
  } catch {
    return {};
  }
}

function persistSelectedProviders(value: Record<string, ExtensionProviderId>): void {
  try {
    localStorage.setItem(EXTENSION_PROVIDER_STORAGE_KEY, JSON.stringify(value));
  } catch {
    // Storage may be unavailable in tests or restricted browser contexts.
  }
}

function normalizeCwd(cwd?: string | null): string {
  return (cwd ?? "")
    .trim()
    .replace(/\\/g, "/")
    .replace(/\/+$/, "")
    .toLowerCase();
}

export function buildExtensionCacheKey(context: ExtensionCatalogContext): string {
  return [
    context.providerId,
    context.workspaceId ?? "global",
    normalizeCwd(context.cwd),
  ].join("::");
}

export function resolveExtensionProvider(
  workspaceId: string | null,
  selectedProviderByWorkspace: Record<string, ExtensionProviderId>,
  activeThreadEngine?: ChatEngineId | null,
  defaultEngine?: ChatEngineId | null,
): ExtensionProviderId {
  if (activeThreadEngine) return activeThreadEngine;
  const persisted = selectedProviderByWorkspace[workspaceId ?? "global"];
  if (persisted) return persisted;
  return defaultEngine ?? "codex";
}

export function isInstalledExtension(item: ExtensionItem): boolean {
  return item.installed === true || item.configured === true;
}

export function isAvailableExtension(item: ExtensionItem): boolean {
  return (
    item.officiallyAvailable === true &&
    item.catalogAuthority === "provider_official" &&
    item.installed !== true &&
    item.configured !== true
  );
}

export function getEffectiveExtensionItems(catalog?: ExtensionCatalog): ExtensionItem[] {
  if (!catalog) return [];
  return catalog.items.filter(
    (item) => isInstalledExtension(item) && item.enabled !== false,
  );
}

export const useExtensionStore = create<ExtensionStoreState>((set, get) => ({
  entries: {},
  selectedProviderByWorkspace: readSelectedProviders(),
  setSelectedProvider: (workspaceId, providerId) =>
    set((state) => {
      const selectedProviderByWorkspace = {
        ...state.selectedProviderByWorkspace,
        [workspaceId ?? "global"]: providerId,
      };
      persistSelectedProviders(selectedProviderByWorkspace);
      return { selectedProviderByWorkspace };
    }),
  loadCatalog: async (context) => {
    ensureCatalogEventListener();
    const key = buildExtensionCacheKey(context);
    const existing = get().entries[key];
    const sequence = ++requestSequence;
    set((state) => ({
      entries: {
        ...state.entries,
        [key]: {
          ...existing,
          context,
          phase: existing?.catalog ? "ready" : "loading",
          error: undefined,
          stale: false,
          requestSequence: sequence,
        },
      },
    }));

    try {
      const catalog = await ipc.getExtensionCatalog(
        context.providerId,
        context.workspaceId,
        context.cwd,
      );
      if (get().entries[key]?.requestSequence !== sequence) return catalog;
      set((state) => ({
        entries: {
          ...state.entries,
          [key]: {
            context,
            phase: "ready",
            catalog,
            fetchedAt: Date.now(),
            stale: false,
            requestSequence: sequence,
            action: state.entries[key]?.action,
            refreshRequested: catalog.refreshing === true,
          },
        },
      }));
      return catalog;
    } catch (error) {
      if (get().entries[key]?.requestSequence === sequence) {
        set((state) => ({
          entries: {
            ...state.entries,
          [key]: {
            ...state.entries[key],
            context,
            phase: state.entries[key]?.catalog ? "ready" : "error",
            error: String(error),
            stale: true,
              requestSequence: sequence,
            },
          },
        }));
      }
      return undefined;
    }
  },
  requestRefresh: async (context) => {
    ensureCatalogEventListener();
    const key = buildExtensionCacheKey(context);
    const existing = get().entries[key];
    const sequence = ++requestSequence;
    set((state) => ({
      entries: {
        ...state.entries,
        [key]: {
          ...existing,
          context,
          phase: existing?.catalog ? "ready" : "loading",
          error: undefined,
          stale: false,
          requestSequence: sequence,
          refreshRequested: true,
        },
      },
    }));
    try {
      const catalog = await ipc.requestExtensionCatalogRefresh(
        context.providerId,
        context.workspaceId,
        context.cwd,
      );
      if (get().entries[key]?.requestSequence !== sequence) return catalog;
      set((state) => ({
        entries: {
          ...state.entries,
          [key]: {
            context,
            phase: "ready",
            catalog,
            fetchedAt: Date.now(),
            stale: false,
            requestSequence: sequence,
            action: state.entries[key]?.action,
            refreshRequested: catalog.refreshing === true,
          },
        },
      }));
      return catalog;
    } catch (error) {
      if (get().entries[key]?.requestSequence === sequence) {
        set((state) => ({
          entries: {
            ...state.entries,
            [key]: {
              ...state.entries[key],
              context,
              phase: state.entries[key]?.catalog ? "ready" : "error",
              error: String(error),
              stale: true,
              requestSequence: sequence,
              refreshRequested: false,
            },
          },
        }));
      }
      return undefined;
    }
  },
  performAction: async (context, item, action, scope) => {
    const key = buildExtensionCacheKey(context);
    set((state) => ({
      entries: {
        ...state.entries,
        [key]: {
          ...(state.entries[key] ?? {
            context,
            phase: "idle",
            stale: true,
            requestSequence: 0,
          }),
          action: { kind: item.kind, extensionId: item.id, action },
        },
      },
    }));
    try {
      const result = await ipc.performExtensionAction(
        context.providerId,
        context.workspaceId,
        item.kind,
        item.id,
        action,
        scope,
        context.cwd,
      );
      set((state) => ({
        entries: {
          ...state.entries,
          [key]: {
            ...state.entries[key],
            context,
            refreshRequested: true,
          },
        },
      }));
      return result;
    } finally {
      set((state) => ({
        entries: {
            ...state.entries,
            [key]: { ...state.entries[key], context, action: undefined },
        },
      }));
    }
  },
  markStale: (context) => {
    const key = buildExtensionCacheKey(context);
    set((state) => {
      const entry = state.entries[key];
      if (!entry) return state;
      return {
        entries: {
          ...state.entries,
          [key]: { ...entry, context, stale: true },
        },
      };
    });
  },
}));
