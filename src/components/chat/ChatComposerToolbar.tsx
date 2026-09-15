import type { Dispatch, MutableRefObject, SetStateAction } from "react";
import { useTranslation } from "react-i18next";
import { Loader2, ListChecks, Plus, Send, Square } from "lucide-react";
import { ipc } from "../../lib/ipc";
import type { MessageSendMode } from "../../lib/chatInputSettings";
import { toast } from "../../stores/toastStore";
import type {
  ChatAttachment,
  CliContextUsage,
  CodexPlugin,
  CodexSkill,
  EngineHealth,
  EngineInfo,
  EngineModel,
  OpenCodeAgent,
  OpenCodeRuntimeCatalog,
  PermissionComponentJson,
  Thread,
  Workspace,
} from "../../types";
import { Dropdown } from "../shared/Dropdown";
import { OpenCodeAgentPicker } from "./OpenCodeAgentPicker";
import { PermissionPicker } from "./PermissionPicker";
import { RuntimeTargetPicker } from "./RuntimeTargetPicker";
import { ModelPicker } from "./ModelPicker";
import { formatContextUsage } from "./formatters";
import type { CodexReferenceCatalogState, ThreadRuntimeSelectionPatch } from "./chatPanelTypes";
import type { CodexConfigPatch, CodexServiceTierValue } from "./CodexConfigPicker";
import { resolveReasoningEffortForModel } from "./reasoningEffort";

