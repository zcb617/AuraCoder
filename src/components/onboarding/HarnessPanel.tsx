import { useCallback, useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { open } from "@tauri-apps/plugin-shell";
import {
  ArrowLeft,
  ArrowRight,
  CheckCircle2,
  ClipboardCopy,
  Download,
  ExternalLink,
  Loader2,
  Play,
  RefreshCw,
  SlidersHorizontal,
  Terminal,
  XCircle,
} from "lucide-react";
import { useHarnessStore } from "../../stores/harnessStore";
import { useEngineStore } from "../../stores/engineStore";
import { useTerminalStore } from "../../stores/terminalStore";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useUiStore } from "../../stores/uiStore";
import { ipc, writeCommandToNewSession } from "../../lib/ipc";
import { copyTextToClipboard } from "../../lib/clipboard";
import { showWorkspaceSurface } from "../../lib/workspacePaneNavigation";
import {
  getHarnessInstallCommand,
  getHarnessTileAction,
} from "../../lib/harnessInstallActions";
import { handleDragDoubleClick, handleDragMouseDown } from "../../lib/windowDrag";
import { getHarnessIcon } from "../shared/HarnessLogos";
import { HarnessLaunchSettingsModal } from "./HarnessLaunchSettingsModal";
import type { HarnessInfo } from "../../types";

/*
当前系统只支持 opencode | codex | claude code 三个运行工具。工具的静态信息
（名称、网站、是否原生）由前端本地维护；已安装状态直接 live 订阅 engineStore
的本机 CLI 目录缓存（enginesByTarget["local"]，启动时已预热、由后端健康检查
事件驱动刷新），进入本页不再实时探测，因此不转圈。
*/
const PANEL_HARNESSES: ReadonlyArray<{
  id: string;
  name: string;
  website: string;
  native: boolean;
}> = [
  {
    id: "codex",
    name: "Codex CLI",
    website: "https://github.com/openai/codex",
    native: true,
  },
  {
    id: "claude-code",
    name: "Claude Code",
    website: "https://docs.anthropic.com/en/docs/claude-code",
    native: false,
  },
  {
    id: "opencode",
    name: "OpenCode",
    website: "https://opencode.ai",
    native: false,
  },
];

/* 运行工具 ID → 本机 CLI 生命周期 MAP 中的 CLI ID（claude-code 在生命周期里登记为 claude）。 */
const ENGINE_ID_BY_HARNESS_ID: Readonly<Record<string, string>> = {
  codex: "codex",
  "claude-code": "claude",
  opencode: "opencode",
};

