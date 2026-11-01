import {
  FormEvent,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
} from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  GitBranch,
  MessageSquare,
  AtSign,
  DollarSign,
  Puzzle,
  Zap,
  RotateCcw,
  Search,
  Scissors,
  Sparkles,
  Server,
  SquareCode,
  FlaskConical,
  UserCircle,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { useShallow } from "zustand/react/shallow";
import { useChatStore } from "../../stores/chatStore";
import {
  getChatComposerSessionKey,
  useChatComposerStore,
} from "../../stores/chatComposerStore";
import type {
  ChatSessionSettings,
  PendingFlexibleMessage,
} from "../../stores/chatComposerStore";
import { isMessageSendMode } from "../../lib/chatInputSettings";
import { useEngineStore } from "../../stores/engineStore";
import { useFileStore } from "../../stores/fileStore";
import { useOnboardingStore } from "../../stores/onboardingStore";
import { useThreadStore } from "../../stores/threadStore";
import { useUiStore } from "../../stores/uiStore";
import {
  buildExtensionCacheKey,
  getEffectiveExtensionItems,
  useExtensionStore,
} from "../../stores/extensionStore";
import { showWorkspaceEditorForDirectFileOpen } from "../../lib/workspacePaneNavigation";
import {
  resolveRelativePathWithinRoot,
} from "../../lib/fileRootUtils";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useGitStore } from "../../stores/gitStore";
import { useTerminalStore, type LayoutMode } from "../../stores/terminalStore";
import { toast } from "../../stores/toastStore";
import { ipc, listenExtensionCatalogUpdated } from "../../lib/ipc";
import {
  clampChatInputHeight,
  normalizeChatInputHeight,
} from "../../lib/chatInputHeight";
import { useChatInputHeightStore } from "../../stores/chatInputHeightStore";
// 斜杠菜单统一由 buildSlashCommandsFromExtensions 按 ExtensionItemDto 解析。
import { buildSlashCommandsFromExtensions } from "../../cli-tools/build-slash-commands";
import type { CliSlashCommand } from "../../cli-tools/contracts/slash-command";
import { resolvePreferredOnboardingChatSelection } from "../../lib/onboarding";
import { recordPerfMetric } from "../../lib/perfTelemetry";
import { isMacDesktop, usesCustomWindowFrame } from "../../lib/windowActions";
import { createPendingSubmissionMessage } from "./pendingSubmissionMessage";
import {
  EMPTY_CHAT_ATTACHMENTS,
  EMPTY_CHAT_INPUT_REFERENCES,
  EMPTY_CHAT_TEXT_ANNOTATIONS,
  EMPTY_PENDING_FLEXIBLE_MESSAGES,
  EMPTY_PERMISSION_COMPONENT,
} from "./chatPanelConstants";
import type {
  CodexReferenceCatalogState,
  TextAnnotationPopover,
  ThreadRuntimeSelectionPatch,
} from "./chatPanelTypes";
import { LazyTerminalPanel, LazyEditorWithExplorer } from "./lazyPanels";
import { ChatStatusBar } from "./ChatStatusBar";
import { ChatTitlebar } from "./ChatTitlebar";
import { ChatMessageArea } from "./ChatMessageArea";
import { ApprovalBanner } from "./ApprovalBanner";
import { ChatComposerInput } from "./ChatComposerInput";
import { ChatComposerToolbar } from "./ChatComposerToolbar";
import {
  canBatchApproveApproval,
  canUseApprovalDecisionActions,
  filterPendingApprovalBannerRows,
  resolvePendingToolInputApproval,
  buildPermissionApprovalResponseForEngine,
  isOpenCodeQuestionApproval,
} from "./approvalUtils";
export {
  canBatchApproveApproval,
  canUseApprovalDecisionActions,
  filterPendingApprovalBannerRows,
  resolvePendingToolInputApproval,
  buildPermissionApprovalResponseForEngine,
  isOpenCodeQuestionApproval,
} from "./approvalUtils";
import {
  blobToBase64,
  clipboardImageFiles,
  fileNameForPastedImage,
  fileNameFromPath,
  getAttachmentFilterConfig,
  getFileExtension,
  guessMimeType,
  imageExtensionForMimeType,
  isSupportedAttachmentName,
  pastedImageFileSupported,
} from "./attachmentUtils";
import {
  encodeModelOptionValue,
  formatEngineModelLabel,
  resolveClaudeModelFamily,
  serializePrettyJson,
} from "./formatters";
import {
  parseStoredOutputSchema,
  readThreadOpenCodeAgentValue,
  readThreadOutputSchemaText,
  readThreadPersonalityValue,
  readThreadServiceTierValue,
} from "./threadExecutionPolicy";
import { resolveEngineCapabilities } from "./engineCapabilities";
import { buildCodexInputItems, buildSelectedInputItems } from "./codexInputItems";
import {
  filterClassicSlashItems,
  findClassicSlashQuery,
  removeClassicSlashToken,
} from "./slashCommandMatching";
import {
  getPlanImplementationCodingMessage,
  shouldPromptToImplementPlan,
} from "./planModePrompt";
import { buildComposerRuntimeSnapshot } from "./composerRuntime";
import { canChangeUnstartedThreadEngine, collectThreadEnvironmentMismatches } from "./threadRuntimeState";
import type { ThreadEnvironmentMismatch, ThreadEnvironmentMismatchKind } from "./threadRuntimeState";
import { resolveReasoningEffortForModel } from "./reasoningEffort";
import { formatTextAnnotationsForSubmission } from "./textAnnotations";
import {
  formatImageAttachmentAnnotationsForSubmission,
  hasImageAttachmentAnnotations,
  stripImageAttachmentAnnotationsForSubmission,
} from "./imageAttachmentAnnotations";
import { ToolInputQuestionnaire } from "./ToolInputQuestionnaire";
import {
  buildPermissionsApprovalResponse,
  buildPermissionsDeclineResponse,
  isPermissionsRequestApproval,
  parseToolInputQuestions,
} from "./toolInputApproval";
import { AttachmentChip } from "./AttachmentChip";
import {
  type CodexConfigPatch,
  type CodexPersonalityValue,
  type CodexServiceTierValue,
} from "./CodexConfigPicker";
// CodexReviewPicker and CodexThreadPicker replaced by slash commands (ChatSlashMenu + ChatCommandPanel)
import { ChatCommandPanel, type ActiveSlashCommand } from "./ChatCommandPanel";
import { ConfirmDialog } from "../shared/ConfirmDialog";
import type {
  ApprovalBlock,
  ApprovalResponse,
  AttachmentBlock,
  ChatAttachment,
  ChatInputItem,
  ChatInputReference,
  ChatTextAnnotation,
  CodexApp,
  CodexPlugin,
  CodexSkill,
  EngineHealth,
  EngineModel,
  ExtensionItem,
  ExtensionProviderId,
  Message,
  OpenCodeRemoteSession,
  OpenCodeRuntimeCatalog,
  PermissionComponentJson,
  Thread,
  TrustLevel,
} from "../../types";

// 旧的经典输入菜单项目结构保留在此处作为迁移记录；现在由 CLI 菜单契约统一描述选择动作。
// type ClassicSlashCommand = SlashCommand & {
//   reference?: ChatInputReference;
//   panel?: ActiveSlashCommand;
//   insertText?: string;
// };

const ENGINE_PREWARM_THROTTLE_MS = 30_000;
const lastPrewarmAttemptAtByEngine = new Map<string, number>();
const inflightPrewarmByEngine = new Map<string, Promise<void>>();

function scheduleIdleTask(callback: () => void): () => void {
  if (typeof window !== "undefined" && typeof window.requestIdleCallback === "function") {
    const idleId = window.requestIdleCallback(() => callback(), { timeout: 600 });
    return () => window.cancelIdleCallback(idleId);
  }

  const timeoutId = window.setTimeout(callback, 120);
  return () => window.clearTimeout(timeoutId);
}

function prewarmEngineTransport(
  engineId: string,
  workspaceId: string,
): Promise<void> {
  const targetEngineKey = `${workspaceId}:${engineId}`;
  const now = Date.now();
  const lastAttemptAt = lastPrewarmAttemptAtByEngine.get(targetEngineKey) ?? 0;
  if (now - lastAttemptAt < ENGINE_PREWARM_THROTTLE_MS) {
    return Promise.resolve();
  }

  const existingTask = inflightPrewarmByEngine.get(targetEngineKey);
  if (existingTask) {
    return existingTask;
  }

  lastPrewarmAttemptAtByEngine.set(targetEngineKey, now);
  const task = ipc.prewarmEngine(engineId, workspaceId)
    .catch(() => {
      // Ignore prewarm failures; engine health/setup surfaces the actionable state.
    })
    .finally(() => {
      inflightPrewarmByEngine.delete(targetEngineKey);
    });
  inflightPrewarmByEngine.set(targetEngineKey, task);
  return task;
}











/** 普通聊天输入区拖动时必须保留的消息区最小可见高度，单位为像素。 */
const CHAT_INPUT_MIN_VISIBLE_MESSAGE_SPACE = 120;

const EMPTY_CHAT_SESSION_SETTINGS: ChatSessionSettings = {};

interface ChatPanelProps {
  embedded?: boolean;
}