/** 输入工具栏属性，承载附件、运行时选择、权限和发送控制所需的业务状态。 */
export interface ChatComposerToolbarProps {
  /** 是否显示特殊工具输入模式，用于隐藏普通聊天工具栏选项。 */
  showSpecialInputComposer: boolean;
  /** 当前活动工作区标识，用于控制工具栏可用状态。 */
  activeWorkspaceId: string | null;
  /** 当前输入草稿待发送的附件列表。 */
  attachments: ChatAttachment[];
  /** 打开附件选择流程并将文件加入当前草稿。 */
  handleAddAttachment: () => Promise<void>;
  /** 当前是否使用 OpenCode 引擎。 */
  isOpenCodeEngine: boolean;
  /** 当前可供选择的 OpenCode Agent 列表。 */
  openCodeSelectableAgents: OpenCodeAgent[];
  /** 当前选中的 OpenCode Agent 名称。 */
  selectedOpenCodeAgent: string;
  /** 持久化 OpenCode Agent 选择。 */
  onOpenCodeAgentChange: (agent: string) => Promise<void>;
  /** OpenCode 运行时目录是否已加载。 */
  openCodeCatalogLoaded: boolean;
  /** 当前 OpenCode 运行时能力目录。 */
  openCodeCatalog: OpenCodeRuntimeCatalog | null;
  /** OpenCode 运行时目录是否正在加载。 */
  openCodeCatalogLoading: boolean;
  /** OpenCode 运行时目录加载错误。 */
  openCodeCatalogError: string | null;
  /** Codex Skill 列表。 */
  codexSkills: CodexSkill[];
  /** Codex Plugin 列表。 */
  codexPlugins: CodexPlugin[];
  /** Codex 引用目录是否正在加载。 */
  codexReferenceCatalogLoading: boolean;
  /** Codex 引用目录加载错误。 */
  codexReferenceCatalogError: string | null;
  /** Codex 引用目录各子目录加载状态。 */
  codexReferenceCatalogState: CodexReferenceCatalogState;
  /** 当前计划模式是否已生效。 */
  activePlanMode: boolean;
  /** 当前计划模式本地状态。 */
  planMode: boolean;
  /** 当前会话发送模式。 */
  sessionMessageSendMode: MessageSendMode;
  /** 当前活动聊天会话标识。 */
  activeChatSessionId: string | null;
  /** 当前活动线程。 */
  activeThread: Thread | null;
  /** 当前活动工作区对象。 */
  activeWorkspace: Workspace | null;
  /** 当前线程是否已锁定运行时选择。 */
  activeThreadRuntimeLocked: boolean;
  /** 可用运行时引擎列表。 */
  engines: EngineInfo[];
  /** 各运行时引擎健康状态。 */
  health: Record<string, EngineHealth>;
  /** 当前选中的运行时引擎标识。 */
  selectedEngineId: string;
  /** 当前选中的运行时引擎。 */
  selectedEngine: EngineInfo | null;
  /** 当前选中的模型标识。 */
  selectedModelId: string | null;
  /** 当前选中的模型。 */
  selectedModel: EngineModel | null;
  /** 当前选中的推理强度。 */
  selectedEffort: string;
  /** 当前选中的 Codex 服务档位。 */
  selectedServiceTier: CodexServiceTierValue;
  /** 引擎列表是否正在加载。 */
  enginesLoading: boolean;
  /** 引擎列表加载错误。 */
  engineLoadError: string | undefined;
  /** 当前权限组件配置。 */
  permissionComponent: PermissionComponentJson;
  /** 当前线程上下文用量。 */
  contextUsage: CliContextUsage | null;
  /** 上下文用量是否正在加载。 */
  contextUsageLoading: boolean;
  /** 当前会话是否正在流式处理。 */
  streaming: boolean;
  /** 当前输入是否具备提交条件。 */
  canSubmitComposer: boolean;
  /** 当前消息是否正在提交。 */
  isSubmitting: boolean;
  /** 当前选中模型标识引用，用于异步回调中的最新值判断。 */
  selectedModelIdRef: MutableRefObject<string | null>;
  /** 当前选中引擎标识引用，用于保持运行时选择同步。 */
  selectedEngineIdRef: MutableRefObject<string>;
  /** 当前推理强度引用，用于计算模型切换后的默认值。 */
  selectedEffortRef: MutableRefObject<string>;
  /** 是否由用户手动覆盖线程运行时选择的引用。 */
  manuallyOverrodeThreadSelectionRef: MutableRefObject<boolean>;
  /** 变更计划模式状态。 */
  setPlanMode: Dispatch<SetStateAction<boolean>>;
  /** 保存线程运行时选择局部变更。 */
  saveThreadRuntimeSelectionPatch: (
    threadId: string | null | undefined,
    patch: ThreadRuntimeSelectionPatch,
  ) => Promise<boolean>;
  /** 保存当前会话的发送模式。 */
  setThreadMessageSendMode: (threadId: string, mode: MessageSendMode) => void;
  /** 保存尚未创建线程时工作区的发送模式。 */
  setPendingMessageSendMode: (workspaceId: string, mode: MessageSendMode) => void;
  /** 加载 OpenCode 运行时目录。 */
  loadOpenCodeRuntimeCatalog: () => Promise<void>;
  /** 刷新 Codex 引用目录。 */
  refreshCodexReferenceCatalogs: () => Promise<void>;
  /** 标记用户已明确选择聊天运行时。 */
  setHasExplicitComposerRuntime: Dispatch<SetStateAction<boolean>>;
  /** 更新当前选中的引擎标识。 */
  setSelectedEngineId: Dispatch<SetStateAction<string>>;
  /** 更新当前选中的模型标识。 */
  setSelectedModelId: Dispatch<SetStateAction<string | null>>;
  /** 更新当前选中的推理强度。 */
  setSelectedEffort: Dispatch<SetStateAction<string>>;
  /** 更新线程本地的最后选中模型。 */
  setThreadLastModelLocal: (threadId: string, modelId: string | null) => void;
  /** 应用线程更新到本地线程状态。 */
  applyThreadUpdateLocal: (thread: Thread) => boolean;
  /** 保存线程推理强度。 */
  onReasoningEffortChange: (nextEffort: string) => Promise<void>;
  /** 保存 Codex 配置变更。 */
  onCodexConfigSave: (patch: CodexConfigPatch) => Promise<void>;
  /** 加载当前工作区的运行时引擎。 */
  loadEngines: (workspaceId?: string | null) => Promise<void>;
  /** 保存权限组件配置变更。 */
  onPermissionComponentChange: (next: PermissionComponentJson) => Promise<boolean>;
  /** 打开上下文用量详情。 */
  openUsageLimitsModal: () => void;
  /** 取消当前流式处理。 */
  cancel: () => Promise<void>;
}