/* ─── Harness tile ─── */
function HarnessTile({
  harness,
  description,
  preferredInstallMethod,
  launchArgs,
  onInstallInTerminal,
  onCopyCommand,
  onLaunch,
  onOpenWebsite,
  onOpenLaunchSettings,
  startupError,
}: {
  harness: HarnessInfo;
  description: string;
  preferredInstallMethod: string | null;
  launchArgs: string | undefined;
  onInstallInTerminal: () => void;
  onCopyCommand: () => void;
  onLaunch: () => void;
  onOpenWebsite: () => void;
  onOpenLaunchSettings: () => void;
  /** 本次健康刷新返回的启动失败原文；为空时按未安装显示。 */
  startupError: string | null;
}) {
  const { t } = useTranslation("app");
  const installCmd = getHarnessInstallCommand(harness.id, preferredInstallMethod);
  const action = getHarnessTileAction(harness);

  return (
    <div className={`hp-tile${harness.native ? " hp-tile-native" : ""}${harness.found ? " hp-tile-installed" : ""}`}>
      <div className="hp-tile-icon">
        {getHarnessIcon(harness.id, harness.native ? 22 : 18)}
      </div>

      <div className="hp-tile-body">
        <div className="hp-tile-name-row">
          <span className="hp-tile-name">{harness.name}</span>
          {harness.native && <span className="hp-tile-badge">{t("harnesses.native")}</span>}
        </div>
        <p className="hp-tile-desc">{description}</p>
        <div className="hp-tile-meta">
          {harness.found ? (
            <span className="hp-tile-status-ok">
              <CheckCircle2 size={10} />
              {t("harnesses.installed")}
            </span>
          ) : startupError ? (
            <span className="hp-tile-status-missing" title={startupError}>
              <XCircle size={10} />
              {t("harnesses.startupFailed")}
            </span>
          ) : (
            <span className="hp-tile-status-missing">
              <XCircle size={10} />
              {t("harnesses.notInstalled")}
            </span>
          )}
          {/* 需求只显示已安装/未安装，版本号不再展示，原逻辑保留备查：
          {harness.found && harness.version && (
            <span className="hp-tile-version">{harness.version}</span>
          )}
          */}
          {launchArgs && (
            <span className="hp-tile-flags" title={launchArgs}>
              {launchArgs}
            </span>
          )}
        </div>
      </div>

      <div className="hp-tile-action">
        <div className="hp-tile-action-group">
          <button
            type="button"
            className="hp-btn hp-btn-copy"
            onClick={onOpenLaunchSettings}
            title={t("harnesses.launchSettings.title")}
            aria-label={t("harnesses.launchSettings.title")}
          >
            <SlidersHorizontal size={11} />
          </button>
          {action === "launch" ? (
            <button type="button" className="hp-btn hp-btn-launch" onClick={onLaunch}>
              <Play size={11} />
              {t("harnesses.launch")}
            </button>
          ) : action === "install" && installCmd ? (
            <>
              <button
                type="button"
                className="hp-btn hp-btn-copy"
                onClick={onCopyCommand}
                title={installCmd}
              >
                <ClipboardCopy size={11} />
              </button>
              <button
                type="button"
                className="hp-btn hp-btn-install"
                onClick={onInstallInTerminal}
              >
                <Download size={11} />
                {t("harnesses.install")}
              </button>
            </>
          ) : action === "manual" ? (
            <button
              type="button"
              className="hp-btn hp-btn-copy"
              onClick={onOpenWebsite}
              title={harness.website}
            >
              <ExternalLink size={11} />
              {t("harnesses.website")}
            </button>
          ) : null}
        </div>
      </div>
    </div>
  );
}

