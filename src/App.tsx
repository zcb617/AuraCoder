import { emit } from "@tauri-apps/api/event";
import { useEffect, useRef, useState } from "react";
import { ThreeColumnLayout } from "./components/layout/ThreeColumnLayout";
import { CommandPalette } from "./components/shared/CommandPalette";
import { ConfirmDialog } from "./components/shared/ConfirmDialog";
import { OnboardingWizard } from "./components/onboarding/OnboardingWizard";
import { ToastContainer } from "./components/shared/ToastContainer";
import { PowerSettingsModal } from "./components/shared/PowerSettingsModal";
import { TerminalNotificationSettingsModal } from "./components/shared/TerminalNotificationSettingsModal";
import { UsageLimitsModal } from "./components/settings/UsageLimitsModal";
import { t } from "./i18n";
import { useUpdateStore } from "./stores/updateStore";
import {
  ipc,
  listenAppStartupProgress,
  listenChatApprovalRequested,
  listenCliServiceRestartRequired,
  listenComputerControlApprovalRequested,
  listenChatTurnFinished,
  listenCliServicesUpdated,
  listenCodexRemoteThreadRemoved,
  listenEngineRuntimeUpdated,
  listenMenuAction,
  listenSshRemoteProjectSessionsRefreshed,
  listenThreadUpdated,
  type AppStartupPhase,
  type AppStartupProgressEvent,
  type CliServiceRestartRequiredEvent,
  type CodexRemoteThreadRemovedEvent,
} from "./lib/ipc";
import { useWorkspaceStore } from "./stores/workspaceStore";
import { useEngineStore } from "./stores/engineStore";
import { useUiStore } from "./stores/uiStore";
import { useThreadStore } from "./stores/threadStore";
import { useChatStore } from "./stores/chatStore";
import { useGitStore } from "./stores/gitStore";
import { useTerminalStore, collectSessionIds } from "./stores/terminalStore";
import { useFileStore } from "./stores/fileStore";
import { useKeepAwakeStore } from "./stores/keepAwakeStore";
import { useTerminalNotificationSettingsStore } from "./stores/terminalNotificationSettingsStore";
import { toast } from "./stores/toastStore";
import type {
  ChatEngineId,
  ComputerControlApprovalRequest,
  RuntimeToast,
  Thread,
} from "./types";
import { getActiveEditorView, openSearchPanel } from "./components/editor/CodeMirrorEditor";
import { CustomWindowFrame } from "./components/shared/CustomWindowFrame";
import { useCustomWindowFrameState } from "./lib/customWindowFrame";
import { runEditMenuAction } from "./lib/nativeEditActions";
import { preventNativeContextMenu } from "./lib/nativeContextMenu";
import { createAndActivateWorkspaceThread } from "./lib/newThreadActions";
import {
  cycleWorkspaceTerminalLayout,
  isWorkspaceSurfaceVisible,
  toggleWorkspaceEditorLayout,
} from "./lib/workspacePaneNavigation";
import {
  usesCustomWindowFrame,
  isTerminalInputFocused,
  requestWindowClose,
  shouldHandleAppShortcutWhileTerminalFocused, toggleWindowFullscreen,
} from "./lib/windowActions";

// Debounce guard: when both the JS keydown handler and the native menu-action
// fire for the same shortcut, only the first one within 100ms takes effect.
const shortcutLastFired = new Map<string, number>();
const SHORTCUT_DEBOUNCE_MS = 100;
const KEEP_AWAKE_REFRESH_MS = 15000;

function fireShortcut(id: string, action: () => void) {
  const now = Date.now();
  const last = shortcutLastFired.get(id) ?? 0;
  if (now - last < SHORTCUT_DEBOUNCE_MS) return;
  shortcutLastFired.set(id, now);
  action();
}

async function createNewWorkspaceThread() {
  const { activeWorkspaceId } = useWorkspaceStore.getState();
  await createAndActivateWorkspaceThread(activeWorkspaceId);
}

function shouldSyncCodexThread(thread: Thread | null | undefined): boolean {
  return (
    thread?.engineId === "codex" &&
    Boolean(thread.engineThreadId?.trim()) &&
    thread.status !== "streaming" &&
    thread.status !== "awaiting_approval"
  );
}

function showRuntimeToast(runtimeToast?: RuntimeToast) {
  if (!runtimeToast) {
    return;
  }

  switch (runtimeToast.variant) {
    case "success":
      toast.success(runtimeToast.message);
      break;
    case "warning":
      toast.warning(runtimeToast.message);
      break;
    case "info":
      toast.info(runtimeToast.message);
      break;
    case "error":
    default:
      toast.error(runtimeToast.message);
      break;
  }
}