/** 渲染聊天输入工具栏，提供附件、运行时、权限和发送操作。 */
export function ChatComposerToolbar({
  showSpecialInputComposer,
  activeWorkspaceId,
  attachments,
  handleAddAttachment,
  isOpenCodeEngine,
  openCodeSelectableAgents,
  selectedOpenCodeAgent,
  onOpenCodeAgentChange,
  openCodeCatalogLoaded,
  openCodeCatalog,
  openCodeCatalogLoading,
  openCodeCatalogError,
  codexSkills,
  codexPlugins,
  codexReferenceCatalogLoading,
  codexReferenceCatalogError,
  codexReferenceCatalogState,
  activePlanMode,
  planMode,
  sessionMessageSendMode,
  activeChatSessionId,
  activeThread,
  activeWorkspace,
  activeThreadRuntimeLocked,
  engines,
  health,
  selectedEngineId,
  selectedEngine,
  selectedModelId,
  selectedModel,
  selectedEffort,
  selectedServiceTier,
  enginesLoading,
  engineLoadError,
  permissionComponent,
  contextUsage,
  contextUsageLoading,
  streaming,
  canSubmitComposer,
  isSubmitting,
  selectedModelIdRef,
  selectedEngineIdRef,
  selectedEffortRef,
  manuallyOverrodeThreadSelectionRef,
  setPlanMode,
  saveThreadRuntimeSelectionPatch,
  setThreadMessageSendMode,
  setPendingMessageSendMode,
  loadOpenCodeRuntimeCatalog,
  refreshCodexReferenceCatalogs,
  setHasExplicitComposerRuntime,
  setSelectedEngineId,
  setSelectedModelId,
  setSelectedEffort,
  setThreadLastModelLocal,
  applyThreadUpdateLocal,
  onReasoningEffortChange,
  onCodexConfigSave,
  loadEngines,
  onPermissionComponentChange,
  openUsageLimitsModal,
  cancel,
}: ChatComposerToolbarProps) {
  const { t } = useTranslation("chat");

  return (
            <div
              style={{
                display: "flex",
                alignItems: "center",
                padding: "6px 10px",
                gap: 6,
              }}
            >
              {/* Attach file button */}
              {!showSpecialInputComposer && (
                <button
                  type="button"
                  className="chat-toolbar-btn chat-toolbar-btn-bordered"
                  onClick={() => void handleAddAttachment()}
                  disabled={!activeWorkspaceId}
                  title={t("panel.attachFiles")}
                >
                  <Plus size={12} />
                  <span style={{ fontSize: 11 }}>{t("panel.attachShort")}</span>
                  {attachments.length > 0 && (
                    <span className="chat-toolbar-badge">{attachments.length}</span>
                  )}
                </button>
              )}

              {!showSpecialInputComposer && (
                isOpenCodeEngine ? (
                  <OpenCodeAgentPicker
                    agents={openCodeSelectableAgents}
                    selectedAgent={selectedOpenCodeAgent}
                    onAgentChange={(agent) => void onOpenCodeAgentChange(agent)}
                    disabled={!openCodeCatalogLoaded && openCodeSelectableAgents.length === 0}
                  />
                ) : (
                  <button
                    type="button"
                    className={`chat-toolbar-btn chat-toolbar-btn-bordered ${activePlanMode ? "chat-toolbar-btn-active" : ""}`}
                    onClick={() => {
                      const nextPlanMode = !planMode;
                      setPlanMode(nextPlanMode);
                      void saveThreadRuntimeSelectionPatch(
                        activeThread?.id ?? activeChatSessionId,
                        { planMode: nextPlanMode },
                      );
                    }}
                    disabled={!activeWorkspaceId}
                    title={
                      selectedEngineId === "codex"
                        ? activePlanMode
                          ? t("panel.disablePlanModeCodex")
                          : t("panel.enablePlanModeCodex")
                        : activePlanMode
                          ? t("panel.disablePlanMode")
                          : t("panel.enablePlanMode")
                    }
                  >
                    <ListChecks size={12} />
                    <span style={{ fontSize: 11 }}>{t("panel.planShort")}</span>
                  </button>
                )
              )}

              {!showSpecialInputComposer && (
                <Dropdown
                  options={[
                    {
                      value: "classic",
                      label: t("panel.messageSendModes.classic"),
                    },
                    {
                      value: "flexible",
                      label: t("panel.messageSendModes.flexible"),
                    },
                  ]}
                  value={sessionMessageSendMode}
                  onChange={(nextMessageSendMode) => {
                    if (
                      nextMessageSendMode !== "classic" &&
                      nextMessageSendMode !== "flexible"
                    ) {
                      return;
                    }
                    if (activeChatSessionId) {
                      setThreadMessageSendMode(
                        activeChatSessionId,
                        nextMessageSendMode,
                      );
                      void saveThreadRuntimeSelectionPatch(activeChatSessionId, {
                        sendMethod: nextMessageSendMode,
                      });
                      return;
                    }
                    if (activeWorkspaceId) {
                      setPendingMessageSendMode(activeWorkspaceId, nextMessageSendMode);
                    }
                  }}
                  disabled={!activeWorkspaceId}
                  title={t("panel.sessionMessageSendMode")}
                  selectedLabel={t(`panel.messageSendModes.${sessionMessageSendMode}`)}
                />
              )}

              {!showSpecialInputComposer && <div className="chat-toolbar-divider" />}

              {/* Engine + Model + Effort selector */}
              {!showSpecialInputComposer && (
                <>
                  <RuntimeTargetPicker
                    engineId={selectedEngineId}
                    engineName={
                      selectedEngineId === "claude"
                        ? "Claude Code"
                        : selectedEngine?.name ?? selectedEngineId
                    }
                    codexSkills={codexSkills}
                    codexPlugins={codexPlugins}
                    /* Apps/连接器不属于 AuraCoder 管理的运行时能力。
                    codexApps={codexApps} */
                    openCodeCatalog={openCodeCatalog}
                    capabilitiesLoading={
                      selectedEngineId === "opencode"
                        ? openCodeCatalogLoading
                        : selectedEngineId === "codex"
                          ? codexReferenceCatalogLoading
                          : false
                    }
                    capabilitiesError={
                      selectedEngineId === "opencode"
                        ? openCodeCatalogError
                        : selectedEngineId === "codex"
                          ? codexReferenceCatalogError
                          : null
                    }
                    capabilitiesPartial={
                      selectedEngineId === "codex" &&
                      Boolean(codexReferenceCatalogError) &&
                      (codexReferenceCatalogState.skillsLoaded ||
                        codexReferenceCatalogState.appsLoaded)
                    }
                    onRefreshCapabilities={
                      selectedEngineId === "opencode"
                        ? loadOpenCodeRuntimeCatalog
                        : selectedEngineId === "codex"
                          ? refreshCodexReferenceCatalogs
                          : undefined
                    }
                    disabled={!activeWorkspaceId}
                  />
                  <ModelPicker
                    engines={
                      activeThreadRuntimeLocked
                        ? engines.filter((engine) => engine.id === activeThread?.engineId)
                        : engines
                    }
                    health={health}
                    selectedEngineId={selectedEngineId}
                    selectedModelId={selectedModelId ?? selectedModel?.id ?? ""}
                    selectedEffort={selectedEffort}
                    selectedServiceTier={selectedServiceTier}
                    onEngineModelChange={(engineId, modelId) => {
                      if (
                        activeThreadRuntimeLocked &&
                        engineId !== activeThread?.engineId
                      ) {
                        return;
                      }
                      const previousEngineId = selectedEngineId;
                      const previousModelId =
                        selectedModelId ?? selectedModel?.id ?? selectedModelIdRef.current;
                      const previousEffort = selectedEffort;
                      manuallyOverrodeThreadSelectionRef.current = true;
                      setHasExplicitComposerRuntime(true);
                      selectedEngineIdRef.current = engineId;
                      const runtimePatch: ThreadRuntimeSelectionPatch = {};
                      if (engineId !== previousEngineId) {
                        runtimePatch.engineId = engineId;
                      }
                      if (modelId !== previousModelId) {
                        runtimePatch.modelId = modelId;
                      }
                      if (engineId === "opencode" && planMode) {
                        setPlanMode(false);
                        runtimePatch.planMode = false;
                      }
                      if (engineId !== selectedEngineId) setSelectedEngineId(engineId);
                      const nextEngine =
                        engines.find((engine) => engine.id === engineId) ?? null;
                      const nextModel =
                        nextEngine?.models.find((model) => model.id === modelId) ?? null;
                      const nextEffort = resolveReasoningEffortForModel(
                        nextModel,
                        selectedEffortRef.current,
                      );
                      selectedModelIdRef.current = modelId;
                      setSelectedModelId(modelId);
                      if (nextEffort && nextEffort !== selectedEffort) {
                        selectedEffortRef.current = nextEffort;
                        setSelectedEffort(nextEffort);
                        if (nextEffort !== previousEffort) {
                          runtimePatch.reasoningEffort = nextEffort;
                        }
                      }
                      void saveThreadRuntimeSelectionPatch(
                        activeThread?.id ?? activeChatSessionId,
                        runtimePatch,
                      );
                      if (
                        activeWorkspace?.locationKind === "ssh" &&
                        activeThread?.workspaceId === activeWorkspaceId &&
                        activeThread.engineId === engineId
                      ) {
                        setThreadLastModelLocal(activeThread.id, modelId);
                        void ipc
                          .setSshRemoteThreadSelectedModel(activeThread.id, modelId)
                          .then((updatedThread) => {
                            if (selectedModelIdRef.current === modelId) {
                              applyThreadUpdateLocal(updatedThread);
                            }
                          })
                          .catch((error) => {
                            toast.error(String(error));
                          });
                      }
                    }}
                    onEffortChange={(effort) => void onReasoningEffortChange(effort)}
                    onServiceTierChange={(serviceTier) => {
                      const updateServiceTier = (): Promise<unknown> =>
                        onCodexConfigSave({
                          updatePersonality: false,
                          personality: null,
                          updateServiceTier: true,
                          serviceTier: serviceTier === "inherit" ? null : serviceTier,
                          updateOutputSchema: false,
                          outputSchema: null,
                          updateApprovalPolicy: false,
                          approvalPolicy: null,
                        }).catch((error) => {
                          toast.error(String(error), {
                            title: t("panel.toasts.speedChangeFailed"),
                            action: {
                              label: t("panel.toasts.retry"),
                              onClick: () => void updateServiceTier(),
                            },
                          });
                        });
                      void updateServiceTier();
                    }}
                    loading={enginesLoading}
                    error={engineLoadError}
                    onRetry={() => loadEngines(activeWorkspaceId)}
                    disabled={!activeWorkspaceId}
                  />
                </>
              )}

              {!showSpecialInputComposer && Boolean(activeThread?.id) && (
                <>
                  <div className="chat-toolbar-divider" />
                  {/*
                   * 旧实现的完整 PermissionPicker 调用保留如下，不参与编译：
                   *
                   * {!showSpecialInputComposer &&
                   *   (activeRepo ||
                   *     repos.length > 0 ||
                   *     activeThread?.engineId === "codex" ||
                   *     activeThread?.engineId === "claude" ||
                   *     activeThread?.engineId === "opencode") && (
                   *     <>
                   *       <div className="chat-toolbar-divider" />
                   *       <PermissionPicker
                   *         engineId={activeThreadAutonomyEngineId}
                   *         presetValue={activeThreadAutonomyPreset}
                   *         codexExternalSandbox={codexExternalSandboxActive}
                   *         onPresetChange={
                   *           activeThreadAutonomyEngineId ? onAutonomyPresetChange : undefined
                   *         }
                   *         defaultPreset={defaultAutonomyPreset}
                   *         onDefaultPresetChange={(preset) =>
                   *           void onDefaultAutonomyPresetChange(preset)
                   *         }
                   *         trustScopeLabel={
                   *           activeRepo
                   *             ? t("panel.repoAccess")
                   *             : repos.length > 0
                   *               ? t("panel.workspaceAccess")
                   *               : undefined
                   *         }
                   *         trustValue={
                   *           activeRepo?.trustLevel ??
                   *           (repos.length > 0 ? workspaceTrustLevel : undefined)
                   *         }
                   *         trustOptions={trustLevelOptions}
                   *         onTrustChange={
                   *           activeRepo
                   *             ? (value) => void onRepoTrustLevelChange(value)
                   *             : repos.length > 0
                   *               ? (value) => void onWorkspaceTrustLevelChange(value)
                   *               : undefined
                   *         }
                   *         customPolicyCount={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude" ||
                   *           activeThread?.engineId === "opencode"
                   *             ? threadPolicyCustomCount
                   *             : 0
                   *         }
                   *         approvalTitle={
                   *           activeThread?.engineId ? activeThreadApprovalTitle : undefined
                   *         }
                   *         approvalValue={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude" ||
                   *           activeThread?.engineId === "opencode"
                   *             ? activeThreadApprovalPolicy
                   *             : undefined
                   *         }
                   *         approvalSelectedLabel={
                   *           activeThread?.engineId === "codex"
                   *             ? activeThreadApprovalSelectedLabel
                   *             : undefined
                   *         }
                   *         approvalOptions={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude" ||
                   *           activeThread?.engineId === "opencode"
                   *             ? activeThreadApprovalOptions
                   *             : undefined
                   *         }
                   *         onApprovalChange={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude" ||
                   *           activeThread?.engineId === "opencode"
                   *             ? (value) => {
                   *                 if (activeThread?.engineId === "codex") {
                   *                   setCustomApprovalPolicyText("");
                   *                 }
                   *                 void onThreadExecutionPolicyChange({
                   *                   approvalPolicy: value as ThreadApprovalPolicyValue,
                   *                 });
                   *               }
                   *             : undefined
                   *         }
                   *         sandboxValue={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude"
                   *             ? activeThreadSandboxMode
                   *             : undefined
                   *         }
                   *         sandboxOptions={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude"
                   *             ? threadSandboxModeOptions
                   *             : undefined
                   *         }
                   *         onSandboxChange={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude"
                   *             ? (value) =>
                   *                 void onThreadExecutionPolicyChange({
                   *                   sandboxMode: value as ThreadSandboxModeValue,
                   *                 })
                   *             : undefined
                   *         }
                   *         sandboxSelectedLabel={activeThreadSandboxSelectedLabel}
                   *         sandboxNotice={activeThreadSandboxNotice}
                   *         networkValue={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude"
                   *             ? activeThreadNetworkPolicy
                   *             : undefined
                   *         }
                   *         networkOptions={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude"
                   *             ? threadNetworkPolicyOptions
                   *             : undefined
                   *         }
                   *         onNetworkChange={
                   *           activeThread?.engineId === "codex" ||
                   *           activeThread?.engineId === "claude"
                   *             ? (value) =>
                   *                 void onThreadExecutionPolicyChange({
                   *                   networkPolicy: value as ThreadNetworkPolicyValue,
                   *                 })
                   *             : undefined
                   *         }
                   *         networkDisabled={
                   *           activeThread?.engineId === "codex" &&
                   *           activeThreadSandboxMode === "danger-full-access"
                   *         }
                   *         networkNotice={
                   *           activeThread?.engineId === "codex" &&
                   *           activeThreadSandboxMode === "danger-full-access"
                   *             ? t("policy.fullAccessNotice")
                   *             : null
                   *         }
                   *       />
                   *     </>
                   *   )}
                   *
                   * 现在组件只接收完整 PermissionComponentJson 和 onChange，前端不参与 CLI 适配，
                   * 且仅在存在 AuraCoder 线程 ID 时展示。
                   */}
                  <PermissionPicker
                    disabled={!activeThread?.id}
                    value={permissionComponent}
                    onChange={(next) => void onPermissionComponentChange(next)}
                  />
                </>
              )}

              <div style={{ flex: 1 }} />

              <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
                {!showSpecialInputComposer &&
                  contextUsage?.currentTokens != null &&
                  contextUsage.maxContextTokens != null &&
                  contextUsage.contextPercent != null && (
                  <button
                    type="button"
                    className={`chat-context-ring${
                      contextUsage.contextPercent <= 10
                        ? " chat-context-ring--critical"
                        : contextUsage.contextPercent <= 25
                          ? " chat-context-ring--warning"
                          : ""
                    }`}
                    onClick={openUsageLimitsModal}
                    title={t("status.contextRingTitle", {
                      usage: formatContextUsage(contextUsage),
                    })}
                    aria-label={t("status.contextRingTitle", {
                      usage: formatContextUsage(contextUsage),
                    })}
                    aria-busy={contextUsageLoading}
                  >
                    <svg width="16" height="16" viewBox="0 0 16 16" style={{ transform: "rotate(-90deg)" }} aria-hidden="true">
                      <circle className="chat-context-ring-track" cx="8" cy="8" r="6" strokeWidth="2" />
                      <circle
                        className="chat-context-ring-value"
                        cx="8"
                        cy="8"
                        r="6"
                        strokeWidth="2"
                        strokeDasharray={2 * Math.PI * 6}
                        strokeDashoffset={
                          2 * Math.PI * 6 * (1 - Math.max(0, Math.min(100, contextUsage.contextPercent)) / 100)
                        }
                      />
                    </svg>
                    <span>{formatContextUsage(contextUsage)}</span>
                  </button>
                )}

                {streaming && !showSpecialInputComposer && (
                  <button
                    type="button"
                    className="chat-stop-btn"
                    onClick={() => void cancel()}
                    title={t("panel.stop")}
                    aria-label={t("panel.stop")}
                  >
                    <Square size={11} fill="currentColor" />
                  </button>
                )}

                {!streaming && !showSpecialInputComposer && (
                <button
                  type="submit"
                  /* className used input text only before image annotations */
                  className={`chat-send-btn${activeWorkspaceId && canSubmitComposer ? " chat-send-btn--ready" : ""}`}
                  /* disabled used input text only before image annotations */
                  disabled={!activeWorkspaceId || !canSubmitComposer || isSubmitting}
                  title={
                    isSubmitting
                      ? t("panel.sendingMessage")
                      : streaming
                        ? t("panel.sendFollowUp")
                      : sessionMessageSendMode === "flexible"
                        ? t("panel.cacheMessage")
                        : t("panel.sendMessage")
                  }
                  aria-label={
                    isSubmitting
                      ? t("panel.sendingMessage")
                      : streaming
                        ? t("panel.sendFollowUp")
                      : sessionMessageSendMode === "flexible"
                        ? t("panel.cacheMessage")
                        : t("panel.sendMessage")
                  }
                  aria-busy={isSubmitting}
                >
                  {isSubmitting ? (
                    <Loader2 size={13} className="chat-send-spinner" aria-hidden="true" />
                  ) : (
                    <Send size={13} aria-hidden="true" />
                  )}
                </button>
                )}
              </div>
            </div>
  );
}