/* ─── Main panel (full page) ─── */
export function HarnessPanel() {
  const { t } = useTranslation("app");
  /*
  旧实现进页面挂载即 ensureScanned() 全量探测本机进程，phase 置为 scanning 导致
  整页转圈；现在已安装状态改为 live 订阅 engineStore 的本机 CLI 目录缓存，不再
  在挂载时扫描，以下 store 订阅随之停用，保留备查：
  const phase = useHarnessStore((s) => s.phase);
  const harnesses = useHarnessStore((s) => s.harnesses);
  const error = useHarnessStore((s) => s.error);
  const loadedOnce = useHarnessStore((s) => s.loadedOnce);
  const scan = useHarnessStore((s) => s.scan);
  const ensureScanned = useHarnessStore((s) => s.ensureScanned);
  */
  const launch = useHarnessStore((s) => s.launch);
  const preferredInstallMethod = useHarnessStore((s) => s.preferredInstallMethod);
  const launchArgs = useHarnessStore((s) => s.launchArgs);
  const launchArgsLoaded = useHarnessStore((s) => s.launchArgsLoaded);
  const loadLaunchArgs = useHarnessStore((s) => s.loadLaunchArgs);

  const localEngines = useEngineStore((s) => s.enginesByTarget["local"]);
  const applyCliServicesUpdated = useEngineStore((s) => s.applyCliServicesUpdated);

  const activeWorkspaceId = useWorkspaceStore((s) => s.activeWorkspaceId);
  const createSession = useTerminalStore((s) => s.createSession);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const [settingsHarness, setSettingsHarness] = useState<HarnessInfo | null>(null);
  const [refreshing, setRefreshing] = useState(false);
  const [startupErrors, setStartupErrors] = useState<Record<string, string>>({});

  const installedEngineIds = new Set((localEngines ?? []).map((engine) => engine.id));
  const harnesses: HarnessInfo[] = PANEL_HARNESSES.map((def) => ({
    id: def.id,
    name: def.name,
    description: "",
    command: "",
    found: installedEngineIds.has(ENGINE_ID_BY_HARNESS_ID[def.id] ?? def.id),
    version: null,
    path: null,
    canAutoInstall: true,
    website: def.website,
    native: def.native,
  }));
  const installedCount = harnesses.filter((h) => h.found).length;
  const goBack = useCallback(() => setActiveView("chat"), [setActiveView]);

  /*
  旧实现的挂载即扫描逻辑，已停用（见上方说明）：
  useEffect(() => {
    if (loadedOnce) {
      return;
    }
    void ensureScanned();
  }, [ensureScanned, loadedOnce]);
  */

  useEffect(() => {
    // 启动预热尚未完成（缓存为 undefined）的极端情况下静默补拉一次本机目录；
    // 后端读的是内存生命周期 MAP，毫秒级返回，页面不转圈，数据落地后由 live
    // 订阅自动填充。
    if (useEngineStore.getState().enginesByTarget["local"] === undefined) {
      void applyCliServicesUpdated({ scope: "local", connectionId: null, revision: 0 });
    }
  }, [applyCliServicesUpdated]);

  useEffect(() => {
    if (launchArgsLoaded) {
      return;
    }
    void loadLaunchArgs();
  }, [launchArgsLoaded, loadLaunchArgs]);

  const handleRefresh = useCallback(async () => {
    setRefreshing(true);
    try {
      // 先等后端健康检查 reconcile 完生命周期 MAP，再刷新本机 CLI 目录缓存，
      // 保证转圈结束时 live 订阅读到的已经是新数据。
      const result = await ipc.refreshLocalCliHealth();
      const nextStartupErrors: Record<string, string> = {};
      for (const [harnessId, cliId] of Object.entries(ENGINE_ID_BY_HARNESS_ID)) {
        const startupErrorPrefix = `本机 ${cliId} CLI 已被探测到，但 AuraCoder 无法启动并登记该服务：`;
        const startupError = result.errors.find((error) => error.startsWith(startupErrorPrefix));
        if (startupError) {
          nextStartupErrors[harnessId] = startupError;
        }
      }
      setStartupErrors(nextStartupErrors);
      await applyCliServicesUpdated({ scope: "local", connectionId: null, revision: 0 });
      const { toast } = await import("../../stores/toastStore");
      if (result.errors.length > 0) {
        toast.error(result.errors.join("\n"));
        return;
      }
      toast.success(t("harnesses.refreshSynced"));
    } catch (error) {
      console.warn("本机 CLI 健康检查失败:", error);
    } finally {
      setRefreshing(false);
    }
  }, [applyCliServicesUpdated, t]);

  const spawnInTerminal = useCallback(
    async (command: string) => {
      if (!activeWorkspaceId) return;

      showWorkspaceSurface(activeWorkspaceId, "terminal");

      const sessionId = await createSession(activeWorkspaceId);
      if (sessionId) {
        void writeCommandToNewSession(activeWorkspaceId, sessionId, command);
      }

      setActiveView("chat");
    },
    [activeWorkspaceId, createSession, setActiveView],
  );

  async function handleLaunch(harnessId: string) {
    const command = await launch(harnessId);
    if (command) await spawnInTerminal(command);
  }

  function handleInstallInTerminal(harnessId: string) {
    const cmd = getHarnessInstallCommand(harnessId, preferredInstallMethod);
    if (cmd) void spawnInTerminal(cmd);
  }

  function handleCopyCommand(harnessId: string) {
    const cmd = getHarnessInstallCommand(harnessId, preferredInstallMethod);
    if (cmd) {
      void copyTextToClipboard(cmd)
        .then(() => {
          void import("../../stores/toastStore").then(({ toast }) => {
            toast.success(t("harnesses.copySuccess"));
          });
        })
        .catch(() => {
          void import("../../stores/toastStore").then(({ toast }) => {
            toast.error(t("harnesses.copyFailed"));
          });
        });
    }
  }

  function handleOpenWebsite(website: string) {
    void open(website).catch(() => {
      void import("../../stores/toastStore").then(({ toast }) => {
        toast.error(t("harnesses.websiteOpenFailed"));
      });
    });
  }

  return (
    <div className="hp-root">
      <div className="hp-scroll">
        <div className="hp-inner">
          {/* Header */}
          <div className="hp-header">
            <div
              className="hp-header-top"
              onMouseDown={handleDragMouseDown}
              onDoubleClick={handleDragDoubleClick}
            >
              <button type="button" className="wsp-back" onClick={goBack} title={t("workspace:actions.back")}>
                <ArrowLeft size={14} />
              </button>
              <div className="hp-header-icon">
                <Terminal size={16} />
              </div>
              <div className="hp-header-text">
                <h1 className="hp-title">{t("harnesses.title")}</h1>
                <p className="hp-subtitle">
                  {t("harnesses.detectedCount", {
                    installed: installedCount,
                    total: PANEL_HARNESSES.length,
                  })}
                </p>
              </div>
              <button
                type="button"
                className="hp-rescan"
                onClick={() => void handleRefresh()}
                disabled={refreshing}
                title={t("harnesses.rescan")}
              >
                <RefreshCw
                  size={12}
                  style={{
                    animation: refreshing ? "spin 1s linear infinite" : "none",
                  }}
                />
              </button>
            </div>
          </div>

          {/* Content */}
          {/* 旧实现进入页面挂载即扫描时显示整页 loading 转圈；现在进入页面直接读缓存
              渲染列表，只有手动点击刷新按钮期间才显示整页转圈： */}
          {refreshing ? (
            <div className="hp-loading">
              <Loader2
                size={20}
                style={{ color: "var(--accent)", animation: "spin 1s linear infinite" }}
              />
              <p>{t("harnesses.loading")}</p>
            </div>
          ) : (
          <div className="hp-grid">
            {harnesses.map((h) => (
              <HarnessTile
                key={h.id}
                harness={h}
                description={t(`harnesses.descriptions.${h.id}`, { defaultValue: h.description })}
                preferredInstallMethod={preferredInstallMethod}
                launchArgs={launchArgs[h.id]}
                onInstallInTerminal={() => handleInstallInTerminal(h.id)}
                onCopyCommand={() => handleCopyCommand(h.id)}
                onLaunch={() => void handleLaunch(h.id)}
                onOpenWebsite={() => handleOpenWebsite(h.website)}
                onOpenLaunchSettings={() => setSettingsHarness(h)}
                startupError={startupErrors[h.id] ?? null}
              />
            ))}
          </div>
          )}

          {/* 扫描错误提示随扫描逻辑一并停用；手动刷新失败只在控制台告警，保留备查：
          {error && (
            <div className="hp-error">
              <p>{error}</p>
              <button
                type="button"
                className="hp-btn hp-btn-install"
                onClick={() => void scan()}
              >
                {t("harnesses.retry")}
              </button>
            </div>
          )}
          */}

          {/* Footer hint */}
          <div className="hp-footer">
            <ArrowRight size={11} />
            <span>{t("harnesses.footerHint")}</span>
          </div>
        </div>
      </div>

      {settingsHarness && (
        <HarnessLaunchSettingsModal
          harness={settingsHarness}
          onClose={() => setSettingsHarness(null)}
        />
      )}
    </div>
  );
}