function resolveAgentDisplayName(engineId: ChatEngineId): string {
  switch (engineId) {
    case "claude":
      return "Claude";
    case "opencode":
      return "OpenCode";
    case "codex":
    default:
      return "Codex";
  }
}

function resolveChatNotificationBody(
  status: "completed" | "interrupted" | "error",
  preview?: string | null,
): string {
  const normalizedPreview = preview?.trim();
  if (normalizedPreview) {
    return normalizedPreview;
  }
  if (status === "error") {
    return t("app:notificationSettings.chatNotificationFallbackError");
  }
  return t("app:notificationSettings.chatNotificationFallbackComplete");
}

export function App() {
  const loadWorkspaces = useWorkspaceStore((s) => s.loadWorkspaces);
  const workspaces = useWorkspaceStore((s) => s.workspaces);
  const activeWorkspaceId = useWorkspaceStore((s) => s.activeWorkspaceId);
  const completeSshSessionSync = useWorkspaceStore((s) => s.completeSshSessionSync);
  const loadEngines = useEngineStore((s) => s.load);
  const preloadEngineCatalogs = useEngineStore((s) => s.preloadCatalogs);
  const applyCliServicesUpdated = useEngineStore((s) => s.applyCliServicesUpdated);
  const engines = useEngineStore((s) => s.engines);
  const applyEngineRuntimeUpdate = useEngineStore((s) => s.applyRuntimeUpdate);
  const loadKeepAwake = useKeepAwakeStore((s) => s.load);
  const loadTerminalNotificationSettings = useTerminalNotificationSettingsStore((s) => s.load);
  const refreshKeepAwake = useKeepAwakeStore((s) => s.refresh);
  const keepAwakeEnabled = useKeepAwakeStore((s) => s.state?.enabled ?? false);
  const keepAwakeSessionTimer = useKeepAwakeStore((s) => s.state?.sessionRemainingSecs);
  const refreshAllThreads = useThreadStore((s) => s.refreshAllThreads);
  const refreshThreads = useThreadStore((s) => s.refreshThreads);
  const reloadThreadsFromLocalDatabase = useThreadStore(
    (s) => s.reloadThreadsFromLocalDatabase,
  );
  const refreshArchivedThreads = useThreadStore((s) => s.refreshArchivedThreads);
  const applyThreadUpdateLocal = useThreadStore((s) => s.applyThreadUpdateLocal);
  const commandPaletteOpen = useUiStore((s) => s.commandPaletteOpen);
  const closeCommandPalette = useUiStore((s) => s.closeCommandPalette);
  const runAutomaticUpdate = useUpdateStore((s) => s.runAutomaticUpdate);
  const restoreUpdateState = useUpdateStore((s) => s.restoreUpdateState);
  const autoUpdateIntervalMinutes = useUpdateStore((s) => s.autoUpdateIntervalMinutes);
  const customWindowFrame = usesCustomWindowFrame();
  const [codexRemoteThreadPrompts, setCodexRemoteThreadPrompts] = useState<
    CodexRemoteThreadRemovedEvent[]
  >([]);
  const codexRemoteThreadPrompt = codexRemoteThreadPrompts[0] ?? null;
  const [computerControlApprovals, setComputerControlApprovals] = useState<
    ComputerControlApprovalRequest[]
  >([]);
  const [computerControlApprovalUpdating, setComputerControlApprovalUpdating] = useState(false);
  const computerControlApproval = computerControlApprovals[0] ?? null;
  const [cliServiceRestartPrompts, setCliServiceRestartPrompts] = useState<
    CliServiceRestartRequiredEvent[]
  >([]);
  const [cliServiceRestartRunning, setCliServiceRestartRunning] = useState(false);
  const cliServiceRestartPrompt = cliServiceRestartPrompts[0] ?? null;
  const computerControlAgentName = computerControlApproval?.agent === "claude"
    ? "Claude Code"
    : computerControlApproval?.agent === "opencode"
      ? "OpenCode"
      : computerControlApproval?.agent === "codex"
        ? "Codex"
        : computerControlApproval?.agent ?? "AI";
  const customWindowFrameState = useCustomWindowFrameState();
  const [workspaceCatalogLoaded, setWorkspaceCatalogLoaded] = useState(false);
  const [startupProgressListenerReady, setStartupProgressListenerReady] = useState(false);
  const [startupCompleted, setStartupCompleted] = useState(false);
  const [startupProgress, setStartupProgress] = useState<AppStartupProgressEvent>({
    phase: "loading-base-data",
    message: "",
  });
  const startupSyncRequestedRef = useRef(false);

  useEffect(() => {
    document.addEventListener("contextmenu", preventNativeContextMenu);
    return () => document.removeEventListener("contextmenu", preventNativeContextMenu);
  }, []);

  useEffect(() => {
    let disposed = false;
    void loadWorkspaces().finally(() => {
      if (!disposed) {
        setWorkspaceCatalogLoaded(true);
      }
    });
    void loadKeepAwake();
    void loadTerminalNotificationSettings();
    return () => {
      disposed = true;
    };
  }, [loadWorkspaces, loadKeepAwake, loadTerminalNotificationSettings]);

  useEffect(() => {
    if (
      !workspaceCatalogLoaded ||
      !startupCompleted ||
      !activeWorkspaceId ||
      useWorkspaceStore.getState().sshSessionSyncingWorkspaceIds[activeWorkspaceId]
    ) {
      return;
    }
    void loadEngines(activeWorkspaceId);
  }, [
    activeWorkspaceId,
    loadEngines,
    startupCompleted,
    workspaceCatalogLoaded,
  ]);

  useEffect(() => {
    const localWorkspaceIds = workspaces
      .filter((workspace) => workspace.locationKind !== "ssh")
      .map((workspace) => workspace.id);
    if (localWorkspaceIds.length > 0) {
      void refreshAllThreads(localWorkspaceIds);
    }

    // SSH 项目只展示本地数据库中的缓存；远端扫描由 SSH 专用后端服务完成，
    // 完成后再通过通知触发同样的数据库重载，绝不能走本地 CLI 发现路线。
    for (const workspace of workspaces) {
      if (workspace.locationKind !== "ssh") {
        continue;
      }
      void reloadThreadsFromLocalDatabase(workspace.id).catch((error) => {
        console.warn(
          `Failed to load cached SSH remote project sessions for workspace ${workspace.id}:`,
          error,
        );
      });
    }
  }, [
    engines,
    workspaces,
    refreshAllThreads,
    reloadThreadsFromLocalDatabase,
  ]);

  useEffect(() => {
    const hasSessionTimer = keepAwakeSessionTimer != null;
    if (!keepAwakeEnabled && !hasSessionTimer) {
      return;
    }

    const pollInterval = hasSessionTimer ? 30_000 : KEEP_AWAKE_REFRESH_MS;
    const intervalId = window.setInterval(() => {
      void refreshKeepAwake();
    }, pollInterval);

    return () => window.clearInterval(intervalId);
  }, [keepAwakeEnabled, keepAwakeSessionTimer, refreshKeepAwake]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenThreadUpdated(async ({ workspaceId, thread }) => {
      if (thread) {
        const applied = applyThreadUpdateLocal(thread);
        const activeThreadId = useThreadStore.getState().activeThreadId;
        if (thread.id === activeThreadId && shouldSyncCodexThread(thread)) {
          try {
            const syncedThread = await ipc.syncThreadFromEngine(thread.id);
            if (useThreadStore.getState().applyThreadUpdateLocal(syncedThread)) {
              return;
            }
          } catch (error) {
            console.warn(`Failed to sync active Codex thread ${thread.id}:`, error);
          }
          void refreshThreads(workspaceId);
          void refreshArchivedThreads(workspaceId);
          return;
        }
        if (applied) {
          return;
        }
      }
      void refreshThreads(workspaceId);
      void refreshArchivedThreads(workspaceId);
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, [applyThreadUpdateLocal, refreshArchivedThreads, refreshThreads]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenSshRemoteProjectSessionsRefreshed(async (event) => {
      try {
        try {
          await reloadThreadsFromLocalDatabase(event.workspaceId);
        } catch (error) {
          console.warn(
            `Failed to reload SSH remote project sessions for workspace ${event.workspaceId}:`,
            error,
          );
        }

        if (
          startupCompleted &&
          useWorkspaceStore.getState().activeWorkspaceId === event.workspaceId
        ) {
          await loadEngines(event.workspaceId);
        }
      } finally {
        completeSshSessionSync(event.workspaceId);
      }

      if (event.failedCliIds.length > 0) {
        toast.warning(
          `SSH 远端项目会话同步失败：${event.failedCliIds.length} 个 CLI 未能同步。`,
        );
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, [completeSshSessionSync, loadEngines, reloadThreadsFromLocalDatabase, startupCompleted]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenAppStartupProgress((event) => {
      setStartupProgress(event);
      if (event.phase === "completed") {
        // 后端初始化完成后，除取会话列表外，同时把本机和各远端机器的 CLI 目录
        // 预热进前端缓存；之后页面只读缓存，目录变化由后端健康检查事件驱动。
        void preloadEngineCatalogs();
        const sshWorkspaceIds = useWorkspaceStore
          .getState()
          .workspaces.filter((workspace) => workspace.locationKind === "ssh")
          .map((workspace) => workspace.id);
        void Promise.all(
          sshWorkspaceIds.map((workspaceId) =>
            reloadThreadsFromLocalDatabase(workspaceId).catch((error) => {
              console.warn(
                `Failed to reload SSH remote project sessions for workspace ${workspaceId}:`,
                error,
              );
            }),
          ),
        ).finally(() => setStartupCompleted(true));
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
        setStartupProgressListenerReady(true);
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, [preloadEngineCatalogs, reloadThreadsFromLocalDatabase]);

  // 后端健康检查 reconcile 生命周期 MAP 后推送本事件；收到后立即用与启动预热
  // 相同的接口刷新对应目标的前端 CLI 目录缓存。
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenCliServicesUpdated((event) => {
      if (event.errors.length > 0) {
        toast.error(event.errors.join("\n"));
      }
      if (event.changed) {
        void applyCliServicesUpdated(event);
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, [applyCliServicesUpdated]);

  useEffect(() => {
    if (
      !workspaceCatalogLoaded ||
      !startupProgressListenerReady ||
      startupSyncRequestedRef.current
    ) {
      return;
    }
    startupSyncRequestedRef.current = true;
    void emit("ssh-remote-project-session-sync-ready").catch((error) => {
      startupSyncRequestedRef.current = false;
      console.warn("Failed to signal SSH remote project session sync readiness:", error);
    });
  }, [startupProgressListenerReady, workspaceCatalogLoaded]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenCodexRemoteThreadRemoved((event) => {
      setCodexRemoteThreadPrompts((current) => {
        if (current.some((item) => item.thread.id === event.thread.id)) {
          return current;
        }
        return [...current, event];
      });
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenComputerControlApprovalRequested((event) => {
      setComputerControlApprovals((current) => {
        if (current.some((item) => item.requestId === event.requestId)) {
          return current;
        }
        return [...current, event];
      });
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenCliServiceRestartRequired((event) => {
      setCliServiceRestartPrompts((current) => {
        if (
          current.some(
            (item) =>
              item.connectionId === event.connectionId && item.engineId === event.engineId,
          )
        ) {
          return current;
        }
        return [...current, event];
      });
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenChatTurnFinished(async (event) => {
      const notificationStore = useTerminalNotificationSettingsStore.getState();
      const settings = notificationStore.settings ?? await notificationStore.load();
      if (!settings?.chatEnabled || event.status === "interrupted") {
        return;
      }

      const activeWorkspaceId = useWorkspaceStore.getState().activeWorkspaceId;
      const activeThreadId = useThreadStore.getState().activeThreadId;
      if (
        document.hasFocus()
        && activeWorkspaceId === event.workspaceId
        && activeThreadId === event.threadId
      ) {
        return;
      }

      const title = event.threadTitle.trim() || resolveAgentDisplayName(event.engineId);
      const body = resolveChatNotificationBody(event.status, event.preview);

      try {
        await ipc.showAgentNotification(title, body);
      } catch (error) {
        console.warn(`Failed to show chat notification for thread ${event.threadId}:`, error);
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenChatApprovalRequested(async (event) => {
      useThreadStore.getState().markThreadAwaitingApproval(event.threadId);

      const activeWorkspaceId = useWorkspaceStore.getState().activeWorkspaceId;
      const activeThreadId = useThreadStore.getState().activeThreadId;
      const viewingThread =
        document.hasFocus()
        && activeWorkspaceId === event.workspaceId
        && activeThreadId === event.threadId;
      if (viewingThread) {
        return;
      }

      const engineName = resolveAgentDisplayName(event.engineId);
      toast.warning(event.summary, {
        title: t("chat:autonomy.approvalToastTitle", { engine: engineName }),
        action: {
          label: t("chat:autonomy.openThread"),
          onClick: () => {
            void (async () => {
              const uiStore = useUiStore.getState();
              if (uiStore.activeView !== "chat") {
                uiStore.setActiveView("chat");
              }
              if (useWorkspaceStore.getState().activeWorkspaceId !== event.workspaceId) {
                await useWorkspaceStore.getState().setActiveWorkspace(event.workspaceId);
              }
              useThreadStore.getState().setActiveThread(event.threadId);
              await useChatStore.getState().setActiveThread(event.threadId);
            })();
          },
        },
      });

      const notificationStore = useTerminalNotificationSettingsStore.getState();
      const settings = notificationStore.settings ?? await notificationStore.load();
      if (!settings?.chatEnabled || document.hasFocus()) {
        return;
      }

      const title = event.threadTitle.trim() || engineName;
      try {
        await ipc.showAgentNotification(title, event.summary);
      } catch (error) {
        console.warn(`Failed to show approval notification for thread ${event.threadId}:`, error);
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listenEngineRuntimeUpdated((event) => {
      applyEngineRuntimeUpdate(event);
      showRuntimeToast(event.toast);
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, [applyEngineRuntimeUpdate]);

  useEffect(() => {
    function onBeforeUnload() {
      const wsId = useWorkspaceStore.getState().activeWorkspaceId;
      if (wsId) {
        useGitStore.getState().flushDrafts(wsId);
      }
    }

    window.addEventListener("beforeunload", onBeforeUnload);
    return () => window.removeEventListener("beforeunload", onBeforeUnload);
  }, []);

  useEffect(() => {
    void restoreUpdateState();
  }, [restoreUpdateState]);

  useEffect(() => {
    if (autoUpdateIntervalMinutes <= 0) return;

    const timer = setTimeout(() => {
      void runAutomaticUpdate();
    }, 3000);
    return () => clearTimeout(timer);
  }, [autoUpdateIntervalMinutes, runAutomaticUpdate]);

  useEffect(() => {
    if (autoUpdateIntervalMinutes <= 0) return;

    const intervalId = window.setInterval(() => {
      void runAutomaticUpdate();
    }, autoUpdateIntervalMinutes * 60_000);

    return () => window.clearInterval(intervalId);
  }, [autoUpdateIntervalMinutes, runAutomaticUpdate]);

  // Handle app-level keyboard shortcuts via JavaScript keydown listeners.
  // On macOS, when a contenteditable element (CodeMirror editor) is focused,
  // WKWebView claims Cmd+key events for text formatting before they reach
  // Tauri's native menu accelerators. JavaScript keydown events still fire,
  // so the JS handler is the primary source of truth for these shortcuts.
  //
  // When the native menu accelerator DOES fire (non-contenteditable focus),
  // both the JS handler and the menu-action listener would toggle the same
  // state, canceling each other out. A debounce guard (`shortcutLastFired`)
  // prevents the second handler from re-toggling within 100ms.
  //
  // Cmd+Alt+F (focus mode) is intercepted before Cmd+F so it wins even in editors.
  // F11 toggles native window fullscreen independently from focus mode.
  // Cmd+Shift+N (new thread) and Cmd+E (editor toggle) are JS-only.
  // Cmd+S always prevents the browser save-page dialog.
  // Cmd+W is debounced like the native menu path so Linux can use the same
  // close behavior even without a native menubar.
  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === "F11") {
        e.preventDefault();
        fireShortcut("toggle-fullscreen", () => {
          void toggleWindowFullscreen();
        });
        return;
      }

      const meta = e.metaKey || e.ctrlKey;
      if (!meta) return;

      // On macOS/WebKit, e.key is lowercase even when Shift is held with Cmd,
      // so normalize to lowercase and use e.shiftKey to differentiate.
      const key = e.key.toLowerCase();
      const allowWhileTerminalFocused = shouldHandleAppShortcutWhileTerminalFocused(key, e.shiftKey);

      if (isTerminalInputFocused() && !allowWhileTerminalFocused) return;

      // Always prevent Cmd+S from opening the browser save dialog
      if (key === "s" && !e.shiftKey) {
        e.preventDefault();
        return;
      }

      if (key === "f" && e.altKey && !e.shiftKey) {
        e.preventDefault();
        fireShortcut("toggle-focus-mode", () => useUiStore.getState().toggleFocusMode());
        return;
      }

      switch (key) {
        case "n":
          if (!e.shiftKey) return;
          e.preventDefault();
          fireShortcut("new-thread", () => {
            void createNewWorkspaceThread();
          });
          break;
        case "e":
          if (e.shiftKey) return;
          e.preventDefault();
          {
            const wsId = useWorkspaceStore.getState().activeWorkspaceId;
            if (!wsId) return;
            toggleWorkspaceEditorLayout(wsId);
          }
          break;
        case "b":
          e.preventDefault();
          if (e.shiftKey) {
            fireShortcut("toggle-git-panel", () => useUiStore.getState().toggleGitPanel());
          } else {
            fireShortcut("toggle-sidebar", () => useUiStore.getState().toggleSidebar());
          }
          break;
        case "f": {
          if (!e.shiftKey) {
            // Cmd+F — editor find (only in editor mode)
            const wsIdF = useWorkspaceStore.getState().activeWorkspaceId;
            if (wsIdF && isWorkspaceSurfaceVisible(wsIdF, "editor")) {
              e.preventDefault();
              const fileState = useFileStore.getState();
              const activeTabId = fileState.activeTabId;
              if (activeTabId) {
                const activeTab = fileState.tabs.find((tab) => tab.id === activeTabId);
                const editorId =
                  activeTab?.renderMode === "git-diff-editor"
                    ? `${activeTabId}:git-modified`
                    : activeTabId;
                const view = getActiveEditorView(editorId);
                if (view) openSearchPanel(view);
              }
            }
            return;
          }
          // Cmd+Shift+F — search-focused command palette
          e.preventDefault();
          fireShortcut("toggle-search", () =>
            useUiStore.getState().openCommandPalette({ variant: "search", initialQuery: "?" })
          );
          break;
        }
        case "h": {
          if (e.shiftKey) return;
          // Cmd+H — editor find & replace (only in editor mode)
          const wsIdH = useWorkspaceStore.getState().activeWorkspaceId;
          if (!wsIdH || !isWorkspaceSurfaceVisible(wsIdH, "editor")) return;
          e.preventDefault();
          const fileState = useFileStore.getState();
          const activeTabIdH = fileState.activeTabId;
          if (activeTabIdH) {
            const activeTab = fileState.tabs.find((tab) => tab.id === activeTabIdH);
            const editorId =
              activeTab?.renderMode === "git-diff-editor"
                ? `${activeTabIdH}:git-modified`
                : activeTabIdH;
            const view = getActiveEditorView(editorId);
            if (view) {
              openSearchPanel(view);
              requestAnimationFrame(() => {
                const replaceInput = view.dom.querySelector<HTMLInputElement>("[name=replace]");
                replaceInput?.focus();
              });
            }
          }
          break;
        }
        case "t":
          e.preventDefault();
          if (e.shiftKey) {
            fireShortcut("toggle-terminal", () => {
              const wsId = useWorkspaceStore.getState().activeWorkspaceId;
              if (wsId) cycleWorkspaceTerminalLayout(wsId);
            });
          } else {
            fireShortcut("new-terminal-tab", () => {
              const wsId = useWorkspaceStore.getState().activeWorkspaceId;
              if (!wsId) return;
              const ws = useTerminalStore.getState().workspaces[wsId];
              if (!ws || (ws.layoutMode !== "split" && ws.layoutMode !== "terminal")) return;
              void useTerminalStore.getState().createSession(wsId);
            });
          }
          break;
        case "w":
          if (e.shiftKey) return;
          e.preventDefault();
          fireShortcut("close-window", () => {
            void requestWindowClose();
          });
          break;
        case "i":
          if (!e.shiftKey) return;
          e.preventDefault();
          fireShortcut("toggle-broadcast", () => {
            const wsId = useWorkspaceStore.getState().activeWorkspaceId;
            if (!wsId) return;
            const ws = useTerminalStore.getState().workspaces[wsId];
            if (!ws || (ws.layoutMode !== "split" && ws.layoutMode !== "terminal")) return;
            const activeGroupId = ws.activeGroupId;
            if (!activeGroupId) return;
            const activeGroup = ws.groups.find((g) => g.id === activeGroupId);
            if (!activeGroup) return;
            const isBroadcastingActiveGroup = ws.broadcastGroupId === activeGroupId;
            if (!isBroadcastingActiveGroup && collectSessionIds(activeGroup.root).length < 2) return;
            useTerminalStore.getState().toggleBroadcast(wsId, activeGroupId);
          });
          break;
        case "d":
          e.preventDefault();
          fireShortcut(e.shiftKey ? "split-horizontal" : "split-vertical", () => {
            const wsId = useWorkspaceStore.getState().activeWorkspaceId;
            if (!wsId) return;
            const ws = useTerminalStore.getState().workspaces[wsId];
            if (!ws || (ws.layoutMode !== "split" && ws.layoutMode !== "terminal")) return;
            const sid = ws.focusedSessionId;
            if (!sid) return;
            void useTerminalStore.getState().splitSession(
              wsId, sid, e.shiftKey ? "horizontal" : "vertical",
            );
          });
          break;
        case "p":
          if (e.shiftKey) return;
          e.preventDefault();
          fireShortcut("open-command-palette-files", () =>
            useUiStore.getState().openCommandPalette({ initialQuery: "%" })
          );
          break;
        case "k":
          e.preventDefault();
          if (e.shiftKey) {
            fireShortcut("open-command-palette-threads", () =>
              useUiStore.getState().openCommandPalette({ initialQuery: "@" })
            );
          } else {
            fireShortcut("toggle-command-palette", () =>
              useUiStore.getState().openCommandPalette()
            );
          }
          break;
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    void listenMenuAction((action) => {
      switch (action) {
        case "toggle-sidebar":
          fireShortcut("toggle-sidebar", () => useUiStore.getState().toggleSidebar());
          break;
        case "toggle-git-panel":
          fireShortcut("toggle-git-panel", () => useUiStore.getState().toggleGitPanel());
          break;
        case "toggle-focus-mode":
          fireShortcut("toggle-focus-mode", () => useUiStore.getState().toggleFocusMode());
          break;
        case "toggle-fullscreen":
          fireShortcut("toggle-fullscreen", () => {
            void toggleWindowFullscreen();
          });
          break;
        case "toggle-search":
          fireShortcut("toggle-search", () =>
            useUiStore.getState().openCommandPalette({ variant: "search", initialQuery: "?" })
          );
          break;
        case "toggle-terminal":
          fireShortcut("toggle-terminal", () => {
            const wsId = useWorkspaceStore.getState().activeWorkspaceId;
            if (wsId) cycleWorkspaceTerminalLayout(wsId);
          });
          break;
        case "close-window": {
          void requestWindowClose();
          break;
        }
        case "edit-undo":
        case "edit-redo":
        case "edit-cut":
        case "edit-copy":
        case "edit-paste":
        case "edit-select-all":
          void runEditMenuAction(action).catch((error) => {
            if (import.meta.env.DEV) {
              console.warn("[App] Failed to execute edit menu action", action, error);
            }
          });
          break;
      }
    }).then((fn) => {
      if (disposed) {
        fn();
      } else {
        unlisten = fn;
      }
    });

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, []);

  function dismissCodexRemoteThreadPrompt() {
    setCodexRemoteThreadPrompts((current) => current.slice(1));
  }

  async function respondComputerControlApproval(allowed: boolean) {
    const approval = computerControlApproval;
    if (!approval || computerControlApprovalUpdating) return;
    setComputerControlApprovalUpdating(true);
    try {
      await ipc.respondComputerControlApproval(approval.requestId, allowed);
    } catch (error) {
      console.warn("Failed to respond to computer control approval:", error);
      toast.error(t("app:settingsPage.computerControl.approvalFailed"));
    } finally {
      setComputerControlApprovals((current) =>
        current.filter((item) => item.requestId !== approval.requestId));
      setComputerControlApprovalUpdating(false);
    }
  }

  async function archiveCodexRemoteThreadLocally() {
    const prompt = codexRemoteThreadPrompt;
    if (!prompt) return;

    const wasActive = useThreadStore.getState().activeThreadId === prompt.thread.id;
    try {
      await ipc.archiveThreadLocally(prompt.thread.id);
      if (wasActive) {
        useThreadStore.getState().setActiveThread(null);
        await useChatStore.getState().setActiveThread(null);
      }
      await refreshThreads(prompt.thread.workspaceId);
      await refreshArchivedThreads(prompt.thread.workspaceId);
      dismissCodexRemoteThreadPrompt();
    } catch (error) {
      console.warn(`Failed to archive local Codex thread ${prompt.thread.id}:`, error);
      toast.error(t("app:sidebar.codexRemoteThreadArchiveFailed"));
    }
  }

  function dismissCliServiceRestartPrompt() {
    if (cliServiceRestartRunning) return;
    setCliServiceRestartPrompts((current) => current.slice(1));
  }

  async function restartRemoteCliService() {
    const prompt = cliServiceRestartPrompt;
    if (!prompt || cliServiceRestartRunning) return;
    const engine = resolveAgentDisplayName(prompt.engineId as ChatEngineId);
    setCliServiceRestartRunning(true);
    try {
      await ipc.restartRemoteCliService(prompt.threadId);
      toast.success(t("app:cliServiceRecovery.restartSuccess", { engine }));
      setCliServiceRestartPrompts((current) =>
        current.filter(
          (item) =>
            item.connectionId !== prompt.connectionId || item.engineId !== prompt.engineId,
        ),
      );
    } catch (error) {
      console.warn(`Failed to restart remote ${prompt.engineId} service:`, error);
      toast.error(
        t("app:cliServiceRecovery.restartFailed", {
          engine,
          error: String(error),
        }),
      );
    } finally {
      setCliServiceRestartRunning(false);
    }
  }

  const startupPhaseMessages: Record<AppStartupPhase, string> = {
    "loading-base-data": t("app:startup.phases.loadingBaseData"),
    "connecting-ssh": t("app:startup.phases.connectingSsh"),
    "creating-cli-tunnels": t("app:startup.phases.creatingCliTunnels"),
    "starting-cli-services": t("app:startup.phases.startingCliServices"),
    "syncing-remote-sessions": t("app:startup.phases.syncingRemoteSessions"),
    completed: t("app:startup.phases.completed"),
  };
  const startupReady = workspaceCatalogLoaded && startupCompleted;

  if (!startupReady) {
    return (
      <div
        className={`app-shell${customWindowFrame ? " app-shell-custom-frame" : ""}${
          customWindowFrameState.isMaximized ? " app-shell-custom-frame-maximized" : ""
        }${customWindowFrameState.isFullscreen ? " app-shell-custom-frame-fullscreen" : ""}`}
      >
        {customWindowFrame && <CustomWindowFrame frameState={customWindowFrameState} />}
        <div className="app-startup-screen" role="status" aria-live="polite">
          <div className="app-startup-spinner" aria-hidden="true" />
          <div className="app-startup-title">{t("app:startup.title")}</div>
          <div className="app-startup-message">
            {startupPhaseMessages[startupProgress.phase] || startupProgress.message}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div
      className={`app-shell${customWindowFrame ? " app-shell-custom-frame" : ""}${
        customWindowFrameState.isMaximized ? " app-shell-custom-frame-maximized" : ""
      }${customWindowFrameState.isFullscreen ? " app-shell-custom-frame-fullscreen" : ""}`}
    >
      {customWindowFrame && <CustomWindowFrame frameState={customWindowFrameState} />}
      <div className="app-shell-body">
        <ThreeColumnLayout />
      </div>
      <CommandPalette open={commandPaletteOpen} onClose={closeCommandPalette} />
      <PowerSettingsModal />
      <TerminalNotificationSettingsModal />
      <UsageLimitsModal />
      <OnboardingWizard />
      <ConfirmDialog
        open={cliServiceRestartPrompt !== null}
        title={t("app:cliServiceRecovery.title", {
          engine: cliServiceRestartPrompt
            ? resolveAgentDisplayName(cliServiceRestartPrompt.engineId as ChatEngineId)
            : "CLI",
        })}
        message={cliServiceRestartPrompt
          ? t("app:cliServiceRecovery.message", {
              engine: resolveAgentDisplayName(
                cliServiceRestartPrompt.engineId as ChatEngineId,
              ),
              reason: cliServiceRestartPrompt.reason,
            })
          : ""}
        confirmLabel={cliServiceRestartRunning
          ? t("app:cliServiceRecovery.restarting")
          : t("app:cliServiceRecovery.restart")}
        cancelLabel={t("common:actions.cancel")}
        confirmVariant="primary"
        onConfirm={() => void restartRemoteCliService()}
        onCancel={dismissCliServiceRestartPrompt}
      />
      <ConfirmDialog
        open={
          cliServiceRestartPrompt === null &&
          codexRemoteThreadPrompt !== null &&
          computerControlApproval === null
        }
        title={t("app:sidebar.codexRemoteThreadRemovedTitle")}
        message={
          codexRemoteThreadPrompt
            ? t(
                codexRemoteThreadPrompt.remoteAction === "deleted"
                  ? "app:sidebar.codexRemoteThreadDeletedMessage"
                  : "app:sidebar.codexRemoteThreadArchivedMessage",
                { name: codexRemoteThreadPrompt.thread.title || t("app:sidebar.untitledThread") },
              )
            : ""
        }
        confirmLabel={t("app:sidebar.archive")}
        cancelLabel={t("app:sidebar.keepLocalThread")}
        onConfirm={() => void archiveCodexRemoteThreadLocally()}
        onCancel={dismissCodexRemoteThreadPrompt}
      />
      <ConfirmDialog
        open={cliServiceRestartPrompt === null && computerControlApproval !== null}
        title={t("app:settingsPage.computerControl.approvalTitle")}
        message={computerControlApproval
          ? t("app:settingsPage.computerControl.approvalMessage", {
              agent: computerControlAgentName,
              tool: computerControlApproval.tool,
              application: computerControlApproval.application,
              operation: computerControlApproval.operation,
              scope: computerControlApproval.scope,
              threadId: computerControlApproval.threadId,
              turnId: computerControlApproval.turnId,
            })
          : ""}
        confirmLabel={computerControlApprovalUpdating
          ? t("app:settingsPage.computerControl.approvalSubmitting")
          : t("app:settingsPage.computerControl.approvalAllow")}
        cancelLabel={t("app:settingsPage.computerControl.approvalDeny")}
        confirmVariant="primary"
        onConfirm={() => void respondComputerControlApproval(true)}
        onCancel={() => void respondComputerControlApproval(false)}
      />
      <ToastContainer />
    </div>
  );
}
