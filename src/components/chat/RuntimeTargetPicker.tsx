import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  AlertTriangle,
  CheckCircle2,
  ChevronDown,
  LoaderCircle,
  Monitor,
  RefreshCw,
  Server,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { useEngineStore } from "../../stores/engineStore";
// Apps/连接器不属于 AuraCoder 的运行时能力展示范围，因此不再接收 CodexApp。
// import type { CodexApp } from "../../types";
import type { CodexPlugin, CodexSkill, OpenCodeRuntimeCatalog } from "../../types";

interface RuntimeTargetPickerProps {
  engineId: string;
  engineName: string;
  codexSkills: CodexSkill[];
  codexPlugins: CodexPlugin[];
  // Apps/连接器不属于 AuraCoder 管理的 Skill、Plugin、MCP 能力范围。
  // codexApps: CodexApp[];
  openCodeCatalog: OpenCodeRuntimeCatalog | null;
  capabilitiesLoading?: boolean;
  capabilitiesError?: string | null;
  capabilitiesPartial?: boolean;
  onRefreshCapabilities?: () => Promise<void>;
  disabled?: boolean;
}

function usageWindowLabel(kind: string, t: (key: string) => string): string {
  switch (kind) {
    case "five_hour":
      return t("runtimeTarget.usageFiveHour");
    case "weekly":
      return t("runtimeTarget.usageWeekly");
    case "fable_weekly":
      return t("runtimeTarget.usageFableWeekly");
    case "opus_weekly":
      return t("runtimeTarget.usageOpusWeekly");
    case "sonnet_weekly":
      return t("runtimeTarget.usageSonnetWeekly");
    default:
      return kind;
  }
}