export function ChatPanel({ embedded = false }: ChatPanelProps = {}) {
  const { t } = useTranslation("chat");
  const persistedChatInputHeight = useChatInputHeightStore((state) => state.chatInputHeight);
  const persistChatInputHeight = useChatInputHeightStore((state) => state.setChatInputHeight);
  const renderStartedAtRef = useRef(performance.now());
  renderStartedAtRef.current = performance.now();

  const [isSubmitting, setIsSubmitting] = useState(false);
  const [pendingSubmission, setPendingSubmission] = useState<Message | null>(null);
  const isSubmittingRef = useRef(false);
  const inputHistoryRef = useRef<string[]>([]);
  const inputHistCursorRef = useRef(-1);
  const inputLiveDraftRef = useRef("");
  const [isFileDropOver, setIsFileDropOver] = useState(false);
  const [planMode, setPlanMode] = useState(false);
  const [slashMenuOpen, setSlashMenuOpen] = useState(false);
  const [slashMenuQuery, setSlashMenuQuery] = useState("");
  const [slashMenuActiveIndex, setSlashMenuActiveIndex] = useState(0);
  const [threadMentionMenuOpen, setThreadMentionMenuOpen] = useState(false);
  const [threadMentionQuery, setThreadMentionQuery] = useState("");
  const [threadMentionActiveIndex, setThreadMentionActiveIndex] = useState(0);
  const [selectedAuraCoderThreadMention, setSelectedAuraCoderThreadMention] = useState<{
    threadId: string;
    title: string;
  } | null>(null);
  const threadMentionRangeRef = useRef<{ start: number; end: number } | null>(null);

  const [activeCommandPanel, setActiveCommandPanel] = useState<ActiveSlashCommand | null>(null);
  const [commandPanelBusy, setCommandPanelBusy] = useState(false);
  const [commandPanelError, setCommandPanelError] = useState<string | null>(null);
  const commandPanelBusyRef = useRef(false);
  // 旧逻辑默认选中 codex，会在本机未装 codex 时以不存在的 CLI 发起会话；
  // 现在初始为空，等 engines 加载后由下方 effect 校正到实际引擎。
  const [selectedEngineId, setSelectedEngineId] = useState("");
  const [selectedModelId, setSelectedModelId] = useState<string | null>(null);
  const [selectedEffort, setSelectedEffort] = useState("medium");
  const selectedEngineIdRef = useRef(selectedEngineId);
  const selectedModelIdRef = useRef<string | null>(selectedModelId);
  const selectedEffortRef = useRef(selectedEffort);
  const [codexSkills, setCodexSkills] = useState<CodexSkill[]>([]);
  const [codexPlugins, setCodexPlugins] = useState<CodexPlugin[]>([]);
  const [codexApps, setCodexApps] = useState<CodexApp[]>([]);
  const [codexReferenceCatalogState, setCodexReferenceCatalogState] =
    useState<CodexReferenceCatalogState>({
      skillsLoaded: false,
      appsLoaded: false,
    });
  const [codexReferenceCatalogLoading, setCodexReferenceCatalogLoading] = useState(false);
  const [codexReferenceCatalogError, setCodexReferenceCatalogError] =
    useState<string | null>(null);
  const [openCodeCatalog, setOpenCodeCatalog] = useState<OpenCodeRuntimeCatalog | null>(null);
  const [openCodeCatalogLoaded, setOpenCodeCatalogLoaded] = useState(false);
  const [openCodeCatalogLoading, setOpenCodeCatalogLoading] = useState(false);
  const [openCodeCatalogError, setOpenCodeCatalogError] = useState<string | null>(null);
  const openCodeCatalogRetriedAfterHealthRef = useRef<string | null>(null);
  const [cliExtensionItems, setCliExtensionItems] = useState<ExtensionItem[]>([]);
  const [selectedOpenCodeAgent, setSelectedOpenCodeAgent] = useState("build");
  const selectedOpenCodeAgentRef = useRef(selectedOpenCodeAgent);
  const [selectedPersonality, setSelectedPersonality] = useState<CodexPersonalityValue>("inherit");
  const [selectedServiceTier, setSelectedServiceTier] = useState<CodexServiceTierValue>("inherit");
  const [outputSchemaText, setOutputSchemaText] = useState("");
  const [editingThreadTitle, setEditingThreadTitle] = useState(false);
  const [threadTitleDraft, setThreadTitleDraft] = useState("");
  const [highlightedMessageId, setHighlightedMessageId] = useState<string | null>(
    null,
  );
  const {
    messages,
    status,
    hasOlderMessages,
    loadingOlderMessages,
    loadOlderMessages,
    send,
    steer,
    cancel,
    respondApproval,
    hydrateActionOutput,
    streaming,
    preparingEngineId,
    preparingAttachments,
    turnStartedAt,
    contextUsage,
    contextUsageLoading,
    usageLimits,
    usageLimitsLoading,
    error,
    setActiveThread: bindChatThread,
    threadId,
    sessionReady,
  } = useChatStore(
    useShallow((state) => ({
      messages: state.messages,
      status: state.status,
      hasOlderMessages: state.hasOlderMessages,
      loadingOlderMessages: state.loadingOlderMessages,
      loadOlderMessages: state.loadOlderMessages,
      send: state.send,
      steer: state.steer,
      cancel: state.cancel,
      respondApproval: state.respondApproval,
      hydrateActionOutput: state.hydrateActionOutput,
      streaming: state.streaming,
      preparingEngineId: state.preparingEngineId,
      preparingAttachments: state.preparingAttachments,
      turnStartedAt: state.turnStartedAt,
      contextUsage: state.contextUsage,
      contextUsageLoading: state.contextUsageLoading,
      usageLimits: state.usageLimits,
      usageLimitsLoading: state.usageLimitsLoading,
      error: state.error,
      setActiveThread: state.setActiveThread,
      threadId: state.threadId,
      /** 当前线程是否已收到 CLI 会话启动事件。 */
      sessionReady:
        state.threadId !== null && state.sessionReadyByThread[state.threadId] === true,
    })),
  );
  const messageFocusTarget = useUiStore((s) => s.messageFocusTarget);
  const clearMessageFocusTarget = useUiStore((s) => s.clearMessageFocusTarget);
  const focusMode = useUiStore((s) => s.focusMode);
  const showSidebar = useUiStore((s) => s.showSidebar);

  const isMac = isMacDesktop();
  const customWindowFrame = usesCustomWindowFrame();
  const useTitlebarSafeInset = !embedded && isMac && focusMode && !showSidebar;
  const engines = useEngineStore((s) => s.engines);
  const health = useEngineStore((s) => s.health);
  const enginesLoading = useEngineStore((s) => s.loading);
  const engineLoadError = useEngineStore((s) => s.error);
  const loadEngines = useEngineStore((s) => s.load);
  // 旧逻辑仅用于线程变化时额外检查 CLI；当前业务只调用后端 CLI 列表接口。
  // const ensureEngineHealth = useEngineStore((s) => s.ensureHealth);
  const onboardingOpen = useOnboardingStore((s) => s.open);
  const onboardingSelectedChatEngines = useOnboardingStore((s) => s.selectedChatEngines);
  const codexProtocolDiagnostics = health.codex?.protocolDiagnostics;
  const preferredOnboardingChatSelection = useMemo(
    () => resolvePreferredOnboardingChatSelection(onboardingSelectedChatEngines, engines),
    [engines, onboardingSelectedChatEngines],
  );
  const {
    activeWorkspaceId,
    activeWorkspace,
  } = useWorkspaceStore(
    useShallow((state) => ({
      activeWorkspaceId: state.activeWorkspaceId,
      activeWorkspace:
        state.workspaces.find((workspace) => workspace.id === state.activeWorkspaceId) ?? null,
    })),
  );
  const {
    activeThread,
    createThread,
    forkCodexThread,
    rollbackCodexThread,
    compactCodexThread,
    attachCodexRemoteThread,
    attachOpenCodeRemoteSession,
    refreshThreads,
    setActiveThread: setActiveThreadInStore,
    setThreadLastModelLocal,
    renameThread,
    threadsByWorkspace,
  } = useThreadStore(
    useShallow((state) => ({
      activeThread: state.threads.find((thread) => thread.id === state.activeThreadId) ?? null,
      createThread: state.createThread,
      forkCodexThread: state.forkCodexThread,
      rollbackCodexThread: state.rollbackCodexThread,
      compactCodexThread: state.compactCodexThread,
      attachCodexRemoteThread: state.attachCodexRemoteThread,
      attachOpenCodeRemoteSession: state.attachOpenCodeRemoteSession,
      refreshThreads: state.refreshThreads,
      setActiveThread: state.setActiveThread,
      setThreadLastModelLocal: state.setThreadLastModelLocal,
      renameThread: state.renameThread,
      threadsByWorkspace: state.threadsByWorkspace,
    })),
  );
  const activeChatSessionId =
    activeThread?.workspaceId === activeWorkspaceId ? activeThread.id : null;
  const composerSessionKey = activeWorkspaceId
    ? getChatComposerSessionKey(
        activeWorkspaceId,
        activeThread?.workspaceId === activeWorkspaceId ? activeThread.id : null,
      )
    : null;
  const sessionSettings = useChatComposerStore((state) =>
    composerSessionKey
      ? state.settingsBySession[composerSessionKey] ?? EMPTY_CHAT_SESSION_SETTINGS
      : EMPTY_CHAT_SESSION_SETTINGS,
  );
  const setSessionSettings = useChatComposerStore((state) => state.setSessionSettings);
  const composerDraftRevision = useChatComposerStore((state) =>
    activeWorkspaceId
      ? state.newDraftRevisionByWorkspace[activeWorkspaceId] ?? 0
      : 0,
  );
  const composerSettingsScopeKey = composerSessionKey
    ? `${composerSessionKey}:${composerSessionKey.startsWith("new:") ? composerDraftRevision : 0}`
    : null;
  const sessionSettingsRef = useRef(sessionSettings);
  sessionSettingsRef.current = sessionSettings;
  const flexibleMessageSessionKey = activeChatSessionId
    ? `thread:${activeChatSessionId}`
    : activeWorkspaceId
      ? `new:${activeWorkspaceId}`
      : null;
  useEffect(() => {
    setThreadMentionMenuOpen(false);
    setThreadMentionQuery("");
    setThreadMentionActiveIndex(0);
    setSelectedAuraCoderThreadMention(null);
    threadMentionRangeRef.current = null;
  }, [activeWorkspaceId, activeThread?.id]);
  const gitStatus = useGitStore((s) => s.status);
  /*
   * 旧项目级读取、getter 和 setter 的完整代码保留如下，不参与编译；它们已由
   * composerSessionKey 对应的会话级 API 替代，避免同一工作区不同会话共用输入内容。
   *
   * const input = useChatComposerStore((state) =>
   *   activeWorkspaceId ? state.draftByWorkspace[activeWorkspaceId] ?? "" : "",
   * );
   * const setWorkspaceDraft = useChatComposerStore((state) => state.setWorkspaceDraft);
   * const attachments = useChatComposerStore((state) =>
   *   activeWorkspaceId
   *     ? state.attachmentsByWorkspace[activeWorkspaceId] ?? EMPTY_CHAT_ATTACHMENTS
   *     : EMPTY_CHAT_ATTACHMENTS,
   * );
   * const setWorkspaceAttachments = useChatComposerStore(
   *   (state) => state.setWorkspaceAttachments,
   * );
   * const textAnnotations = useChatComposerStore((state) =>
   *   activeWorkspaceId
   *     ? state.textAnnotationsByWorkspace[activeWorkspaceId] ?? EMPTY_CHAT_TEXT_ANNOTATIONS
   *     : EMPTY_CHAT_TEXT_ANNOTATIONS,
   * );
   * const setWorkspaceTextAnnotations = useChatComposerStore(
   *   (state) => state.setWorkspaceTextAnnotations,
   * );
   * const references = useChatComposerStore((state) =>
   *   activeWorkspaceId
   *     ? state.referencesByWorkspace[activeWorkspaceId] ?? EMPTY_CHAT_INPUT_REFERENCES
   *     : EMPTY_CHAT_INPUT_REFERENCES,
   * );
   * const setWorkspaceReferences = useChatComposerStore(
   *   (state) => state.setWorkspaceReferences,
   * );
   * const setInput = useCallback(
   *   (draft: string) => {
   *     if (activeWorkspaceId) {
   *       setWorkspaceDraft(activeWorkspaceId, draft);
   *     }
   *   },
   *   [activeWorkspaceId, setWorkspaceDraft],
   * );
   * const setAttachments = useCallback(
   *   (
   *     nextAttachments:
   *       | ChatAttachment[]
   *       | ((currentAttachments: ChatAttachment[]) => ChatAttachment[]),
   *   ) => {
   *     if (!activeWorkspaceId) {
   *       return;
   *     }
   *     const currentAttachments =
   *       useChatComposerStore.getState().attachmentsByWorkspace[activeWorkspaceId] ??
   *       EMPTY_CHAT_ATTACHMENTS;
   *     setWorkspaceAttachments(
   *       activeWorkspaceId,
   *       typeof nextAttachments === "function"
   *         ? nextAttachments(currentAttachments)
   *         : nextAttachments,
   *     );
   *   },
   *   [activeWorkspaceId, setWorkspaceAttachments],
   * );
   * const setTextAnnotations = useCallback(
   *   (
   *     nextAnnotations:
   *       | ChatTextAnnotation[]
   *       | ((currentAnnotations: ChatTextAnnotation[]) => ChatTextAnnotation[]),
   *   ) => {
   *     if (!activeWorkspaceId) {
   *       return;
   *     }
   *     const currentAnnotations =
   *       useChatComposerStore.getState().textAnnotationsByWorkspace[activeWorkspaceId] ??
   *       EMPTY_CHAT_TEXT_ANNOTATIONS;
   *     setWorkspaceTextAnnotations(
   *       activeWorkspaceId,
   *       typeof nextAnnotations === "function"
   *         ? nextAnnotations(currentAnnotations)
   *         : nextAnnotations,
   *     );
   *   },
   *   [activeWorkspaceId, setWorkspaceTextAnnotations],
   * );
   * const setReferences = useCallback(
   *   (
   *     nextReferences:
   *       | ChatInputReference[]
   *       | ((currentReferences: ChatInputReference[]) => ChatInputReference[]),
   *   ) => {
   *     if (!activeWorkspaceId) {
   *       return;
   *     }
   *     const currentReferences =
   *       useChatComposerStore.getState().referencesByWorkspace[activeWorkspaceId] ??
   *       EMPTY_CHAT_INPUT_REFERENCES;
   *     setWorkspaceReferences(
   *       activeWorkspaceId,
   *       typeof nextReferences === "function"
   *         ? nextReferences(currentReferences)
   *         : nextReferences,
   *     );
   *   },
   *   [activeWorkspaceId, setWorkspaceReferences],
   * );
   */
  const input = useChatComposerStore((state) =>
    composerSessionKey ? state.draftBySession[composerSessionKey] ?? "" : "",
  );
  const setSessionDraft = useChatComposerStore((state) => state.setSessionDraft);
  const attachments = useChatComposerStore((state) =>
    composerSessionKey
      ? state.attachmentsBySession[composerSessionKey] ?? EMPTY_CHAT_ATTACHMENTS
      : EMPTY_CHAT_ATTACHMENTS,
  );
  const setSessionAttachments = useChatComposerStore(
    (state) => state.setSessionAttachments,
  );
  const textAnnotations = useChatComposerStore((state) =>
    composerSessionKey
      ? state.textAnnotationsBySession[composerSessionKey] ?? EMPTY_CHAT_TEXT_ANNOTATIONS
      : EMPTY_CHAT_TEXT_ANNOTATIONS,
  );
  const setSessionTextAnnotations = useChatComposerStore(
    (state) => state.setSessionTextAnnotations,
  );
  const references = useChatComposerStore((state) =>
    composerSessionKey
      ? state.referencesBySession[composerSessionKey] ?? EMPTY_CHAT_INPUT_REFERENCES
      : EMPTY_CHAT_INPUT_REFERENCES,
  );
  const setSessionReferences = useChatComposerStore(
    (state) => state.setSessionReferences,
  );
  const canSubmitComposer = Boolean(
    input.trim()
      || references.length > 0
      || textAnnotations.length > 0
      || hasImageAttachmentAnnotations(attachments),
  );
  const sendShortcut = useChatComposerStore((state) => state.sendShortcut);
  const chatInputMode = useChatComposerStore((state) => state.chatInputMode);
  const sessionMessageSendMode = useChatComposerStore((state) => {
    const configuredSendMethod = composerSessionKey
      ? state.settingsBySession[composerSessionKey]?.sendMethod
      : undefined;
    if (configuredSendMethod && isMessageSendMode(configuredSendMethod)) {
      return configuredSendMethod;
    }
    if (activeChatSessionId) {
      return state.messageSendModeByThread[activeChatSessionId] ?? state.messageSendMode;
    }
    if (activeWorkspaceId) {
      return (
        state.pendingMessageSendModeByWorkspace[activeWorkspaceId] ?? state.messageSendMode
      );
    }
    return state.messageSendMode;
  });
  const pendingFlexibleMessages = useChatComposerStore((state) =>
    flexibleMessageSessionKey
      ? state.pendingFlexibleMessagesBySession[flexibleMessageSessionKey] ??
        EMPTY_PENDING_FLEXIBLE_MESSAGES
      : EMPTY_PENDING_FLEXIBLE_MESSAGES,
  );
  const addPendingFlexibleMessage = useChatComposerStore(
    (state) => state.addPendingFlexibleMessage,
  );
  const removePendingFlexibleMessage = useChatComposerStore(
    (state) => state.removePendingFlexibleMessage,
  );
  const clearPendingFlexibleMessages = useChatComposerStore(
    (state) => state.clearPendingFlexibleMessages,
  );
  const setPendingMessageSendMode = useChatComposerStore(
    (state) => state.setPendingMessageSendMode,
  );
  const clearPendingMessageSendMode = useChatComposerStore(
    (state) => state.clearPendingMessageSendMode,
  );
  const setThreadMessageSendMode = useChatComposerStore(
    (state) => state.setThreadMessageSendMode,
  );
  const setComposerRuntime = useChatComposerStore((state) => state.setWorkspaceRuntime);
  const clearComposerRuntime = useChatComposerStore((state) => state.clearWorkspaceRuntime);
  const setInput = useCallback(
    (draft: string) => {
      if (composerSessionKey) {
        setSessionDraft(composerSessionKey, draft);
      }
    },
    [composerSessionKey, setSessionDraft],
  );
  const setAttachments = useCallback(
    (
      nextAttachments:
        | ChatAttachment[]
        | ((currentAttachments: ChatAttachment[]) => ChatAttachment[]),
    ) => {
      if (!composerSessionKey) {
        return;
      }

      const currentAttachments =
        useChatComposerStore.getState().attachmentsBySession[composerSessionKey] ??
        EMPTY_CHAT_ATTACHMENTS;
      setSessionAttachments(
        composerSessionKey,
        typeof nextAttachments === "function"
          ? nextAttachments(currentAttachments)
          : nextAttachments,
      );
    },
    [composerSessionKey, setSessionAttachments],
  );
  const setTextAnnotations = useCallback(
    (
      nextAnnotations:
        | ChatTextAnnotation[]
        | ((currentAnnotations: ChatTextAnnotation[]) => ChatTextAnnotation[]),
    ) => {
      if (!composerSessionKey) {
        return;
      }

      const currentAnnotations =
        useChatComposerStore.getState().textAnnotationsBySession[composerSessionKey] ??
        EMPTY_CHAT_TEXT_ANNOTATIONS;
      setSessionTextAnnotations(
        composerSessionKey,
        typeof nextAnnotations === "function"
          ? nextAnnotations(currentAnnotations)
          : nextAnnotations,
      );
    },
    [composerSessionKey, setSessionTextAnnotations],
  );
  const setReferences = useCallback(
    (
      nextReferences:
        | ChatInputReference[]
        | ((
            currentReferences: ChatInputReference[],
          ) => ChatInputReference[]),
    ) => {
      if (!composerSessionKey) {
        return;
      }

      const currentReferences =
        useChatComposerStore.getState().referencesBySession[composerSessionKey] ??
        EMPTY_CHAT_INPUT_REFERENCES;
      setSessionReferences(
        composerSessionKey,
        typeof nextReferences === "function"
          ? nextReferences(currentReferences)
          : nextReferences,
      );
    },
    [composerSessionKey, setSessionReferences],
  );
  const restoreComposerContent = useCallback(
    (
      sessionKey: string,
      draft: string,
      nextAttachments: ChatAttachment[],
      nextAnnotations: ChatTextAnnotation[],
      nextReferences: ChatInputReference[],
    ) => {
      // 失败恢复必须写回提交产生时绑定的会话，避免用户切换会话后写错目标。
      setSessionDraft(sessionKey, draft);
      setSessionAttachments(sessionKey, nextAttachments);
      setSessionTextAnnotations(sessionKey, nextAnnotations);
      setSessionReferences(sessionKey, nextReferences);
    },
    [setSessionAttachments, setSessionDraft, setSessionReferences, setSessionTextAnnotations],
  );
  /*
   * 旧项目级恢复调用完整保留如下，不参与编译；当前统一由
   * restoreComposerContent(sessionKey, ...) 按提交来源会话执行：
   *
   * if (!accepted) {
   *   setInput(submittedText);
   *   setAttachments(submittedAttachments);
   *   setTextAnnotations(submittedTextAnnotations);
   *   setReferences(submittedReferences);
   * }
   *
   * catch (error) {
   *   setInput(submittedText);
   *   setAttachments(submittedAttachments);
   *   setTextAnnotations(submittedTextAnnotations);
   *   setReferences(submittedReferences);
   *   throw error;
   * }
   *
   * if (!(await applyCodexConfigToThread(prompt.threadId, {
   *   engineId: prompt.engineId,
   *   personality: prompt.personality,
   *   serviceTier: prompt.serviceTier,
   *   outputSchemaText: prompt.outputSchemaText,
   * }))) {
   *   setInput(prompt.draftText);
   *   setAttachments(prompt.attachments);
   *   setTextAnnotations(prompt.textAnnotations);
   *   setReferences(prompt.references);
   *   return;
   * }
   * if (!(await applyOpenCodeConfigToThread(prompt.threadId, {
   *   engineId: prompt.engineId,
   *   agent: prompt.openCodeAgent,
   * }))) {
   *   setInput(prompt.draftText);
   *   setAttachments(prompt.attachments);
   *   setTextAnnotations(prompt.textAnnotations);
   *   setReferences(prompt.references);
   *   return;
   * }
   * if (!sent) {
   *   setInput(prompt.draftText);
   *   setAttachments(prompt.attachments);
   *   setTextAnnotations(prompt.textAnnotations);
   *   setReferences(prompt.references);
   *   return;
   * }
   *
   * catch {
   *   setInput(prompt.draftText);
   *   setAttachments(prompt.attachments);
   *   setTextAnnotations(prompt.textAnnotations);
   *   setReferences(prompt.references);
   * }
   *
   * pendingPlanImplementationThreadIdRef.current = promptPlanMode ? prompt.threadId : null;
   * setInput("");
   * setAttachments([]);
   * setTextAnnotations([]);
   * setReferences([]);
   *
   * if (prompt) {
   *   setInput(prompt.draftText);
   *   setAttachments(prompt.attachments);
   *   setTextAnnotations(prompt.textAnnotations);
   *   setReferences(prompt.references);
   * }
   */
  const withdrawFlexibleMessage = useCallback(
    (message: PendingFlexibleMessage) => {
      if (!activeWorkspaceId || !flexibleMessageSessionKey) {
        return;
      }

      removePendingFlexibleMessage(flexibleMessageSessionKey, message.id);
      setInput(message.text);
      setAttachments(message.attachments);
      setTextAnnotations([]);
      setReferences(message.references);
      inputRef.current?.focus();
    },
    [
      activeWorkspaceId,
      flexibleMessageSessionKey,
      removePendingFlexibleMessage,
      setAttachments,
      setInput,
      setReferences,
      setTextAnnotations,
    ],
  );
  const terminalWorkspaceState = useTerminalStore((s) =>
    activeWorkspaceId ? s.workspaces[activeWorkspaceId] : undefined,
  );
  const setLayoutMode = useTerminalStore((s) => s.setLayoutMode);
  const setTerminalPanelSize = useTerminalStore((s) => s.setPanelSize);
  const syncTerminalSessions = useTerminalStore((s) => s.syncSessions);
  const viewportRef = useRef<HTMLDivElement>(null);
  const chatSectionRef = useRef<HTMLDivElement>(null);
  const chatInputBoxRef = useRef<HTMLDivElement>(null);
  const chatInputHeightDragCleanupRef = useRef<(() => void) | null>(null);
  const chatInputHeightDragValueRef = useRef<number | null>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const textAnnotationPopoverRef = useRef<HTMLDivElement>(null);
  const annotationCommentInputRef = useRef<HTMLInputElement>(null);
  const titleInputRef = useRef<HTMLInputElement>(null);
  const effortSyncKeyRef = useRef<string | null>(null);
  // 记录普通提交开始时的会话范围，异步失败时按该范围恢复。
  const activeSubmissionSessionKeyRef = useRef<string | null>(null);
  const manuallyOverrodeThreadSelectionRef = useRef(false);
  const manualThreadBindTargetRef = useRef<string | null>(null);
  const lastSyncedThreadIdRef = useRef<string | null>(null);
  const unavailableSavedModelNoticeRef = useRef<string | null>(null);
  const highlightTimeoutRef = useRef<number | null>(null);
  const prependLoadInFlightRef = useRef(false);
  const threadActivatedAtRef = useRef(0);
  const initialScrollThreadRef = useRef<string | null>(null);
  // 旧线程运行选择串行持久化队列：
  // const threadRuntimeSelectionRequestsRef = useRef<Record<string, Promise<boolean> | undefined>>({});
  const viewportScrollTopRef = useRef(0);
  const [textAnnotationPopover, setTextAnnotationPopover] =
    useState<TextAnnotationPopover | null>(null);
  const [textAnnotationComment, setTextAnnotationComment] = useState("");
  const [autoScrollLocked, setAutoScrollLocked] = useState(false);
  const [chatInputHeightValue, setChatInputHeightValue] = useState<number | null>(
    persistedChatInputHeight,
  );
  const [hasExplicitComposerRuntime, setHasExplicitComposerRuntime] = useState(false);
  const [workspaceRootPrompt, setWorkspaceRootPrompt] = useState<any>(null);
  const [planImplementationPrompt, setPlanImplementationPrompt] = useState<{
    threadId: string;
    engineId: string;
    modelId: string;
    effort: string | null;
    personality: CodexPersonalityValue;
    serviceTier: CodexServiceTierValue;
    outputSchemaText: string;
    openCodeAgent: string;
  } | null>(null);

  useEffect(() => {
    // 会话或新草稿重置序号变化时清除只属于上一张表单的局部交互状态。
    setSlashMenuOpen(false);
    setSlashMenuQuery("");
    setSlashMenuActiveIndex(0);
    setThreadMentionMenuOpen(false);
    setThreadMentionQuery("");
    setThreadMentionActiveIndex(0);
    setSelectedAuraCoderThreadMention(null);
    threadMentionRangeRef.current = null;
    setActiveCommandPanel(null);
    setCommandPanelBusy(false);
    setCommandPanelError(null);
    setEditingThreadTitle(false);
    setThreadTitleDraft("");
    setTextAnnotationPopover(null);
    setTextAnnotationComment("");
    setPlanImplementationPrompt(null);
    setWorkspaceRootPrompt(null);
    setPlanMode(false);
    inputHistoryRef.current = [];
    inputHistCursorRef.current = -1;
    inputLiveDraftRef.current = "";
    const animationFrame = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(animationFrame);
  }, [composerSettingsScopeKey]);

  useEffect(() => {
    if (textAnnotationPopover?.stage !== "comment") {
      return;
    }
    const animationFrame = requestAnimationFrame(() => {
      annotationCommentInputRef.current?.focus();
    });
    return () => cancelAnimationFrame(animationFrame);
  }, [textAnnotationPopover?.stage]);

  const selectedEngine = useMemo(
    () => engines.find((engine) => engine.id === selectedEngineId) ?? engines[0] ?? null,
    [engines, selectedEngineId],
  );

  const availableModels = useMemo(() => selectedEngine?.models ?? [], [selectedEngine]);

  const activeModels = useMemo(
    () => availableModels.filter((m) => !m.hidden),
    [availableModels],
  );
  const codexReferenceRoot = activeWorkspace?.rootPath ?? null;
  const openCodeRuntimeRoot = codexReferenceRoot;
  const extensionProviderId: ExtensionProviderId =
    selectedEngineId === "claude"
      ? "claude"
      : selectedEngineId === "opencode"
        ? "opencode"
        : "codex";
  const extensionContext = useMemo(
    () => ({
      providerId: extensionProviderId,
      workspaceId: activeWorkspaceId,
      cwd: codexReferenceRoot,
    }),
    [activeWorkspaceId, codexReferenceRoot, extensionProviderId],
  );
  const extensionCacheKey = buildExtensionCacheKey(extensionContext);
  const extensionCatalog = useExtensionStore(
    (state) => state.entries[extensionCacheKey]?.catalog,
  );
  const loadExtensionCatalog = useExtensionStore((state) => state.loadCatalog);
  const effectiveExtensionItems = useMemo(
    () => getEffectiveExtensionItems(extensionCatalog),
    [extensionCatalog],
  );

  const legacyModels = useMemo(
    () => availableModels.filter((m) => m.hidden),
    [availableModels],
  );

  // All models from other engines, grouped by engine name
  const otherEngineGroups = useMemo(() => {
    return engines
      .filter((e) => e.id !== selectedEngineId)
        .map((engine) => ({
          label: engine.name,
          options: engine.models
            .filter((m) => !m.hidden)
            .map((model) => ({
              value: encodeModelOptionValue(engine.id, model.id),
              label: formatEngineModelLabel(t, engine.name, model.displayName),
            })),
      }))
      .filter((g) => g.options.length > 0);
  }, [engines, selectedEngineId, t]);

  const selectedModel = useMemo(
    () => availableModels.find((model) => model.id === selectedModelId) ?? availableModels[0] ?? null,
    [availableModels, selectedModelId],
  );
  const selectedClaudeWeeklyUsage = useMemo(() => {
    if (selectedEngineId !== "claude" || !usageLimits) {
      return null;
    }

    switch (resolveClaudeModelFamily(selectedModel)) {
      case "fable":
        return {
          label: t("status.windowFableWeeklyLeft"),
          percent: usageLimits.windowFableWeeklyPercent,
          resetsAt: usageLimits.windowFableWeeklyResetsAt,
        };
      case "opus":
        return {
          label: t("status.windowOpusWeeklyLeft"),
          percent: usageLimits.windowOpusWeeklyPercent,
          resetsAt: usageLimits.windowOpusWeeklyResetsAt,
        };
      case "sonnet":
        return {
          label: t("status.windowSonnetWeeklyLeft"),
          percent: usageLimits.windowSonnetWeeklyPercent,
          resetsAt: usageLimits.windowSonnetWeeklyResetsAt,
        };
      default:
        return null;
    }
  }, [selectedEngineId, selectedModel, t, usageLimits]);
  const hasUserMessage = useMemo(
    () => messages.some((message) => message.role === "user"),
    [messages],
  );
  const selectedModelSupportsPersonality = selectedEngineId === "codex" &&
    selectedModel?.supportsPersonality === true;
  const codexConfigActiveCount =
    (selectedPersonality !== "inherit" ? 1 : 0) +
    (selectedServiceTier !== "inherit" ? 1 : 0) +
    (outputSchemaText.trim().length > 0 ? 1 : 0);
  const selectedOutputSchemaValue = useMemo(() => {
    try {
      return parseStoredOutputSchema(outputSchemaText);
    } catch {
      return null;
    }
  }, [outputSchemaText]);
  /*
   * 旧逻辑只按 CLI 会话 ID或消息数锁定运行目标；该表达式由统一的空会话判断接管。
   * const activeThreadRuntimeLocked = Boolean(
   *   activeThread?.engineThreadId || (activeThread?.messageCount ?? 0) > 0,
   * );
   */
  const activeThreadRuntimeLocked =
    Boolean(activeThread) && !canChangeUnstartedThreadEngine(activeThread);
  useEffect(() => {
    selectedEngineIdRef.current = selectedEngineId;
  }, [selectedEngineId]);

  useEffect(() => {
    if (selectedEngineId === "opencode" && planMode) {
      setPlanMode(false);
    }
  }, [planMode, selectedEngineId]);

  useEffect(() => {
    selectedModelIdRef.current = selectedModelId;
  }, [selectedModelId]);

  useEffect(() => {
    selectedEffortRef.current = selectedEffort;
  }, [selectedEffort]);

  useEffect(() => {
    selectedOpenCodeAgentRef.current = selectedOpenCodeAgent;
  }, [selectedOpenCodeAgent]);
  const canSteerActiveTurn = useMemo(() => {
    if (
      !streaming ||
      !threadId ||
      !activeThread ||
      !activeWorkspaceId ||
      selectedEngineId !== "codex"
    ) {
      return false;
    }

    return (
      activeThread.id === threadId &&
      activeThread.workspaceId === activeWorkspaceId &&
      activeThread.engineId === "codex"
    );
  }, [
    activeThread,
    activeWorkspaceId,
    selectedEngineId,
    streaming,
    threadId,
  ]);
  const codexReferencesAvailable = codexSkills.length > 0 || codexApps.length > 0;
  const openCodeSelectableAgents = useMemo(
    () =>
      (openCodeCatalog?.agents ?? []).filter(
        (agent) => !agent.hidden && (agent.mode === "primary" || agent.mode === "all"),
      ),
    [openCodeCatalog?.agents],
  );

  const loadCodexReferenceCatalogs = useCallback(async (): Promise<{
    skills: CodexSkill[];
    plugins: CodexPlugin[];
    apps: CodexApp[];
    skillsLoaded: boolean;
    pluginsLoaded: boolean;
    appsLoaded: boolean;
  }> => {
    if (!codexReferenceRoot) {
      return {
        skills: [],
        plugins: [],
        apps: [],
        skillsLoaded: false,
        pluginsLoaded: false,
        appsLoaded: false,
      };
    }

    // Apps/连接器不属于 AuraCoder 管理的 Skill、Plugin、MCP 能力范围。
    // const [skillsResult, appsResult] = await Promise.allSettled([
    //   ipc.listCodexSkills(codexReferenceRoot, activeWorkspaceId),
    //   ipc.listCodexApps(activeWorkspaceId),
    // ]);
    const [skillsResult, pluginsResult] = await Promise.allSettled([
      ipc.listCodexSkills(codexReferenceRoot, activeWorkspaceId),
      ipc.listCodexPlugins(codexReferenceRoot, activeWorkspaceId),
    ]);
    const skillsLoaded = skillsResult.status === "fulfilled";
    const pluginsLoaded = pluginsResult.status === "fulfilled";
    // const appsLoaded = appsResult.status === "fulfilled";
    const appsLoaded = true;
    const skills =
      skillsResult.status === "fulfilled"
        ? skillsResult.value.filter((skill) => skill.enabled)
        : [];
    const plugins =
      pluginsResult.status === "fulfilled" ? pluginsResult.value : [];
    // const apps =
    //   appsResult.status === "fulfilled"
    //     ? appsResult.value.filter((app) => app.isEnabled && app.isAccessible)
    //     : [];
    const apps: CodexApp[] = [];

    return {
      skills,
      plugins,
      apps,
      skillsLoaded,
      pluginsLoaded,
      appsLoaded,
    };
  }, [activeWorkspaceId, codexReferenceRoot]);

  const refreshCodexReferenceCatalogs = useCallback(async (): Promise<void> => {
    const requestWorkspaceId = activeWorkspaceId;
    setCodexReferenceCatalogLoading(true);
    setCodexReferenceCatalogError(null);
    const { skills, plugins, apps, skillsLoaded, pluginsLoaded, appsLoaded } =
      await loadCodexReferenceCatalogs();
    if (useWorkspaceStore.getState().activeWorkspaceId !== requestWorkspaceId) {
      return;
    }
    if (skillsLoaded) {
      setCodexSkills(skills);
    }
    if (pluginsLoaded) {
      setCodexPlugins(plugins);
    }
    if (appsLoaded) {
      setCodexApps(apps);
    }
    setCodexReferenceCatalogState({ skillsLoaded, appsLoaded });
    // Apps/连接器的加载状态不再参与 AuraCoder 运行时能力判断。
    // if (!skillsLoaded || !appsLoaded) {
    if (!skillsLoaded || !pluginsLoaded) {
      setCodexReferenceCatalogError(
        t(
          skillsLoaded || pluginsLoaded
            ? "runtimeTarget.capabilitiesPartial"
            : "runtimeTarget.capabilitiesReadFailed",
        ),
      );
    }
    setCodexReferenceCatalogLoading(false);
  }, [activeWorkspaceId, loadCodexReferenceCatalogs, t]);

  const loadOpenCodeRuntimeCatalog = useCallback(async (): Promise<void> => {
    if (selectedEngineId !== "opencode" || !activeWorkspaceId || !openCodeRuntimeRoot) {
      setOpenCodeCatalog(null);
      setOpenCodeCatalogLoaded(false);
      setOpenCodeCatalogLoading(false);
      setOpenCodeCatalogError(null);
      return;
    }

    const requestWorkspaceId = activeWorkspaceId;
    setOpenCodeCatalog(null);
    setOpenCodeCatalogLoaded(false);
    setOpenCodeCatalogLoading(true);
    setOpenCodeCatalogError(null);
    try {
      const catalog = await ipc.getOpenCodeRuntimeCatalog(
        openCodeRuntimeRoot,
        requestWorkspaceId,
      );
      if (useWorkspaceStore.getState().activeWorkspaceId !== requestWorkspaceId) {
        return;
      }
      setOpenCodeCatalog(catalog);
      setOpenCodeCatalogLoaded(true);
    } catch (error) {
      if (useWorkspaceStore.getState().activeWorkspaceId !== requestWorkspaceId) {
        return;
      }
      setOpenCodeCatalog({ agents: [], commands: [], mcpServers: [] });
      setOpenCodeCatalogLoaded(false);
      setOpenCodeCatalogError(String(error));
      console.warn("Failed to load OpenCode runtime catalog", error);
    } finally {
      if (useWorkspaceStore.getState().activeWorkspaceId === requestWorkspaceId) {
        setOpenCodeCatalogLoading(false);
      }
    }
  }, [activeWorkspaceId, openCodeRuntimeRoot, selectedEngineId]);

  const loadCliExtensions = useCallback(async (): Promise<void> => {
    if (!selectedEngineId || !activeWorkspaceId) {
      setCliExtensionItems([]);
      return;
    }
    const requestWorkspaceId = activeWorkspaceId;
    try {
      const items = await ipc.getCliExtensions(selectedEngineId, requestWorkspaceId);
      if (useWorkspaceStore.getState().activeWorkspaceId !== requestWorkspaceId) {
        return;
      }
      setCliExtensionItems(items);
    } catch (error) {
      if (useWorkspaceStore.getState().activeWorkspaceId !== requestWorkspaceId) {
        return;
      }
      setCliExtensionItems([]);
      console.warn("Failed to load CLI extensions", error);
    }
  }, [activeWorkspaceId, selectedEngineId]);

  // 统一将聊天文字和已选引用转换为 CLI 输入项；只有 Codex 继续解析 $token。
  const resolveInputItems = useCallback(
    async (
      message: string,
      engineId: string,
      references: ChatInputReference[] = EMPTY_CHAT_INPUT_REFERENCES,
    ): Promise<ChatInputItem[] | undefined> => {
      if (engineId === "opencode") {
        return undefined;
      }

      if (references.length > 0 || chatInputMode === "classic") {
        return buildSelectedInputItems(message, references);
      }

      if (engineId !== "codex") {
        return engineId === "claude" ? [{ type: "text", text: message }] : undefined;
      }

      let skills = codexSkills;
      let apps = codexApps;
      let skillsLoaded = codexReferenceCatalogState.skillsLoaded;
      let appsLoaded = codexReferenceCatalogState.appsLoaded;
      if ((!skillsLoaded || !appsLoaded) && message.includes("$")) {
        const loaded = await loadCodexReferenceCatalogs();
        if (loaded.skillsLoaded) {
          skills = loaded.skills;
          skillsLoaded = true;
          setCodexSkills(skills);
        }
        if (loaded.appsLoaded) {
          apps = loaded.apps;
          appsLoaded = true;
          setCodexApps(apps);
        }
        setCodexReferenceCatalogState({
          skillsLoaded,
          appsLoaded,
        });
      }

      return buildCodexInputItems(message, skills, apps);
    },
    [
      chatInputMode,
      codexApps,
      codexReferenceCatalogState.appsLoaded,
      codexReferenceCatalogState.skillsLoaded,
      codexSkills,
      loadCodexReferenceCatalogs,
    ],
  );

  const supportedEfforts = useMemo(
    () => selectedModel?.supportedReasoningEfforts ?? [],
    [selectedModel],
  );
  const activeThreadInCurrentWorkspace =
    activeThread?.workspaceId === activeWorkspaceId;
  // 仅当前工作区线程的保存强度可作为当前表单的回退值。
  const activeThreadReasoningEffort =
    activeThreadInCurrentWorkspace && typeof activeThread?.reasoningEffort === "string"
      ? activeThread.reasoningEffort
      : undefined;
  const modelPickerLabel = useMemo(() => {
    return formatEngineModelLabel(t, selectedEngine?.name, selectedModel?.displayName);
  }, [t, selectedEngine?.name, selectedModel?.displayName]);
  const selectedModelOptionValue = useMemo(() => {
    if (!selectedEngineId || !selectedModelId) {
      return "";
    }
    return encodeModelOptionValue(selectedEngineId, selectedModelId);
  }, [selectedEngineId, selectedModelId]);
  const resolveComposerRuntimeSelection = useCallback(() => {
    const engineId = selectedEngineId || selectedEngineIdRef.current;
    const modelId = selectedModelId ?? selectedModel?.id ?? selectedModelIdRef.current;
    if (!engineId || !modelId) {
      return null;
    }

    const engine = engines.find((candidate) => candidate.id === engineId) ?? null;
    const model = engine?.models.find((candidate) => candidate.id === modelId) ?? null;

    return {
      engineId,
      modelId,
      reasoningEffort: resolveReasoningEffortForModel(model, selectedEffortRef.current),
    };
  }, [engines, selectedEngineId, selectedModel?.id, selectedModelId]);

  const renderAssistantIdentity = useCallback((message: Message) => {
    const messageEngineId =
      typeof message.turnEngineId === "string" && message.turnEngineId.trim()
        ? message.turnEngineId.trim()
        : activeThread?.engineId ?? selectedEngineId;
    const engineInfo =
      engines.find((engine) => engine.id === messageEngineId) ?? selectedEngine ?? null;
    const messageModelId =
      typeof message.turnModelId === "string" && message.turnModelId.trim()
        ? message.turnModelId.trim()
        : activeThread?.modelId ?? selectedModel?.id ?? null;
    const modelDisplayName = messageModelId
      ? engineInfo?.models.find((model) => model.id === messageModelId)?.displayName ?? messageModelId
      : undefined;
    const messageReasoningEffort =
      typeof message.turnReasoningEffort === "string" && message.turnReasoningEffort.trim()
        ? message.turnReasoningEffort.trim()
        : undefined;

    return {
      label: formatEngineModelLabel(
        t,
        engineInfo?.name,
        modelDisplayName,
        messageReasoningEffort,
      ),
      engineId: messageEngineId,
      engineName: engineInfo?.name ?? messageEngineId,
    };
  }, [activeThread?.engineId, activeThread?.modelId, engines, selectedEngine, selectedEngineId, selectedModel?.id, t]);

  const workspaceTrustLevel: TrustLevel = activeWorkspace?.trustLevel ?? "standard";
  const permissionComponent =
    sessionSettings.permissionMode ??
    (activeThreadInCurrentWorkspace ? activeThread?.permissionMode : null) ??
    EMPTY_PERMISSION_COMPONENT;

  /** 将未发送的运行选项和权限暂存在当前会话表单中。 */
  const saveThreadRuntimeSelectionPatch = useCallback(
    (
      threadId: string | null | undefined,
      patch: ThreadRuntimeSelectionPatch,
    ): Promise<boolean> => {
      const sessionKey = threadId ? `thread:${threadId}` : composerSessionKey;
      if (!sessionKey || Object.keys(patch).length === 0) {
        return Promise.resolve(true);
      }
      const settings =
        useChatComposerStore.getState().settingsBySession[sessionKey] ??
        EMPTY_CHAT_SESSION_SETTINGS;
      const targetThread = threadId
        ? useThreadStore.getState().threads.find((thread) => thread.id === threadId) ?? null
        : activeThreadInCurrentWorkspace
          ? activeThread
          : null;
      const settingsPatch: ChatSessionSettings = { ...patch };
      if (patch.permissionMode !== undefined) {
        const previousPermission =
          settings.permissionMode ?? targetThread?.permissionMode ?? EMPTY_PERMISSION_COMPONENT;
        const nextPermission = patch.permissionMode ?? EMPTY_PERMISSION_COMPONENT;
        settingsPatch.permissionDefaultsChanged =
          settings.permissionDefaultsChanged === true ||
          JSON.stringify(previousPermission.defaultForNewThreads) !==
            JSON.stringify(nextPermission.defaultForNewThreads);
      }
      setSessionSettings(sessionKey, settingsPatch);
      return Promise.resolve(true);
    },
    [activeThread, activeThreadInCurrentWorkspace, composerSessionKey, setSessionSettings],
  );

  /*
   * 旧线程运行选择串行持久化完整实现保留如下，不再参与编辑流程：
   * const saveThreadRuntimeSelectionPatch = useCallback(
   *   (threadId: string | null | undefined, patch: ThreadRuntimeSelectionPatch): Promise<boolean> => {
   *     if (!threadId || Object.keys(patch).length === 0) return Promise.resolve(true);
   *     const currentThread =
   *       useThreadStore.getState().threads.find((thread) => thread.id === threadId) ?? null;
   *     if (currentThread) {
   *       applyThreadUpdateLocal({
   *         ...currentThread,
   *         ...patch,
   *         engineId: (patch.engineId ?? currentThread.engineId) as Thread["engineId"],
   *         modelId: patch.modelId ?? currentThread.modelId,
   *       });
   *     }
   *     const previous =
   *       threadRuntimeSelectionRequestsRef.current[threadId] ?? Promise.resolve(true);
   *     let trackedPromise: Promise<boolean>;
   *     trackedPromise = previous.catch(() => true).then(async () => {
   *       try {
   *         const updatedThread = await ipc.updateThread({ id: threadId, ...patch });
   *         if (threadRuntimeSelectionRequestsRef.current[threadId] === trackedPromise) {
   *           applyThreadUpdateLocal(updatedThread);
   *         }
   *         return true;
   *       } catch (error) {
   *         if (threadRuntimeSelectionRequestsRef.current[threadId] === trackedPromise) {
   *           toast.error(String(error));
   *           if (currentThread) {
   *             try {
   *               await refreshThreads(currentThread.workspaceId);
   *             } catch {
   *               // 保存失败时刷新线程用于恢复服务端已存状态。
   *             }
   *           }
   *         }
   *         return false;
   *       }
   *     });
   *     threadRuntimeSelectionRequestsRef.current[threadId] = trackedPromise;
   *     void trackedPromise.then(
   *       () => {
   *         if (threadRuntimeSelectionRequestsRef.current[threadId] === trackedPromise) {
   *           delete threadRuntimeSelectionRequestsRef.current[threadId];
   *         }
   *       },
   *       () => {
   *         if (threadRuntimeSelectionRequestsRef.current[threadId] === trackedPromise) {
   *           delete threadRuntimeSelectionRequestsRef.current[threadId];
   *         }
   *       },
   *     );
   *     return trackedPromise;
   *   },
   *   [applyThreadUpdateLocal, refreshThreads],
   * );
   */

  /** 清除已提交且未在发送期间再次编辑的会话默认权限变更标记。 */
  function clearSubmittedPermissionDefaultsChange(
    sessionKey: string | null,
    submittedPermission: PermissionComponentJson | undefined,
  ) {
    if (!sessionKey || !submittedPermission) {
      return;
    }
    const currentSettings =
      useChatComposerStore.getState().settingsBySession[sessionKey] ??
      EMPTY_CHAT_SESSION_SETTINGS;
    const currentDefaultPermission = JSON.stringify(
      currentSettings.permissionMode?.defaultForNewThreads ?? [],
    );
    const submittedDefaultPermission = JSON.stringify(
      submittedPermission.defaultForNewThreads,
    );
    if (
      currentSettings.permissionDefaultsChanged === true &&
      currentDefaultPermission === submittedDefaultPermission
    ) {
      setSessionSettings(sessionKey, { permissionDefaultsChanged: false });
    }
  }

  const activeThreadEngineInfo = useMemo(
    () =>
      activeThread?.engineId
        ? engines.find((engine) => engine.id === activeThread.engineId) ?? null
        : null,
    [activeThread?.engineId, engines],
  );
  const activeThreadCapabilities = useMemo(
    () => resolveEngineCapabilities(activeThread?.engineId, activeThreadEngineInfo?.capabilities),
    [activeThread?.engineId, activeThreadEngineInfo?.capabilities],
  );
  const activeThreadApprovalDecisionCapabilities = activeThreadCapabilities.approvalDecisions;

  const pendingApprovals = useMemo<ApprovalBlock[]>(() => {
    const approvals: ApprovalBlock[] = [];
    const seen = new Set<string>();

    for (const message of messages) {
      if (message.role !== "assistant") continue;
      for (const block of message.blocks ?? []) {
        if (block.type !== "approval") continue;
        if (block.status !== "pending") continue;
        if (seen.has(block.approvalId)) continue;
        seen.add(block.approvalId);
        approvals.push(block);
      }
    }

    return approvals;
  }, [messages]);
  const [selectedPendingToolInputApprovalId, setSelectedPendingToolInputApprovalId] =
    useState<string | null>(null);

  const pendingToolInputApproval = useMemo(
    () =>
      resolvePendingToolInputApproval(
        pendingApprovals,
        activeThread?.engineId,
        selectedPendingToolInputApprovalId,
      ),
    [activeThread?.engineId, pendingApprovals, selectedPendingToolInputApprovalId],
  );
  const pendingPlanImplementationThreadIdRef = useRef<string | null>(null);
  const previousStreamingRef = useRef(false);

  useEffect(() => {
    if (!selectedPendingToolInputApprovalId) {
      return;
    }

    const selectedApprovalStillPending = pendingApprovals.some(
      (approval) => approval.approvalId === selectedPendingToolInputApprovalId,
    );
    if (!selectedApprovalStillPending) {
      setSelectedPendingToolInputApprovalId(null);
    }
  }, [pendingApprovals, selectedPendingToolInputApprovalId]);

  useEffect(() => {
    if (planImplementationPrompt && planImplementationPrompt.threadId !== threadId) {
      setPlanImplementationPrompt(null);
    }

    if (
      pendingPlanImplementationThreadIdRef.current &&
      pendingPlanImplementationThreadIdRef.current !== threadId &&
      !streaming
    ) {
      pendingPlanImplementationThreadIdRef.current = null;
    }
  }, [planImplementationPrompt, streaming, threadId]);

  useEffect(() => {
    const wasStreaming = previousStreamingRef.current;
    previousStreamingRef.current = streaming;

    const armedThreadId = pendingPlanImplementationThreadIdRef.current;
    if (
      shouldPromptToImplementPlan({
        wasStreaming,
        streaming,
        status,
        activeThreadId: threadId,
        armedThreadId,
        engineId: activeThread?.engineId,
        messages,
      })
    ) {
      const promptThreadId = threadId ?? armedThreadId;
      if (!promptThreadId) {
        pendingPlanImplementationThreadIdRef.current = null;
        return;
      }
      const promptThread =
        useThreadStore
          .getState()
          .threads.find((thread) => thread.id === promptThreadId) ??
        (activeThread?.id === promptThreadId ? activeThread : null);
      if (!promptThread) {
        pendingPlanImplementationThreadIdRef.current = null;
        return;
      }
      pendingPlanImplementationThreadIdRef.current = null;
      const promptSettings =
        useChatComposerStore.getState().settingsBySession[`thread:${promptThreadId}`] ??
        EMPTY_CHAT_SESSION_SETTINGS;
      setPlanImplementationPrompt({
        threadId: promptThreadId,
        engineId: promptThread.engineId,
        modelId: promptThread.modelId,
        effort:
          promptSettings.reasoningEffort ??
          promptThread.reasoningEffort ??
          activeThreadReasoningEffort ??
          null,
        personality:
          promptSettings.personality !== undefined
            ? (promptSettings.personality ?? "inherit") as CodexPersonalityValue
            : selectedPersonality,
        serviceTier:
          promptSettings.serviceTier !== undefined
            ? (promptSettings.serviceTier ?? "inherit") as CodexServiceTierValue
            : selectedServiceTier,
        outputSchemaText: promptSettings.outputSchemaText ?? outputSchemaText,
        openCodeAgent:
          promptSettings.openCodeAgent ?? readThreadOpenCodeAgentValue(promptThread),
      });
      return;
    }

    if (wasStreaming && !streaming && armedThreadId === threadId && status !== "completed") {
      pendingPlanImplementationThreadIdRef.current = null;
    }
  }, [
    activeThread,
    activeThreadReasoningEffort,
    messages,
    outputSchemaText,
    selectedPersonality,
    selectedServiceTier,
    status,
    streaming,
    threadId,
  ]);

  const pendingApprovalBannerRows = useMemo(
    () =>
      filterPendingApprovalBannerRows(
        pendingApprovals,
        activeThread?.engineId,
        pendingToolInputApproval?.approvalId,
      ),
    [activeThread?.engineId, pendingApprovals, pendingToolInputApproval?.approvalId],
  );
  const batchApprovableRows = useMemo(
    () =>
      pendingApprovalBannerRows.filter((approval) =>
        canBatchApproveApproval(approval, activeThread?.engineId),
      ),
    [activeThread?.engineId, pendingApprovalBannerRows],
  );
  const canTrustApprovalScope = workspaceTrustLevel !== "trusted";
  const showApprovalBannerHeader =
    pendingApprovalBannerRows.length > 1 || canTrustApprovalScope;

  const pendingToolInputQuestions = useMemo(
    () =>
      pendingToolInputApproval
        ? parseToolInputQuestions(pendingToolInputApproval.details ?? {})
        : [],
    [pendingToolInputApproval],
  );

  const showPendingToolInputComposer = Boolean(
    pendingToolInputApproval && pendingToolInputQuestions.length > 0,
  );
  const planImplementationQuestionChoiceImplement = useMemo(
    () => t("panel.planImplementationOptionImplement"),
    [t],
  );
  const planImplementationQuestionChoiceStay = useMemo(
    () => t("panel.planImplementationOptionStay"),
    [t],
  );
  const planImplementationQuestionDetails = useMemo(
    () => ({
      questions: [
        {
          id: "plan_implementation_decision",
          question: t("panel.planImplementationQuestion"),
          options: [
            {
              label: planImplementationQuestionChoiceImplement,
              description: t("panel.planImplementationOptionImplementDescription"),
              recommended: true,
            },
            {
              label: planImplementationQuestionChoiceStay,
              description: t("panel.planImplementationOptionStayDescription"),
            },
          ],
        },
      ],
    }),
    [
      planImplementationQuestionChoiceImplement,
      planImplementationQuestionChoiceStay,
      t,
    ],
  );
  const showPlanImplementationComposer = Boolean(
    planImplementationPrompt && !showPendingToolInputComposer,
  );
  const showSpecialInputComposer =
    showPendingToolInputComposer || showPlanImplementationComposer;

  /** 根据普通聊天 textarea 当前 computed 样式和布局空间计算可用高度边界。 */
  const getChatInputHeightBounds = useCallback(() => {
    const textarea = inputRef.current;
    const inputBox = chatInputBoxRef.current;
    const chatSection = chatSectionRef.current;
    if (!textarea || !inputBox || !chatSection) {
      return null;
    }

    const computedStyle = window.getComputedStyle(textarea);
    const lineHeight = Number.parseFloat(computedStyle.lineHeight);
    const paddingTop = Number.parseFloat(computedStyle.paddingTop) || 0;
    const paddingBottom = Number.parseFloat(computedStyle.paddingBottom) || 0;
    const borderTop = Number.parseFloat(computedStyle.borderTopWidth) || 0;
    const borderBottom = Number.parseFloat(computedStyle.borderBottomWidth) || 0;
    const minimumHeight = Math.ceil(
      (Number.isFinite(lineHeight) ? lineHeight : (Number.parseFloat(computedStyle.fontSize) || 13) * 1.6) +
        paddingTop +
        paddingBottom +
        borderTop +
        borderBottom,
    );
    const sectionHeight = chatSection.getBoundingClientRect().height;
    const inputBoxHeight = inputBox.getBoundingClientRect().height;
    const textareaHeight = textarea.getBoundingClientRect().height;
    const composerSurface = inputBox.closest(".chat-composer-surface");
    const composerHeight = composerSurface?.getBoundingClientRect().height ?? inputBoxHeight;
    const outsideTextareaHeight = Math.max(0, composerHeight - textareaHeight);
    const maximumHeight = Math.floor(
      sectionHeight - CHAT_INPUT_MIN_VISIBLE_MESSAGE_SPACE - outsideTextareaHeight,
    );

    return { min: minimumHeight, max: Math.max(minimumHeight, maximumHeight) };
  }, []);

  /** 清理普通聊天输入区拖动期间注册的全局鼠标和窗口事件。 */
  const cleanupChatInputHeightDrag = useCallback(() => {
    chatInputHeightDragCleanupRef.current?.();
    chatInputHeightDragCleanupRef.current = null;
    chatInputHeightDragValueRef.current = null;
  }, []);

  const scrollViewportToBottom = useCallback((behavior: ScrollBehavior = "auto") => {
    const viewport = viewportRef.current;
    if (!viewport) {
      return;
    }

    viewport.scrollTo({ top: viewport.scrollHeight, behavior });
  }, []);

  /** 启动普通聊天 textarea 顶部拖动，移动过程仅更新界面，结束时才保存数据库。 */
  const handleChatInputHeightStart = useCallback(
    (event: React.MouseEvent<HTMLDivElement>) => {
      if (showSpecialInputComposer || !inputRef.current) {
        return;
      }
      event.preventDefault();
      event.stopPropagation();
      cleanupChatInputHeightDrag();
      const startY = event.clientY;
      const startHeight = inputRef.current.getBoundingClientRect().height;
      const initialHeight = normalizeChatInputHeight(chatInputHeightValue) ?? Math.round(startHeight);
      chatInputHeightDragValueRef.current = initialHeight;
      document.body.style.userSelect = "none";

      const onMove = (moveEvent: MouseEvent) => {
        const bounds = getChatInputHeightBounds();
        if (!bounds) {
          return;
        }
        const nextHeight = clampChatInputHeight(
          initialHeight - (moveEvent.clientY - startY),
          bounds,
        );
        chatInputHeightDragValueRef.current = nextHeight;
        setChatInputHeightValue(nextHeight);
        // 输入框变高会压缩消息区可见窗口，未手动上翻时跟随到底部避免内容被遮挡
        if (!autoScrollLocked) {
          scrollViewportToBottom("auto");
        }
      };
      const onUp = () => {
        const finalHeight = chatInputHeightDragValueRef.current;
        cleanupChatInputHeightDrag();
        document.body.style.userSelect = "";
        if (finalHeight !== null) {
          void persistChatInputHeight(finalHeight).then((saved) => {
            if (!saved) {
              setChatInputHeightValue(useChatInputHeightStore.getState().chatInputHeight);
            }
          });
        }
      };
      const onBlur = () => onUp();

      document.addEventListener("mousemove", onMove);
      document.addEventListener("mouseup", onUp);
      window.addEventListener("blur", onBlur);
      chatInputHeightDragCleanupRef.current = () => {
        document.removeEventListener("mousemove", onMove);
        document.removeEventListener("mouseup", onUp);
        window.removeEventListener("blur", onBlur);
        document.body.style.userSelect = "";
      };
    },
    [
      chatInputHeightValue,
      cleanupChatInputHeightDrag,
      getChatInputHeightBounds,
      persistChatInputHeight,
      showSpecialInputComposer,
      autoScrollLocked,
      scrollViewportToBottom,
    ],
  );

  useEffect(() => {
    if (!chatInputHeightDragCleanupRef.current) {
      setChatInputHeightValue(persistedChatInputHeight);
    }
  }, [persistedChatInputHeight]);

  useEffect(() => {
    const clampRestoredHeight = () => {
      const bounds = getChatInputHeightBounds();
      if (!bounds || chatInputHeightValue === null) {
        return;
      }
      const clamped = clampChatInputHeight(chatInputHeightValue, bounds);
      if (clamped !== chatInputHeightValue) {
        setChatInputHeightValue(clamped);
      }
    };
    const frame = requestAnimationFrame(clampRestoredHeight);
    window.addEventListener("resize", clampRestoredHeight);
    return () => {
      cancelAnimationFrame(frame);
      window.removeEventListener("resize", clampRestoredHeight);
    };
  }, [
    chatInputHeightValue,
    getChatInputHeightBounds,
    showSpecialInputComposer,
    terminalWorkspaceState?.layoutMode,
  ]);

  useEffect(() => {
    return () => cleanupChatInputHeightDrag();
  }, [cleanupChatInputHeightDrag]);

  useEffect(() => {
    const currentLayoutMode = terminalWorkspaceState?.layoutMode ?? "chat";
    if (showSpecialInputComposer || (currentLayoutMode !== "chat" && currentLayoutMode !== "split")) {
      cleanupChatInputHeightDrag();
    }
  }, [
    cleanupChatInputHeightDrag,
    showSpecialInputComposer,
    terminalWorkspaceState?.layoutMode,
  ]);

  const pendingToolInputCanUseDecisionActions = canUseApprovalDecisionActions(
    activeThread?.engineId,
    pendingToolInputApproval?.details,
  );
  const pendingToolInputIsOpenCodeQuestion =
    activeThread?.engineId === "opencode" &&
    isOpenCodeQuestionApproval(pendingToolInputApproval?.details);
  const pendingToolInputSupportsDecline =
    (pendingToolInputCanUseDecisionActions || pendingToolInputIsOpenCodeQuestion) &&
    activeThreadApprovalDecisionCapabilities.includes("decline");
  const pendingToolInputSupportsCancel =
    (pendingToolInputCanUseDecisionActions || pendingToolInputIsOpenCodeQuestion) &&
    activeThreadApprovalDecisionCapabilities.includes("cancel");

  const appendAttachmentsFromPaths = useCallback((paths: string[]) => {
    if (!activeWorkspaceId || paths.length === 0) {
      return;
    }

    let nextAttachments: ChatAttachment[] = [];
    for (const rawPath of paths) {
      const normalizedPath = rawPath.trim();
      if (!normalizedPath) {
        continue;
      }
      const fileName = fileNameFromPath(normalizedPath);
      nextAttachments.push({
        id: crypto.randomUUID(),
        fileName,
        filePath: normalizedPath,
        sizeBytes: 0,
        mimeType: guessMimeType(fileName),
      });
    }

    const attachmentFilterConfig = getAttachmentFilterConfig(t, selectedEngineId, selectedModel);
    if (attachmentFilterConfig) {
      const supportedExtensions = new Set(attachmentFilterConfig.supportedExtensions);
      const supportedAttachments = nextAttachments.filter((attachment) =>
        isSupportedAttachmentName(attachment.fileName, supportedExtensions),
      );
      const skippedCount = nextAttachments.length - supportedAttachments.length;
      if (skippedCount > 0) {
        toast.warning(attachmentFilterConfig.warningMessage);
      }
      nextAttachments = supportedAttachments;
    }

    if (nextAttachments.length === 0) {
      return;
    }

    setAttachments((prev) => {
      const knownPaths = new Set(prev.map((attachment) => attachment.filePath));
      const merged = [...prev];
      for (const attachment of nextAttachments) {
        if (knownPaths.has(attachment.filePath)) {
          continue;
        }
        knownPaths.add(attachment.filePath);
        merged.push(attachment);
      }
      return merged;
    });
  }, [activeWorkspaceId, selectedEngineId, selectedModel, setAttachments, t]);

  const appendPastedImageFiles = useCallback(async (files: File[]) => {
    if (!activeWorkspaceId || files.length === 0) {
      return;
    }

    const attachmentFilterConfig = getAttachmentFilterConfig(t, selectedEngineId, selectedModel);
    if (!attachmentFilterConfig || attachmentFilterConfig.imageExtensions.length === 0) {
      toast.warning(attachmentFilterConfig?.warningMessage ?? t("attachments.pasteFailed"));
      return;
    }

    const supportedImageExtensions = new Set(attachmentFilterConfig.imageExtensions);
    const supportedFiles = files.filter((file) =>
      pastedImageFileSupported(file, supportedImageExtensions),
    );
    if (supportedFiles.length < files.length) {
      toast.warning(attachmentFilterConfig.warningMessage);
    }
    if (supportedFiles.length === 0) {
      return;
    }

    try {
      const nextAttachments = await Promise.all(
        supportedFiles.map(async (file, index) => {
          const fileName = fileNameForPastedImage(file, index);
          const mimeType = file.type || guessMimeType(fileName) || "image/png";
          const dataBase64 = await blobToBase64(file);
          const savedAttachment = await ipc.savePastedImageAttachment(fileName, mimeType, dataBase64);
          return {
            ...savedAttachment,
            id: crypto.randomUUID(),
          };
        }),
      );

      setAttachments((prev) => {
        const knownPaths = new Set(prev.map((attachment) => attachment.filePath));
        const merged = [...prev];
        for (const attachment of nextAttachments) {
          if (knownPaths.has(attachment.filePath)) {
            continue;
          }
          knownPaths.add(attachment.filePath);
          merged.push(attachment);
        }
        return merged;
      });
    } catch (error) {
      console.warn("Failed to attach pasted image", error);
      toast.warning(t("attachments.pasteFailed"));
    }
  }, [activeWorkspaceId, selectedEngineId, selectedModel, setAttachments, t]);

  const appendNativeClipboardImage = useCallback(async (): Promise<boolean> => {
    if (!activeWorkspaceId) {
      return false;
    }
    const attachmentFilterConfig = getAttachmentFilterConfig(t, selectedEngineId, selectedModel);
    if (!attachmentFilterConfig || attachmentFilterConfig.imageExtensions.length === 0) {
      return false;
    }
    try {
      const payloads = await ipc.readClipboardImage();
      if (payloads.length === 0) {
        return false;
      }
      const supportedImageExtensions = new Set(attachmentFilterConfig.imageExtensions);
      const supportedPayloads = payloads.filter((payload) => {
        const extension = getFileExtension(payload.fileName);
        return Boolean(extension && supportedImageExtensions.has(extension));
      });
      if (supportedPayloads.length === 0) {
        return false;
      }
      const nextAttachments = await Promise.all(
        supportedPayloads.map(async (payload) => {
          const savedAttachment = await ipc.savePastedImageAttachment(
            payload.fileName,
            payload.mimeType,
            payload.dataBase64,
          );
          return { ...savedAttachment, id: crypto.randomUUID() };
        }),
      );
      setAttachments((prev) => {
        const knownPaths = new Set(prev.map((attachment) => attachment.filePath));
        const merged = [...prev];
        for (const attachment of nextAttachments) {
          if (knownPaths.has(attachment.filePath)) {
            continue;
          }
          knownPaths.add(attachment.filePath);
          merged.push(attachment);
        }
        return merged;
      });
      // uri-list 兜底场景的安全网：历史实现下 WebKitGTK 的默认粘贴可能已把路径文本
      // 插入输入框，附件挂上后把已知源路径文本从输入框精确移除。
      // 当前 handleInputPaste 已完全接管默认粘贴（先 preventDefault），正常不会再有残留，
      // 此段保留作为兜底。
      const pastedPaths = supportedPayloads
        .map((payload) => payload.sourcePath)
        .filter((value): value is string => Boolean(value));
      if (pastedPaths.length > 0 && composerSessionKey) {
        const currentDraft =
          useChatComposerStore.getState().draftBySession[composerSessionKey] ?? "";
        let next = currentDraft;
        for (const pastedPath of pastedPaths) {
          for (const variant of [
            pastedPath,
            `file://${pastedPath}`,
            encodeURI(pastedPath),
            `file://${encodeURI(pastedPath)}`,
          ]) {
            next = next.replace(variant, "");
          }
        }
        if (next !== currentDraft) {
          setInput(next);
        }
      }
      return true;
    } catch (error) {
      console.warn("Failed to read native clipboard image", error);
      return false;
    }
  }, [activeWorkspaceId, selectedEngineId, selectedModel, setAttachments, composerSessionKey, setInput, t]);

  const handleInputPaste = useCallback((event: ReactClipboardEvent<HTMLElement>) => {
    if (showSpecialInputComposer) {
      return;
    }
    const imageFiles = clipboardImageFiles(event.clipboardData);
    if (imageFiles.length > 0) {
      event.preventDefault();
      void appendPastedImageFiles(imageFiles);
      return;
    }
    // 完全接管 textarea 的默认粘贴：先阻止默认插入，等原生剪贴板检查返回后——
    // 是图片（截图数据或文件管理器复制的图片文件）只挂附件，路径文本从根上不会进输入框；
    // 不是图片再把纯文本手动插入光标处，行为与默认粘贴一致。
    event.preventDefault();
    const plainText = event.clipboardData.getData("text/plain");
    const pasteTarget = event.target instanceof HTMLTextAreaElement ? event.target : inputRef.current;
    const selectionStart = pasteTarget?.selectionStart ?? null;
    const selectionEnd = pasteTarget?.selectionEnd ?? null;
    void appendNativeClipboardImage().then((attached) => {
      if (
        attached ||
        plainText.length === 0 ||
        selectionStart === null ||
        selectionEnd === null ||
        !composerSessionKey
      ) {
        return;
      }
      const currentDraft =
        useChatComposerStore.getState().draftBySession[composerSessionKey] ?? "";
      const nextDraft =
        currentDraft.slice(0, selectionStart) + plainText + currentDraft.slice(selectionEnd);
      const nextCursor = selectionStart + plainText.length;
      inputHistCursorRef.current = -1;
      setInput(nextDraft);
      handleSlashDetection(nextDraft, nextCursor);
      handleThreadMentionDetection(nextDraft, nextCursor);
      requestAnimationFrame(() => {
        const element = inputRef.current;
        if (!element) {
          return;
        }
        element.selectionStart = nextCursor;
        element.selectionEnd = nextCursor;
      });
    });
    // 旧的"按剪贴板文本启发式判断是否全是图片路径再决定拦截"方案已停用：WebKitGTK
    // 粘贴事件里剪贴板文本形态不稳定（实测启发式未命中，路径文本先插入再被异步移除，
    // 用户看到"闪一下"）。旧实现注释保留备查：
    // const clipboardText =
    //   event.clipboardData.getData("text/uri-list") ||
    //   event.clipboardData.getData("text/plain");
    // const clipboardTextLines = clipboardText
    //   .split(/\r?\n/)
    //   .map((line) => line.trim())
    //   .filter((line) => line.length > 0 && line !== "copy" && line !== "cut" && !line.startsWith("#"));
    // const clipboardTextIsImagePaths =
    //   clipboardTextLines.length > 0 &&
    //   clipboardTextLines.every((line) => {
    //     const withoutScheme = line.startsWith("file://") ? line.slice("file://".length) : line;
    //     let decodedLine = withoutScheme;
    //     try {
    //       decodedLine = decodeURIComponent(withoutScheme);
    //     } catch {
    //       // 非法编码时按原文判断。
    //     }
    //     return decodedLine.startsWith("/") && /\.(png|jpe?g|gif|webp|bmp|tiff?|svg)$/i.test(decodedLine);
    //   });
    // if (clipboardTextIsImagePaths) {
    //   event.preventDefault();
    // }
  }, [appendPastedImageFiles, appendNativeClipboardImage, showSpecialInputComposer, composerSessionKey, setInput]);

  useEffect(() => {
    const attachmentFilterConfig = getAttachmentFilterConfig(t, selectedEngineId, selectedModel);
    if (!attachmentFilterConfig) {
      return;
    }

    const supportedExtensions = new Set(attachmentFilterConfig.supportedExtensions);
    setAttachments((prev) => {
      const supportedAttachments = prev.filter((attachment) =>
        isSupportedAttachmentName(attachment.fileName, supportedExtensions),
      );
      if (supportedAttachments.length === prev.length) {
        return prev;
      }
      toast.warning(attachmentFilterConfig.warningMessage);
      return supportedAttachments;
    });
  }, [selectedEngineId, selectedModel, t]);

  const isDropPositionInsideChatSection = useCallback((x: number, y: number): boolean => {
    const container = chatSectionRef.current;
    if (!container) {
      return false;
    }
    const rect = container.getBoundingClientRect();
    return x >= rect.left && x <= rect.right && y >= rect.top && y <= rect.bottom;
  }, []);

  useEffect(() => {
    if (activeWorkspaceId) {
      void syncTerminalSessions(activeWorkspaceId);
    }
  }, [activeWorkspaceId, syncTerminalSessions]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    const bindDropListener = async () => {
      try {
        unlisten = await getCurrentWindow().onDragDropEvent((event) => {
          if (disposed) {
            return;
          }

          if (!activeWorkspaceId) {
            setIsFileDropOver(false);
            return;
          }

          if (event.payload.type === "leave") {
            setIsFileDropOver(false);
            return;
          }

          const scale = window.devicePixelRatio || 1;
          const logicalX = event.payload.position.x / scale;
          const logicalY = event.payload.position.y / scale;
          const isInsideDropArea = isDropPositionInsideChatSection(logicalX, logicalY);

          if (event.payload.type === "drop") {
            setIsFileDropOver(false);
            if (!isInsideDropArea || event.payload.paths.length === 0) {
              return;
            }
            appendAttachmentsFromPaths(event.payload.paths);
            return;
          }

          setIsFileDropOver(isInsideDropArea);
        });
      } catch (error) {
        console.debug("drag-drop listener unavailable", error);
      }
    };

    void bindDropListener();

    return () => {
      disposed = true;
      if (unlisten) {
        unlisten();
      }
    };
  }, [
    activeWorkspaceId,
    appendAttachmentsFromPaths,
    isDropPositionInsideChatSection,
  ]);

  useEffect(() => {
    if (!activeWorkspaceId) {
      setIsFileDropOver(false);
    }
  }, [activeWorkspaceId]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport) {
      return;
    }

    let rafId = 0;
    const updateScroll = () => {
      viewportScrollTopRef.current = viewport.scrollTop;
      const nearBottom =
        viewport.scrollTop + viewport.clientHeight >= viewport.scrollHeight - 120;
      const nextAutoScrollLocked = !nearBottom;
      setAutoScrollLocked((prev) =>
        prev === nextAutoScrollLocked ? prev : nextAutoScrollLocked,
      );
    };
    updateScroll();

    const onScroll = () => {
      if (rafId !== 0) {
        return;
      }
      rafId = window.requestAnimationFrame(() => {
        rafId = 0;
        updateScroll();
      });
    };

    viewport.addEventListener("scroll", onScroll, { passive: true });

    return () => {
      viewport.removeEventListener("scroll", onScroll);
      if (rafId !== 0) {
        window.cancelAnimationFrame(rafId);
      }
    };
  }, []);

  useEffect(() => {
    if (!editingThreadTitle) {
      setThreadTitleDraft(activeThread?.title ?? "");
    }
  }, [activeThread?.id, activeThread?.title, editingThreadTitle]);

  useEffect(() => {
    if (!editingThreadTitle) {
      return;
    }
    titleInputRef.current?.focus();
    titleInputRef.current?.select();
  }, [editingThreadTitle]);

  useEffect(() => {
    if (!engines.length) {
      return;
    }
    // 有真实库引擎的会话不自动校正：第一条路已把库值写上界面，
    // 是否切换为默认引擎由第二条路询问用户决定。
    // Thread.engineId 类型为 ChatEngineId，但新建会话落库前以 "unknown" 占位
    // （见 threadStore.ts UNKNOWN_ENGINE_ID），比较处按 string 宽化。
    if (activeThread && (activeThread.engineId as string) !== "unknown") {
      return;
    }
    if (!engines.some((engine) => engine.id === selectedEngineId)) {
      setSelectedEngineId(engines[0].id);
    }
  }, [engines, selectedEngineId, activeThread?.engineId]);

  useEffect(() => {
    if (
      onboardingOpen ||
      activeThread ||
      !preferredOnboardingChatSelection ||
      sessionSettingsRef.current.engineId !== undefined ||
      sessionSettingsRef.current.modelId !== undefined
    ) {
      return;
    }

    setHasExplicitComposerRuntime(false);
    setSelectedEngineId((current) =>
      current === preferredOnboardingChatSelection.engineId
        ? current
        : preferredOnboardingChatSelection.engineId,
    );
    setSelectedModelId((current) =>
      current === preferredOnboardingChatSelection.modelId
        ? current
        : preferredOnboardingChatSelection.modelId,
    );
  }, [activeThread, composerSettingsScopeKey, onboardingOpen, preferredOnboardingChatSelection]);

  useEffect(() => {
    if (!selectedModel) {
      setSelectedModelId(null);
      return;
    }
    if (selectedModelId !== selectedModel.id) {
      setSelectedModelId(selectedModel.id);
    }
  }, [selectedModel, selectedModelId]);

  /*
   * 旧逻辑在切换或新建会话时重新加载当前电脑的 CLI 工具列表：
   * useEffect(() => {
   *   if (!activeWorkspaceId) {
   *     return;
   *   }
   *   void loadEngines(activeWorkspaceId);
   * }, [activeThread?.id, activeWorkspaceId, loadEngines]);
   *
   * CLI 工具列表属于本机或 SSH 连接，不属于具体会话。切换会话时由下方同步逻辑
   * 从 activeThread 恢复该会话固定的 engineId 和 modelId，不再重复加载 CLI 列表。
   */

  /*
   * 旧逻辑在线程变化时额外执行健康检查和预热。当前业务只需要携带 workspaceId
   * 调用后端 list_actived_clis；后端从 CLI 生命周期返回已经 Ready 的 CLI。
   *
   * useEffect(() => {
   *   if (!activeWorkspaceId || engines.length === 0) {
   *     return;
   *   }
   *
   *   const engineIds = new Set<string>();
   *   if (selectedEngineId) {
   *     engineIds.add(selectedEngineId);
   *   }
   *   if (activeThread?.engineId) {
   *     engineIds.add(activeThread.engineId);
   *   }
   *
   *   for (const engineId of engineIds) {
   *     if (!engines.some((engine) => engine.id === engineId) || health[engineId]) {
   *       continue;
   *     }
   *     void ensureEngineHealth(engineId);
   *   }
   * }, [
   *   activeWorkspaceId,
   *   activeThread?.engineId,
   *   engines,
   *   ensureEngineHealth,
   *   health,
   *   selectedEngineId,
   * ]);
   *
   * useEffect(() => {
   *   if (!activeWorkspaceId || engines.length === 0) {
   *     return;
   *   }
   *
   *   const engineIds = new Set<string>();
   *   if (selectedEngineId) {
   *     engineIds.add(selectedEngineId);
   *   }
   *   if (activeThread?.engineId) {
   *     engineIds.add(activeThread.engineId);
   *   }
   *
   *   const cancelers = Array.from(engineIds)
   *     .filter((engineId) => engines.some((engine) => engine.id === engineId))
   *     .map((engineId) =>
   *       scheduleIdleTask(() => {
   *         void prewarmEngineTransport(engineId, activeWorkspaceId);
   *       }),
   *     );
   *
   *   return () => {
   *     cancelers.forEach((cancel) => cancel());
   *   };
   * }, [activeWorkspaceId, activeThread?.engineId, engines, selectedEngineId]);
   */

  useEffect(() => {
    // 旧逻辑跳过 SSH 项目的扩展目录，导致 Claude Code 等远端 CLI 没有自己的
    // Skill/MCP 菜单。目录请求现在携带 workspaceId，并由当前 CLI 解析执行目标。
    // if (activeWorkspace?.locationKind === "ssh") {
    //   return;
    // }
    void loadExtensionCatalog(extensionContext);
  }, [extensionContext, loadExtensionCatalog]);

  useEffect(() => {
    if (
      selectedEngineId !== "codex" ||
      !activeWorkspaceId ||
      !codexReferenceRoot ||
      health.codex?.available !== true
      // 阶段计划 5 前 SSH 项目不读取 Codex Skills/Apps；现在由 workspace 上下文路由远端。
      // || activeWorkspace?.locationKind === "ssh"
    ) {
      setCodexSkills([]);
      setCodexPlugins([]);
      setCodexApps([]);
      setCodexReferenceCatalogState({
        skillsLoaded: false,
        appsLoaded: false,
      });
      setCodexReferenceCatalogLoading(false);
      setCodexReferenceCatalogError(null);
      return;
    }

    setCodexSkills([]);
    setCodexPlugins([]);
    setCodexApps([]);
    setCodexReferenceCatalogState({
      skillsLoaded: false,
      appsLoaded: false,
    });
    setCodexReferenceCatalogError(null);

    // 阶段计划 5 前在这里单独维护一份 Skills/Apps 写入逻辑；现在与目标详情的
    // “刷新”按钮共用同一个目标化刷新入口。
    void refreshCodexReferenceCatalogs();
  }, [
    activeWorkspaceId,
    codexReferenceRoot,
    health.codex?.available,
    refreshCodexReferenceCatalogs,
    selectedEngineId,
  ]);

  useEffect(() => {
    // 阶段计划 5 前会在 SSH tunnel 就绪前立即查询，导致首次进入远端项目稳定失败。
    // 现在先清除上一目标的目录，等当前目标健康检查成功后再读取。
    if (selectedEngineId !== "opencode") {
      void loadOpenCodeRuntimeCatalog();
      return;
    }
    if (health.opencode?.available === true) {
      void loadOpenCodeRuntimeCatalog();
      return;
    }
    setOpenCodeCatalog(null);
    setOpenCodeCatalogLoaded(false);
    setOpenCodeCatalogLoading(false);
    setOpenCodeCatalogError(null);
  }, [health.opencode?.available, loadOpenCodeRuntimeCatalog, selectedEngineId]);

  useEffect(() => {
    if (!selectedEngineId || !activeWorkspaceId) {
      return;
    }
    void loadCliExtensions();
  }, [activeWorkspaceId, selectedEngineId, loadCliExtensions]);

  // 斜杠菜单数据（cliExtensionItems）由本组件单独持有，extensionStore 的目录事件
  // 订阅覆盖不到它。后台目录刷新会广播 extension-catalog-updated（一次刷新会发
  // 多个事件），这里只对当前引擎、当前工作区匹配的事件去抖后重拉，避免首次
  // 打开赶在后台刷新完成之前拿到空目录后，菜单在本会话内一直为空。
  const cliExtensionsReloadTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    const normalizeEventCwd = (cwd?: string | null) =>
      (cwd ?? "").trim().replace(/\\/g, "/").replace(/\/+$/, "").toLowerCase();
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    void listenExtensionCatalogUpdated((event) => {
      if (event.providerId !== extensionProviderId) {
        return;
      }
      const eventCwd = normalizeEventCwd(event.cwd);
      if (eventCwd && eventCwd !== normalizeEventCwd(codexReferenceRoot)) {
        return;
      }
      if (cliExtensionsReloadTimerRef.current) {
        clearTimeout(cliExtensionsReloadTimerRef.current);
      }
      cliExtensionsReloadTimerRef.current = setTimeout(() => {
        cliExtensionsReloadTimerRef.current = null;
        void loadCliExtensions();
      }, 400);
    })
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlisten = fn;
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
      unlisten?.();
      if (cliExtensionsReloadTimerRef.current) {
        clearTimeout(cliExtensionsReloadTimerRef.current);
        cliExtensionsReloadTimerRef.current = null;
      }
    };
  }, [codexReferenceRoot, extensionProviderId, loadCliExtensions]);

  useEffect(() => {
    if (
      selectedEngineId === "opencode" &&
      health.opencode?.available === true &&
      Boolean(openCodeCatalogError) &&
      !openCodeCatalogLoaded &&
      openCodeCatalogRetriedAfterHealthRef.current !== activeWorkspaceId
    ) {
      // 首次目录查询可能早于 SSH tunnel 就绪；健康检查成功后自动重试一次。
      openCodeCatalogRetriedAfterHealthRef.current = activeWorkspaceId;
      void loadOpenCodeRuntimeCatalog();
    }
  }, [
    activeWorkspaceId,
    health.opencode?.available,
    loadOpenCodeRuntimeCatalog,
    openCodeCatalogError,
    openCodeCatalogLoaded,
    selectedEngineId,
  ]);

  useEffect(() => {
    if (
      selectedEngineId !== "codex" ||
      !activeWorkspaceId ||
      !codexReferenceRoot ||
      !codexProtocolDiagnostics?.fetchedAt ||
      (!codexReferenceCatalogState.skillsLoaded &&
        !codexReferenceCatalogState.appsLoaded)
    ) {
      return;
    }

    let disposed = false;
    void loadCodexReferenceCatalogs().then(
      ({ skills, plugins, apps, skillsLoaded, pluginsLoaded, appsLoaded }) => {
        if (disposed) {
          return;
        }

        if (skillsLoaded) {
          setCodexSkills(skills);
        }
        if (pluginsLoaded) {
          setCodexPlugins(plugins);
        }
        if (appsLoaded) {
          setCodexApps(apps);
        }
        setCodexReferenceCatalogState((current) => ({
          skillsLoaded: current.skillsLoaded || skillsLoaded,
          appsLoaded: current.appsLoaded || appsLoaded,
        }));
      },
    );

    return () => {
      disposed = true;
    };
  }, [
    activeWorkspaceId,
    codexProtocolDiagnostics?.fetchedAt,
    codexReferenceCatalogState.appsLoaded,
    codexReferenceCatalogState.skillsLoaded,
    codexReferenceRoot,
    loadCodexReferenceCatalogs,
    selectedEngineId,
  ]);

  useEffect(() => {
    const syncKey = `${composerSettingsScopeKey ?? "none"}:${selectedModel?.id ?? "none"}`;
    if (effortSyncKeyRef.current === syncKey) {
      return;
    }
    effortSyncKeyRef.current = syncKey;

    // 第一条路：threads 表 reasoning_effort 有值就原样显示（是否被当前环境支持由
    // 第二条路比对后询问用户）；为空时回退当前模型默认强度，不沿用上个会话残留值。
    /*
     * 旧实现：库字段为空时沿用输入框残留值，且会把不被当前模型支持的库值静默钳制为默认：
     * const nextEffort = resolveReasoningEffortForModel(
     *   selectedModel,
     *   activeThreadReasoningEffort ?? selectedEffort,
     * );
     */
    const configuredEffort = sessionSettingsRef.current.reasoningEffort;
    const nextEffort =
      (typeof configuredEffort === "string" && configuredEffort) ||
      activeThreadReasoningEffort ||
      resolveReasoningEffortForModel(selectedModel, null);

    if (nextEffort && selectedEffort !== nextEffort) {
      setSelectedEffort(nextEffort);
    }
  }, [
    activeThread?.id,
    activeThreadReasoningEffort,
    composerSettingsScopeKey,
    selectedModel?.id,
    selectedModel?.defaultReasoningEffort,
    selectedEffort,
    supportedEfforts,
  ]);

  // 切换会话时优先恢复未发送的表单选择，再读取已保存线程字段和前端默认值。
  const lastSelectionSyncScopeRef = useRef<string | null>(null);
  useEffect(() => {
    if (lastSelectionSyncScopeRef.current === composerSettingsScopeKey) {
      return;
    }
    lastSelectionSyncScopeRef.current = composerSettingsScopeKey;
    const settings = sessionSettingsRef.current;
    const restoredEngineId =
      settings.engineId ??
      (activeThreadInCurrentWorkspace ? activeThread?.engineId : null) ??
      preferredOnboardingChatSelection?.engineId ??
      engines[0]?.id ??
      "";
    if (typeof settings.planMode === "boolean") {
      setPlanMode(restoredEngineId === "opencode" ? false : settings.planMode);
    } else if (typeof activeThread?.planMode === "boolean") {
      setPlanMode(activeThread.engineId === "opencode" ? false : activeThread.planMode);
    } else {
      setPlanMode(false);
    }
    if (
      settings.sendMethod === undefined &&
      typeof activeThread?.sendMethod === "string" &&
      isMessageSendMode(activeThread.sendMethod)
    ) {
      setThreadMessageSendMode(activeThread.id, activeThread.sendMethod);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    activeThread?.id,
    activeThread?.engineId,
    activeThreadInCurrentWorkspace,
    composerSettingsScopeKey,
    engines,
    preferredOnboardingChatSelection,
  ]);

  const composerRuntimeSnapshot = useMemo(
    () =>
      buildComposerRuntimeSnapshot({
        hasActiveThread: activeThreadInCurrentWorkspace,
        hasExplicitOverride: hasExplicitComposerRuntime,
        selectedEngineId,
        selectedModel,
        selectedEffort,
        selectedServiceTier,
      }),
    [
      activeThreadInCurrentWorkspace,
      hasExplicitComposerRuntime,
      selectedEffort,
      selectedEngineId,
      selectedModel,
      selectedServiceTier,
    ],
  );

  useEffect(() => {
    if (!activeWorkspaceId) {
      return;
    }

    if (!composerRuntimeSnapshot) {
      clearComposerRuntime(activeWorkspaceId);
      return;
    }

    setComposerRuntime(activeWorkspaceId, composerRuntimeSnapshot);
  }, [
    activeWorkspaceId,
    clearComposerRuntime,
    composerRuntimeSnapshot,
    setComposerRuntime,
  ]);

  useEffect(() => {
    if (
      activeThreadInCurrentWorkspace &&
      activeThread?.engineId !== "codex" &&
      selectedServiceTier !== "inherit" &&
      sessionSettingsRef.current.serviceTier === undefined
    ) {
      setSelectedServiceTier("inherit");
    }
  }, [activeThread?.engineId, activeThreadInCurrentWorkspace, selectedServiceTier]);

  useEffect(() => {
    const settings = sessionSettingsRef.current;
    const savedCodexThread =
      activeThreadInCurrentWorkspace && activeThread?.engineId === "codex"
        ? activeThread
        : null;
    setSelectedPersonality(
      settings.personality !== undefined
        ? (settings.personality ?? "inherit") as CodexPersonalityValue
        : savedCodexThread
          ? readThreadPersonalityValue(savedCodexThread)
          : "inherit",
    );
    setSelectedServiceTier(
      settings.serviceTier !== undefined
        ? (settings.serviceTier ?? "inherit") as CodexServiceTierValue
        : savedCodexThread
          ? readThreadServiceTierValue(savedCodexThread)
          : "inherit",
    );
    setOutputSchemaText(
      settings.outputSchemaText !== undefined
        ? settings.outputSchemaText
        : savedCodexThread
          ? readThreadOutputSchemaText(savedCodexThread)
          : "",
    );
  }, [
    activeThread?.engineId,
    activeThread?.id,
    activeThread?.engineMetadata,
    activeThreadInCurrentWorkspace,
    composerSettingsScopeKey,
  ]);

  useEffect(() => {
    const settings = sessionSettingsRef.current;
    const savedOpenCodeThread =
      activeThreadInCurrentWorkspace && activeThread?.engineId === "opencode"
        ? activeThread
        : null;
    setSelectedOpenCodeAgent(
      settings.openCodeAgent !== undefined
        ? settings.openCodeAgent
        : savedOpenCodeThread
          ? readThreadOpenCodeAgentValue(savedOpenCodeThread)
          : "build",
    );
  }, [
    activeThread?.engineId,
    activeThread?.id,
    activeThread?.engineMetadata,
    activeThreadInCurrentWorkspace,
    composerSettingsScopeKey,
  ]);

  useEffect(() => {
    if (lastSyncedThreadIdRef.current === composerSettingsScopeKey) {
      return;
    }
    lastSyncedThreadIdRef.current = composerSettingsScopeKey;
    manuallyOverrodeThreadSelectionRef.current = false;
    setHasExplicitComposerRuntime(false);
    // 新建会话落库时引擎以 unknown 占位，等首发消息才确定真实引擎；占位值不在
    // 已登记 engines 列表里，回写会被上方校正 effect 反复改回，形成无限 ping-pong
    // （logo/文案高频闪烁）。引擎未落定前不回写，由校正 effect 维持可用引擎，
    // 首发消息落库真实引擎后这里自然接管。
    /*
     * 停用原因：按两条路架构，第一条路直接回写 threads 表值，环境不一致的切换
     * 由第二条路询问接管（collectThreadEnvironmentMismatches + ConfirmDialog）。
     * 旧实现把引擎回写、模型存在性判断、本地静默 fallback、SSH toast+落库自动切换
     * 揉在会话同步里，未经用户确认就改动界面并落库，且依赖环境列表就绪。
     * if (
     *   activeThread.engineId !== selectedEngineId &&
     *   engines.some((engine) => engine.id === activeThread.engineId)
     * ) {
     *   setSelectedEngineId(activeThread.engineId);
     * }
     * const threadEngine =
     *   engines.find((engine) => engine.id === activeThread.engineId) ?? null;
     * const preferredModelId = activeThread.modelId;
     * const preferredModelExists =
     *   threadEngine?.models.some((model) => model.id === preferredModelId) ?? false;
     * const threadModelExists =
     *   threadEngine?.models.some((model) => model.id === activeThread.modelId) ?? false;
     * if (preferredModelExists) {
     *   setSelectedModelId(preferredModelId);
     * } else if (threadModelExists) {
     *   setSelectedModelId(activeThread.modelId);
     * } else if (threadEngine && threadEngine.models.length > 0) {
     *   const fallbackModel =
     *     threadEngine.models.find((model) => model.isDefault && !model.hidden) ??
     *     threadEngine.models.find((model) => !model.hidden) ??
     *     threadEngine.models[0];
     *   setSelectedModelId(fallbackModel.id);
     *   if (activeWorkspace?.locationKind === "ssh") {
     *     const noticeKey = `${activeThread.id}:${preferredModelId}:${fallbackModel.id}`;
     *     if (unavailableSavedModelNoticeRef.current !== noticeKey) {
     *       unavailableSavedModelNoticeRef.current = noticeKey;
     *       toast.warning(
     *         t("modelPicker.savedModelUnavailable", {
     *           previous: preferredModelId,
     *           fallback: fallbackModel.displayName,
     *         }),
     *       );
     *     }
     *     setThreadLastModelLocal(activeThread.id, fallbackModel.id);
     *     void ipc
     *       .setSshRemoteThreadSelectedModel(activeThread.id, fallbackModel.id)
     *       .then((updatedThread) => applyThreadUpdateLocal(updatedThread))
     *       .catch((error) => {
     *         console.warn("Failed to persist SSH fallback model", error);
     *       });
     *   }
     * }
     */
    // 第一条路：点击会话（含启动默认选中）直接取 threads 表显示，与 CLI 环境加载无关。
    // unknown 是新会话落库前的占位值，不回写；环境不一致的切换由第二条路询问接管。
    // Thread.engineId 类型为 ChatEngineId，运行时为 unknown 占位（见 threadStore.ts
    // UNKNOWN_ENGINE_ID），比较处按 string 宽化。
    const settings = sessionSettingsRef.current;
    const savedThread = activeThreadInCurrentWorkspace ? activeThread : null;
    const savedThreadHasKnownEngine =
      Boolean(savedThread) && (savedThread!.engineId as string) !== "unknown";
    const defaultEngineId =
      preferredOnboardingChatSelection?.engineId ?? engines[0]?.id ?? "";
    const nextEngineId =
      settings.engineId !== undefined
        ? settings.engineId ?? defaultEngineId
        : savedThreadHasKnownEngine
          ? savedThread!.engineId
          : defaultEngineId;
    if (nextEngineId && nextEngineId !== selectedEngineId) {
      setSelectedEngineId(nextEngineId);
    }

    const nextEngine = engines.find((engine) => engine.id === nextEngineId) ?? null;
    const defaultModelId =
      preferredOnboardingChatSelection?.engineId === nextEngineId
        ? preferredOnboardingChatSelection.modelId
        : nextEngine?.models.find((model) => model.isDefault && !model.hidden)?.id ??
          nextEngine?.models.find((model) => !model.hidden)?.id ??
          nextEngine?.models[0]?.id ??
          null;
    const nextModelId =
      settings.modelId !== undefined
        ? settings.modelId
        : savedThreadHasKnownEngine && savedThread!.modelId !== "unknown"
          ? savedThread!.modelId
          : defaultModelId;
    if (nextModelId !== selectedModelId) {
      setSelectedModelId(nextModelId);
    }
  }, [
    activeThread?.id,
    activeThread?.engineId,
    activeThread?.modelId,
    activeThread?.engineMetadata,
    activeThreadInCurrentWorkspace,
    composerSettingsScopeKey,
    engines,
    preferredOnboardingChatSelection,
  ]);

  // 第二条路：CLI 环境加载完成后与数据库比对，不一致时询问用户是否切换为默认。
  // 用户确认前界面与数据库都保持原值；同一会话同一字段只询问一次。
  const [envMismatchPrompt, setEnvMismatchPrompt] = useState<{
    threadId: string;
    mismatch: ThreadEnvironmentMismatch;
  } | null>(null);
  const envMismatchHandledRef = useRef<Map<string, Set<ThreadEnvironmentMismatchKind>>>(new Map());

  useEffect(() => {
    if (envMismatchPrompt && envMismatchPrompt.threadId !== activeThread?.id) {
      setEnvMismatchPrompt(null);
    }
    if (!activeThread || engines.length === 0 || envMismatchPrompt) {
      return;
    }
    const handled = envMismatchHandledRef.current.get(activeThread.id);
    const mismatch = collectThreadEnvironmentMismatches({
      engineId: activeThread.engineId,
      modelId: activeThread.modelId,
      reasoningEffort: activeThread.reasoningEffort,
      engines,
    }).find((item) => !handled?.has(item.kind));
    if (mismatch) {
      setEnvMismatchPrompt({ threadId: activeThread.id, mismatch });
    }
  }, [
    activeThread?.id,
    activeThread?.engineId,
    activeThread?.modelId,
    activeThread?.reasoningEffort,
    engines,
    envMismatchPrompt,
  ]);

  const markEnvMismatchHandled = (threadId: string, kind: ThreadEnvironmentMismatchKind) => {
    const handled = envMismatchHandledRef.current.get(threadId) ?? new Set<ThreadEnvironmentMismatchKind>();
    handled.add(kind);
    envMismatchHandledRef.current.set(threadId, handled);
  };

  // 用户确认：切换到当前运行环境默认值，并暂存到会话表单。
  const handleEnvMismatchConfirm = () => {
    const prompt = envMismatchPrompt;
    if (!prompt) {
      return;
    }
    markEnvMismatchHandled(prompt.threadId, prompt.mismatch.kind);
    const { kind, fallbackValue } = prompt.mismatch;
    if (kind === "engine") {
      setSelectedEngineId(fallbackValue);
      void saveThreadRuntimeSelectionPatch(prompt.threadId, { engineId: fallbackValue });
    } else if (kind === "model") {
      setSelectedModelId(fallbackValue);
      selectedModelIdRef.current = fallbackValue;
      void saveThreadRuntimeSelectionPatch(prompt.threadId, { modelId: fallbackValue });
    } else {
      setSelectedEffort(fallbackValue);
      void saveThreadRuntimeSelectionPatch(prompt.threadId, { reasoningEffort: fallbackValue });
    }
    setEnvMismatchPrompt(null);
  };

  // 用户拒绝：保持数据库的值和界面显示不动，仅标记该字段不再询问。
  const handleEnvMismatchCancel = () => {
    const prompt = envMismatchPrompt;
    if (prompt) {
      markEnvMismatchHandled(prompt.threadId, prompt.mismatch.kind);
    }
    setEnvMismatchPrompt(null);
  };

  useEffect(() => {
    if (!activeWorkspaceId) {
      if (threadId !== null) {
        setActiveThreadInStore(null);
        void bindChatThread(null);
      }
      return;
    }

    const activeThreadInCurrentWorkspace =
      activeThread &&
      activeThread.workspaceId === activeWorkspaceId;

    const targetThreadId = activeThreadInCurrentWorkspace ? activeThread.id : null;
    if (targetThreadId === threadId) {
      return;
    }

    if (!activeThreadInCurrentWorkspace) {
      setActiveThreadInStore(null);
    }
    if (targetThreadId && manualThreadBindTargetRef.current === targetThreadId) {
      return;
    }
    void bindChatThread(targetThreadId);
  }, [
    activeWorkspaceId,
    activeThread?.id,
    activeThread?.workspaceId,
    threadId,
    bindChatThread,
    setActiveThreadInStore,
  ]);

  /** 当前流式文本完成 DOM 提交后，在未锁定时跟随消息视口到底部。 */
  const handleStreamingTextContentCommitted = useCallback(() => {
    if (autoScrollLocked) {
      return;
    }
    scrollViewportToBottom("auto");
  }, [autoScrollLocked, scrollViewportToBottom]);

  useEffect(() => {
    threadActivatedAtRef.current = performance.now();
    prependLoadInFlightRef.current = false;
  }, [threadId]);

  useEffect(() => {
    if (!threadId) {
      initialScrollThreadRef.current = null;
      setAutoScrollLocked(false);
      return;
    }

    if (messages.length === 0) {
      return;
    }

    if (initialScrollThreadRef.current === threadId) {
      return;
    }

    if (messageFocusTarget?.threadId === threadId) {
      return;
    }

    initialScrollThreadRef.current = threadId;

    let raf2 = 0;
    const raf1 = window.requestAnimationFrame(() => {
      scrollViewportToBottom("auto");
      raf2 = window.requestAnimationFrame(() => {
        scrollViewportToBottom("auto");
      });
    });

    return () => {
      window.cancelAnimationFrame(raf1);
      if (raf2 !== 0) {
        window.cancelAnimationFrame(raf2);
      }
    };
  }, [threadId, messages.length, messageFocusTarget?.threadId, scrollViewportToBottom]);

  /*
   * 已停用：messages 变化只代表远端 TextDelta 到达，不能作为消息 DOM 已提交的滚动触发信号。
   * useEffect(() => {
   *   const viewport = viewportRef.current;
   *   if (!viewport) return;
   *   if (!autoScrollLocked) {
   *     scrollViewportToBottom("smooth");
   *   }
   * }, [messages, autoScrollLocked, scrollViewportToBottom]);
   */

  useEffect(() => {
    if (!pendingSubmission || autoScrollLocked) {
      return;
    }
    scrollViewportToBottom("auto");
  }, [autoScrollLocked, pendingSubmission, scrollViewportToBottom]);

  const hasPendingFlexibleMessages = pendingFlexibleMessages.length > 0;
  const flexibleMessageConfirmDisabled =
    isSubmitting || (streaming && !canSteerActiveTurn);
  const flexibleMessageConfirmTitle = isSubmitting
    ? t("panel.sendingMessage")
    : streaming && !canSteerActiveTurn
      ? t("panel.flexibleMessageGroup.waitForResponse")
      : t("panel.flexibleMessageGroup.confirmDescription");

  useEffect(() => {
    if (!hasPendingFlexibleMessages || autoScrollLocked) {
      return;
    }
    scrollViewportToBottom("auto");
  }, [autoScrollLocked, hasPendingFlexibleMessages, pendingFlexibleMessages, scrollViewportToBottom]);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport || !threadId || !hasOlderMessages || loadingOlderMessages) {
      return;
    }
    if (performance.now() - threadActivatedAtRef.current < 700) {
      return;
    }
    if (viewportScrollTopRef.current > 80 || prependLoadInFlightRef.current) {
      return;
    }

    prependLoadInFlightRef.current = true;
    const previousScrollHeight = viewport.scrollHeight;
    void loadOlderMessages()
      .then(() => {
        window.requestAnimationFrame(() => {
          const latestViewport = viewportRef.current;
          if (!latestViewport) {
            return;
          }
          const nextScrollHeight = latestViewport.scrollHeight;
          const delta = nextScrollHeight - previousScrollHeight;
          if (delta > 0) {
            latestViewport.scrollTop = latestViewport.scrollTop + delta;
          }
        });
      })
      .finally(() => {
        prependLoadInFlightRef.current = false;
      });
  }, [
    hasOlderMessages,
    loadOlderMessages,
    loadingOlderMessages,
    threadId,
  ]);

  useEffect(() => {
    if (!messageFocusTarget) {
      return;
    }
    if (messageFocusTarget.threadId !== threadId) {
      return;
    }

    const targetIndex = messages.findIndex(
      (message) => message.id === messageFocusTarget.messageId,
    );
    if (targetIndex < 0) {
      if (hasOlderMessages && !loadingOlderMessages && !prependLoadInFlightRef.current) {
        prependLoadInFlightRef.current = true;
        void loadOlderMessages().finally(() => {
          prependLoadInFlightRef.current = false;
        });
      }
      return;
    }

    const viewport = viewportRef.current;
    if (!viewport) {
      return;
    }

    const targetMessageId = messages[targetIndex].id;
    const targetElement = viewport.querySelector<HTMLElement>(
      `[data-message-id="${targetMessageId}"]`,
    );
    if (targetElement) {
      targetElement.scrollIntoView({ block: "center", behavior: "smooth" });
    }
    setHighlightedMessageId(targetMessageId);

    if (highlightTimeoutRef.current !== null) {
      window.clearTimeout(highlightTimeoutRef.current);
    }
    highlightTimeoutRef.current = window.setTimeout(() => {
      setHighlightedMessageId((current) =>
        current === targetMessageId ? null : current,
      );
      highlightTimeoutRef.current = null;
    }, 2400);

    clearMessageFocusTarget();
  }, [
    clearMessageFocusTarget,
    hasOlderMessages,
    loadOlderMessages,
    loadingOlderMessages,
    messageFocusTarget,
    messages,
    threadId,
  ]);

  useEffect(() => {
    return () => {
      if (highlightTimeoutRef.current !== null) {
        window.clearTimeout(highlightTimeoutRef.current);
      }
    };
  }, []);

  useEffect(() => {
    setHighlightedMessageId(null);
  }, [activeThread?.id]);

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if ((e.metaKey || e.ctrlKey) && e.key === ".") {
        e.preventDefault();
        void cancel();
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [cancel]);

  /** 验证 Codex 输出结构草稿并返回可保存值。 */
  function parseOutputSchemaDraft(
    draftText: string = outputSchemaText,
  ): { ok: true; value: unknown | null } | { ok: false } {
    try {
      return { ok: true, value: parseStoredOutputSchema(draftText) };
    } catch (error) {
      toast.error(
        t("configPicker.invalidOutputSchema", {
          error: error instanceof Error ? error.message : String(error),
        }),
      );
      return { ok: false };
    }
  }

  /** 在当前会话表单中暂存 Codex 的人格、服务档位和输出结构配置。 */
  async function onCodexConfigSave(patch: CodexConfigPatch) {
    const nextPersonality =
      (patch.updatePersonality
        ? (patch.personality ?? "inherit")
        : selectedPersonality) as CodexPersonalityValue;
    const nextServiceTier =
      (patch.updateServiceTier
        ? (patch.serviceTier ?? "inherit")
        : selectedServiceTier) as CodexServiceTierValue;
    const nextOutputSchemaText = patch.updateOutputSchema
      ? serializePrettyJson(patch.outputSchema)
      : outputSchemaText;
    const applyLocalState = () => {
      if (patch.updateServiceTier) {
        setHasExplicitComposerRuntime(true);
      }
      setSelectedPersonality(nextPersonality);
      setSelectedServiceTier(nextServiceTier);
      setOutputSchemaText(nextOutputSchemaText);
    };

    applyLocalState();
    if (composerSessionKey) {
      setSessionSettings(composerSessionKey, {
        personality: nextPersonality,
        serviceTier: nextServiceTier,
        outputSchemaText: nextOutputSchemaText,
      });
    }
  }

  /** 将当前 Codex 非权限配置应用到目标线程。 */
  async function applyCodexConfigToThread(
    targetThreadId: string,
    config?: {
      engineId: string;
      personality: CodexPersonalityValue;
      serviceTier: CodexServiceTierValue;
      outputSchemaText: string;
    },
  ): Promise<boolean> {
    const effectiveEngineId = config?.engineId ?? selectedEngineId;
    if (effectiveEngineId !== "codex") {
      return true;
    }

    const effectiveConfig = config ?? {
      engineId: selectedEngineId,
      personality: selectedPersonality,
      serviceTier: selectedServiceTier,
      outputSchemaText,
    };

    const outputSchemaDraft = parseOutputSchemaDraft(effectiveConfig.outputSchemaText);
    if (!outputSchemaDraft.ok) {
      return false;
    }
    setSessionSettings(`thread:${targetThreadId}`, {
      personality: effectiveConfig.personality,
      serviceTier: effectiveConfig.serviceTier,
      outputSchemaText: effectiveConfig.outputSchemaText,
    });
    return true;
  }

  /** 在当前会话表单中暂存 OpenCode Agent 配置。 */
  async function onOpenCodeAgentChange(agent: string) {
    setSelectedOpenCodeAgent(agent);
    selectedOpenCodeAgentRef.current = agent;
    setHasExplicitComposerRuntime(true);
    if (composerSessionKey) {
      setSessionSettings(composerSessionKey, { openCodeAgent: agent });
    }
  }

  async function applyOpenCodeConfigToThread(
    targetThreadId: string,
    config?: {
      engineId: string;
      agent: string;
    },
  ): Promise<boolean> {
    const effectiveEngineId = config?.engineId ?? selectedEngineId;
    if (effectiveEngineId !== "opencode") {
      return true;
    }

    const agent = config?.agent ?? selectedOpenCodeAgentRef.current;
    setSessionSettings(`thread:${targetThreadId}`, { openCodeAgent: agent });
    return true;
  }

  async function onForkCodexThread() {
    if (!activeThread || activeThread.engineId !== "codex") {
      throw new Error(t("panel.toasts.codexThreadToolUnavailable"));
    }

    const forkedThread = await forkCodexThread(activeThread.id);
    if (!forkedThread) {
      throw new Error(t("panel.toasts.codexThreadForkFailed"));
    }

    setActiveThreadInStore(forkedThread.id);
    await bindChatThread(forkedThread.id);
    toast.success(t("panel.toasts.codexThreadForked"));
  }

  async function onStartCodexReview(request: {
    target: import("../../types").CodexReviewTarget;
    delivery: import("../../types").CodexReviewDelivery;
  }) {
    if (
      !activeThread ||
      activeThread.engineId !== "codex" ||
      !activeThread.engineThreadId
    ) {
      throw new Error(t("panel.toasts.codexReviewUnavailable"));
    }

    const reviewThread = await ipc.startCodexReview(
      activeThread.id,
      request.target,
      request.delivery,
    );

    await refreshThreads(reviewThread.workspaceId);
    setActiveThreadInStore(reviewThread.id);
    await bindChatThread(reviewThread.id);
    toast.success(
      t(
        request.delivery === "detached"
          ? "panel.toasts.codexReviewDetachedStarted"
          : "panel.toasts.codexReviewStarted",
      ),
    );
  }

  async function onRollbackCodexThread(numTurns: number) {
    if (!activeThread || activeThread.engineId !== "codex") {
      throw new Error(t("panel.toasts.codexThreadToolUnavailable"));
    }

    const rolledBackThread = await rollbackCodexThread(activeThread.id, numTurns);
    if (!rolledBackThread) {
      throw new Error(t("panel.toasts.codexThreadRollbackFailed"));
    }

    setActiveThreadInStore(rolledBackThread.id);
    await bindChatThread(rolledBackThread.id);
    toast.success(t("panel.toasts.codexThreadRolledBack", { count: numTurns }));
  }

  async function onCompactCodexThread() {
    if (!activeThread || activeThread.engineId !== "codex") {
      throw new Error(t("panel.toasts.codexThreadToolUnavailable"));
    }

    const compactedThread = await compactCodexThread(activeThread.id);
    if (!compactedThread) {
      throw new Error(t("panel.toasts.codexThreadCompactFailed"));
    }

    // 旧成功提示由对话内持久化压缩状态替代：toast.success(t("panel.toasts.codexThreadCompacted"));
    // 旧用量读取与直接写 store 由统一竞态保护 action 替代。
    await useChatStore.getState().refreshContextUsage(compactedThread.id);
  }

  async function onAttachCodexRemoteThread(engineThreadId: string) {
    // if (!activeWorkspaceId || !selectedModelId) {
    //   throw new Error(t("panel.toasts.codexThreadResumeUnavailable"));
    // }
    if (!activeWorkspaceId) {
      throw new Error(t("panel.toasts.codexThreadResumeUnavailable"));
    }

    // const attachedThread = await attachCodexRemoteThread(
    //   activeWorkspaceId,
    //   engineThreadId,
    //   selectedModelId,
    // );
    const attachedThread = await attachCodexRemoteThread(
      activeWorkspaceId,
      engineThreadId,
    );
    if (!attachedThread) {
      throw new Error(t("panel.toasts.codexThreadResumeFailed"));
    }

    setActiveThreadInStore(attachedThread.id);
    await bindChatThread(attachedThread.id);
    toast.success(t("panel.toasts.codexThreadResumed"));
  }

  async function onAttachOpenCodeRemoteSession(session: OpenCodeRemoteSession) {
    if (!activeWorkspaceId || !selectedModelId) {
      throw new Error(t("panel.toasts.openCodeSessionResumeUnavailable"));
    }

    const attachedThread = await attachOpenCodeRemoteSession(
      activeWorkspaceId,
      session.engineThreadId,
      session.cwd,
      selectedModelId,
    );
    if (!attachedThread) {
      throw new Error(t("panel.toasts.openCodeSessionResumeFailed"));
    }

    setActiveThreadInStore(attachedThread.id);
    await bindChatThread(attachedThread.id);
    toast.success(t("panel.toasts.openCodeSessionResumed"));
  }

  /* ── Slash command system ── */

  const canManageActiveCodexThread =
    !!activeThread &&
    activeThread.engineId === "codex" &&
    !!activeThread.engineThreadId &&
    !streaming;
  const canUseNativeCodexHistoryTools =
    canManageActiveCodexThread &&
    activeThread?.engineMetadata?.codexTranscriptImported !== false;

  const isCodexEngine = selectedEngineId === "codex";
  const isOpenCodeEngine = selectedEngineId === "opencode";
  const activePlanMode = planMode && !isOpenCodeEngine;

  /* 旧实现把三个 CLI 的项目合并到同一个数组，再用 disabled 隐藏不可用能力。
   * 它会让不属于当前 CLI 的命令仍出现在“/”菜单中，保留为迁移记录。
  const slashCommands: SlashCommand[] = useMemo(
    () => [
      ...[
        {
          id: "review",
          name: "review",
          description: t("reviewPicker.subtitle"),
          icon: Search,
          codexOnly: true,
          disabled: !canManageActiveCodexThread,
        },
        {
          id: "fork",
          name: "fork",
          description: t("threadPicker.forkDescription"),
          icon: GitBranch,
          codexOnly: true,
          disabled: !canUseNativeCodexHistoryTools,
        },
        {
          id: "rollback",
          name: "rollback",
          description: t("threadPicker.rollbackDescription"),
          icon: RotateCcw,
          codexOnly: true,
          disabled: !canUseNativeCodexHistoryTools,
        },
        {
          id: "compact",
          name: "compact",
          description: t("threadPicker.compactDescription"),
          icon: Scissors,
          codexOnly: true,
          disabled: !canManageActiveCodexThread,
        },
        {
          id: "fast",
          name: "fast",
          description: t("configPicker.serviceTierDescription"),
          icon: Zap,
          codexOnly: true,
          disabled: !isCodexEngine,
        },
        {
          id: "personality",
          name: "personality",
          description: t("configPicker.personalityDescription"),
          icon: UserCircle,
          codexOnly: true,
          disabled: !isCodexEngine,
        },
        {
          id: "skills",
          name: "skills",
          description: t("slashCommands.panels.skills.description"),
          icon: Sparkles,
          codexOnly: true,
          disabled: !isCodexEngine,
        },
        {
          id: "mcp",
          name: "MCP",
          description: t("slashCommands.panels.mcp.description"),
          icon: Server,
          codexOnly: isCodexEngine,
          disabled: !(isCodexEngine || isOpenCodeEngine),
        },
        {
          id: "experimental",
          name: "experimental",
          description: t("slashCommands.panels.experimental.description"),
          icon: FlaskConical,
          codexOnly: true,
          disabled: !isCodexEngine,
        },
        {
          id: "agents",
          name: "agents",
          description: t("slashCommands.panels.openCodeAgents.description"),
          icon: UserCircle,
          disabled: !isOpenCodeEngine,
        },
        {
          id: "commands",
          name: "commands",
          description: t("slashCommands.panels.openCodeCommands.description"),
          icon: SquareCode,
          disabled: !isOpenCodeEngine,
        },
        {
          id: "sessions",
          name: "sessions",
          description: t("slashCommands.panels.openCodeSessions.description"),
          icon: GitBranch,
          disabled: !isOpenCodeEngine,
        },
      ],
      ...openCodeSlashCommands,
    ],
    [
      canManageActiveCodexThread,
      canUseNativeCodexHistoryTools,
      isCodexEngine,
      isOpenCodeEngine,
      openCodeSlashCommands,
      t,
    ],
  );

  const classicSlashCommands = useMemo<ClassicSlashCommand[]>(() => {
    const commandGroup = t("slashCommands.groups.commands");
    const skillGroup = t("slashCommands.groups.skills");
    const appGroup = t("slashCommands.groups.apps");
    const pluginGroup = t("slashCommands.groups.plugins");
    const mcpGroup = t("slashCommands.groups.mcp");
    const commandItems = slashCommands
      .filter((command) => command.id !== "skills" && command.id !== "mcp")
      .map((command) => ({ ...command, group: commandGroup }));
    const skillItems =
      activeWorkspace?.locationKind === "ssh" && isCodexEngine
        ? codexSkills.map((skill) => ({
            id: `skill:${skill.path}`,
            name: skill.name,
            description: skill.description || skill.scope,
            icon: Sparkles,
            group: skillGroup,
            searchTerms: [skill.path, skill.scope],
            reference: {
              type: "skill" as const,
              name: skill.name,
              path: skill.path,
            },
          }))
        : effectiveExtensionItems
            .filter(
              (item) =>
                item.kind === "skill" &&
                (isCodexEngine || selectedEngineId === "claude"),
            )
            .map((skill) => ({
              id: `skill:${skill.id}`,
              name: skill.name,
              description: skill.description || skill.scope,
              icon: Sparkles,
              group: skillGroup,
              searchTerms: [skill.id, skill.path ?? "", skill.scope],
              reference: isCodexEngine
                ? {
                    type: "skill" as const,
                    name: skill.name,
                    path: skill.path || skill.id,
                  }
                : undefined,
              insertText: selectedEngineId === "claude" ? `/${skill.name} ` : undefined,
            }));
    const appItems = isCodexEngine
      ? codexApps.map((app) => ({
          id: `app:${app.id}`,
          name: app.name,
          description: app.description || app.id,
          icon: AtSign,
          group: appGroup,
          searchTerms: [app.id],
          reference: {
            type: "mention" as const,
            name: app.name,
            path: `app://${app.id}`,
          },
        }))
      : [];
    const pluginItems = isCodexEngine
      ? effectiveExtensionItems
          .filter((item) => item.kind === "plugin")
          .map((plugin) => ({
            id: `plugin:${plugin.id}`,
            name: plugin.name,
            description: plugin.description || plugin.marketplace || plugin.scope,
            icon: Puzzle,
            group: pluginGroup,
            searchTerms: [
              plugin.id,
              plugin.marketplace ?? "",
              plugin.source ?? "",
              plugin.scope,
            ],
            panel: { type: "plugins" as const },
          }))
      : [];
    const mcpItems = isCodexEngine || isOpenCodeEngine
      ? effectiveExtensionItems
          .filter((item) => item.kind === "mcp")
          .map((server) => ({
          id: `mcp:${server.id}`,
          name: server.name,
          description: server.warning || server.description || server.health,
          icon: Server,
          group: mcpGroup,
          searchTerms: [server.health, server.authState ?? "", server.scope],
          panel: { type: "mcp" as const },
        }))
      : [];

    const remoteCodexPluginItems =
      activeWorkspace?.locationKind === "ssh" && isCodexEngine
        ? codexPlugins.map((plugin) => ({
            id: `plugin:${plugin.id}`,
            name: plugin.name,
            description: plugin.description || plugin.id,
            icon: Puzzle,
            group: pluginGroup,
            searchTerms: [plugin.id, plugin.developerName ?? ""],
            panel: { type: "plugins" as const },
          }))
        : [];
    const remoteCodexMcpItems =
      activeWorkspace?.locationKind === "ssh" && isCodexEngine
        ? (codexProtocolDiagnostics?.mcpServers ?? []).map((server) => ({
            id: `mcp:${server.name}`,
            name: server.name,
            description: `${server.authStatus} · ${server.toolCount} tools`,
            icon: Server,
            group: mcpGroup,
            searchTerms: [server.authStatus],
            panel: { type: "mcp" as const },
          }))
        : [];

    return [
      ...commandItems,
      ...skillItems,
      ...appItems,
      ...(activeWorkspace?.locationKind === "ssh" && isCodexEngine
        ? remoteCodexPluginItems
        : pluginItems),
      ...(activeWorkspace?.locationKind === "ssh" && isCodexEngine
        ? remoteCodexMcpItems
        : mcpItems),
    ];
  }, [
    activeWorkspace?.locationKind,
    codexApps,
    codexPlugins,
    codexProtocolDiagnostics?.mcpServers,
    codexSkills,
    effectiveExtensionItems,
    isCodexEngine,
    isOpenCodeEngine,
    selectedEngineId,
    slashCommands,
    t,
  ]);

  const displayedSlashCommands =
    chatInputMode === "classic" ? classicSlashCommands : slashCommands;

  const filteredSlashCommands = useMemo(() => {
    if (chatInputMode === "classic") {
      return filterClassicSlashItems(displayedSlashCommands, slashMenuQuery);
    }

    if (!slashMenuQuery) return displayedSlashCommands;
    const q = slashMenuQuery.toLowerCase();
    return displayedSlashCommands.filter(
      (c) =>
        c.name.toLowerCase().startsWith(q) ||
        c.id.startsWith(q) ||
        c.description.toLowerCase().includes(q),
    );
  }, [chatInputMode, displayedSlashCommands, slashMenuQuery]);
  */

  // 斜杠菜单统一由 buildSlashCommandsFromExtensions 按 ExtensionItemDto 的
  // kind/insert_text/panel 规则解析，不再按 CLI 工具分别解析。
  const displayedSlashCommands = useMemo<CliSlashCommand[]>(() => {
    return buildSlashCommandsFromExtensions(cliExtensionItems, {
      inputMode: chatInputMode,
      t,
      canManageActiveCodexThread,
      canUseNativeCodexHistoryTools,
    });
  }, [
    canManageActiveCodexThread,
    canUseNativeCodexHistoryTools,
    chatInputMode,
    cliExtensionItems,
    t,
  ]);
  const filteredSlashCommands = useMemo(() => {
    if (chatInputMode === "classic") {
      return filterClassicSlashItems(displayedSlashCommands, slashMenuQuery);
    }

    if (!slashMenuQuery) return displayedSlashCommands;
    const query = slashMenuQuery.toLowerCase();
    return displayedSlashCommands.filter(
      (command) =>
        command.name.toLowerCase().startsWith(query) ||
        command.id.startsWith(query) ||
        command.description.toLowerCase().includes(query),
    );
  }, [chatInputMode, displayedSlashCommands, slashMenuQuery]);

  /* 旧的选择处理依赖 ChatPanel 识别 Codex、OpenCode 和 Claude Code 的命令标识。
  function handleSlashCommandSelect(commandId: string) {
    setSlashMenuOpen(false);
    setSlashMenuQuery("");

    const cmd = displayedSlashCommands.find((c) => c.id === commandId);
    if (!cmd || cmd.disabled) return;

    const classicCommand =
      chatInputMode === "classic"
        ? classicSlashCommands.find((command) => command.id === commandId)
        : null;
    const selectedReference = classicCommand?.reference;
    if (selectedReference) {
      const cursorPosition = inputRef.current?.selectionStart ?? input.length;
      const replacement = removeClassicSlashToken(input, cursorPosition);
      setReferences((currentReferences) =>
        currentReferences.some(
          (reference) =>
            reference.type === selectedReference.type &&
            reference.path === selectedReference.path,
        )
          ? currentReferences
          : [...currentReferences, selectedReference],
      );
      setInput(replacement.value);
      requestAnimationFrame(() => {
        const element = inputRef.current;
        element?.focus();
        element?.setSelectionRange(
          replacement.cursorPosition,
          replacement.cursorPosition,
        );
      });
      return;
    }

    if (classicCommand?.insertText) {
      const cursorPosition = inputRef.current?.selectionStart ?? input.length;
      const replacement = removeClassicSlashToken(input, cursorPosition);
      const suffix = replacement.value.slice(replacement.cursorPosition);
      const prefix = replacement.value.slice(0, replacement.cursorPosition);
      const insertText = classicCommand.insertText;
      const nextInput = `${prefix}${insertText}${suffix}`;
      const nextCursor = prefix.length + insertText.length;
      setInput(nextInput);
      requestAnimationFrame(() => {
        inputRef.current?.focus();
        inputRef.current?.setSelectionRange(nextCursor, nextCursor);
      });
      return;
    }

    const commandPanel = classicCommand?.panel;
    if (commandPanel) {
      const cursorPosition = inputRef.current?.selectionStart ?? input.length;
      const replacement = removeClassicSlashToken(input, cursorPosition);
      setInput(replacement.value);
      setActiveCommandPanel(commandPanel);
      setCommandPanelError(null);
      requestAnimationFrame(() => {
        const element = inputRef.current;
        element?.focus();
        element?.setSelectionRange(
          replacement.cursorPosition,
          replacement.cursorPosition,
        );
      });
      return;
    }

    if (commandId.startsWith("opencode-command:")) {
      const commandName = commandId.slice("opencode-command:".length);
      if (chatInputMode === "classic") {
        const cursorPosition = inputRef.current?.selectionStart ?? input.length;
        const slashPosition = input.slice(0, cursorPosition).lastIndexOf("/");
        if (slashPosition >= 0) {
          const suffix = input.slice(cursorPosition);
          const insertedCommand = `/${commandName}${/^\s/.test(suffix) ? "" : " "}`;
          const nextInput = `${input.slice(0, slashPosition)}${insertedCommand}${suffix}`;
          setInput(nextInput);
          requestAnimationFrame(() => {
            const element = inputRef.current;
            element?.focus();
            const nextCursor = slashPosition + insertedCommand.length;
            element?.setSelectionRange(nextCursor, nextCursor);
          });
          return;
        }
      }
      setInput(`/${commandName} `);
      requestAnimationFrame(() => inputRef.current?.focus());
      return;
    }

    // /fast is a simple toggle — no panel needed
    if (commandId === "fast") {
      setInput("");
      const nextTier = selectedServiceTier === "fast" ? "inherit" : "fast";
      handleCommandPanelConfirm(
        { type: "fast" } as ActiveSlashCommand,
        { serviceTier: nextTier },
      );
      return;
    }

    setInput("");
    setActiveCommandPanel({ type: commandId } as ActiveSlashCommand);
    setCommandPanelError(null);
  }
  */

  function handleSlashCommandSelect(commandId: string) {
    setSlashMenuOpen(false);
    setSlashMenuQuery("");

    const command = displayedSlashCommands.find((item) => item.id === commandId);
    if (!command || command.disabled || !command.action) {
      return;
    }

    const cursorPosition = inputRef.current?.selectionStart ?? input.length;
    const replacement =
      chatInputMode === "classic"
        ? removeClassicSlashToken(input, cursorPosition)
        : { value: input, cursorPosition };
    if (command.action.type === "reference") {
      const selectedReference = command.action.reference;
      setReferences((currentReferences) =>
        currentReferences.some(
          (reference) =>
            reference.type === selectedReference.type &&
            reference.path === selectedReference.path,
        )
          ? currentReferences
          : [...currentReferences, selectedReference],
      );
      setInput(replacement.value);
      requestAnimationFrame(() => {
        const element = inputRef.current;
        element?.focus();
        element?.setSelectionRange(replacement.cursorPosition, replacement.cursorPosition);
      });
      return;
    }

    if (command.action.type === "insert") {
      if (chatInputMode !== "classic") {
        setInput(command.action.text);
        requestAnimationFrame(() => inputRef.current?.focus());
        return;
      }
      const prefix = replacement.value.slice(0, replacement.cursorPosition);
      const suffix = replacement.value.slice(replacement.cursorPosition);
      const nextInput = `${prefix}${command.action.text}${suffix}`;
      const nextCursor = prefix.length + command.action.text.length;
      setInput(nextInput);
      requestAnimationFrame(() => {
        inputRef.current?.focus();
        inputRef.current?.setSelectionRange(nextCursor, nextCursor);
      });
      return;
    }

    if (command.action.type === "panel") {
      setInput(chatInputMode === "classic" ? replacement.value : "");
      setActiveCommandPanel(command.action.panel);
      setCommandPanelError(null);
      requestAnimationFrame(() => {
        const element = inputRef.current;
        element?.focus();
        if (chatInputMode === "classic") {
          element?.setSelectionRange(replacement.cursorPosition, replacement.cursorPosition);
        }
      });
      return;
    }

    setInput("");
    const nextTier = selectedServiceTier === "fast" ? "inherit" : "fast";
    handleCommandPanelConfirm(
      { type: "fast" },
      { serviceTier: nextTier },
    );
  }

  async function handleCommandPanelConfirm(
    command: ActiveSlashCommand,
    payload?: import("./ChatCommandPanel").SlashCommandPayload,
  ) {
    if (commandPanelBusyRef.current) {
      return;
    }
    commandPanelBusyRef.current = true;
    setCommandPanelBusy(true);
    setCommandPanelError(null);
    try {
      switch (command.type) {
        case "fork":
          await onForkCodexThread();
          break;
        case "compact":
          await onCompactCodexThread();
          break;
        case "rollback":
          if (payload?.numTurns) {
            await onRollbackCodexThread(payload.numTurns);
          }
          break;
        case "review":
          if (payload?.target && payload?.delivery) {
            await onStartCodexReview({
              target: payload.target,
              delivery: payload.delivery,
            });
          }
          break;
        case "fast":
          if (payload?.serviceTier !== undefined) {
            const tier = payload.serviceTier === "inherit" ? null : payload.serviceTier;
            await onCodexConfigSave({
              updatePersonality: false,
              personality: null,
              updateServiceTier: true,
              serviceTier: tier,
              updateOutputSchema: false,
              outputSchema: null,
            });
            toast.success(
              t("panel.toasts.fastToggled", {
                state: payload.serviceTier === "fast"
                  ? t("panel.toasts.on")
                  : t("panel.toasts.off"),
              }),
            );
          }
          break;
        case "personality":
          if (payload?.personality !== undefined) {
            const p = payload.personality === "inherit" ? null : payload.personality;
            await onCodexConfigSave({
              updatePersonality: true,
              personality: p,
              updateServiceTier: false,
              serviceTier: null,
              updateOutputSchema: false,
              outputSchema: null,
            });
            toast.success(t("panel.toasts.personalityUpdated", { value: payload.personality }));
          }
          break;
      }
      setActiveCommandPanel(null);
    } catch (err) {
      setCommandPanelError(err instanceof Error ? err.message : String(err));
    } finally {
      commandPanelBusyRef.current = false;
      setCommandPanelBusy(false);
    }
  }

  function handleSlashDetection(value: string, cursorPos: number) {
    const slashQuery =
      chatInputMode === "classic"
        ? findClassicSlashQuery(value, cursorPos)
        : /(?:^|\s)(\/([a-z]*))$/.exec(value.slice(0, cursorPos))?.[2] ?? null;
    if (slashQuery !== null) {
      setSlashMenuOpen(true);
      setSlashMenuQuery(slashQuery);
      setSlashMenuActiveIndex(0);
    } else if (slashMenuOpen) {
      setSlashMenuOpen(false);
    }
  }

  const threadMentionCandidates = useMemo(() => {
    const query = threadMentionQuery.toLocaleLowerCase();
    return (activeWorkspaceId ? threadsByWorkspace[activeWorkspaceId] ?? [] : [])
      .filter((thread) => thread.id !== activeThread?.id)
      .filter((thread) => (thread.title || "未命名会话").toLocaleLowerCase().includes(query));
  }, [activeThread?.id, activeWorkspaceId, threadMentionQuery, threadsByWorkspace]);

  function handleThreadMentionDetection(value: string, cursorPos: number) {
    const atIndex = value.lastIndexOf("@", Math.max(0, cursorPos - 1));
    if (atIndex < 0 || (atIndex > 0 && !/\s/.test(value[atIndex - 1] ?? ""))) {
      setThreadMentionMenuOpen(false);
      return;
    }
    const query = value.slice(atIndex + 1, cursorPos);
    const selectedTitle = selectedAuraCoderThreadMention?.title ?? "";
    if (
      selectedTitle &&
      value.slice(atIndex + 1, atIndex + 1 + selectedTitle.length) === selectedTitle &&
      query.length >= selectedTitle.length
    ) {
      setThreadMentionMenuOpen(false);
      return;
    }
    threadMentionRangeRef.current = { start: atIndex, end: cursorPos };
    setThreadMentionQuery(query);
    setThreadMentionActiveIndex(0);
    setThreadMentionMenuOpen(true);
  }

  function handleThreadMentionSelect(thread: Thread) {
    const range = threadMentionRangeRef.current;
    if (!range) return;
    const title = thread.title.trim() || "未命名会话";
    const mention = `@${title}`;
    const nextValue = `${input.slice(0, range.start)}${mention}${input.slice(range.end)}`;
    const nextCursor = range.start + mention.length;
    setInput(nextValue);
    setSelectedAuraCoderThreadMention({ threadId: thread.id, title });
    setThreadMentionMenuOpen(false);
    requestAnimationFrame(() => {
      inputRef.current?.focus();
      inputRef.current?.setSelectionRange(nextCursor, nextCursor);
    });
  }

  // async function submitMessage(): Promise<boolean> {
  /** 将当前会话消息、附件、计划与运行配置作为一次发送提交。 */
  async function submitMessage(
    submittedText: string,
    submittedAttachments: ChatAttachment[],
    submittedTextAnnotations: ChatTextAnnotation[],
    submittedReferences: ChatInputReference[],
    submittedPlanMode: boolean,
    referencedAuraCoderThreadId: string | null = null,
  ): Promise<boolean> {
    // if ((!input.trim() && textAnnotations.length === 0) || !activeWorkspaceId) return false;
    // if ((!submittedText.trim() && submittedTextAnnotations.length === 0) || !activeWorkspaceId) return false;
    if (
      (!submittedText.trim()
        && submittedReferences.length === 0
        && submittedTextAnnotations.length === 0
        && !hasImageAttachmentAnnotations(submittedAttachments))
      || !activeWorkspaceId
    ) return false;
    const preflightStartedAt = performance.now();
    // const text = formatTextAnnotationsForSubmission(input, textAnnotations);
    // const text = formatTextAnnotationsForSubmission(submittedText, submittedTextAnnotations);
    const text = formatImageAttachmentAnnotationsForSubmission(
      formatTextAnnotationsForSubmission(submittedText, submittedTextAnnotations),
      submittedAttachments,
    );
    // const currentAttachments = [...attachments];
    const currentAttachments = [...submittedAttachments];
    const transportAttachments = stripImageAttachmentAnnotationsForSubmission(currentAttachments);
    // const currentTextAnnotations = [...textAnnotations];
    const currentTextAnnotations = [...submittedTextAnnotations];
    // const currentReferences = [...references];
    const currentReferences = [...submittedReferences];
    const submissionSourceSessionKey =
      activeSubmissionSessionKeyRef.current ?? composerSessionKey;
    const sourceSettings = submissionSourceSessionKey
      ? useChatComposerStore.getState().settingsBySession[submissionSourceSessionKey] ??
        EMPTY_CHAT_SESSION_SETTINGS
      : EMPTY_CHAT_SESSION_SETTINGS;
    const inheritNewDraftPermissions =
      Boolean(composerSessionKey?.startsWith("new:")) &&
      !activeThreadInCurrentWorkspace &&
      sourceSettings.permissionMode === undefined;
    const submittedPermissionValues = inheritNewDraftPermissions
      ? undefined
      : permissionComponent;
    const submittedSendMethod = sessionMessageSendMode;
    const submittedSettings: ChatSessionSettings = {
      engineId: selectedEngineId,
      modelId: selectedModelId,
      reasoningEffort: selectedEffort,
      planMode: submittedPlanMode,
      sendMethod: submittedSendMethod,
      permissionMode: submittedPermissionValues,
      permissionDefaultsChanged: sourceSettings.permissionDefaultsChanged === true,
      personality: selectedPersonality,
      serviceTier: selectedServiceTier,
      outputSchemaText,
      openCodeAgent: selectedOpenCodeAgentRef.current,
    };
    if (submissionSourceSessionKey) {
      setSessionSettings(submissionSourceSessionKey, submittedSettings);
    }

    if (streaming) {
      if (!canSteerActiveTurn) {
        return false;
      }

      const activeThreadId = threadId ?? activeThread?.id ?? null;
      if (!activeThreadId) {
        return false;
      }

      const inputItems = await resolveInputItems(
        text,
        activeThread?.engineId ?? selectedEngineId,
        currentReferences,
      );
      const steered = await steer(text, {
        threadIdOverride: activeThreadId,
        // attachments: currentAttachments.length > 0 ? currentAttachments : undefined,
        attachments: transportAttachments.length > 0 ? transportAttachments : undefined,
        inputItems,
        planMode: submittedPlanMode,
        permissionValues: permissionComponent,
        referencedAuraCoderThreadId,
      });
      if (steered) {
        // const trimmed = input.trim();
        const trimmed = submittedText.trim();
        if (trimmed) {
          const hist = inputHistoryRef.current;
          if (hist[0] !== trimmed) {
            inputHistoryRef.current = [trimmed, ...hist].slice(0, 50);
          }
        }
        inputHistCursorRef.current = -1;
        inputLiveDraftRef.current = "";
        // setInput("");
        // setAttachments([]);
        // setTextAnnotations([]);
        // setReferences([]);
      }
      return steered;
    }

    const composerRuntime = resolveComposerRuntimeSelection();
    if (!composerRuntime) {
      return false;
    }
    const submitEngineId = composerRuntime.engineId;
    const submitModelId = composerRuntime.modelId;
    const submitReasoningEffort = composerRuntime.reasoningEffort;
    // const submitPlanMode = submitEngineId === "opencode" ? false : planMode;
    const submitPlanMode = submitEngineId === "opencode" ? false : submittedPlanMode;
    submittedSettings.engineId = submitEngineId;
    submittedSettings.modelId = submitModelId;
    submittedSettings.reasoningEffort = submitReasoningEffort;
    submittedSettings.planMode = submitPlanMode;
    if (submissionSourceSessionKey) {
      setSessionSettings(submissionSourceSessionKey, submittedSettings);
    }
    const outputSchemaDraft =
      submitEngineId === "codex"
        ? parseOutputSchemaDraft(submittedSettings.outputSchemaText ?? "")
        : { ok: true as const, value: null };
    if (!outputSchemaDraft.ok) {
      return false;
    }

    const activeThreadInScope = activeThread
      ? activeThread.workspaceId === activeWorkspaceId
      : false;
    /*
     * 旧逻辑要求当前模型与会话创建时模型相同，才会复用会话：
     * const activeThreadModelMatch = activeThread
     *   ? activeThreadHasRemoteSession ||
     *     submitEngineId === "codex" ||
     *     activeThread.modelId === submitModelId ||
     *     readThreadLastModelId(activeThread) === submitModelId
     *   : false;
     *
     * 这会使手动新建的同一 CLI 会话在首次发送时因模型不同被误判为不可复用，
     * 从而额外创建“工作区对话”。同一 CLI 支持切换具体模型，因此不再以模型
     * 是否相同决定会话是否复用；CLI 是否相同仍由 activeThreadEngineMatch 限制。
     */
    const activeThreadEngineMatch = activeThread
      ? activeThread.engineId === submitEngineId
      : false;
    /*
     * 旧逻辑只允许 SSH 空会话更换 CLI，会使本机空会话另建“工作区对话”；
     * const activeThreadCanChangeEngine =
     *   activeWorkspace?.locationKind === "ssh" &&
     *   Boolean(activeThread) &&
     *   activeThread?.engineThreadId === null &&
     *   activeThread?.messageCount === 0;
     */
    const activeThreadCanChangeEngine =
      canChangeUnstartedThreadEngine(activeThread);

    let targetThreadId =
      threadId &&
      activeThreadInScope &&
      (activeThreadEngineMatch || activeThreadCanChangeEngine)
        ? threadId
        : null;
    let createdThread = false;

    /*
     * 旧的发送前重配置 IPC 完整保留在注释中，现由 send_message 请求一次提交：
     * if (targetThreadId && !activeThreadEngineMatch) {
     *   const reconfiguredThread = await ipc.reconfigureUnstartedThreadRuntime(...);
     *   applyThreadUpdateLocal(reconfiguredThread);
     * }
     */

    if (!targetThreadId) {
      const createdThreadId = await createThread({
        workspaceId: activeWorkspaceId,
        engineId: submitEngineId,
        modelId: submitModelId,
        reasoningEffort: submitReasoningEffort,
        serviceTier:
          submitEngineId === "codex" && selectedServiceTier !== "inherit"
            ? selectedServiceTier
            : null,
        title: t("panel.workspaceChatTitle"),
      });
      if (!createdThreadId) {
        return false;
      }
      targetThreadId = createdThreadId;
      // 首发前把来源草稿的表单快照复制到前端待发送线程，防止绑定触发恢复时丢失选择。
      setSessionSettings(`thread:${createdThreadId}`, submittedSettings);
      // 新线程创建成功后，失败恢复目标从临时范围转为正式线程范围。
      activeSubmissionSessionKeyRef.current = getChatComposerSessionKey(
        activeWorkspaceId,
        targetThreadId,
      );
      createdThread = true;
      manualThreadBindTargetRef.current = createdThreadId;
      try {
        await bindChatThread(createdThreadId);
      } finally {
        if (manualThreadBindTargetRef.current === createdThreadId) {
          manualThreadBindTargetRef.current = null;
        }
      }
    }

    setThreadMessageSendMode(targetThreadId, sessionMessageSendMode);
    clearPendingMessageSendMode(activeWorkspaceId);

    const inputItems = await resolveInputItems(text, submitEngineId, currentReferences);

    recordPerfMetric("chat.send.preflight.ms", performance.now() - preflightStartedAt, {
      engineId: submitEngineId,
      modelId: submitModelId,
      createdThread,
    });

    const sent = await send(text, {
      threadIdOverride: targetThreadId,
      engineId: submitEngineId,
      modelId: submitModelId,
      reasoningEffort: submitReasoningEffort,
      // attachments: currentAttachments.length > 0 ? currentAttachments : undefined,
      attachments: transportAttachments.length > 0 ? transportAttachments : undefined,
      inputItems,
      planMode: submitPlanMode,
      sendMethod: submittedSendMethod,
      permissionValues: submittedPermissionValues,
      configuration: {
        permissionDefaultsChanged: submittedSettings.permissionDefaultsChanged === true,
        ...(submitEngineId === "codex"
          ? {
              codexConfig: {
                personality:
                  submittedSettings.personality === "inherit"
                    ? null
                    : submittedSettings.personality ?? null,
                serviceTier:
                  submittedSettings.serviceTier === "inherit"
                    ? null
                    : submittedSettings.serviceTier ?? null,
                outputSchema: outputSchemaDraft.value,
              },
            }
          : submitEngineId === "opencode"
            ? {
                opencodeConfig: {
                  agent:
                    submittedSettings.openCodeAgent === "build"
                      ? null
                      : submittedSettings.openCodeAgent ?? null,
                },
              }
            : {}),
      },
      remoteAttachmentUpload:
        activeWorkspace?.locationKind === "ssh" && currentAttachments.length > 0,
      referencedAuraCoderThreadId,
    });
    if (sent) {
      // 仅在发送期间权限默认数组仍未变化时清除本次已提交的全局默认变更标记。
      if (
        submittedSettings.permissionDefaultsChanged === true &&
        submittedPermissionValues
      ) {
        const committedSessionKeys = [
          submissionSourceSessionKey,
          `thread:${targetThreadId}`,
        ].filter((sessionKey): sessionKey is string => Boolean(sessionKey));
        for (const sessionKey of new Set(committedSessionKeys)) {
          clearSubmittedPermissionDefaultsChange(sessionKey, submittedPermissionValues);
        }
      }
      pendingPlanImplementationThreadIdRef.current = submitPlanMode ? targetThreadId : null;
      // const trimmed = input.trim();
      const trimmed = submittedText.trim();
      if (trimmed) {
        const hist = inputHistoryRef.current;
        if (hist[0] !== trimmed) {
          inputHistoryRef.current = [trimmed, ...hist].slice(0, 50);
        }
      }
      inputHistCursorRef.current = -1;
      inputLiveDraftRef.current = "";
      // setInput("");
      // setAttachments([]);
      // setTextAnnotations([]);
      // setReferences([]);
    }
    return sent;
  }

  async function onSubmit(event: FormEvent) {
    event.preventDefault();
    if (
      !canSubmitComposer ||
      !activeWorkspaceId ||
      isSubmittingRef.current
    ) {
      return;
    }

    const submittedText = input.trim();
    const referencedAuraCoderThreadId =
      selectedAuraCoderThreadMention &&
      submittedText.includes(`@${selectedAuraCoderThreadMention.title}`)
        ? selectedAuraCoderThreadMention.threadId
        : null;
    const submittedAttachments = [...attachments];
    const submittedTextAnnotations = [...textAnnotations];
    const submittedReferences = [...references];
    const submittedMessage = formatTextAnnotationsForSubmission(
      submittedText,
      submittedTextAnnotations,
    );
    const submittedMessageWithImageAnnotations =
      formatImageAttachmentAnnotationsForSubmission(submittedMessage, submittedAttachments);
    if (sessionMessageSendMode === "flexible") {
      if (!flexibleMessageSessionKey) {
        return;
      }
      if (activeChatSessionId) {
        setThreadMessageSendMode(activeChatSessionId, sessionMessageSendMode);
      } else {
        setPendingMessageSendMode(activeWorkspaceId, sessionMessageSendMode);
      }
      addPendingFlexibleMessage(flexibleMessageSessionKey, {
        auracoderThreadId: referencedAuraCoderThreadId,
        id: crypto.randomUUID(),
        text: submittedMessage,
        attachments: submittedAttachments,
        references: submittedReferences,
      });
      setInput("");
      setAttachments([]);
      setTextAnnotations([]);
      setReferences([]);
      setSelectedAuraCoderThreadMention(null);
      return;
    }
    if (!streaming) {
      setPendingSubmission(
        createPendingSubmissionMessage(
          threadId ?? activeThread?.id ?? "pending",
          submittedMessageWithImageAnnotations,
          submittedAttachments,
          submittedReferences,
          planMode,
        ),
      );
    }

    isSubmittingRef.current = true;
    setIsSubmitting(true);
    activeSubmissionSessionKeyRef.current = composerSessionKey;
    // const submission = submitMessage();
    const submission = submitMessage(
      submittedText,
      submittedAttachments,
      submittedTextAnnotations,
      submittedReferences,
      planMode,
      referencedAuraCoderThreadId,
    );
    setInput("");
    setAttachments([]);
    setTextAnnotations([]);
    setReferences([]);
    try {
      const accepted = await submission;
      if (!accepted) {
        const restoreSessionKey = activeSubmissionSessionKeyRef.current;
        if (restoreSessionKey) {
          restoreComposerContent(
            restoreSessionKey,
            submittedText,
            submittedAttachments,
            submittedTextAnnotations,
            submittedReferences,
          );
        }
      }
      if (accepted) {
        setSelectedAuraCoderThreadMention(null);
      }
    } catch (error) {
      const restoreSessionKey = activeSubmissionSessionKeyRef.current;
      if (restoreSessionKey) {
        restoreComposerContent(
          restoreSessionKey,
          submittedText,
          submittedAttachments,
          submittedTextAnnotations,
          submittedReferences,
        );
      }
      throw error;
    } finally {
      setPendingSubmission(null);
      activeSubmissionSessionKeyRef.current = null;
      isSubmittingRef.current = false;
      setIsSubmitting(false);
    }
  }

  /** 汇总灵活模式待发送消息，并在失败时将队列保留到实际会话范围。 */
  async function submitFlexibleMessages() {
    if (
      !activeWorkspaceId ||
      !flexibleMessageSessionKey ||
      pendingFlexibleMessages.length === 0 ||
      isSubmittingRef.current ||
      (streaming && !canSteerActiveTurn)
    ) {
      return;
    }

    const submittedText = pendingFlexibleMessages
      .map((message) => message.text)
      .join("\n\n");
    const submittedAttachments = pendingFlexibleMessages.flatMap(
      (message) => message.attachments,
    );
    const submittedReferences = pendingFlexibleMessages.flatMap(
      (message) => message.references,
    );
    const referencedAuraCoderThreadId =
      pendingFlexibleMessages.find((message) => Boolean(message.auracoderThreadId))?.auracoderThreadId ??
      null;
    setPendingSubmission(
      createPendingSubmissionMessage(
        threadId ?? activeThread?.id ?? "pending",
        formatImageAttachmentAnnotationsForSubmission(submittedText, submittedAttachments),
        submittedAttachments,
        submittedReferences,
        planMode,
      ),
    );

    isSubmittingRef.current = true;
    setIsSubmitting(true);
    activeSubmissionSessionKeyRef.current = composerSessionKey;
    let accepted = false;
    try {
      accepted = await submitMessage(
        submittedText,
        submittedAttachments,
        [],
        submittedReferences,
        planMode,
        referencedAuraCoderThreadId,
      );
      if (accepted) {
        clearPendingFlexibleMessages(flexibleMessageSessionKey);
      }
    } finally {
      // 首发失败后把原新草稿的灵活消息队列迁移到刚创建的前端待发送线程。
      const restoreSessionKey = activeSubmissionSessionKeyRef.current;
      if (
        !accepted &&
        restoreSessionKey &&
        restoreSessionKey !== flexibleMessageSessionKey
      ) {
        for (const message of pendingFlexibleMessages) {
          addPendingFlexibleMessage(restoreSessionKey, message);
        }
        clearPendingFlexibleMessages(flexibleMessageSessionKey);
      }
      setPendingSubmission(null);
      activeSubmissionSessionKeyRef.current = null;
      isSubmittingRef.current = false;
      setIsSubmitting(false);
    }
  }

  /** 将已确认的工作区根目录操作与该会话配置一起提交。 */
  async function executeWorkspaceOptInSend() {
    const prompt = workspaceRootPrompt;
    if (!prompt) return;
    const promptSessionKey = getChatComposerSessionKey(prompt.workspaceId, prompt.threadId);
    setPendingSubmission(
      createPendingSubmissionMessage(
        prompt.threadId,
        prompt.text,
        prompt.attachments,
        prompt.references,
        prompt.planMode,
      ),
    );
    setWorkspaceRootPrompt(null);

    try {
      // 项目根目录是唯一写入范围，无需额外确认。

      if (!(await applyCodexConfigToThread(prompt.threadId, {
        engineId: prompt.engineId,
        personality: prompt.personality,
        serviceTier: prompt.serviceTier,
        outputSchemaText: prompt.outputSchemaText,
      }))) {
        restoreComposerContent(
          promptSessionKey,
          prompt.draftText,
          prompt.attachments,
          prompt.textAnnotations,
          prompt.references,
        );
        return;
      }
      if (!(await applyOpenCodeConfigToThread(prompt.threadId, {
        engineId: prompt.engineId,
        agent: prompt.openCodeAgent,
      }))) {
        restoreComposerContent(
          promptSessionKey,
          prompt.draftText,
          prompt.attachments,
          prompt.textAnnotations,
          prompt.references,
        );
        return;
      }

      const promptPlanMode = prompt.engineId === "opencode" ? false : prompt.planMode;
      const promptOutputSchema = prompt.engineId === "codex"
        ? parseOutputSchemaDraft(prompt.outputSchemaText)
        : { ok: true as const, value: null };
      if (!promptOutputSchema.ok) {
        restoreComposerContent(
          promptSessionKey,
          prompt.draftText,
          prompt.attachments,
          prompt.textAnnotations,
          prompt.references,
        );
        return;
      }
      const promptSettings =
        useChatComposerStore.getState().settingsBySession[promptSessionKey] ??
        EMPTY_CHAT_SESSION_SETTINGS;
      const promptThread =
        useThreadStore.getState().threads.find((thread) => thread.id === prompt.threadId) ?? null;
      const promptPermissionValues =
        promptSettings.permissionMode ?? promptThread?.permissionMode ?? undefined;
      const sent = await send(prompt.text, {
        threadIdOverride: prompt.threadId,
        engineId: prompt.engineId,
        modelId: prompt.modelId,
        reasoningEffort: prompt.effort,
        // attachments: prompt.attachments.length > 0 ? prompt.attachments : undefined,
        attachments: prompt.attachments.length > 0
          ? stripImageAttachmentAnnotationsForSubmission(prompt.attachments)
          : undefined,
        inputItems: prompt.inputItems ?? undefined,
        planMode: promptPlanMode,
        sendMethod: promptSettings.sendMethod ?? promptThread?.sendMethod ?? undefined,
        permissionValues: promptPermissionValues,
        remoteAttachmentUpload:
          activeWorkspace?.locationKind === "ssh" && prompt.attachments.length > 0,
        configuration: {
          permissionDefaultsChanged: promptSettings.permissionDefaultsChanged === true,
          ...(prompt.engineId === "codex"
            ? {
                codexConfig: {
                  personality: prompt.personality === "inherit" ? null : prompt.personality,
                  serviceTier: prompt.serviceTier === "inherit" ? null : prompt.serviceTier,
                  outputSchema: promptOutputSchema.value,
                },
              }
            : prompt.engineId === "opencode"
              ? {
                  opencodeConfig: {
                    agent: prompt.openCodeAgent === "build" ? null : prompt.openCodeAgent,
                  },
                }
              : {}),
        },
        referencedAuraCoderThreadId: prompt.referencedAuraCoderThreadId,
      });
      if (!sent) {
        restoreComposerContent(
          promptSessionKey,
          prompt.draftText,
          prompt.attachments,
          prompt.textAnnotations,
          prompt.references,
        );
        return;
      }

      clearSubmittedPermissionDefaultsChange(promptSessionKey, promptPermissionValues);
      pendingPlanImplementationThreadIdRef.current = promptPlanMode ? prompt.threadId : null;
      setThreadLastModelLocal(prompt.threadId, prompt.modelId);
      restoreComposerContent(promptSessionKey, "", [], [], []);

      await refreshThreads(prompt.workspaceId);
    } catch {
      restoreComposerContent(
        promptSessionKey,
        prompt.draftText,
        prompt.attachments,
        prompt.textAnnotations,
        prompt.references,
      );
    } finally {
      setPendingSubmission(null);
    }
  }

  /** 将计划模式确认后的实现请求连同原会话配置一起发送。 */
  async function executePlanImplementation() {
    const prompt = planImplementationPrompt;
    if (!prompt || !activeWorkspaceId) {
      return;
    }
    const implementationMessage = getPlanImplementationCodingMessage(prompt.engineId);

    const currentThread =
      useThreadStore.getState().threads.find((thread) => thread.id === prompt.threadId) ??
      (activeThread?.id === prompt.threadId ? activeThread : null);
    if (!currentThread) {
      toast.error(t("panel.toasts.planImplementationThreadUnavailable"));
      return;
    }

    setPlanImplementationPrompt(null);
    setPlanMode(false);
    try {
      if (
        !(await applyCodexConfigToThread(currentThread.id, {
          engineId: prompt.engineId,
          personality: prompt.personality,
          serviceTier: prompt.serviceTier,
          outputSchemaText: prompt.outputSchemaText,
        }))
      ) {
        setPlanMode(true);
        setPlanImplementationPrompt(prompt);
        return;
      }
      if (
        !(await applyOpenCodeConfigToThread(currentThread.id, {
          engineId: prompt.engineId,
          agent: prompt.openCodeAgent,
        }))
      ) {
        setPlanMode(true);
        setPlanImplementationPrompt(prompt);
        return;
      }
      const outputSchemaDraft = prompt.engineId === "codex"
        ? parseOutputSchemaDraft(prompt.outputSchemaText)
        : { ok: true as const, value: null };
      if (!outputSchemaDraft.ok) {
        setPlanMode(true);
        setPlanImplementationPrompt(prompt);
        return;
      }
      const promptSessionKey = `thread:${prompt.threadId}`;
      const promptSettings =
        useChatComposerStore.getState().settingsBySession[promptSessionKey] ??
        EMPTY_CHAT_SESSION_SETTINGS;
      const promptPermissionValues =
        promptSettings.permissionMode ?? currentThread.permissionMode ?? undefined;

      const sent = await send(implementationMessage, {
        threadIdOverride: currentThread.id,
        engineId: prompt.engineId,
        modelId: prompt.modelId,
        reasoningEffort: prompt.effort,
        planMode: false,
        sendMethod: promptSettings.sendMethod ?? currentThread.sendMethod ?? undefined,
        permissionValues: promptPermissionValues,
        configuration: {
          permissionDefaultsChanged: promptSettings.permissionDefaultsChanged === true,
          ...(prompt.engineId === "codex"
            ? {
                codexConfig: {
                  personality: prompt.personality === "inherit" ? null : prompt.personality,
                  serviceTier: prompt.serviceTier === "inherit" ? null : prompt.serviceTier,
                  outputSchema: outputSchemaDraft.value,
                },
              }
            : prompt.engineId === "opencode"
              ? {
                  opencodeConfig: {
                    agent: prompt.openCodeAgent === "build" ? null : prompt.openCodeAgent,
                  },
                }
              : {}),
        },
      });
      if (!sent) {
        setPlanMode(true);
        setPlanImplementationPrompt(prompt);
      } else {
        clearSubmittedPermissionDefaultsChange(promptSessionKey, promptPermissionValues);
        setThreadLastModelLocal(currentThread.id, prompt.modelId);
      }
    } catch {
      setPlanMode(true);
      setPlanImplementationPrompt(prompt);
    }
  }

  function handlePlanImplementationQuestionnaireSubmit(response: ApprovalResponse) {
    const answerMap =
      "answers" in response &&
      response.answers &&
      typeof response.answers === "object" &&
      !Array.isArray(response.answers) &&
      "plan_implementation_decision" in response.answers
        ? (response.answers as Record<string, { answers?: string[] }>)
        : null;
    const selectedAnswer = answerMap?.plan_implementation_decision?.answers?.[0]?.trim();
    if (selectedAnswer === planImplementationQuestionChoiceStay) {
      setPlanImplementationPrompt(null);
      setPlanMode(true);
      return;
    }

    void executePlanImplementation();
  }

  function dismissWorkspaceOptInPrompt() {
    const prompt = workspaceRootPrompt;
    if (prompt?.restorePlanModeOnCancel) {
      setPlanMode(true);
    }
    if (prompt) {
      restoreComposerContent(
        getChatComposerSessionKey(prompt.workspaceId, prompt.threadId),
        prompt.draftText,
        prompt.attachments,
        prompt.textAnnotations,
        prompt.references,
      );
    }
    setPendingSubmission(null);
    setWorkspaceRootPrompt(null);
  }

  /** 更新思考强度的本地表单值并暂存到当前会话。 */
  async function onReasoningEffortChange(nextEffort: string) {
    setHasExplicitComposerRuntime(true);
    selectedEffortRef.current = nextEffort;
    setSelectedEffort(nextEffort);
    await saveThreadRuntimeSelectionPatch(activeThread?.id, {
      reasoningEffort: nextEffort,
    });
  }

  /** 更新工作区全局信任级别。 */
  async function onWorkspaceTrustLevelChange(nextTrustLevel: TrustLevel) {
    if (activeWorkspaceId) {
      await useWorkspaceStore.getState().setWorkspaceTrustLevel(activeWorkspaceId, nextTrustLevel);
    }
  }

  const batchApprovalInFlightRef = useRef(false);

  /** 按当前权限预设处理全部待审批请求。 */
  async function allowAllPendingApprovals(stopAsking: boolean) {
    if (batchApprovalInFlightRef.current) {
      return;
    }
    const targetThreadId = activeThread?.id;
    const engineId = activeThread?.engineId;
    if (!targetThreadId) {
      return;
    }
    if (batchApprovableRows.length === 0) {
      return;
    }

    batchApprovalInFlightRef.current = true;
    try {
      if (stopAsking) {
        const next = {
          ...permissionComponent,
          autonomyPreset: ["full"],
        } satisfies PermissionComponentJson;
        const permissionsSaved = await saveThreadRuntimeSelectionPatch(targetThreadId, {
          permissionMode: next,
        });
        if (!permissionsSaved) {
          return;
        }
      }

      for (const approval of batchApprovableRows) {
        const details = approval.details ?? {};
        const decision = stopAsking ? "accept_for_session" : "accept";
        const accepted = await respondApproval(
          approval.approvalId,
          isPermissionsRequestApproval(details)
            ? buildPermissionApprovalResponseForEngine(engineId, details, decision)
            : { decision },
          targetThreadId,
        );
        if (!accepted) {
          toast.error(t("panel.toasts.approvalBatchFailed"));
          return;
        }
      }
    } finally {
      batchApprovalInFlightRef.current = false;
    }
  }

  function startThreadTitleEdit() {
    if (!activeThread) {
      return;
    }
    setThreadTitleDraft(activeThread.title ?? "");
    setEditingThreadTitle(true);
  }

  function cancelThreadTitleEdit() {
    setThreadTitleDraft(activeThread?.title ?? "");
    setEditingThreadTitle(false);
  }

  async function saveThreadTitleEdit() {
    if (!activeThread) {
      setEditingThreadTitle(false);
      return;
    }

    const normalized = threadTitleDraft.trim();
    if (!normalized) {
      cancelThreadTitleEdit();
      return;
    }

    if (normalized !== (activeThread.title ?? "")) {
      await renameThread(activeThread.id, normalized);
    }

    setEditingThreadTitle(false);
  }

  async function handleAddAttachment() {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const attachmentFilterConfig = getAttachmentFilterConfig(t, selectedEngineId, selectedModel);
      if (attachmentFilterConfig?.supportedExtensions.length === 0) {
        toast.warning(attachmentFilterConfig.warningMessage);
        return;
      }
      const selected = await open({
        multiple: true,
        title: attachmentFilterConfig?.title ?? t("panel.attachFiles"),
        filters: attachmentFilterConfig
          ? [
              {
                name: attachmentFilterConfig.supportedLabel,
                extensions: attachmentFilterConfig.supportedExtensions,
              },
              attachmentFilterConfig.imageExtensions.length > 0
                ? {
                    name: attachmentFilterConfig.imagesLabel,
                    extensions: attachmentFilterConfig.imageExtensions,
                  }
                : null,
              attachmentFilterConfig.textExtensions.length > 0
                ? {
                    name: attachmentFilterConfig.textFilesLabel,
                    extensions: attachmentFilterConfig.textExtensions,
                  }
                : null,
            ].filter((filter): filter is { name: string; extensions: string[] } =>
              Boolean(filter),
            )
          : undefined,
      });
      if (!selected) return;
      const paths = Array.isArray(selected) ? selected : [selected];
      appendAttachmentsFromPaths(paths);
    } catch {
      // User cancelled or dialog failed
    }
  }

  function removeAttachment(id: string) {
    setAttachments((prev) => prev.filter((a) => a.id !== id));
  }

  useEffect(() => {
    recordPerfMetric("chat.render.commit.ms", performance.now() - renderStartedAtRef.current, {
      threadId,
      messageCount: messages.length,
      streaming,
    });
  }, [messages.length, streaming, threadId]);

  const handleApproval = useCallback(
    (approvalId: string, response: ApprovalResponse) => {
      void respondApproval(approvalId, response);
    },
    [respondApproval],
  );

  const handleLoadActionOutput = useCallback(
    (messageId: string, actionId: string) => hydrateActionOutput(messageId, actionId),
    [hydrateActionOutput],
  );

  const handleEditResend = useCallback((text: string) => {
    if (!text.trim()) {
      return;
    }
    setInput(text);
    inputRef.current?.focus();
  }, []);

  const openFileInEditor = useFileStore((s) => s.openFile);
  const openUsageLimitsModal = useUiStore((s) => s.openUsageLimitsModal);
  const openImageAttachmentPreview = useUiStore((s) => s.openImageAttachmentPreview);
  const openMessageImageAttachmentPreview = useUiStore(
    (s) => s.openMessageImageAttachmentPreview,
  );
  const handleOpenImageAttachment = useCallback(
    (attachment: AttachmentBlock) => {
      if (!activeWorkspaceId) {
        return;
      }
      openMessageImageAttachmentPreview(activeWorkspaceId, attachment);
    },
    [activeWorkspaceId, openMessageImageAttachmentPreview],
  );

  const diffFileRootPath = activeWorkspace?.rootPath ?? null;
  const handleOpenDiffFile = useCallback(
    (filePath: string) => {
      if (!diffFileRootPath || !activeWorkspaceId) {
        return;
      }
      const normalized = filePath.replace(/\\/g, "/");
      const relativePath = /^(\/|[A-Za-z]:)/.test(normalized)
        ? resolveRelativePathWithinRoot(normalized, diffFileRootPath)
        : normalized;
      if (!relativePath) {
        return;
      }
      void openFileInEditor(diffFileRootPath, relativePath);
      showWorkspaceEditorForDirectFileOpen(activeWorkspaceId);
    },
    [activeWorkspaceId, diffFileRootPath, openFileInEditor],
  );

  const visibleMessages = messages;

  const assistantIdentityByMessageId = useMemo(() => {
    const identityByMessageId = new Map<
      string,
      { label: string; engineId: string; engineName: string }
    >();
    for (const message of visibleMessages) {
      if (message.role !== "assistant") {
        continue;
      }
      identityByMessageId.set(message.id, renderAssistantIdentity(message));
    }
    return identityByMessageId;
  }, [renderAssistantIdentity, visibleMessages]);

  const workspaceName = activeWorkspace?.name || activeWorkspace?.rootPath?.split("/").pop() || "";

  // Compute total diff stats for header display
  const gitFiles = gitStatus?.files ?? [];
  const totalAdded = gitFiles.length;
  const layoutMode: LayoutMode = embedded
    ? "chat"
    : activeWorkspaceId
      ? (terminalWorkspaceState?.layoutMode ?? "chat")
      : "chat";
  const isChatLayoutActive = layoutMode === "chat";
  const isSplitLayoutActive = layoutMode === "split";
  const isTerminalLayoutActive = layoutMode === "terminal";
  const isEditorLayoutActive = layoutMode === "editor";
  const showFocusModeHeader = !embedded && focusMode && !showSidebar && (layoutMode === "chat" || layoutMode === "split");
  const terminalPanelSize = activeWorkspaceId
    ? terminalWorkspaceState?.panelSize ?? 32
    : 32;

  const hasTerminalMountedRef = useRef(false);
  const hasEditorMountedRef = useRef(false);
  // Set refs during render (not in an effect) so the conditional mount below
  // sees the updated value in the same render pass that triggers it.
  if (
    activeWorkspaceId
    && !embedded
    && (layoutMode === "split" || layoutMode === "terminal" || terminalWorkspaceState?.isOpen)
  ) {
    hasTerminalMountedRef.current = true;
  }
  if (!embedded && layoutMode === "editor" && activeWorkspaceId) {
    hasEditorMountedRef.current = true;
  }

  const contentAreaRef = useRef<HTMLDivElement>(null);
  const terminalPanelSizeRef = useRef(terminalPanelSize);
  terminalPanelSizeRef.current = terminalPanelSize;
  const resizeCleanupRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    return () => resizeCleanupRef.current?.();
  }, []);

  const handleResizeStart = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const container = contentAreaRef.current;
    if (!container || !activeWorkspaceId) return;
    const startY = e.clientY;
    const containerHeight = container.getBoundingClientRect().height;
    const startTerminalPct = terminalPanelSizeRef.current;

    const onMove = (moveEvent: MouseEvent) => {
      const deltaY = moveEvent.clientY - startY;
      const deltaPct = (deltaY / containerHeight) * 100;
      const newSize = Math.max(15, Math.min(72, startTerminalPct - deltaPct));
      setTerminalPanelSize(activeWorkspaceId, newSize);
    };
    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      resizeCleanupRef.current = null;
    };
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
    resizeCleanupRef.current = onUp;
  }, [activeWorkspaceId, setTerminalPanelSize]);

  return (
    <div
      style={{
        height: "100%",
        display: "flex",
        flexDirection: "column",
        background: "var(--content-bg)",
      }}
    >
      <ChatTitlebar
        embedded={embedded}
        focusMode={focusMode}
        showSidebar={showSidebar}
        showFocusModeHeader={showFocusModeHeader}
        useTitlebarSafeInset={useTitlebarSafeInset}
        customWindowFrame={customWindowFrame}
        workspaceName={workspaceName}
        editingThreadTitle={editingThreadTitle}
        threadTitleDraft={threadTitleDraft}
        titleInputRef={titleInputRef}
        activeThread={activeThread}
        totalAdded={totalAdded}
        layoutMode={layoutMode}
        activeWorkspaceId={activeWorkspaceId}
        isChatLayoutActive={isChatLayoutActive}
        isSplitLayoutActive={isSplitLayoutActive}
        isTerminalLayoutActive={isTerminalLayoutActive}
        isEditorLayoutActive={isEditorLayoutActive}
        setLayoutMode={setLayoutMode}
        startThreadTitleEdit={startThreadTitleEdit}
        saveThreadTitleEdit={saveThreadTitleEdit}
        cancelThreadTitleEdit={cancelThreadTitleEdit}
        setThreadTitleDraft={setThreadTitleDraft}
      />

      <div ref={contentAreaRef} className="chat-terminal-content">
        {/* Chat section */}
        <div
          ref={chatSectionRef}
          className="chat-section"
          onPointerDownCapture={(event) => {
            if (textAnnotationPopoverRef.current?.contains(event.target as Node)) {
              return;
            }
            setTextAnnotationPopover(null);
            setTextAnnotationComment("");
          }}
          style={{
            flex: (layoutMode === "terminal" || layoutMode === "editor") ? "0 0 0px"
                 : layoutMode === "chat" ? "1 1 0px"
                 : `0 0 ${100 - terminalPanelSize}%`,
            position: "relative",
            overflow: "hidden",
            visibility: (layoutMode === "terminal" || layoutMode === "editor") ? "hidden" : "visible",
            display: "flex",
            flexDirection: "column",
            outline: isFileDropOver ? "2px dashed rgba(var(--info-rgb), 0.7)" : "none",
            outlineOffset: isFileDropOver ? "-8px" : undefined,
          }}
        >
            <ChatMessageArea
              isFileDropOver={isFileDropOver}
              streaming={streaming}
              turnStartedAt={turnStartedAt}
              viewportRef={viewportRef}
              activeWorkspaceId={activeWorkspaceId}
              activeWorkspace={activeWorkspace}
              workspaceName={workspaceName}
              gitStatus={gitStatus}
              messages={messages}
              visibleMessages={visibleMessages}
              pendingSubmission={pendingSubmission}
              assistantIdentityByMessageId={assistantIdentityByMessageId}
              highlightedMessageId={highlightedMessageId}
              activeThread={activeThreadInCurrentWorkspace ? activeThread : null}
              sessionReady={sessionReady}
              preparingAttachments={preparingAttachments}
              preparingEngineId={preparingEngineId}
              hasPendingFlexibleMessages={hasPendingFlexibleMessages}
              pendingFlexibleMessages={pendingFlexibleMessages}
              flexibleMessageConfirmDisabled={flexibleMessageConfirmDisabled}
              flexibleMessageConfirmTitle={flexibleMessageConfirmTitle}
              autoScrollLocked={autoScrollLocked}
              onStreamingTextContentCommitted={handleStreamingTextContentCommitted}
              textAnnotationPopover={textAnnotationPopover}
              textAnnotationComment={textAnnotationComment}
              textAnnotationPopoverRef={textAnnotationPopoverRef}
              annotationCommentInputRef={annotationCommentInputRef}
              inputRef={inputRef}
              setTextAnnotationPopover={setTextAnnotationPopover}
              setTextAnnotationComment={setTextAnnotationComment}
              setTextAnnotations={setTextAnnotations}
              setAutoScrollLocked={setAutoScrollLocked}
              scrollViewportToBottom={scrollViewportToBottom}
              handleEditResend={handleEditResend}
              handleApproval={handleApproval}
              handleLoadActionOutput={handleLoadActionOutput}
              handleOpenDiffFile={handleOpenDiffFile}
              handleOpenImageAttachment={handleOpenImageAttachment}
              submitFlexibleMessages={submitFlexibleMessages}
              withdrawFlexibleMessage={withdrawFlexibleMessage}
            />

            {/* ── Input Area ── */}
            <div className="chat-composer-surface">
            {/* 跳至最新：钉在输入框上沿（bottom:100%），移出滚动容器避免干扰 overlay 滚动条 */}
            {autoScrollLocked && messages.length > 0 && (
              <button
                type="button"
                className={`chat-jump-pill${streaming ? " chat-jump-pill--activity" : ""}`}
                onClick={() => {
                  setAutoScrollLocked(false);
                  scrollViewportToBottom("smooth");
                }}
              >
                {streaming && <span className="chat-jump-pill-dot" />}
                {streaming ? t("panel.newActivity") : t("panel.jumpToLatest")}
              </button>
            )}
        <form
          onSubmit={onSubmit}
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 8,
          }}
        >
          {/* Pending approvals */}
          <ApprovalBanner
            pendingApprovalBannerRows={pendingApprovalBannerRows}
            batchApprovableRows={batchApprovableRows}
            showApprovalBannerHeader={showApprovalBannerHeader}
            activeThread={activeThread}
            activeThreadApprovalDecisionCapabilities={activeThreadApprovalDecisionCapabilities}
            workspaceTrustLevel={workspaceTrustLevel}
            respondApproval={respondApproval}
            allowAllPendingApprovals={allowAllPendingApprovals}
            onWorkspaceTrustLevelChange={onWorkspaceTrustLevelChange}
            pendingToolInputApproval={pendingToolInputApproval}
            setSelectedPendingToolInputApprovalId={setSelectedPendingToolInputApprovalId}
          />

          {/* Input container */}
          <div
            ref={chatInputBoxRef}
            className={`chat-input-box ${activePlanMode && !showSpecialInputComposer ? "chat-input-box-plan" : ""} ${showSpecialInputComposer ? "chat-input-box-tool-input" : ""}`.trim()}
            onPaste={handleInputPaste}
          >
            {!showSpecialInputComposer && (
              <div
                className="chat-input-height-resize-handle"
                role="separator"
                aria-orientation="horizontal"
                aria-label={t("panel.chatInputHeightResize")}
                onMouseDown={handleChatInputHeightStart}
              />
            )}
            {showPendingToolInputComposer && pendingToolInputApproval ? (
              <ToolInputQuestionnaire
                details={pendingToolInputApproval.details ?? {}}
                onCancel={
                  pendingToolInputSupportsCancel
                    ? () =>
                        void respondApproval(pendingToolInputApproval.approvalId, {
                          [pendingToolInputIsOpenCodeQuestion ? "decision" : "action"]:
                            "cancel",
                        })
                    : undefined
                }
                onDecline={
                  pendingToolInputSupportsDecline
                    ? () =>
                        void respondApproval(pendingToolInputApproval.approvalId, {
                          [pendingToolInputIsOpenCodeQuestion ? "decision" : "action"]:
                            "decline",
                        })
                    : undefined
                }
                onSubmit={(response) => {
                  void respondApproval(pendingToolInputApproval.approvalId, response);
                }}
              />
            ) : showPlanImplementationComposer ? (
              <ToolInputQuestionnaire
                details={planImplementationQuestionDetails}
                allowCustomAnswer={false}
                submitLabel={t("panel.continue")}
                onSubmit={handlePlanImplementationQuestionnaireSubmit}
              />
            ) : (
              <>
                {/* Composer references and attachments */}
                {(references.length > 0 || attachments.length > 0 || textAnnotations.length > 0) && (
                  <div className="chat-attachments-bar">
                    {textAnnotations.map((annotation, index) => (
                      <div key={annotation.id} className="chat-text-annotation-chip">
                        <div className="chat-text-annotation-chip-header">
                          <span className="chat-text-annotation-chip-label">
                            <MessageSquare size={11} />
                            {t("panel.textAnnotations.item", { count: index + 1 })}
                          </span>
                          <button
                            type="button"
                            className="chat-attachment-chip-remove"
                            onClick={() =>
                              setTextAnnotations((current) =>
                                current.filter((candidate) => candidate.id !== annotation.id),
                              )
                            }
                            title={t("panel.textAnnotations.remove")}
                            aria-label={t("panel.textAnnotations.remove")}
                          >
                            <X size={10} />
                          </button>
                        </div>
                        <span
                          className="chat-text-annotation-chip-selection"
                          title={annotation.selectedText}
                        >
                          {annotation.selectedText}
                        </span>
                        <span className="chat-text-annotation-chip-comment">
                          {annotation.comment}
                        </span>
                      </div>
                    ))}
                    {references.map((reference) => (
                      <span
                        key={`${reference.type}:${reference.path}`}
                        className={`chat-attachment-chip ${
                          reference.type === "skill"
                            ? "chat-attachment-chip--skill"
                            : "chat-attachment-chip--mention"
                        }`}
                      >
                        {reference.type === "skill" ? (
                          <DollarSign size={12} />
                        ) : (
                          <AtSign size={12} />
                        )}
                        <span className="chat-attachment-chip-name">
                          {reference.name}
                        </span>
                        <button
                          type="button"
                          className="chat-attachment-chip-remove"
                          onClick={() =>
                            setReferences((currentReferences) =>
                              currentReferences.filter(
                                (candidate) =>
                                  candidate.type !== reference.type ||
                                  candidate.path !== reference.path,
                              ),
                            )
                          }
                          title={t("attachments.remove")}
                          aria-label={t("attachments.remove")}
                        >
                          <X size={10} />
                        </button>
                      </span>
                    ))}
                    {attachments.map((attachment) => {
                      return (
                        <AttachmentChip
                          key={attachment.id}
                          attachment={attachment}
                          showSize
                          removeLabel={t("attachments.remove")}
                          onRemove={() => removeAttachment(attachment.id)}
                          onOpen={activeWorkspaceId
                            ? () => openImageAttachmentPreview(activeWorkspaceId, attachment.id)
                            : undefined}
                        />
                      );
                    })}
                  </div>
                )}

                {/* Slash command panel (inline) */}
                {activeCommandPanel && (
                  <ChatCommandPanel
                    command={activeCommandPanel}
                    busy={commandPanelBusy}
                    error={commandPanelError}
                    defaultBaseBranch={
                      gitStatus?.branch ?? null
                    }
                    currentServiceTier={selectedServiceTier}
                    currentPersonality={selectedPersonality}
                    personalitySupported={selectedModelSupportsPersonality}
                    skills={
                      codexReferenceCatalogState.skillsLoaded
                        ? codexSkills
                        : (codexProtocolDiagnostics?.skills ?? [])
                    }
                    extensionSkills={
                      selectedEngineId === "claude"
                        ? effectiveExtensionItems.filter((item) => item.kind === "skill")
                        : undefined
                    }
                    pluginMarketplaces={
                      activeWorkspace?.locationKind === "ssh" &&
                      selectedEngineId === "codex"
                        ? [
                            {
                              name: "SSH",
                              path: codexReferenceRoot ?? "",
                              plugins: codexPlugins,
                            },
                          ]
                        : codexProtocolDiagnostics?.pluginMarketplaces
                    }
                    extensionPlugins={
                      selectedEngineId === "claude"
                        ? effectiveExtensionItems.filter((item) => item.kind === "plugin")
                        : undefined
                    }
                    openCodeAgents={openCodeCatalog?.agents}
                    openCodeCommands={openCodeCatalog?.commands}
                    openCodeMcpServers={
                      selectedEngineId === "opencode"
                        ? openCodeCatalog?.mcpServers ?? []
                        : undefined
                    }
                    workspaceId={activeWorkspaceId}
                    selectedModelId={selectedModelId}
                    onAttachOpenCodeSession={onAttachOpenCodeRemoteSession}
                    mcpServers={
                      selectedEngineId === "opencode"
                        ? undefined
                        : codexProtocolDiagnostics?.mcpServers
                    }
                    extensionMcpServers={
                      selectedEngineId === "claude"
                        ? effectiveExtensionItems.filter((item) => item.kind === "mcp")
                        : undefined
                    }
                    experimentalFeatures={codexProtocolDiagnostics?.experimentalFeatures}
                    onConfirm={handleCommandPanelConfirm}
                    onDismiss={() => {
                      setActiveCommandPanel(null);
                      setCommandPanelError(null);
                    }}
                  />
                )}

                <ChatComposerInput
                  input={input}
                  inputRef={inputRef}
                  inputHeight={chatInputHeightValue}
                  activeWorkspaceId={activeWorkspaceId}
                  activePlanMode={activePlanMode}
                  planMode={planMode}
                  isOpenCodeEngine={isOpenCodeEngine}
                  streaming={streaming}
                  canSteerActiveTurn={canSteerActiveTurn}
                  sessionMessageSendMode={sessionMessageSendMode}
                  sendShortcut={sendShortcut}
                  threadMentionMenuOpen={threadMentionMenuOpen}
                  threadMentionCandidates={threadMentionCandidates}
                  threadMentionActiveIndex={threadMentionActiveIndex}
                  slashMenuOpen={slashMenuOpen}
                  filteredSlashCommands={filteredSlashCommands}
                  slashMenuActiveIndex={slashMenuActiveIndex}
                  activeCommandPanel={activeCommandPanel}
                  activeThread={activeThread}
                  activeChatSessionId={activeChatSessionId}
                  inputHistoryRef={inputHistoryRef}
                  inputHistCursorRef={inputHistCursorRef}
                  inputLiveDraftRef={inputLiveDraftRef}
                  setInput={setInput}
                  setPlanMode={setPlanMode}
                  setThreadMentionMenuOpen={setThreadMentionMenuOpen}
                  setThreadMentionActiveIndex={setThreadMentionActiveIndex}
                  setSlashMenuOpen={setSlashMenuOpen}
                  setSlashMenuActiveIndex={setSlashMenuActiveIndex}
                  setActiveCommandPanel={setActiveCommandPanel}
                  setCommandPanelError={setCommandPanelError}
                  handleSlashDetection={handleSlashDetection}
                  handleThreadMentionDetection={handleThreadMentionDetection}
                  handleThreadMentionSelect={handleThreadMentionSelect}
                  handleSlashCommandSelect={handleSlashCommandSelect}
                  onSubmit={onSubmit}
                  saveThreadRuntimeSelectionPatch={saveThreadRuntimeSelectionPatch}
                />
              </>
            )}

            {/* Input toolbar with selectors */}
            <ChatComposerToolbar
              key={composerSettingsScopeKey ?? "no-session"}
              showSpecialInputComposer={showSpecialInputComposer}
              activeWorkspaceId={activeWorkspaceId}
              attachments={attachments}
              handleAddAttachment={handleAddAttachment}
              isOpenCodeEngine={isOpenCodeEngine}
              openCodeSelectableAgents={openCodeSelectableAgents}
              selectedOpenCodeAgent={selectedOpenCodeAgent}
              onOpenCodeAgentChange={onOpenCodeAgentChange}
              openCodeCatalogLoaded={openCodeCatalogLoaded}
              openCodeCatalog={openCodeCatalog}
              openCodeCatalogLoading={openCodeCatalogLoading}
              openCodeCatalogError={openCodeCatalogError}
              codexSkills={codexSkills}
              codexPlugins={codexPlugins}
              codexReferenceCatalogLoading={codexReferenceCatalogLoading}
              codexReferenceCatalogError={codexReferenceCatalogError}
              codexReferenceCatalogState={codexReferenceCatalogState}
              activePlanMode={activePlanMode}
              planMode={planMode}
              sessionMessageSendMode={sessionMessageSendMode}
              activeThread={activeThread}
              activeThreadRuntimeLocked={
                activeThreadInCurrentWorkspace && activeThreadRuntimeLocked
              }
              engines={engines}
              health={health}
              selectedEngineId={selectedEngineId}
              selectedEngine={selectedEngine}
              selectedModelId={selectedModelId}
              selectedModel={selectedModel}
              selectedEffort={selectedEffort}
              selectedServiceTier={selectedServiceTier}
              enginesLoading={enginesLoading}
              engineLoadError={engineLoadError}
              permissionComponent={permissionComponent}
              contextUsage={contextUsage}
              contextUsageLoading={contextUsageLoading}
              streaming={streaming}
              canSubmitComposer={canSubmitComposer}
              isSubmitting={isSubmitting}
              selectedModelIdRef={selectedModelIdRef}
              selectedEngineIdRef={selectedEngineIdRef}
              selectedEffortRef={selectedEffortRef}
              manuallyOverrodeThreadSelectionRef={manuallyOverrodeThreadSelectionRef}
              setPlanMode={setPlanMode}
              saveThreadRuntimeSelectionPatch={saveThreadRuntimeSelectionPatch}
              setThreadMessageSendMode={setThreadMessageSendMode}
              setPendingMessageSendMode={setPendingMessageSendMode}
              loadOpenCodeRuntimeCatalog={loadOpenCodeRuntimeCatalog}
              refreshCodexReferenceCatalogs={refreshCodexReferenceCatalogs}
              setHasExplicitComposerRuntime={setHasExplicitComposerRuntime}
              setSelectedEngineId={setSelectedEngineId}
              setSelectedModelId={setSelectedModelId}
              setSelectedEffort={setSelectedEffort}
              onReasoningEffortChange={onReasoningEffortChange}
              onCodexConfigSave={onCodexConfigSave}
              loadEngines={loadEngines}
              openUsageLimitsModal={openUsageLimitsModal}
              cancel={cancel}
            />
          </div>

          <ChatStatusBar
            isCodexEngine={isCodexEngine}
            selectedEngineId={selectedEngineId}
            usageLimits={usageLimits}
            usageLimitsLoading={usageLimitsLoading}
            selectedClaudeWeeklyUsage={selectedClaudeWeeklyUsage}
            hasUserMessage={hasUserMessage}
            streaming={streaming}
            gitStatus={gitStatus}
            error={error}
            onOpenUsageLimits={openUsageLimitsModal}
          />
        </form>
            </div>
        </div>

        {/* Resize handle — split mode only */}
        {layoutMode === "split" && (
          <div className="layout-resize-handle-vertical" onMouseDown={handleResizeStart} />
        )}

        {/* Terminal section */}
        <div
          className="terminal-section"
          style={{
            flex: (layoutMode === "chat" || layoutMode === "editor") ? "0 0 0px"
                 : layoutMode === "terminal" ? "1 1 0px"
                 : `0 0 ${terminalPanelSize}%`,
            overflow: "hidden",
            visibility: (layoutMode === "chat" || layoutMode === "editor") ? "hidden" : "visible",
          }}
        >
          {hasTerminalMountedRef.current && activeWorkspaceId && (
            <div className="terminal-split-panel" style={{ height: "100%" }}>
              <Suspense
                fallback={
                  <div
                    style={{
                      height: "100%",
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "center",
                      fontSize: 12,
                      color: "var(--text-3)",
                    }}
                  >
                    {t("panel.loadingTerminal")}
                  </div>
                }
              >
                <LazyTerminalPanel workspaceId={activeWorkspaceId} />
              </Suspense>
            </div>
          )}
        </div>

        {/* Editor section */}
        <div
          style={{
            flex: layoutMode === "editor" ? "1 1 0px" : "0 0 0px",
            minHeight: 0,
            overflow: "hidden",
            visibility: layoutMode === "editor" ? "visible" : "hidden",
          }}
        >
          {hasEditorMountedRef.current && (
            <Suspense
              fallback={
                <div
                  style={{
                    height: "100%",
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "center",
                    fontSize: 12,
                    color: "var(--text-3)",
                  }}
                >
                  {t("panel.loadingEditor")}
                </div>
              }
            >
              <LazyEditorWithExplorer />
            </Suspense>
          )}
        </div>
      </div>

      <ConfirmDialog
        open={envMismatchPrompt !== null}
        title={t("threadEnvMismatch.title")}
        message={
          envMismatchPrompt
            ? t(
                envMismatchPrompt.mismatch.kind === "engine"
                  ? "threadEnvMismatch.engineMessage"
                  : envMismatchPrompt.mismatch.kind === "model"
                    ? "threadEnvMismatch.modelMessage"
                    : "threadEnvMismatch.effortMessage",
                {
                  saved: envMismatchPrompt.mismatch.savedValue,
                  fallback: envMismatchPrompt.mismatch.fallbackLabel,
                },
              )
            : ""
        }
        confirmLabel={t("threadEnvMismatch.confirm")}
        cancelLabel={t("threadEnvMismatch.cancel")}
        confirmVariant="primary"
        onConfirm={handleEnvMismatchConfirm}
        onCancel={handleEnvMismatchCancel}
      />
    </div>
  );
}