export function RuntimeTargetPicker({
  engineId,
  engineName,
  codexSkills,
  codexPlugins,
  // Apps/连接器不属于当前运行时能力展示范围。
  // codexApps,
  openCodeCatalog,
  capabilitiesLoading = false,
  capabilitiesError = null,
  capabilitiesPartial = false,
  onRefreshCapabilities,
  disabled = false,
}: RuntimeTargetPickerProps) {
  const { t } = useTranslation("chat");
  const target = useEngineStore((state) => state.target);
  const targetLoading = useEngineStore((state) => state.loading);
  const targetError = useEngineStore((state) => state.error);
  const health = useEngineStore((state) => state.health[engineId]);
  const healthLoading = useEngineStore((state) => Boolean(state.healthLoading[engineId]));
  const usage = useEngineStore((state) => state.usage[engineId]);
  const usageLoading = useEngineStore((state) => Boolean(state.usageLoading[engineId]));
  const ensureHealth = useEngineStore((state) => state.ensureHealth);
  const ensureUsage = useEngineStore((state) => state.ensureUsage);
  const refreshEngineCatalog = useEngineStore((state) => state.refreshEngineCatalog);
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const popoverRef = useRef<HTMLDivElement>(null);
  const [pos, setPos] = useState({ bottom: 0, left: 0 });

  const targetUnavailable =
    target?.connectionStatus === "failed" ||
    target?.connectionStatus === "disabled" ||
    target?.connectionStatus === "deleted" ||
    health?.available === false;
  const targetReady = health?.available === true;
  const statusClass = targetUnavailable
    ? " runtime-target-status-error"
    : healthLoading
      ? " runtime-target-status-loading"
      : targetReady
        ? " runtime-target-status-ready"
        : "";
  const address = useMemo(() => {
    if (!target || target.kind !== "ssh" || !target.hostName) {
      return null;
    }
    const user = target.user ? `${target.user}@` : "";
    const port = target.port && target.port !== 22 ? `:${target.port}` : "";
    return `${user}${target.hostName}${port}`;
  }, [target]);
  const diagnostics = health?.protocolDiagnostics;
  const account = diagnostics?.account;
  const capabilitySummary = useMemo(() => {
    if (engineId === "codex") {
      return [
        t("runtimeTarget.skillsCount", { count: codexSkills.length }),
        // Apps/连接器不属于 AuraCoder 管理的运行时能力。
        // t("runtimeTarget.appsCount", { count: codexApps.length }),
        t("runtimeTarget.pluginsCount", { count: codexPlugins.length }),
        t("runtimeTarget.mcpCount", { count: diagnostics?.mcpServers.length ?? 0 }),
      ];
    }
    if (engineId === "opencode") {
      return [
        t("runtimeTarget.agentsCount", { count: openCodeCatalog?.agents.length ?? 0 }),
        t("runtimeTarget.commandsCount", { count: openCodeCatalog?.commands.length ?? 0 }),
        t("runtimeTarget.mcpCount", { count: openCodeCatalog?.mcpServers.length ?? 0 }),
      ];
    }
    return [];
  }, [
    codexPlugins.length,
    codexSkills.length,
    diagnostics?.mcpServers.length,
    engineId,
    openCodeCatalog,
    t,
  ]);

  useEffect(() => {
    if (!open) {
      return;
    }
    void ensureHealth(engineId);
    if (engineId === "codex" || engineId === "claude") {
      void ensureUsage(engineId);
    }
    if (engineId === "opencode" && !openCodeCatalog && onRefreshCapabilities) {
      void onRefreshCapabilities();
    }
  }, [engineId, ensureHealth, ensureUsage, onRefreshCapabilities, open, openCodeCatalog]);

  useLayoutEffect(() => {
    if (!open || !triggerRef.current) {
      return;
    }
    const rect = triggerRef.current.getBoundingClientRect();
    const width = Math.min(420, window.innerWidth - 16);
    setPos({
      bottom: window.innerHeight - rect.top + 6,
      left: Math.max(8, Math.min(rect.left, window.innerWidth - width - 8)),
    });
  }, [open]);

  useEffect(() => {
    if (!open) {
      return;
    }
    function onPointerDown(event: PointerEvent) {
      const node = event.target as Node;
      if (triggerRef.current?.contains(node) || popoverRef.current?.contains(node)) {
        return;
      }
      setOpen(false);
    }
    function onKeyDown(event: KeyboardEvent) {
      if (event.key === "Escape") {
        setOpen(false);
      }
    }
    document.addEventListener("pointerdown", onPointerDown, true);
    document.addEventListener("keydown", onKeyDown, true);
    return () => {
      document.removeEventListener("pointerdown", onPointerDown, true);
      document.removeEventListener("keydown", onKeyDown, true);
    };
  }, [open]);

  if (!target) {
    return (
      <button
        type="button"
        className="chat-toolbar-btn chat-toolbar-btn-bordered"
        disabled
        title={targetError}
      >
        {targetLoading ? (
          <LoaderCircle size={12} className="mp-loading-spinner" />
        ) : (
          <AlertTriangle size={12} />
        )}
        <span className={`runtime-target-status${targetLoading ? " runtime-target-status-loading" : " runtime-target-status-error"}`} />
        <span style={{ fontSize: 11 }}>
          {targetLoading
            ? t("runtimeTarget.loadingTarget")
            : t("runtimeTarget.targetUnavailable")}
        </span>
      </button>
    );
  }

  return (
    <>
      <button
        ref={triggerRef}
        type="button"
        className={`chat-toolbar-btn chat-toolbar-btn-bordered${open ? " chat-toolbar-btn-active" : ""}`}
        disabled={disabled}
        title={t("runtimeTarget.openDetails")}
        onClick={() => setOpen((current) => !current)}
      >
        {target.kind === "ssh" ? <Server size={12} /> : <Monitor size={12} />}
        <span className={`runtime-target-status${statusClass}`} aria-hidden="true" />
        <span style={{ fontSize: 11 }}>
          {target.kind === "ssh"
            ? t("runtimeTarget.sshShort", { name: target.displayName })
            : t("runtimeTarget.local")}
        </span>
        <ChevronDown size={12} />
      </button>

      {open &&
        createPortal(
          <div
            ref={popoverRef}
            className="codex-config-popover runtime-target-popover"
            style={{
              position: "fixed",
              zIndex: 1300,
              bottom: pos.bottom,
              left: pos.left,
              width: "min(420px, calc(100vw - 16px))",
              maxHeight: "min(72vh, 580px)",
              overflowY: "auto",
            }}
          >
            <div className="codex-config-header runtime-target-header">
              <div className="runtime-target-heading">
                <div className="runtime-target-icon">
                  {target.kind === "ssh" ? <Server size={15} /> : <Monitor size={15} />}
                </div>
                <div>
                  <div className="codex-config-title">
                    {target.kind === "ssh"
                      ? t("runtimeTarget.remoteTitle")
                      : t("runtimeTarget.localTitle")}
                  </div>
                  <div className="codex-config-subtitle">
                    {target.displayName}{address ? ` · ${address}` : ""}
                  </div>
                </div>
              </div>
              <button
                type="button"
                className="runtime-target-refresh"
                disabled={healthLoading || usageLoading || capabilitiesLoading}
                onClick={() => {
                  void Promise.all([
                    ensureHealth(engineId, { force: true }),
                    refreshEngineCatalog(engineId),
                    engineId === "codex" || engineId === "claude"
                      ? ensureUsage(engineId, { force: true })
                      : Promise.resolve(null),
                    onRefreshCapabilities?.() ?? Promise.resolve(),
                  ]);
                }}
              >
                <RefreshCw size={12} className={healthLoading ? "mp-loading-spinner" : ""} />
                {t("runtimeTarget.refresh")}
              </button>
            </div>

            <div className="runtime-target-body">
              {target.projectPath ? (
                <div className="runtime-target-path" title={target.projectPath}>
                  {target.projectPath}
                </div>
              ) : null}

              <section className="runtime-target-section">
                <div className="runtime-target-section-title">{t("runtimeTarget.currentCli")}</div>
                <div className="runtime-target-cli-row">
                  <div>
                    <strong>{engineName}</strong>
                    {health?.version ? <span> · {health.version}</span> : null}
                  </div>
                  <div className={`runtime-target-health${targetUnavailable ? " is-error" : targetReady ? " is-ready" : ""}`}>
                    {healthLoading ? (
                      <LoaderCircle size={12} className="mp-loading-spinner" />
                    ) : targetUnavailable ? (
                      <AlertTriangle size={12} />
                    ) : targetReady ? (
                      <CheckCircle2 size={12} />
                    ) : (
                      <span className="runtime-target-status" />
                    )}
                    {healthLoading
                      ? t("runtimeTarget.checking")
                      : targetUnavailable
                        ? t("runtimeTarget.unavailable")
                        : targetReady
                          ? t("runtimeTarget.available")
                          : t("runtimeTarget.notChecked")}
                  </div>
                </div>
                {health?.details ? (
                  <div className={`runtime-target-note${targetUnavailable ? " is-error" : ""}`}>
                    {health.details}
                  </div>
                ) : null}
              </section>

              <section className="runtime-target-section">
                <div className="runtime-target-section-title">{t("runtimeTarget.account")}</div>
                <div className="runtime-target-value">
                  {account
                    ? [account.email, account.planType, account.provider].filter(Boolean).join(" · ")
                    : targetReady
                      ? t("runtimeTarget.accountReady")
                      : t("runtimeTarget.accountUnavailable")}
                </div>
              </section>

              {(engineId === "codex" || engineId === "claude") ? (
                <section className="runtime-target-section">
                  <div className="runtime-target-section-title">{t("runtimeTarget.usage")}</div>
                  {usageLoading ? (
                    <div className="runtime-target-inline-loading">
                      <LoaderCircle size={12} className="mp-loading-spinner" />
                      {t("runtimeTarget.loadingUsage")}
                    </div>
                  ) : usage?.available && usage.windows.length > 0 ? (
                    <div className="runtime-target-usage-list">
                      {usage.windows.map((window) => (
                        <div key={window.kind} className="runtime-target-usage-row">
                          <div className="runtime-target-usage-label">
                            <span>{usageWindowLabel(window.kind, t)}</span>
                            <strong>{window.usedPercent}%</strong>
                          </div>
                          <div className="runtime-target-usage-track">
                            <span style={{ width: `${Math.max(0, Math.min(100, window.usedPercent))}%` }} />
                          </div>
                        </div>
                      ))}
                    </div>
                  ) : (
                    <div className="runtime-target-note">{t("runtimeTarget.usageUnavailable")}</div>
                  )}
                </section>
              ) : null}

              {capabilitySummary.length > 0 || capabilitiesError || capabilitiesLoading || engineId === "claude" ? (
                <section className="runtime-target-section">
                  <div className="runtime-target-section-title">{t("runtimeTarget.capabilities")}</div>
                  {capabilitiesLoading ? (
                    <div className="runtime-target-inline-loading">
                      <LoaderCircle size={12} className="mp-loading-spinner" />
                      {t("runtimeTarget.loadingCapabilities")}
                    </div>
                  ) : (
                    <>
                      {capabilitySummary.length > 0 ? (
                        <div className="runtime-target-chips">
                          {capabilitySummary.map((item) => <span key={item}>{item}</span>)}
                        </div>
                      ) : null}
                      {capabilitiesError ? (
                        <div className={`runtime-target-note${capabilitiesPartial ? " is-warning" : " is-error"}`}>
                          {capabilitiesError}
                        </div>
                      ) : capabilitySummary.length === 0 ? (
                        <div className="runtime-target-note">
                          {t("runtimeTarget.capabilitiesUnavailable")}
                        </div>
                      ) : null}
                    </>
                  )}
                </section>
              ) : null}

              {diagnostics?.fetchedAt ? (
                <div className="runtime-target-fetched">
                  {t("runtimeTarget.fetchedAt", {
                    time: new Date(diagnostics.fetchedAt).toLocaleString(),
                  })}
                </div>
              ) : null}
            </div>
          </div>,
          document.body,
        )}
    </>
  );
}
