import {
  FormEvent,
  Suspense,
  lazy,
  memo,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ClipboardEvent as ReactClipboardEvent,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { TFunction } from "i18next";
import {
  Send,
  Loader2,
  Square,
  GitBranch,
  Brain,
  Shield,
  Monitor,
  SquareTerminal,
  MessageSquare,
  FilePen,
  Pencil,
  AlertTriangle,
  AtSign,
  DollarSign,
  Puzzle,
  Plus,
  ListChecks,
  Copy,
  Check,
  Clock,
  Zap,
  RotateCcw,
  Minimize2,
  Search,
  Scissors,
  Sparkles,
  Server,
  SquareCode,
  FlaskConical,
  UserCircle,
  Lightbulb,
  Eye,
  Compass,
  BookOpen,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { useShallow } from "zustand/react/shallow";
import { useChatStore } from "../../stores/chatStore";
import {
  getChatComposerSessionKey,
  useChatComposerStore,
} from "../../stores/chatComposerStore";
import type { PendingFlexibleMessage } from "../../stores/chatComposerStore";
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
import { getHarnessIcon } from "../shared/HarnessLogos";
import { showWorkspaceEditorForDirectFileOpen } from "../../lib/workspacePaneNavigation";
import {
  resolveRelativePathWithinRoot,
} from "../../lib/fileRootUtils";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useGitStore } from "../../stores/gitStore";
import { useTerminalStore, type LayoutMode } from "../../stores/terminalStore";
import { toast } from "../../stores/toastStore";
import { ipc, listenExtensionCatalogUpdated } from "../../lib/ipc";
// 斜杠菜单统一由 buildSlashCommandsFromExtensions 按 ExtensionItemDto 解析。
import { buildSlashCommandsFromExtensions } from "../../cli-tools/build-slash-commands";
import type { CliSlashCommand } from "../../cli-tools/contracts/slash-command";
import {
  // 旧 PermissionPicker 的 CLI 权限适配导入已停用，保留迁移记录：
  // autonomyPresetExecutionPolicyRequest,
  // autonomyPresetPatch,
  // detectAutonomyPreset,
  isAutonomyPresetId,
} from "../../lib/autonomyPresets";
import type { AutonomyPresetId } from "../../lib/autonomyPresets";
import {
  codexUsesExternalSandbox,
  // 旧 PermissionPicker 保存分支使用，现已停用：
  // isCodexExternalSandboxWarning,
} from "../../lib/codexSandbox";
import { resolvePreferredOnboardingChatSelection } from "../../lib/onboarding";
import { recordPerfMetric } from "../../lib/perfTelemetry";
import { isMacDesktop, usesCustomWindowFrame } from "../../lib/windowActions";
import { MessageBlocks, shouldShowClaudeUnsupportedApproval } from "./MessageBlocks";
import { resolveEngineCapabilities } from "./engineCapabilities";
import { buildCodexInputItems, buildSelectedCodexInputItems } from "./codexInputItems";
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
import { canChangeUnstartedThreadEngine } from "./threadRuntimeState";
import { resolveReasoningEffortForModel } from "./reasoningEffort";
import { resolveUsageStatusKey } from "./usageStatus";
import { formatWorkingDuration } from "./workingDuration";
import { formatTextAnnotationsForSubmission } from "./textAnnotations";
import {
  formatImageAttachmentAnnotationsForSubmission,
  hasImageAttachmentAnnotations,
  stripImageAttachmentAnnotationsForSubmission,
} from "./imageAttachmentAnnotations";
import { ToolInputQuestionnaire } from "./ToolInputQuestionnaire";
import {
  buildMcpElicitationApprovalResponse,
  buildPermissionsApprovalResponse,
  buildPermissionsDeclineResponse,
  isMcpElicitationApproval,
  isPermissionsRequestApproval,
  isRequestUserInputApproval,
  isSupportedClaudeToolInputApproval,
  parseApprovalCommand,
  parseApprovalReason,
  parseProposedExecpolicyAmendment,
  parseProposedNetworkPolicyAmendments,
  parseToolInputQuestions,
  requiresCustomApprovalPayload,
} from "./toolInputApproval";
import { ModelPicker } from "./ModelPicker";
import { RuntimeTargetPicker } from "./RuntimeTargetPicker";
import { AttachmentChip } from "./AttachmentChip";
import {
  type CodexConfigPatch,
  type CodexPersonalityValue,
  type CodexServiceTierValue,
} from "./CodexConfigPicker";
import { PermissionPicker } from "./PermissionPicker";
import { OpenCodeAgentPicker } from "./OpenCodeAgentPicker";
// CodexReviewPicker and CodexThreadPicker replaced by slash commands (ChatSlashMenu + ChatCommandPanel)
import { ChatSlashMenu } from "./ChatSlashMenu";
import { ChatThreadMentionMenu } from "./ChatThreadMentionMenu";
import { ChatCommandPanel, type ActiveSlashCommand } from "./ChatCommandPanel";
import { ConfirmDialog } from "../shared/ConfirmDialog";
import { Dropdown } from "../shared/Dropdown";
import { handleDragMouseDown, handleDragDoubleClick } from "../../lib/windowDrag";
import { shouldSubmitChatInput } from "./chatInputShortcuts";
import type {
  ActionType,
  ApprovalBlock,
  ApprovalResponse,
  AttachmentBlock,
  ChatAttachment,
  ChatInputItem,
  ChatInputReference,
  ChatTextAnnotation,
  // 旧 PermissionPicker 请求适配使用，现已停用：
  // CodexApprovalsReviewer,
  CodexApp,
  CodexPlugin,
  CodexSkill,
  ContentBlock,
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

const MESSAGE_VIRTUALIZATION_THRESHOLD = 40;
const MESSAGE_ESTIMATED_ROW_HEIGHT = 220;
const MESSAGE_ROW_GAP = 12;
const EMPTY_CHAT_ATTACHMENTS: ChatAttachment[] = [];
const EMPTY_CHAT_INPUT_REFERENCES: ChatInputReference[] = [];
const EMPTY_CHAT_TEXT_ANNOTATIONS: ChatTextAnnotation[] = [];
const EMPTY_PENDING_FLEXIBLE_MESSAGES: PendingFlexibleMessage[] = [];
const EMPTY_PERMISSION_COMPONENT: PermissionComponentJson = {
  autonomyPreset: ["automatic"],
  trust: ["automatic"],
  approval: ["automatic"],
  sandbox: ["automatic"],
  network: ["automatic"],
  defaultForNewThreads: [],
};

/** 线程运行时五项可局部保存字段；权限字段不属于本契约。 */
type ThreadRuntimeSelectionPatch = {
  engineId?: string | null;
  modelId?: string | null;
  planMode?: boolean | null;
  sendMethod?: string | null;
  reasoningEffort?: string | null;
};

/**
 * 判断权限保存等待结果；先处理更新中的新请求，再处理当前请求失败，
 * 保证旧请求清理 ref 后的失败不会被发送流程忽略。
 */
export function permissionSaveWaitAction(
  permissionSaved: boolean,
  pendingPermissionRequest: Promise<boolean>,
  latestPendingPermissionRequest: Promise<boolean> | undefined,
): "continue" | "fail" | "complete" {
  if (
    latestPendingPermissionRequest &&
    latestPendingPermissionRequest !== pendingPermissionRequest
  ) {
    return "continue";
  }
  if (!permissionSaved) {
    return "fail";
  }
  return "complete";
}

/**
 * 判断权限变更是否需要同步更新项目级信任等级，避免相同或非法等级触发无效写入。
 */
export function shouldUpdateWorkspaceTrustLevel(
  nextTrust: string | null,
  currentTrust: TrustLevel | null | undefined,
): nextTrust is TrustLevel {
  return (
    (nextTrust === "trusted" || nextTrust === "standard" || nextTrust === "restricted")
    && nextTrust !== currentTrust
  );
}

/** 统一权限组件使用 automatic 表示旧全局默认 inherit。 */
function autonomyPresetToComponentValue(preset: AutonomyPresetId | null): string | null {
  if (preset === "inherit") return "automatic";
  return preset;
}

/** 将统一组件的 automatic 默认值还原为现有全局默认 inherit。 */
function componentValueToAutonomyPreset(value: string | null): AutonomyPresetId | null {
  if (value === "automatic") return "inherit";
  return isAutonomyPresetId(value) ? value : null;
}

// 旧的经典输入菜单项目结构保留在此处作为迁移记录；现在由 CLI 菜单契约统一描述选择动作。
// type ClassicSlashCommand = SlashCommand & {
//   reference?: ChatInputReference;
//   panel?: ActiveSlashCommand;
//   insertText?: string;
// };

interface TextAnnotationPopover {
  selectedText: string;
  left: number;
  top: number;
  stage: "actions" | "comment";
}

function createPendingSubmissionMessage(
  threadId: string,
  text: string,
  attachments: ChatAttachment[],
  references: ChatInputReference[],
  planMode: boolean,
): Message {
  const blocks: ContentBlock[] = [
    ...references.map((reference) => ({ ...reference })),
    ...attachments.map((attachment) => ({
      type: "attachment" as const,
      fileName: attachment.fileName,
      filePath: attachment.filePath,
      sizeBytes: attachment.sizeBytes,
      mimeType: attachment.mimeType,
      browserAnnotation: attachment.browserAnnotation,
    })),
  ];
  blocks.push({ type: "text", content: text, planMode: planMode || undefined });

  return {
    id: `pending-${crypto.randomUUID()}`,
    threadId,
    role: "user",
    content: text,
    blocks,
    status: "completed",
    schemaVersion: 1,
    createdAt: new Date().toISOString(),
    hydration: "full",
    hasDeferredContent: false,
  };
}
const MESSAGE_OVERSCAN_PX = 700;
const LazyTerminalPanel = lazy(() =>
  import("../terminal/TerminalPanel").then((module) => ({
    default: module.TerminalPanel,
  })),
);
const LazyEditorWithExplorer = lazy(() =>
  import("../editor/EditorWithExplorer").then((module) => ({
    default: module.EditorWithExplorer,
  })),
);

interface MeasuredMessageRowProps {
  messageId: string;
  onHeightChange: (messageId: string, height: number) => void;
  children: ReactNode;
}

export function resolvePendingToolInputApproval(
  pendingApprovals: ApprovalBlock[],
  engineId?: string,
  preferredApprovalId?: string | null,
): ApprovalBlock | null {
  const eligibleApprovals = pendingApprovals.filter((approval) => {
    const details = approval.details ?? {};
    if (
      !isRequestUserInputApproval(details) ||
      parseToolInputQuestions(details).length === 0
    ) {
      return false;
    }

    return engineId !== "claude" || isSupportedClaudeToolInputApproval(details);
  });

  if (eligibleApprovals.length === 0) {
    return null;
  }

  if (preferredApprovalId) {
    const preferredApproval = eligibleApprovals.find(
      (approval) => approval.approvalId === preferredApprovalId,
    );
    if (preferredApproval) {
      return preferredApproval;
    }
  }

  return eligibleApprovals[eligibleApprovals.length - 1] ?? null;
}

export function filterPendingApprovalBannerRows(
  pendingApprovals: ApprovalBlock[],
  engineId?: string,
  activeToolInputApprovalId?: string | null,
): ApprovalBlock[] {
  return pendingApprovals.filter((approval) => {
    const details = approval.details ?? {};
    if (!isRequestUserInputApproval(details)) {
      return true;
    }

    if (parseToolInputQuestions(details).length === 0) {
      return true;
    }

    if (approval.approvalId === activeToolInputApprovalId) {
      return false;
    }

    return engineId === "claude";
  });
}

export function isOpenCodeQuestionApproval(details?: Record<string, unknown>): boolean {
  return details?._opencodeRequestKind === "question";
}

export function canUseApprovalDecisionActions(
  engineId?: string,
  details?: Record<string, unknown>,
): boolean {
  return engineId !== "opencode" || !isOpenCodeQuestionApproval(details);
}

export function canBatchApproveApproval(
  approval: ApprovalBlock,
  engineId?: string,
): boolean {
  const details = approval.details ?? {};
  return (
    canUseApprovalDecisionActions(engineId, details) &&
    !isRequestUserInputApproval(details) &&
    !requiresCustomApprovalPayload(details) &&
    !shouldShowClaudeUnsupportedApproval(details, true, engineId === "claude")
  );
}

function approvalRowIcon(actionType: ActionType) {
  switch (actionType) {
    case "command":
      return <SquareTerminal size={13} />;
    case "file_write":
    case "file_edit":
    case "file_delete":
      return <FilePen size={13} />;
    case "git":
      return <GitBranch size={13} />;
    default:
      return <Shield size={13} />;
  }
}

export function buildPermissionApprovalResponseForEngine(
  engineId: string | undefined,
  details: Record<string, unknown> | undefined,
  decision: "accept" | "decline" | "accept_for_session",
): ApprovalResponse {
  if (engineId === "opencode") {
    return { decision };
  }

  if (decision === "decline") {
    return buildPermissionsDeclineResponse();
  }

  return buildPermissionsApprovalResponse(
    details,
    decision === "accept_for_session" ? "session" : "turn",
  );
}

function MeasuredMessageRow({ messageId, onHeightChange, children }: MeasuredMessageRowProps) {
  const rowRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const element = rowRef.current;
    if (!element) {
      return;
    }

    const publishHeight = () => {
      onHeightChange(messageId, element.getBoundingClientRect().height);
    };

    publishHeight();

    if (typeof ResizeObserver === "undefined") {
      return;
    }

    const observer = new ResizeObserver(() => publishHeight());
    observer.observe(element);
    return () => observer.disconnect();
  }, [messageId, onHeightChange]);

  return <div ref={rowRef}>{children}</div>;
}

const MODEL_TOKEN_LABELS: Record<string, string> = {
  gpt: "GPT",
  codex: "Codex",
  mini: "Mini",
  nano: "Nano",
};

type CodexThreadApprovalPolicyValue =
  | "inherit"
  | "untrusted"
  | "on-request"
  | "never"
  | "custom";
/*
 * 旧 PermissionPicker 的 CLI 权限适配类型已停用，保留迁移记录：
type ClaudeThreadPermissionModeValue = "inherit" | "restricted" | "standard" | "trusted";
type OpenCodeThreadPermissionModeValue = "inherit" | "ask" | "allow" | "deny";
type ThreadApprovalPolicyValue =
  | CodexThreadApprovalPolicyValue
  | ClaudeThreadPermissionModeValue
  | OpenCodeThreadPermissionModeValue;
type ThreadApprovalPolicyStateValue =
  | ThreadApprovalPolicyValue
  | Record<string, unknown>;
type ThreadSandboxModeValue =
  | "inherit"
  | "read-only"
  | "workspace-write"
  | "danger-full-access";
type ThreadNetworkPolicyValue = "inherit" | "enabled" | "restricted";
type ThreadExecutionPolicyPatch = Partial<{
  approvalPolicy: ThreadApprovalPolicyStateValue;
  sandboxMode: ThreadSandboxModeValue;
  networkPolicy: ThreadNetworkPolicyValue;
  permissionProfile: Record<string, unknown> | null;
  approvalsReviewer: CodexApprovalsReviewer | null;
}>;
 */
type ThreadSandboxModeValue =
  | "inherit"
  | "read-only"
  | "workspace-write"
  | "danger-full-access";
interface CodexReferenceCatalogState {
  skillsLoaded: boolean;
  appsLoaded: boolean;
}

/* 旧 PermissionPicker 逐 CLI 选项构造函数已停用，保留完整迁移记录。
function getTrustLevelOptions(
  t: TFunction<"chat">,
): Array<{ value: TrustLevel; label: string; description: string }> {
  return [
    {
      value: "trusted",
      label: t("policy.trusted"),
      description: t("policy.trustedDescription"),
    },
    {
      value: "standard",
      label: t("policy.standard"),
      description: t("policy.standardDescription"),
    },
    {
      value: "restricted",
      label: t("policy.restricted"),
      description: t("policy.restrictedDescription"),
    },
  ];
}

function getCodexThreadApprovalPolicyOptions(
  t: TFunction<"chat">,
): Array<{
  value: CodexThreadApprovalPolicyValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoRepoTrust"),
    },
    {
      value: "untrusted",
      label: t("policy.untrusted"),
      description: t("policy.untrustedDescription"),
    },
    {
      value: "on-request",
      label: t("policy.onRequest"),
      description: t("policy.onRequestDescription"),
    },
    {
      value: "never",
      label: t("policy.never"),
      description: t("policy.neverDescription"),
    },
  ];
}

function getClaudeThreadPermissionModeOptions(
  t: TFunction<"chat">,
): Array<{
  value: ClaudeThreadPermissionModeValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoClaude"),
    },
    {
      value: "restricted",
      label: t("policy.restricted"),
      description: t("policy.claudeRestrictedDescription"),
    },
    {
      value: "standard",
      label: t("policy.standard"),
      description: t("policy.claudeStandardDescription"),
    },
    {
      value: "trusted",
      label: t("policy.trusted"),
      description: t("policy.claudeTrustedDescription"),
    },
  ];
}

function getOpenCodeThreadPermissionModeOptions(
  t: TFunction<"chat">,
): Array<{
  value: OpenCodeThreadPermissionModeValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoOpenCode"),
    },
    {
      value: "ask",
      label: t("policy.openCodeAsk"),
      description: t("policy.openCodeAskDescription"),
    },
    {
      value: "allow",
      label: t("policy.openCodeAllow"),
      description: t("policy.openCodeAllowDescription"),
    },
    {
      value: "deny",
      label: t("policy.openCodeDeny"),
      description: t("policy.openCodeDenyDescription"),
    },
  ];
}

function getThreadSandboxModeOptions(
  t: TFunction<"chat">,
): Array<{
  value: ThreadSandboxModeValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoSandbox"),
    },
    {
      value: "read-only",
      label: t("policy.readOnly"),
      description: t("policy.readOnlyDescription"),
    },
    {
      value: "workspace-write",
      label: t("policy.workspaceWrite"),
      description: t("policy.workspaceWriteDescription"),
    },
    {
      value: "danger-full-access",
      label: t("policy.fullAccess"),
      description: t("policy.fullAccessDescription"),
    },
  ];
}

function getThreadNetworkPolicyOptions(
  t: TFunction<"chat">,
): Array<{
  value: ThreadNetworkPolicyValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoNetwork"),
    },
    {
      value: "enabled",
      label: t("policy.enabled"),
      description: t("policy.enabledDescription"),
    },
    {
      value: "restricted",
      label: t("policy.restricted"),
      description: t("policy.networkRestrictedDescription"),
    },
  ];
}
*/

const IMAGE_ATTACHMENT_EXTENSIONS = new Set([
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "bmp",
  "tif",
  "tiff",
  "svg",
]);
const TEXT_ATTACHMENT_EXTENSIONS = new Set([
  "txt",
  "md",
  "json",
  "js",
  "ts",
  "tsx",
  "jsx",
  "py",
  "rs",
  "go",
  "css",
  "html",
  "yaml",
  "yml",
  "toml",
  "xml",
  "sql",
  "sh",
  "csv",
]);
const CODEX_ATTACHMENT_EXTENSIONS = Array.from(
  new Set([...IMAGE_ATTACHMENT_EXTENSIONS, ...TEXT_ATTACHMENT_EXTENSIONS]),
);
const CLAUDE_TEXT_ATTACHMENT_EXTENSIONS = Array.from(
  new Set([...TEXT_ATTACHMENT_EXTENSIONS, "svg"]),
);
const CLAUDE_IMAGE_ATTACHMENT_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp"];
const CLAUDE_ATTACHMENT_EXTENSIONS = Array.from(
  new Set([...CLAUDE_TEXT_ATTACHMENT_EXTENSIONS, ...CLAUDE_IMAGE_ATTACHMENT_EXTENSIONS]),
);
const PDF_ATTACHMENT_EXTENSIONS = ["pdf"];
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

interface AttachmentFilterConfig {
  supportedExtensions: string[];
  textExtensions: string[];
  imageExtensions: string[];
  title: string;
  warningMessage: string;
  supportedLabel: string;
  imagesLabel: string;
  textFilesLabel: string;
}

function attachmentExtensionsForModalities(modalities: string[]): {
  supportedExtensions: string[];
  textExtensions: string[];
  imageExtensions: string[];
} {
  const normalized = new Set(modalities.map((modality) => modality.trim().toLowerCase()));
  const textExtensions = normalized.has("text") ? [...TEXT_ATTACHMENT_EXTENSIONS] : [];
  const imageExtensions = normalized.has("image") ? [...IMAGE_ATTACHMENT_EXTENSIONS] : [];
  const pdfExtensions = normalized.has("pdf") ? PDF_ATTACHMENT_EXTENSIONS : [];

  return {
    supportedExtensions: Array.from(
      new Set([...textExtensions, ...imageExtensions, ...pdfExtensions]),
    ),
    textExtensions,
    imageExtensions,
  };
}

function getAttachmentFilterConfig(
  t: TFunction<"chat">,
  engineId: string,
  model?: EngineModel | null,
): AttachmentFilterConfig | null {
  switch (engineId) {
    case "codex":
      return {
        supportedExtensions: CODEX_ATTACHMENT_EXTENSIONS,
        textExtensions: [...TEXT_ATTACHMENT_EXTENSIONS],
        imageExtensions: [...IMAGE_ATTACHMENT_EXTENSIONS],
        title: t("attachments.codexTitle"),
        warningMessage: t("attachments.codexWarning"),
        supportedLabel: t("attachments.filters.supportedFiles"),
        imagesLabel: t("attachments.filters.images"),
        textFilesLabel: t("attachments.filters.textFiles"),
      };
    case "claude":
      return {
        supportedExtensions: CLAUDE_ATTACHMENT_EXTENSIONS,
        textExtensions: CLAUDE_TEXT_ATTACHMENT_EXTENSIONS,
        imageExtensions: CLAUDE_IMAGE_ATTACHMENT_EXTENSIONS,
        title: t("attachments.claudeTitle"),
        warningMessage: t("attachments.claudeWarning"),
        supportedLabel: t("attachments.filters.supportedFiles"),
        imagesLabel: t("attachments.filters.images"),
        textFilesLabel: t("attachments.filters.textFiles"),
      };
    case "opencode": {
      const openCodeExtensions = attachmentExtensionsForModalities(
        model?.attachmentModalities ?? [],
      );
      return {
        supportedExtensions: openCodeExtensions.supportedExtensions,
        textExtensions: openCodeExtensions.textExtensions,
        imageExtensions: openCodeExtensions.imageExtensions,
        title: t("attachments.opencodeTitle"),
        warningMessage: t("attachments.opencodeWarning"),
        supportedLabel: t("attachments.filters.supportedFiles"),
        imagesLabel: t("attachments.filters.images"),
        textFilesLabel: t("attachments.filters.textFiles"),
      };
    }
    default:
      return null;
  }
}

function formatModelName(modelName: string): string {
  return modelName
    .split("-")
    .filter(Boolean)
    .map((segment) => {
      const lowerSegment = segment.toLowerCase();
      const knownLabel = MODEL_TOKEN_LABELS[lowerSegment];
      if (knownLabel) {
        return knownLabel;
      }
      if (/^\d+(\.\d+)*$/.test(segment)) {
        return segment;
      }
      if (/^[a-z]?\d+(\.\d+)*$/i.test(segment)) {
        return segment.toUpperCase();
      }
      return segment.charAt(0).toUpperCase() + segment.slice(1);
    })
    .join("-");
}

function formatReasoningEffortLabel(
  t: TFunction<"chat">,
  effort?: string,
): string {
  if (!effort) {
    return "";
  }
  switch (effort.toLowerCase()) {
    case "none":
      return t("modelPicker.effort.none");
    case "minimal":
      return t("modelPicker.effort.minimal");
    case "low":
      return t("modelPicker.effort.low");
    case "medium":
      return t("modelPicker.effort.medium");
    case "high":
      return t("modelPicker.effort.high");
    case "xhigh":
      return t("modelPicker.effort.xhigh");
    default:
      break;
  }
  return effort.charAt(0).toUpperCase() + effort.slice(1);
}

function formatEngineModelLabel(
  t: TFunction<"chat">,
  engineName?: string,
  modelDisplayName?: string,
  reasoningEffort?: string,
): string {
  const modelLabel = modelDisplayName ? formatModelName(modelDisplayName) : "";
  const baseLabel = engineName && modelLabel
    ? `${engineName} - ${modelLabel}`
    : modelLabel || engineName || t("panel.assistantFallback");
  const effortLabel = formatReasoningEffortLabel(t, reasoningEffort);
  return effortLabel ? `${baseLabel} ${effortLabel}` : baseLabel;
}

function serializePrettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return "";
  }
}

function isCustomCodexApprovalPolicyValue(
  value: unknown,
): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/*
// 旧实现从 threads.engine_metadata_json 读取 Codex 权限，已停用；权限统一由
// ipc.getThreadPermissions 返回的 PermissionComponentJson 提供。
function readCodexThreadApprovalPolicyValue(thread: Thread | null): CodexThreadApprovalPolicyValue {
  const value = thread?.engineMetadata?.sandboxApprovalPolicy;
  if (value === "on-failure") {
    return "on-request";
  }
  if (value === "untrusted" || value === "on-request" || value === "never") {
    return value;
  }
  if (isCustomCodexApprovalPolicyValue(value)) {
    return "custom";
  }
  return "inherit";
}

function readCodexThreadCustomApprovalPolicyText(thread: Thread | null): string {
  const value = thread?.engineMetadata?.sandboxApprovalPolicy;
  return isCustomCodexApprovalPolicyValue(value) ? serializePrettyJson(value) : "";
}
*/

function readThreadPersonalityValue(thread: Thread | null): CodexPersonalityValue {
  const value = thread?.engineMetadata?.personality;
  if (value === "none" || value === "friendly" || value === "pragmatic") {
    return value;
  }
  return "inherit";
}

function readThreadServiceTierValue(thread: Thread | null): CodexServiceTierValue {
  const value = thread?.engineMetadata?.serviceTier;
  if (value === "fast" || value === "flex") {
    return value;
  }
  return "inherit";
}

function readThreadOutputSchemaText(thread: Thread | null): string {
  const value = thread?.engineMetadata?.outputSchema;
  if (value === undefined || value === null) {
    return "";
  }
  return serializePrettyJson(value);
}

/* 旧 PermissionPicker 的 CLI 权限读取适配已停用，保留完整迁移记录。
function readClaudeThreadPermissionModeValue(
  thread: Thread | null,
): ClaudeThreadPermissionModeValue {
  const value = thread?.engineMetadata?.claudePermissionMode;
  if (value === "restricted" || value === "standard" || value === "trusted") {
    return value;
  }
  return "inherit";
}

function readOpenCodeThreadPermissionModeValue(
  thread: Thread | null,
): OpenCodeThreadPermissionModeValue {
  const value = thread?.engineMetadata?.opencodePermissionMode;
  if (value === "ask" || value === "allow" || value === "deny") {
    return value;
  }
  return "inherit";
}

function readThreadOpenCodeAgentValue(thread: Thread | null): string {
  const value = thread?.engineMetadata?.opencodeAgent;
  return typeof value === "string" && value.trim() ? value.trim() : "build";
}

function readThreadApprovalPolicyValue(thread: Thread | null): ThreadApprovalPolicyValue {
  if (thread?.engineId === "claude") {
    return readClaudeThreadPermissionModeValue(thread);
  }
  if (thread?.engineId === "opencode") {
    return readOpenCodeThreadPermissionModeValue(thread);
  }

  return readCodexThreadApprovalPolicyValue(thread);
}

function readThreadSandboxModeValue(thread: Thread | null): ThreadSandboxModeValue {
  const value = thread?.engineMetadata?.sandboxMode;
  if (value === "read-only" || value === "workspace-write" || value === "danger-full-access") {
    return value;
  }
  return "inherit";
}

function readThreadStoredNetworkPolicyValue(thread: Thread | null): ThreadNetworkPolicyValue {
  const value = thread?.engineMetadata?.sandboxAllowNetwork;
  if (value === true) {
    return "enabled";
  }
  if (value === false) {
    return "restricted";
  }
  return "inherit";
}

function readThreadNetworkPolicyValue(thread: Thread | null): ThreadNetworkPolicyValue {
  if (readThreadSandboxModeValue(thread) === "danger-full-access") {
    return "enabled";
  }

  return readThreadStoredNetworkPolicyValue(thread);
}
*/

function readThreadOpenCodeAgentValue(thread: Thread | null): string {
  const value = thread?.engineMetadata?.opencodeAgent;
  return typeof value === "string" && value.trim() ? value.trim() : "build";
}

/*
// 旧实现从 threads.engine_metadata_json 读取 sandboxMode，已停用；当前活动会话
// 直接使用权限组件返回的统一 sandbox 值。
function readThreadSandboxModeValue(thread: Thread | null): ThreadSandboxModeValue {
  const value = thread?.engineMetadata?.sandboxMode;
  if (value === "read-only" || value === "workspace-write" || value === "danger-full-access") {
    return value;
  }
  return "inherit";
}
*/

/* 旧 PermissionPicker 的本地策略合并、回写和请求适配已停用，保留完整迁移记录。
function readThreadExecutionPolicyState(thread: Thread | null): {
  approvalPolicy: ThreadApprovalPolicyStateValue;
  sandboxMode: ThreadSandboxModeValue;
  networkPolicy: ThreadNetworkPolicyValue;
} {
  // 权限优先读独立字段 permissionMode 的整包 JSON；解析失败回退到 engineMetadata 逐键读取。
  if (typeof thread?.permissionMode === "string" && thread.permissionMode) {
    try {
      const parsed = JSON.parse(thread.permissionMode) as {
        approvalPolicy?: ThreadApprovalPolicyStateValue;
        sandboxMode?: ThreadSandboxModeValue;
        networkPolicy?: ThreadNetworkPolicyValue;
      };
      if (
        parsed &&
        typeof parsed === "object" &&
        parsed.approvalPolicy !== undefined &&
        parsed.sandboxMode !== undefined &&
        parsed.networkPolicy !== undefined
      ) {
        return {
          approvalPolicy: parsed.approvalPolicy,
          sandboxMode: parsed.sandboxMode,
          networkPolicy: parsed.networkPolicy,
        };
      }
    } catch {
      // 落入下面的回退读取。
    }
  }
  const rawApprovalPolicy = thread?.engineMetadata?.sandboxApprovalPolicy;
  return {
    approvalPolicy:
      thread?.engineId === "codex" && isCustomCodexApprovalPolicyValue(rawApprovalPolicy)
        ? rawApprovalPolicy
        : readThreadApprovalPolicyValue(thread),
    sandboxMode: readThreadSandboxModeValue(thread),
    networkPolicy: readThreadStoredNetworkPolicyValue(thread),
  };
}

function applyThreadExecutionPolicyPatch(
  thread: Thread,
  patch: ThreadExecutionPolicyPatch,
): Thread {
  const metadata = { ...(thread.engineMetadata ?? {}) };
  const currentCodexApprovalPolicy = thread.engineMetadata?.sandboxApprovalPolicy;
  const nextApprovalPolicy =
    Object.prototype.hasOwnProperty.call(patch, "approvalPolicy")
      ? patch.approvalPolicy
      : isCustomCodexApprovalPolicyValue(currentCodexApprovalPolicy)
        ? currentCodexApprovalPolicy
        : readThreadApprovalPolicyValue(thread);
  const nextState = {
    ...readThreadExecutionPolicyState(thread),
    ...patch,
    approvalPolicy: nextApprovalPolicy,
  };

  if (thread.engineId === "claude") {
    if (
      nextState.approvalPolicy === "restricted" ||
      nextState.approvalPolicy === "standard" ||
      nextState.approvalPolicy === "trusted"
    ) {
      metadata.claudePermissionMode = nextState.approvalPolicy;
    } else {
      delete metadata.claudePermissionMode;
    }
  } else if (thread.engineId === "opencode") {
    if (
      nextState.approvalPolicy === "ask" ||
      nextState.approvalPolicy === "allow" ||
      nextState.approvalPolicy === "deny"
    ) {
      metadata.opencodePermissionMode = nextState.approvalPolicy;
    } else {
      delete metadata.opencodePermissionMode;
    }
    delete metadata.sandboxMode;
    delete metadata.sandboxAllowNetwork;
  } else {
    if (isCustomCodexApprovalPolicyValue(nextState.approvalPolicy)) {
      metadata.sandboxApprovalPolicy = nextState.approvalPolicy;
    } else if (
      nextState.approvalPolicy === "untrusted" ||
      nextState.approvalPolicy === "on-request" ||
      nextState.approvalPolicy === "never"
    ) {
      metadata.sandboxApprovalPolicy = nextState.approvalPolicy;
    } else {
      delete metadata.sandboxApprovalPolicy;
    }
  }

  if (thread.engineId !== "opencode") {
    if (nextState.sandboxMode === "inherit") {
      delete metadata.sandboxMode;
    } else {
      metadata.sandboxMode = nextState.sandboxMode;
    }

    if (nextState.networkPolicy === "inherit") {
      delete metadata.sandboxAllowNetwork;
    } else {
      metadata.sandboxAllowNetwork = nextState.networkPolicy === "enabled";
    }

    if ("sandboxMode" in patch || "networkPolicy" in patch) {
      delete metadata.permissionProfile;
    }
  }

  if ("permissionProfile" in patch) {
    if (patch.permissionProfile === null || patch.permissionProfile === undefined) {
      delete metadata.permissionProfile;
    } else {
      metadata.permissionProfile = patch.permissionProfile;
      delete metadata.sandboxMode;
      delete metadata.sandboxAllowNetwork;
    }
  }

  if ("approvalsReviewer" in patch) {
    if (patch.approvalsReviewer) {
      metadata.approvalsReviewer = patch.approvalsReviewer;
    } else {
      delete metadata.approvalsReviewer;
    }
  }

  return {
    ...thread,
    engineMetadata: Object.keys(metadata).length > 0 ? metadata : undefined,
  };
}

function toThreadExecutionPolicyRequest(
  patch: ThreadExecutionPolicyPatch,
  clearPermissionProfileOnSandboxChange = false,
): {
  approvalPolicy?: unknown;
  sandboxMode?: string | null;
  allowNetwork?: boolean | null;
  permissionProfile?: Record<string, unknown> | null;
  approvalsReviewer?: CodexApprovalsReviewer | null;
} {
  const request: {
    approvalPolicy?: unknown;
    sandboxMode?: string | null;
    allowNetwork?: boolean | null;
    permissionProfile?: Record<string, unknown> | null;
    approvalsReviewer?: CodexApprovalsReviewer | null;
  } = {};

  if ("approvalPolicy" in patch) {
    request.approvalPolicy = patch.approvalPolicy === "inherit" ? null : patch.approvalPolicy;
  }

  if ("sandboxMode" in patch) {
    request.sandboxMode = patch.sandboxMode === "inherit" ? null : patch.sandboxMode;
  }

  if ("networkPolicy" in patch) {
    request.allowNetwork =
      patch.networkPolicy === "inherit"
        ? null
        : patch.networkPolicy === "enabled";
  }

  if ("permissionProfile" in patch) {
    request.permissionProfile = patch.permissionProfile ?? null;
  } else if (
    clearPermissionProfileOnSandboxChange &&
    ("sandboxMode" in patch || "networkPolicy" in patch)
  ) {
    request.permissionProfile = null;
  }

  if ("approvalsReviewer" in patch) {
    request.approvalsReviewer = patch.approvalsReviewer ?? null;
  }

  return request;
}
*/

function parseStoredOutputSchema(
  text: string,
): Record<string, unknown> | boolean | null {
  const normalized = text.trim();
  if (!normalized) {
    return null;
  }

  const parsed = JSON.parse(normalized) as unknown;
  if (
    typeof parsed !== "boolean" &&
    (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
  ) {
    throw new Error("output schema must be a JSON Schema object or boolean");
  }

  return parsed as Record<string, unknown> | boolean;
}

function parseStoredApprovalPolicy(
  text: string,
): Record<string, unknown> | null {
  const normalized = text.trim();
  if (!normalized) {
    return null;
  }

  const parsed = JSON.parse(normalized) as unknown;
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("approval policy must be a JSON object");
  }

  return parsed as Record<string, unknown>;
}

function encodeModelOptionValue(engineId: string, modelId: string): string {
  return JSON.stringify([engineId, modelId]);
}

function decodeModelOptionValue(value: string): { engineId: string; modelId: string } | null {
  if (!value) {
    return null;
  }

  try {
    const parsed = JSON.parse(value);
    if (
      Array.isArray(parsed) &&
      parsed.length === 2 &&
      typeof parsed[0] === "string" &&
      typeof parsed[1] === "string"
    ) {
      return { engineId: parsed[0], modelId: parsed[1] };
    }
  } catch {
    // Ignore malformed legacy values.
  }

  return null;
}

function readThreadLastModelId(thread: { modelId?: string | null }): string | null {
  const normalized = thread.modelId?.trim();
  return normalized && normalized.length > 0 ? normalized : null;
}

function hasVisibleContent(blocks?: ContentBlock[]): boolean {
  if (!blocks || blocks.length === 0) return false;
  return blocks.some((b) => {
    if (b.type === "text" || b.type === "thinking") return Boolean(b.content?.trim());
    return true;
  });
}

function parseMessageDate(raw?: string): Date | null {
  if (!raw) {
    return null;
  }

  const sqliteUtcPattern = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/;
  const normalized = sqliteUtcPattern.test(raw) ? `${raw.replace(" ", "T")}Z` : raw;
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) {
    return null;
  }

  return date;
}

function formatMessageTimestamp(raw: string | undefined, locale: string): string {
  const date = parseMessageDate(raw);
  if (!date) {
    return "";
  }

  const now = new Date();
  const sameDay = now.toDateString() === date.toDateString();

  if (sameDay) {
    return date.toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" });
  }

  return date.toLocaleString(locale, {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function formatResetTime(
  t: TFunction<"chat">,
  isoDate: string | null,
): string {
  if (!isoDate) return "";
  const date = new Date(isoDate);
  if (Number.isNaN(date.getTime())) return "";
  const now = new Date();
  const diffMs = date.getTime() - now.getTime();
  if (diffMs <= 0) return t("status.now");
  const diffMin = Math.floor(diffMs / 60000);
  if (diffMin < 60) return t("status.minutesShort", { count: diffMin });
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) {
    return t("status.hoursMinutesShort", { hours: diffHr, minutes: diffMin % 60 });
  }
  const diffDays = Math.floor(diffHr / 24);
  return t("status.daysHoursShort", { days: diffDays, hours: diffHr % 24 });
}

function estimateMessageOffset(
  messages: Message[],
  index: number,
  measuredHeights: Map<string, number>,
): number {
  let offset = 0;
  for (let current = 0; current < index; current += 1) {
    const currentMessageId = messages[current].id;
    const rowHeight =
      measuredHeights.get(currentMessageId) ?? MESSAGE_ESTIMATED_ROW_HEIGHT;
    offset += rowHeight + MESSAGE_ROW_GAP;
  }
  return offset;
}

interface MessageRowProps {
  message: Message;
  index: number;
  isHighlighted: boolean;
  assistantLabel: string;
  assistantEngineId: string;
  /** 当前消息回合实际使用的 CLI 名称，用于运行状态文案。 */
  assistantEngineName: string;
  /** 允许当前尾部空流式助手显示首轮会话准备占位。 */
  allowInitialPreparation: boolean;
  /** 允许当前尾部空流式助手在会话就绪后显示已收到消息并思考状态。 */
  allowTurnStartedThinking: boolean;
  preparingLabel?: string;
  onApproval: (approvalId: string, response: ApprovalResponse) => void;
  onLoadActionOutput: (messageId: string, actionId: string) => Promise<void>;
  onEditResend?: (text: string) => void;
  onOpenDiffFile?: (filePath: string) => void;
  onOpenImageAttachment?: (attachment: AttachmentBlock) => void;
}

const THINKING_VARIANTS = [
  { icon: Brain, key: "thinkingVariants.thinking" },
  { icon: Lightbulb, key: "thinkingVariants.reasoning" },
  { icon: Eye, key: "thinkingVariants.analyzing" },
  { icon: Compass, key: "thinkingVariants.exploring" },
  { icon: Search, key: "thinkingVariants.researching" },
  { icon: Sparkles, key: "thinkingVariants.generating" },
  { icon: BookOpen, key: "thinkingVariants.reading" },
  { icon: Brain, key: "thinkingVariants.considering" },
] as const;

function useThinkingVariant(active: boolean) {
  const [index, setIndex] = useState(() => Math.floor(Math.random() * THINKING_VARIANTS.length));
  useEffect(() => {
    if (!active) return;
    const interval = setInterval(() => {
      setIndex((i) => (i + 1) % THINKING_VARIANTS.length);
    }, 4000);
    return () => clearInterval(interval);
  }, [active]);
  return THINKING_VARIANTS[index];
}

function extractMessageCopyText(message: Message): string {
  if (message.role === "user") {
    if (message.content) return message.content;
    return (message.blocks ?? [])
      .filter((b) => b.type === "text")
      .map((b) => String(b.content ?? ""))
      .join("\n");
  }
  return (message.blocks ?? [])
    .filter((b) => b.type === "text" || b.type === "code")
    .map((b) => {
      if (b.type === "code") return `\`\`\`${b.language ?? ""}\n${b.content ?? ""}\n\`\`\``;
      return String(b.content ?? "");
    })
    .join("\n\n");
}

function MessageCopyButton({ message }: { message: Message }) {
  const [copied, setCopied] = useState(false);
  const handleCopy = useCallback(() => {
    const text = extractMessageCopyText(message);
    if (!text) return;
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }, [message]);
  return (
    <button
      type="button"
      onClick={handleCopy}
      style={{
        cursor: "pointer",
        background: "none",
        border: "none",
        padding: "2px 4px",
        display: "inline-flex",
        alignItems: "center",
        color: copied ? "var(--success)" : "var(--text-3)",
      }}
      aria-label="Copy message"
    >
      {copied ? <Check size={11} /> : <Copy size={11} />}
    </button>
  );
}

function WorkingDurationIndicator({ startedAt }: { startedAt: number }) {
  const { t } = useTranslation("chat");
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    setNow(Date.now());
    const intervalId = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(intervalId);
  }, [startedAt]);

  return (
    <div className="chat-working-duration" role="status" aria-live="polite">
      <span className="chat-working-duration-dot" aria-hidden="true" />
      <span>{t("panel.workingFor", { duration: formatWorkingDuration(now - startedAt) })}</span>
    </div>
  );
}

function MessageRowView({
  message,
  index,
  isHighlighted,
  assistantLabel,
  assistantEngineId,
  assistantEngineName,
  allowInitialPreparation,
  allowTurnStartedThinking,
  preparingLabel,
  onApproval,
  onLoadActionOutput,
  onEditResend,
  onOpenDiffFile,
  onOpenImageAttachment,
}: MessageRowProps) {
  const { t, i18n } = useTranslation("chat");
  const isUser = message.role === "user";
  const messageTimestamp = useMemo(
    () => formatMessageTimestamp(message.createdAt, i18n.language),
    [i18n.language, message.createdAt],
  );
  const userContent = useMemo(() => {
    if (message.content) {
      return message.content;
    }
    return (message.blocks ?? [])
      .filter((block) => block.type === "text")
      .map((block) => block.content)
      .join("\n");
  }, [message.blocks, message.content]);
  const userAuxiliaryBlocks = useMemo(
    () =>
      (message.blocks ?? []).filter(
        (block) =>
          block.type === "attachment" ||
          block.type === "skill" ||
          block.type === "mention",
      ),
    [message.blocks],
  );
  const userPlanMode = useMemo(
    () =>
      (message.blocks ?? []).some(
        (block) => block.type === "text" && Boolean(block.planMode),
      ),
    [message.blocks],
  );
  const hasAssistantContent = !isUser && hasVisibleContent(message.blocks);
  const showInitialPreparation =
    !isUser &&
    message.status === "streaming" &&
    !hasAssistantContent &&
    allowInitialPreparation;
  const showReceivedAndThinking =
    !isUser &&
    message.status === "streaming" &&
    !hasAssistantContent &&
    allowTurnStartedThinking;
  const showAssistantShell =
    !isUser &&
    (hasAssistantContent ||
      showInitialPreparation ||
      showReceivedAndThinking ||
      Boolean(preparingLabel));
  const showThinkingPlaceholder =
    !isUser &&
    !hasAssistantContent &&
    (showInitialPreparation || showReceivedAndThinking || Boolean(preparingLabel));
  const showTurnTail = hasAssistantContent && message.status === "streaming";
  const hasPendingApproval = (message.blocks ?? []).some(
    (block) => block.type === "approval" && block.status === "pending",
  );
  const runningAction = [...(message.blocks ?? [])].reverse().find(
    (block) => block.type === "action" && block.status === "running",
  );
  const hasAcceptedSteer = (message.blocks ?? []).some(
    (block) => block.type === "steer" && block.deliveryStatus === "accepted",
  );
  const thinkingVariant = useThinkingVariant(showThinkingPlaceholder);

  if (!isUser && !showAssistantShell) {
    return null;
  }

  return (
    <div
      data-message-id={message.id}
      className="animate-slide-up msg-row"
      style={{
        animationDelay: `${Math.min(index * 20, 200)}ms`,
        display: "flex",
        flexDirection: "column",
        alignItems: isUser ? "flex-end" : "flex-start",
        maxWidth: "100%",
        borderRadius: "var(--radius-md)",
        outline: isHighlighted ? "2px solid rgba(var(--accent-rgb), 0.35)" : "none",
        boxShadow: isHighlighted
          ? "0 10px 28px rgba(var(--accent-rgb), 0.12)"
          : "none",
        transition:
          "outline-color var(--duration-normal) var(--ease-out), box-shadow var(--duration-normal) var(--ease-out)",
      }}
    >
      {isUser ? (
        <>
          <div className="msg-user-bubble">
            {userAuxiliaryBlocks.length > 0 && (
              <div style={{ display: "flex", flexWrap: "wrap", gap: 4, marginBottom: 6 }}>
                {userAuxiliaryBlocks.map((block, i) => {
                  if (block.type === "attachment") {
                    return (
                      <AttachmentChip
                        key={i}
                        attachment={block}
                        compact
                        onOpen={onOpenImageAttachment
                          ? () => onOpenImageAttachment(block)
                          : undefined}
                      />
                    );
                  }

                  return (
                    <span
                      key={i}
                      className={`chat-attachment-chip ${block.type === "skill" ? "chat-attachment-chip--skill" : "chat-attachment-chip--mention"}`}
                    >
                      {block.type === "skill" ? (
                        <DollarSign size={10} />
                      ) : (
                        <AtSign size={10} />
                      )}
                      <span className="chat-attachment-chip-name" style={{ fontSize: 10 }}>
                        {block.name}
                      </span>
                    </span>
                  );
                })}
              </div>
            )}
            {userPlanMode && (
              <div style={{ display: "flex", alignItems: "center", gap: 4, marginBottom: 6, fontSize: 10, color: "var(--accent-2)" }}>
                <ListChecks size={10} />
                <span>{t("panel.planMode")}</span>
              </div>
            )}
            {userContent}
          </div>
          <div className="msg-row-timestamp" style={{ display: "flex", alignItems: "center", gap: 2, justifyContent: "flex-end", marginTop: 4, paddingRight: 4 }}>
            {onEditResend && (
              <button
                type="button"
                className="msg-row-action-btn"
                onClick={() => onEditResend(userContent)}
                title={t("panel.editResend")}
                aria-label={t("panel.editResend")}
              >
                <Pencil size={11} />
              </button>
            )}
            <MessageCopyButton message={message} />
            {messageTimestamp && <span>{messageTimestamp}</span>}
          </div>
        </>
      ) : showAssistantShell ? (
        <div
          style={{
            width: "100%",
            maxWidth: "100%",
            padding: "4px 0",
          }}
        >
          {assistantLabel && (
            <div className="msg-turn-header">
              {getHarnessIcon(assistantEngineId, 11)}
              <span className="msg-turn-header-label">{assistantLabel}</span>
              <span className="msg-turn-actions">
                {messageTimestamp && <span style={{ padding: "0 2px" }}>{messageTimestamp}</span>}
                <MessageCopyButton message={message} />
              </span>
            </div>
          )}
          {hasAssistantContent ? (
            <>
              <MessageBlocks
                messageId={message.id}
                blocks={message.blocks}
                status={message.status}
                engineId={assistantEngineId}
                onApproval={onApproval}
                onLoadActionOutput={(actionId) => onLoadActionOutput(message.id, actionId)}
                onOpenDiffFile={onOpenDiffFile}
                onOpenImageAttachment={onOpenImageAttachment}
              />
              {showTurnTail && (
                <div className="chat-turn-tail-status" role="status" aria-live="polite">
                  <Loader2 size={12} className="chat-send-spinner" aria-hidden="true" />
                  <span>
                    {t(
                      hasPendingApproval
                        ? "messageBlocks.turnProgress.waitingForApproval"
                        : runningAction?.type === "action"
                          ? "messageBlocks.turnProgress.runningAction"
                          : hasAcceptedSteer
                            ? "messageBlocks.turnProgress.runningWithSteer"
                            : "messageBlocks.turnProgress.running",
                      runningAction?.type === "action"
                        ? { summary: runningAction.summary }
                        : { engine: assistantEngineName },
                    )}
                  </span>
                  <span className="chat-streaming-dots" aria-hidden="true">
                    <span />
                    <span />
                    <span />
                  </span>
                </div>
              )}
            </>
          ) : showReceivedAndThinking && !preparingLabel ? (
            <div className="chat-turn-tail-status" role="status" aria-live="polite">
              <Loader2 size={12} className="chat-send-spinner" aria-hidden="true" />
              <span>
                {t("messageBlocks.turnProgress.receivedAndThinking", {
                  engine: assistantEngineName,
                })}
              </span>
              <span className="chat-streaming-dots" aria-hidden="true">
                <span />
                <span />
                <span />
              </span>
            </div>
          ) : (
            <div
              style={{
                padding: "4px 14px 8px",
                display: "inline-flex",
                alignItems: "center",
                gap: 8,
                color: "var(--text-3)",
                fontSize: 12,
              }}
            >
              {(() => {
                const ThinkIcon = thinkingVariant.icon;
                return <ThinkIcon size={12} className="thinking-icon-active" style={{ color: "var(--info)" }} />;
              })()}
              <span>{preparingLabel ?? t(thinkingVariant.key)}</span>
              <span className="chat-streaming-dots">
                <span />
                <span />
                <span />
              </span>
            </div>
          )}
        </div>
      ) : null}
    </div>
  );
}

const MessageRow = memo(
  MessageRowView,
  (prev, next) =>
    prev.message === next.message &&
    prev.index === next.index &&
    prev.isHighlighted === next.isHighlighted &&
    prev.assistantLabel === next.assistantLabel &&
    prev.assistantEngineId === next.assistantEngineId &&
    prev.assistantEngineName === next.assistantEngineName &&
    prev.allowInitialPreparation === next.allowInitialPreparation &&
    prev.allowTurnStartedThinking === next.allowTurnStartedThinking &&
    prev.preparingLabel === next.preparingLabel &&
    prev.onApproval === next.onApproval &&
    prev.onLoadActionOutput === next.onLoadActionOutput &&
    prev.onEditResend === next.onEditResend &&
    prev.onOpenDiffFile === next.onOpenDiffFile &&
    prev.onOpenImageAttachment === next.onOpenImageAttachment,
);

function getFileExtension(fileName: string): string {
  const lastDot = fileName.lastIndexOf(".");
  return lastDot >= 0 ? fileName.slice(lastDot + 1).toLowerCase() : "";
}

function fileNameFromPath(filePath: string): string {
  return filePath.split("/").pop() ?? filePath.split("\\").pop() ?? filePath;
}

function isSupportedAttachmentName(fileName: string, supportedExtensions: ReadonlySet<string>): boolean {
  const extension = getFileExtension(fileName);
  return supportedExtensions.has(extension);
}

function guessMimeType(fileName: string): string | undefined {
  const ext = getFileExtension(fileName);
  const mimeMap: Record<string, string> = {
    txt: "text/plain",
    md: "text/markdown",
    json: "application/json",
    js: "text/javascript",
    ts: "text/typescript",
    tsx: "text/typescript",
    jsx: "text/javascript",
    py: "text/x-python",
    rs: "text/x-rust",
    go: "text/x-go",
    css: "text/css",
    html: "text/html",
    svg: "image/svg+xml",
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    gif: "image/gif",
    webp: "image/webp",
    pdf: "application/pdf",
    yaml: "text/yaml",
    yml: "text/yaml",
    toml: "text/toml",
    xml: "text/xml",
    sql: "text/x-sql",
    sh: "text/x-shellscript",
    csv: "text/csv",
  };
  return mimeMap[ext];
}

function imageExtensionForMimeType(mimeType: string): string | null {
  switch (mimeType.toLowerCase()) {
    case "image/png":
      return "png";
    case "image/jpeg":
    case "image/jpg":
      return "jpg";
    case "image/gif":
      return "gif";
    case "image/webp":
      return "webp";
    case "image/bmp":
      return "bmp";
    case "image/tiff":
      return "tiff";
    case "image/svg+xml":
      return "svg";
    default:
      return null;
  }
}

function fileNameForPastedImage(file: File, index: number): string {
  if (file.name.trim()) {
    return file.name.trim();
  }
  const extension = imageExtensionForMimeType(file.type) ?? "png";
  return `pasted-image-${index + 1}.${extension}`;
}

function pastedImageFileSupported(file: File, supportedExtensions: ReadonlySet<string>): boolean {
  const mimeExtension = file.type ? imageExtensionForMimeType(file.type) : null;
  if (file.type && !mimeExtension) {
    return false;
  }
  const fileName = fileNameForPastedImage(file, 0);
  const extension = getFileExtension(fileName) || mimeExtension;
  return Boolean(extension && supportedExtensions.has(extension));
}

function clipboardImageFiles(clipboardData: DataTransfer): File[] {
  const files: File[] = [];
  for (const item of Array.from(clipboardData.items)) {
    if (item.kind !== "file" || !item.type.toLowerCase().startsWith("image/")) {
      continue;
    }
    const file = item.getAsFile();
    if (file) {
      files.push(file);
    }
  }
  if (files.length > 0) {
    return files;
  }
  return Array.from(clipboardData.files).filter((file) =>
    file.type.toLowerCase().startsWith("image/"),
  );
}

function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      const [, base64 = ""] = result.split(",", 2);
      resolve(base64);
    };
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read image data."));
    reader.readAsDataURL(blob);
  });
}

function formatUsagePercent(percent: number | null): string {
  if (typeof percent !== "number" || !Number.isFinite(percent)) {
    return "--";
  }
  return `${Math.max(0, Math.min(100, Math.round(percent)))}%`;
}

function usagePercentToWidth(percent: number | null): string {
  if (typeof percent !== "number" || !Number.isFinite(percent)) {
    return "0%";
  }
  return `${Math.max(0, Math.min(100, Math.round(percent)))}%`;
}

function usageProgressLevelClass(percent: number | null): string {
  if (typeof percent !== "number" || !Number.isFinite(percent)) {
    return "";
  }
  if (percent <= 10) return " chat-context-progress-fill-critical";
  if (percent <= 25) return " chat-context-progress-fill-warning";
  return "";
}

function resolveClaudeModelFamily(model: EngineModel | null): "fable" | "opus" | "sonnet" | null {
  if (!model) {
    return null;
  }
  const identity = `${model.id} ${model.displayName} ${model.description}`.toLowerCase();
  if (identity.includes("fable")) return "fable";
  if (identity.includes("opus")) return "opus";
  if (identity.includes("sonnet")) return "sonnet";
  return null;
}

interface ChatPanelProps {
  embedded?: boolean;
}

function FlexibleMessageGroup({
  messages,
  confirmDisabled,
  confirmTitle,
  onConfirm,
  onWithdraw,
}: {
  messages: PendingFlexibleMessage[];
  confirmDisabled: boolean;
  confirmTitle: string;
  onConfirm: () => void;
  onWithdraw: (message: PendingFlexibleMessage) => void;
}) {
  const { t } = useTranslation("chat");

  return (
    <div className="flexible-message-group" role="group" aria-label={t("panel.flexibleMessageGroup.label")}>
      <div className="flexible-message-group-content">
        {messages.map((message) => (
          <div key={message.id} className="flexible-message-group-item">
            <span className="flexible-message-group-item-text">{message.text}</span>
            <button
              type="button"
              className="flexible-message-group-withdraw"
              title={t("panel.flexibleMessageGroup.withdraw")}
              aria-label={t("panel.flexibleMessageGroup.withdraw")}
              onClick={() => onWithdraw(message)}
            >
              {t("panel.flexibleMessageGroup.withdraw")}
            </button>
          </div>
        ))}
      </div>
      <div className="flexible-message-group-actions">
        <button
          type="button"
          className="flexible-message-group-confirm"
          disabled={confirmDisabled}
          title={confirmTitle}
          onClick={onConfirm}
        >
          {t("panel.flexibleMessageGroup.confirm")}
        </button>
      </div>
    </div>
  );
}

export function ChatPanel({ embedded = false }: ChatPanelProps = {}) {
  const { t } = useTranslation("chat");
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
  const [selectedEngineId, setSelectedEngineId] = useState("codex");
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
  const [customApprovalPolicyText, setCustomApprovalPolicyText] = useState("");
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
  const engineWorkspaceId = useWorkspaceStore((s) => s.activeWorkspaceId);
  const onboardingOpen = useOnboardingStore((s) => s.open);
  const onboardingSelectedChatEngines = useOnboardingStore((s) => s.selectedChatEngines);
  // The health warning is only a heuristic; the backend answer is what
  // set_thread_execution_policy actually validates against.
  const [codexExternalSandboxActive, setCodexExternalSandboxActive] = useState(false);
  useEffect(() => {
    let disposed = false;
    const heuristic = codexUsesExternalSandbox(health);
    setCodexExternalSandboxActive((prev) => prev || heuristic);
    void ipc
      .codexUsesExternalSandbox(engineWorkspaceId)
      .then((active) => {
        if (!disposed) {
          setCodexExternalSandboxActive(active);
        }
      })
      .catch(() => {});
    return () => {
      disposed = true;
    };
  }, [engineWorkspaceId, health]);
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
    applyThreadUpdateLocal,
    setThreadReasoningEffortLocal,
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
      applyThreadUpdateLocal: state.applyThreadUpdateLocal,
      setThreadReasoningEffortLocal: state.setThreadReasoningEffortLocal,
      setThreadLastModelLocal: state.setThreadLastModelLocal,
      renameThread: state.renameThread,
      threadsByWorkspace: state.threadsByWorkspace,
    })),
  );
  const activeChatSessionId = activeThread?.id ?? threadId ?? null;
  const composerSessionKey = activeWorkspaceId
    ? getChatComposerSessionKey(
        activeWorkspaceId,
        activeThread?.workspaceId === activeWorkspaceId ? activeThread.id : null,
      )
    : null;
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
    input.trim() || textAnnotations.length > 0 || hasImageAttachmentAnnotations(attachments),
  );
  const sendShortcut = useChatComposerStore((state) => state.sendShortcut);
  const chatInputMode = useChatComposerStore((state) => state.chatInputMode);
  const sessionMessageSendMode = useChatComposerStore((state) => {
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
   *   customApprovalPolicyText: prompt.customApprovalPolicyText,
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
  const messageHeightsRef = useRef<Map<string, number>>(new Map());
  const layoutVersionRafRef = useRef<number | null>(null);
  const threadExecutionPolicyRequestIdsRef = useRef<Record<string, number>>({});
  // 保存每个线程正在执行的权限持久化任务，普通消息发送必须等待同线程任务完成。
  const threadExecutionPolicyRequestsRef = useRef<Record<string, Promise<boolean> | undefined>>({});
  // 保存每个线程正在执行的五项运行选择局部更新，保证同一线程按顺序落库。
  const threadRuntimeSelectionRequestsRef = useRef<Record<string, Promise<boolean> | undefined>>({});
  const [listLayoutVersion, setListLayoutVersion] = useState(0);
  const [viewportScrollTop, setViewportScrollTop] = useState(0);
  const [viewportHeight, setViewportHeight] = useState(0);
  const [textAnnotationPopover, setTextAnnotationPopover] =
    useState<TextAnnotationPopover | null>(null);
  const [textAnnotationComment, setTextAnnotationComment] = useState("");
  const [autoScrollLocked, setAutoScrollLocked] = useState(false);
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
    customApprovalPolicyText: string;
    openCodeAgent: string;
  } | null>(null);

  useEffect(() => {
    if (textAnnotationPopover?.stage !== "comment") {
      return;
    }
    const animationFrame = requestAnimationFrame(() => {
      annotationCommentInputRef.current?.focus();
    });
    return () => cancelAnimationFrame(animationFrame);
  }, [textAnnotationPopover?.stage]);

  /* 旧 PermissionPicker 的 CLI 选项计算已停用，保留迁移记录。
  const trustLevelOptions = useMemo(() => getTrustLevelOptions(t), [t]);
  const codexThreadApprovalPolicyOptions = useMemo(
    () => getCodexThreadApprovalPolicyOptions(t),
    [t],
  );
  const claudeThreadPermissionModeOptions = useMemo(
    () => getClaudeThreadPermissionModeOptions(t),
    [t],
  );
  const openCodeThreadPermissionModeOptions = useMemo(
    () => getOpenCodeThreadPermissionModeOptions(t),
    [t],
  );
  const threadSandboxModeOptionsAll = useMemo(
    () => getThreadSandboxModeOptions(t),
    [t],
  );
  const threadNetworkPolicyOptions = useMemo(
    () => getThreadNetworkPolicyOptions(t),
    [t],
  );
  */

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
    (outputSchemaText.trim().length > 0 ? 1 : 0) +
    (customApprovalPolicyText.trim().length > 0 ? 1 : 0);
  const selectedOutputSchemaValue = useMemo(() => {
    try {
      return parseStoredOutputSchema(outputSchemaText);
    } catch {
      return null;
    }
  }, [outputSchemaText]);
  const selectedCustomApprovalPolicyValue = useMemo(() => {
    try {
      return parseStoredApprovalPolicy(customApprovalPolicyText);
    } catch {
      return null;
    }
  }, [customApprovalPolicyText]);
  const activeThreadHasRemoteSession =
    activeWorkspace?.locationKind === "ssh" && Boolean(activeThread?.engineThreadId);
  /*
   * 旧逻辑只按 CLI 会话 ID或消息数锁定运行目标；该表达式由统一的空会话判断接管。
   * const activeThreadRuntimeLocked = Boolean(
   *   activeThread?.engineThreadId || (activeThread?.messageCount ?? 0) > 0,
   * );
   */
  const activeThreadRuntimeLocked =
    Boolean(activeThread) && !canChangeUnstartedThreadEngine(activeThread);
  const activeThreadMatchesComposer = useMemo(() => {
    if (!activeThread || !activeWorkspaceId || !selectedModelId) {
      return false;
    }

    const inScope = activeThread.workspaceId === activeWorkspaceId;
    const engineMatch = activeThread.engineId === selectedEngineId;
    const modelMatch =
      activeThreadHasRemoteSession ||
      selectedEngineId === "codex" ||
      activeThread.modelId === selectedModelId ||
      readThreadLastModelId(activeThread) === selectedModelId;

    return inScope && engineMatch && modelMatch;
  }, [
    activeThread,
    activeThreadHasRemoteSession,
    activeWorkspaceId,
    selectedEngineId,
    selectedModelId,
  ]);

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

  const resolveCodexInputItems = useCallback(
    async (
      message: string,
      engineId: string,
      references: ChatInputReference[] = EMPTY_CHAT_INPUT_REFERENCES,
    ): Promise<ChatInputItem[] | undefined> => {
      if (engineId !== "codex") {
        return undefined;
      }

      if (references.length > 0 || chatInputMode === "classic") {
        return buildSelectedCodexInputItems(message, references);
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
  // 思考强度只读 threads 独立字段。
  const activeThreadReasoningEffort =
    typeof activeThread?.reasoningEffort === "string" ? activeThread.reasoningEffort : undefined;
  const activeThreadInCurrentWorkspace =
    activeThread?.workspaceId === activeWorkspaceId;
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

  /* 旧 PermissionPicker 的信任选项提示已停用，保留迁移记录。
  function trustLevelTooltip(level: TrustLevel): string {
    return (
      trustLevelOptions.find((option) => option.value === level)?.description ??
      t("policy.permissionPolicyFallback")
    );
  }
  */

  const [permissionComponent, setPermissionComponent] = useState<PermissionComponentJson>(
    EMPTY_PERMISSION_COMPONENT,
  );
  const permissionLoadRequestRef = useRef(0);
  const permissionSaveRequestRef = useRef(0);
  const permissionSavePromisesRef = useRef<Record<string, Promise<boolean>>>({});
  const [defaultAutonomyPreset, setDefaultAutonomyPreset] =
    useState<AutonomyPresetId | null>(null);
  const workspaceTrustLevel: TrustLevel = activeWorkspace?.trustLevel ?? "standard";

  // 权限组件只依赖 AuraCoder 线程 ID；新建线程在发送前也立即显示统一默认值。
  useEffect(() => {
    const threadId = activeThread?.id;
    const requestId = permissionLoadRequestRef.current + 1;
    permissionLoadRequestRef.current = requestId;
    if (!threadId) {
      setPermissionComponent(EMPTY_PERMISSION_COMPONENT);
      return;
    }
    let disposed = false;
    setPermissionComponent(EMPTY_PERMISSION_COMPONENT);
    void ipc
      .getThreadPermissions(threadId)
      .then((values) => {
        if (!disposed && permissionLoadRequestRef.current === requestId) {
          const repoTrust = workspaceTrustLevel;
          setPermissionComponent({
            ...values,
            trust: repoTrust ? [repoTrust] : values.trust,
            defaultForNewThreads: autonomyPresetToComponentValue(defaultAutonomyPreset)
              ? [autonomyPresetToComponentValue(defaultAutonomyPreset)!]
              : values.defaultForNewThreads,
          });
        }
      })
      .catch((error) => {
        if (!disposed && permissionLoadRequestRef.current === requestId) {
          toast.error(String(error));
        }
      });
    return () => {
      disposed = true;
    };
  }, [activeThread?.id, defaultAutonomyPreset, workspaceTrustLevel]);

  function onPermissionComponentChange(next: PermissionComponentJson): Promise<boolean> {
    const thread = activeThread;
    if (!thread?.id) return Promise.resolve(false);
    setPermissionComponent(next);
    const requestId = permissionSaveRequestRef.current + 1;
    permissionSaveRequestRef.current = requestId;
    const threadId = thread.id;
    const previous = permissionSavePromisesRef.current[threadId] ?? Promise.resolve(true);
    let trackedPromise: Promise<boolean>;
    trackedPromise = previous
      .catch(() => true)
      .then(async () => {
      try {
        const nextTrust = Array.isArray(next.trust) && typeof next.trust[0] === "string"
          ? next.trust[0] as TrustLevel
          : null;
        const currentWorkspaceTrustLevel =
          useWorkspaceStore.getState().workspaces.find(
            (workspace) => workspace.id === thread.workspaceId,
          )?.trustLevel ?? workspaceTrustLevel;
        if (shouldUpdateWorkspaceTrustLevel(nextTrust, currentWorkspaceTrustLevel)) {
          await onWorkspaceTrustLevelChange(nextTrust);
        }
        const nextDefaultValue = Array.isArray(next.defaultForNewThreads)
          && typeof next.defaultForNewThreads[0] === "string"
          ? next.defaultForNewThreads[0]
          : null;
        const nextDefault = componentValueToAutonomyPreset(nextDefaultValue);
        const currentDefault = Array.isArray(permissionComponent.defaultForNewThreads)
          && typeof permissionComponent.defaultForNewThreads[0] === "string"
          ? permissionComponent.defaultForNewThreads[0]
          : null;
        const currentDefaultPreset = componentValueToAutonomyPreset(currentDefault);
        if (nextDefault !== currentDefaultPreset) {
          await onDefaultAutonomyPresetChange(nextDefault);
        }
        const values = await ipc.setThreadPermissions(threadId, next);
        if (
          permissionSaveRequestRef.current === requestId
          && useThreadStore.getState().activeThreadId === threadId
          && permissionSavePromisesRef.current[threadId] === trackedPromise
        ) {
          setPermissionComponent(values);
        }
        return true;
      } catch (error) {
        if (
          permissionSaveRequestRef.current === requestId
          && useThreadStore.getState().activeThreadId === threadId
          && permissionSavePromisesRef.current[threadId] === trackedPromise
        ) {
          toast.error(String(error));
          try {
            const values = await ipc.getThreadPermissions(threadId);
            setPermissionComponent(values);
          } catch {
            setPermissionComponent(EMPTY_PERMISSION_COMPONENT);
          }
        }
        return false;
      }
      });
    permissionSavePromisesRef.current[threadId] = trackedPromise;
    void trackedPromise.then(
      () => {
        if (permissionSavePromisesRef.current[threadId] === trackedPromise) {
          delete permissionSavePromisesRef.current[threadId];
        }
      },
      () => {
        if (permissionSavePromisesRef.current[threadId] === trackedPromise) {
          delete permissionSavePromisesRef.current[threadId];
        }
      },
    );
    return trackedPromise;
  }

  /** 串行保存线程五项运行时选择；不同线程之间互不阻塞。 */
  const saveThreadRuntimeSelectionPatch = useCallback(
    (
      threadId: string | null | undefined,
      patch: ThreadRuntimeSelectionPatch,
    ): Promise<boolean> => {
      if (!threadId || Object.keys(patch).length === 0) {
        return Promise.resolve(true);
      }
      const previous =
        threadRuntimeSelectionRequestsRef.current[threadId] ?? Promise.resolve(true);
      let trackedPromise: Promise<boolean>;
      trackedPromise = previous
        .catch(() => true)
        .then(async () => {
          try {
            const updatedThread = await ipc.updateThread({ id: threadId, ...patch });
            applyThreadUpdateLocal(updatedThread);
            return true;
          } catch (error) {
            toast.error(String(error));
            return false;
          }
        });
      threadRuntimeSelectionRequestsRef.current[threadId] = trackedPromise;
      void trackedPromise.then(
        () => {
          if (threadRuntimeSelectionRequestsRef.current[threadId] === trackedPromise) {
            delete threadRuntimeSelectionRequestsRef.current[threadId];
          }
        },
        () => {
          if (threadRuntimeSelectionRequestsRef.current[threadId] === trackedPromise) {
            delete threadRuntimeSelectionRequestsRef.current[threadId];
          }
        },
      );
      return trackedPromise;
    },
    [applyThreadUpdateLocal],
  );

  const activeThreadEngineInfo = useMemo(
    () =>
      activeThread?.engineId
        ? engines.find((engine) => engine.id === activeThread.engineId) ?? null
        : null,
    [activeThread?.engineId, engines],
  );
  const activeThreadSandboxMode: ThreadSandboxModeValue =
    permissionComponent.sandbox[0] === "read-only"
      ? "read-only"
      : permissionComponent.sandbox[0] === "workspace-write"
        ? "workspace-write"
        : permissionComponent.sandbox[0] === "full-access"
          ? "danger-full-access"
          : "inherit";
  /* 旧 PermissionPicker 的 CLI 权限状态计算已停用，保留完整迁移记录。
  const activeThreadApprovalPolicy = readThreadApprovalPolicyValue(activeThread);
  const activeThreadApprovalTitle =
    activeThread?.engineId === "claude"
      ? t("policy.approvalTitleClaude")
      : activeThread?.engineId === "opencode"
        ? t("policy.approvalTitleOpenCode")
      : t("permissionPicker.approvalPolicy");
  const activeThreadApprovalOptions =
    activeThread?.engineId === "claude"
      ? claudeThreadPermissionModeOptions
      : activeThread?.engineId === "opencode"
        ? openCodeThreadPermissionModeOptions
      : codexThreadApprovalPolicyOptions;
  const activeThreadApprovalSelectedLabel =
    activeThread?.engineId === "codex" && activeThreadApprovalPolicy === "custom"
      ? t("permissionPicker.custom")
      : undefined;
  const activeThreadNetworkPolicy = readThreadNetworkPolicyValue(activeThread);
  const activeThreadAutonomyEngineId =
    activeThread?.engineId === "codex" ||
    activeThread?.engineId === "claude" ||
    activeThread?.engineId === "opencode"
      ? activeThread.engineId
      : undefined;
  const activeThreadAutonomyPreset = useMemo(
    () =>
      activeThreadAutonomyEngineId
        ? detectAutonomyPreset(
            activeThreadAutonomyEngineId,
            {
              approvalPolicy: activeThreadApprovalPolicy,
              sandboxMode: activeThreadSandboxMode,
              networkPolicy: readThreadStoredNetworkPolicyValue(activeThread),
            },
            { codexExternalSandbox: codexExternalSandboxActive },
          )
        : undefined,
    [
      activeThread,
      activeThreadApprovalPolicy,
      activeThreadAutonomyEngineId,
      activeThreadSandboxMode,
      codexExternalSandboxActive,
    ],
  );
  */
  useEffect(() => {
    let disposed = false;
    void ipc
      .getDefaultAutonomyPreset()
      .then((preset) => {
        if (!disposed && isAutonomyPresetId(preset)) {
          setDefaultAutonomyPreset(preset);
        }
      })
      .catch(() => {});
    return () => {
      disposed = true;
    };
  }, []);
  const activeThreadCapabilities = useMemo(
    () => resolveEngineCapabilities(activeThread?.engineId, activeThreadEngineInfo?.capabilities),
    [activeThread?.engineId, activeThreadEngineInfo?.capabilities],
  );
  const activeThreadApprovalDecisionCapabilities = activeThreadCapabilities.approvalDecisions;
  /* 旧 PermissionPicker 的 CLI 沙箱选项过滤和自定义计数已停用，保留完整迁移记录。
  const activeThreadSandboxCapabilities = activeThreadCapabilities.sandboxModes;
  const threadSandboxModeOptions = useMemo(
    () => {
      const supportedByEngine = threadSandboxModeOptionsAll.filter(
        (option) =>
          option.value === "inherit" ||
          activeThreadSandboxCapabilities.includes(option.value),
      );

      if (activeThread?.engineId === "codex" && codexExternalSandboxActive) {
        return supportedByEngine.filter(
          (option) =>
            option.value === "inherit" || option.value === "danger-full-access",
        );
      }

      return supportedByEngine;
    },
    [
      activeThread?.engineId,
      activeThreadSandboxCapabilities,
      codexExternalSandboxActive,
      threadSandboxModeOptionsAll,
    ],
  );
  const activeThreadSandboxModeOption = threadSandboxModeOptionsAll.find(
    (option) => option.value === activeThreadSandboxMode,
  );
  const activeThreadSandboxModeSupported = threadSandboxModeOptions.some(
    (option) => option.value === activeThreadSandboxMode,
  );
  const activeThreadSandboxNotice =
    activeThread?.engineId === "claude" && !activeThreadSandboxModeSupported
        ? t("policy.claudeSandboxNotice")
        : null;
  const activeThreadSandboxSelectedLabel =
    !activeThreadSandboxModeSupported && activeThreadSandboxModeOption
      ? `${activeThreadSandboxModeOption.label} ${t("panel.unsupportedSuffix")}`
      : undefined;
  const threadPolicyCustomCount =
    activeThread?.engineId === "opencode"
      ? activeThreadApprovalPolicy !== "inherit" ? 1 : 0
      : activeThread?.engineId === "claude"
      ? (activeThreadApprovalPolicy !== "inherit" ? 1 : 0) +
        (activeThreadSandboxMode !== "inherit" ? 1 : 0) +
        (activeThreadNetworkPolicy !== "inherit" ? 1 : 0)
      : (activeThreadApprovalPolicy !== "inherit" ? 1 : 0) +
        (activeThreadSandboxMode !== "inherit" ? 1 : 0) +
        (activeThreadSandboxMode !== "danger-full-access" && activeThreadNetworkPolicy !== "inherit"
          ? 1
          : 0);
  */

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
      setPlanImplementationPrompt({
        threadId: promptThreadId,
        engineId: promptThread.engineId,
        modelId: promptThread.modelId,
        effort: promptThread.reasoningEffort ?? activeThreadReasoningEffort ?? null,
        personality: selectedPersonality,
        serviceTier: selectedServiceTier,
        outputSchemaText,
        customApprovalPolicyText,
        openCodeAgent: readThreadOpenCodeAgentValue(promptThread),
      });
      return;
    }

    if (wasStreaming && !streaming && armedThreadId === threadId && status !== "completed") {
      pendingPlanImplementationThreadIdRef.current = null;
    }
  }, [
    activeThread,
    activeThreadReasoningEffort,
    customApprovalPolicyText,
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

  const handleInputPaste = useCallback((event: ReactClipboardEvent<HTMLElement>) => {
    if (showSpecialInputComposer) {
      return;
    }
    const imageFiles = clipboardImageFiles(event.clipboardData);
    if (imageFiles.length === 0) {
      return;
    }

    event.preventDefault();
    void appendPastedImageFiles(imageFiles);
  }, [appendPastedImageFiles, showSpecialInputComposer]);

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

  const scheduleListLayoutVersionBump = useCallback(() => {
    if (layoutVersionRafRef.current !== null) {
      return;
    }
    layoutVersionRafRef.current = window.requestAnimationFrame(() => {
      layoutVersionRafRef.current = null;
      setListLayoutVersion((version) => version + 1);
    });
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
      setViewportScrollTop(viewport.scrollTop);
      const nearBottom =
        viewport.scrollTop + viewport.clientHeight >= viewport.scrollHeight - 120;
      setAutoScrollLocked(!nearBottom);
    };
    const updateHeight = () => {
      setViewportHeight(viewport.clientHeight);
    };

    updateScroll();
    updateHeight();

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

    let resizeObserver: ResizeObserver | null = null;
    if (typeof ResizeObserver !== "undefined") {
      resizeObserver = new ResizeObserver(() => updateHeight());
      resizeObserver.observe(viewport);
    } else {
      window.addEventListener("resize", updateHeight);
    }

    return () => {
      viewport.removeEventListener("scroll", onScroll);
      if (rafId !== 0) {
        window.cancelAnimationFrame(rafId);
      }
      if (resizeObserver) {
        resizeObserver.disconnect();
      } else {
        window.removeEventListener("resize", updateHeight);
      }
    };
  }, []);

  useEffect(() => {
    messageHeightsRef.current.clear();
    scheduleListLayoutVersionBump();
  }, [activeThread?.id, scheduleListLayoutVersionBump]);

  useEffect(() => {
    const existingIds = new Set(messages.map((message) => message.id));
    let changed = false;
    for (const messageId of messageHeightsRef.current.keys()) {
      if (!existingIds.has(messageId)) {
        messageHeightsRef.current.delete(messageId);
        changed = true;
      }
    }
    if (changed) {
      scheduleListLayoutVersionBump();
    }
  }, [messages, scheduleListLayoutVersionBump]);

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
    if (!engines.some((engine) => engine.id === selectedEngineId)) {
      setSelectedEngineId(engines[0].id);
    }
  }, [engines, selectedEngineId]);

  useEffect(() => {
    if (onboardingOpen || activeThread || !preferredOnboardingChatSelection) {
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
  }, [activeThread, onboardingOpen, preferredOnboardingChatSelection]);

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
    if (!selectedModel) {
      return;
    }

    const syncKey = `${activeThread?.id ?? "none"}:${selectedModel.id}`;
    if (effortSyncKeyRef.current === syncKey) {
      return;
    }
    effortSyncKeyRef.current = syncKey;

    const nextEffort = resolveReasoningEffortForModel(
      selectedModel,
      activeThreadReasoningEffort ?? selectedEffort,
    );

    if (nextEffort && selectedEffort !== nextEffort) {
      setSelectedEffort(nextEffort);
    }
  }, [
    activeThread?.id,
    activeThreadReasoningEffort,
    selectedModel?.id,
    selectedModel?.defaultReasoningEffort,
    selectedEffort,
    supportedEfforts,
  ]);

  // 切换会话时，从 threads 表独立字段恢复计划与发送方法。
  // 计划默认 false，发送方法默认沿用全局；字段为 NULL（历史数据）时不动本地状态。
  const lastSelectionSyncThreadIdRef = useRef<string | null>(null);
  useEffect(() => {
    const threadId = activeThread?.id ?? null;
    if (lastSelectionSyncThreadIdRef.current === threadId) {
      return;
    }
    lastSelectionSyncThreadIdRef.current = threadId;
    if (!activeThread) {
      return;
    }
    if (typeof activeThread.planMode === "boolean") {
      setPlanMode(activeThread.engineId === "opencode" ? false : activeThread.planMode);
    }
    if (typeof activeThread.sendMethod === "string" && isMessageSendMode(activeThread.sendMethod)) {
      setThreadMessageSendMode(activeThread.id, activeThread.sendMethod);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeThread?.id]);

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
      selectedServiceTier !== "inherit"
    ) {
      setSelectedServiceTier("inherit");
    }
  }, [activeThread?.engineId, activeThreadInCurrentWorkspace, selectedServiceTier]);

  useEffect(() => {
    if (activeThread?.engineId !== "codex") {
      return;
    }

    setSelectedPersonality(readThreadPersonalityValue(activeThread));
    setSelectedServiceTier(readThreadServiceTierValue(activeThread));
    setOutputSchemaText(readThreadOutputSchemaText(activeThread));
    // 旧实现从 engineMetadata 读取自定义权限，已停用；统一权限组件不向这里回填 CLI 原始 JSON。
    // setCustomApprovalPolicyText(readCodexThreadCustomApprovalPolicyText(activeThread));
    setCustomApprovalPolicyText("");
  }, [activeThread?.engineId, activeThread?.id, activeThread?.engineMetadata]);

  useEffect(() => {
    if (activeThread?.engineId === "opencode") {
      setSelectedOpenCodeAgent(readThreadOpenCodeAgentValue(activeThread));
    } else if (selectedEngineId !== "opencode") {
      setSelectedOpenCodeAgent("build");
    }
  }, [activeThread?.engineId, activeThread?.id, activeThread?.engineMetadata, selectedEngineId]);

  useEffect(() => {
    if (!activeThread) {
      lastSyncedThreadIdRef.current = null;
      manuallyOverrodeThreadSelectionRef.current = false;
      return;
    }
    const threadChanged = lastSyncedThreadIdRef.current !== activeThread.id;
    if (!threadChanged && manuallyOverrodeThreadSelectionRef.current) {
      return;
    }
    lastSyncedThreadIdRef.current = activeThread.id;
    manuallyOverrodeThreadSelectionRef.current = false;
    setHasExplicitComposerRuntime(false);
    if (activeThread.engineId !== selectedEngineId) {
      setSelectedEngineId(activeThread.engineId);
    }
    const threadEngine =
      engines.find((engine) => engine.id === activeThread.engineId) ?? null;
    const preferredModelId = activeThread.modelId;
    const preferredModelExists =
      threadEngine?.models.some((model) => model.id === preferredModelId) ?? false;
    const threadModelExists =
      threadEngine?.models.some((model) => model.id === activeThread.modelId) ?? false;
    if (preferredModelExists) {
      setSelectedModelId(preferredModelId);
    } else if (threadModelExists) {
      setSelectedModelId(activeThread.modelId);
    } else if (threadEngine && threadEngine.models.length > 0) {
      const fallbackModel =
        threadEngine.models.find((model) => model.isDefault && !model.hidden) ??
        threadEngine.models.find((model) => !model.hidden) ??
        threadEngine.models[0];
      setSelectedModelId(fallbackModel.id);
      if (activeWorkspace?.locationKind === "ssh") {
        const noticeKey = `${activeThread.id}:${preferredModelId}:${fallbackModel.id}`;
        if (unavailableSavedModelNoticeRef.current !== noticeKey) {
          unavailableSavedModelNoticeRef.current = noticeKey;
          toast.warning(
            t("modelPicker.savedModelUnavailable", {
              previous: preferredModelId,
              fallback: fallbackModel.displayName,
            }),
          );
        }
        setThreadLastModelLocal(activeThread.id, fallbackModel.id);
        void ipc
          .setSshRemoteThreadSelectedModel(activeThread.id, fallbackModel.id)
          .then((updatedThread) => applyThreadUpdateLocal(updatedThread))
          .catch((error) => {
            console.warn("Failed to persist SSH fallback model", error);
          });
      }
    }
  }, [
    activeThread?.id,
    activeThread?.engineId,
    activeThread?.modelId,
    activeThread?.engineMetadata,
    activeWorkspace?.locationKind,
    applyThreadUpdateLocal,
    engines,
    selectedEngineId,
    setThreadLastModelLocal,
    t,
  ]);

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

  const scrollViewportToBottom = useCallback((behavior: ScrollBehavior = "auto") => {
    const viewport = viewportRef.current;
    if (!viewport) {
      return;
    }

    viewport.scrollTo({ top: viewport.scrollHeight, behavior });
  }, []);

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

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport) return;
    if (!autoScrollLocked) {
      scrollViewportToBottom("smooth");
    }
  }, [messages, autoScrollLocked, scrollViewportToBottom]);

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
    if (viewportScrollTop > 80 || prependLoadInFlightRef.current) {
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
    viewportScrollTop,
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
    const targetHeight =
      messageHeightsRef.current.get(targetMessageId) ??
      MESSAGE_ESTIMATED_ROW_HEIGHT;
    const targetTopOffset = estimateMessageOffset(
      messages,
      targetIndex,
      messageHeightsRef.current,
    );
    const centeredTop = Math.max(
      0,
      targetTopOffset - Math.max((viewport.clientHeight - targetHeight) / 2, 0),
    );

    viewport.scrollTo({ top: centeredTop, behavior: "smooth" });
    window.setTimeout(() => {
      const targetElement = viewport.querySelector<HTMLElement>(
        `[data-message-id="${targetMessageId}"]`,
      );
      if (targetElement) {
        targetElement.scrollIntoView({ block: "center", behavior: "smooth" });
      }
    }, 120);
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
      if (layoutVersionRafRef.current !== null) {
        window.cancelAnimationFrame(layoutVersionRafRef.current);
        layoutVersionRafRef.current = null;
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

  function parseCustomApprovalPolicyDraft(
    draftText: string = customApprovalPolicyText,
  ):
    | { ok: true; value: Record<string, unknown> | null }
    | { ok: false } {
    try {
      return { ok: true, value: parseStoredApprovalPolicy(draftText) };
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      toast.error(
        message === "approval policy must be a JSON object"
          ? t("panel.toasts.invalidCustomApprovalPolicy")
          : t("panel.toasts.invalidCustomApprovalPolicyWithError", {
              error: message,
            }),
      );
      return { ok: false };
    }
  }

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
    const nextCustomApprovalPolicyText = patch.updateApprovalPolicy
      ? serializePrettyJson(patch.approvalPolicy)
      : customApprovalPolicyText;
    const applyLocalState = () => {
      if (patch.updateServiceTier) {
        setHasExplicitComposerRuntime(true);
      }
      setSelectedPersonality(nextPersonality);
      setSelectedServiceTier(nextServiceTier);
      setOutputSchemaText(nextOutputSchemaText);
      setCustomApprovalPolicyText(nextCustomApprovalPolicyText);
    };

    if (!activeThreadMatchesComposer || activeThread?.engineId !== "codex") {
      applyLocalState();
      return;
    }

    try {
      if (patch.updatePersonality || patch.updateServiceTier || patch.updateOutputSchema) {
        const updatedThread = await ipc.setThreadCodexConfig(activeThread.id, {
          personality: patch.updatePersonality ? patch.personality : undefined,
          serviceTier: patch.updateServiceTier ? patch.serviceTier : undefined,
          outputSchema: patch.updateOutputSchema ? patch.outputSchema : undefined,
        });
        applyThreadUpdateLocal(updatedThread);
      }

      if (patch.updateApprovalPolicy) {
        const updatedThread = await ipc.setThreadExecutionPolicy(activeThread.id, {
          approvalPolicy: patch.approvalPolicy,
        });
        applyThreadUpdateLocal(updatedThread);
      }

      applyLocalState();
    } catch (error) {
      throw new Error(
        t("panel.toasts.updateCodexConfigFailed", { error: String(error) }),
      );
    }
  }

  async function applyCodexConfigToThread(
    targetThreadId: string,
    config?: {
      engineId: string;
      personality: CodexPersonalityValue;
      serviceTier: CodexServiceTierValue;
      outputSchemaText: string;
      customApprovalPolicyText: string;
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
      customApprovalPolicyText,
    };

    const outputSchemaDraft = parseOutputSchemaDraft(effectiveConfig.outputSchemaText);
    if (!outputSchemaDraft.ok) {
      return false;
    }
    const customApprovalDraft =
      parseCustomApprovalPolicyDraft(effectiveConfig.customApprovalPolicyText);
    if (!customApprovalDraft.ok) {
      return false;
    }

    try {
      const updatedConfigThread = await ipc.setThreadCodexConfig(targetThreadId, {
        personality:
          effectiveConfig.personality === "inherit" ? null : effectiveConfig.personality,
        serviceTier:
          effectiveConfig.serviceTier === "inherit" ? null : effectiveConfig.serviceTier,
        outputSchema: outputSchemaDraft.value,
      });
      applyThreadUpdateLocal(updatedConfigThread);

      if (customApprovalDraft.value) {
        const updatedThread = await ipc.setThreadExecutionPolicy(targetThreadId, {
          approvalPolicy: customApprovalDraft.value,
        });
        applyThreadUpdateLocal(updatedThread);
      }
      /*
      // 旧逻辑读取 latestThread.engineMetadata 判断 custom，并在空草稿时
      // 自动清除数据库权限；该行为已停用，权限清除统一由 PermissionPicker
      // 的 setThreadPermissions 路径处理。
      const latestThread =
        useThreadStore.getState().threads.find((thread) => thread.id === targetThreadId) ??
        updatedConfigThread;
      const approvalMode = readCodexThreadApprovalPolicyValue(latestThread);
      if (!customApprovalDraft.value && approvalMode === "custom") {
        const updatedThread = await ipc.setThreadExecutionPolicy(targetThreadId, {
          approvalPolicy: null,
        });
        applyThreadUpdateLocal(updatedThread);
      }
      */

      return true;
    } catch (error) {
      toast.error(
        t("panel.toasts.updateCodexConfigFailed", { error: String(error) }),
      );
      return false;
    }
  }

  async function onOpenCodeAgentChange(agent: string) {
    setSelectedOpenCodeAgent(agent);
    selectedOpenCodeAgentRef.current = agent;
    setHasExplicitComposerRuntime(true);

    if (!activeThreadMatchesComposer || activeThread?.engineId !== "opencode") {
      return;
    }

    try {
      const updatedThread = await ipc.setThreadOpenCodeConfig(activeThread.id, {
        agent: agent === "build" ? null : agent,
      });
      applyThreadUpdateLocal(updatedThread);
    } catch (error) {
      toast.error(
        t("panel.toasts.updateOpenCodeConfigFailed", { error: String(error) }),
      );
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
    try {
      const updatedThread = await ipc.setThreadOpenCodeConfig(targetThreadId, {
        agent: agent === "build" ? null : agent,
      });
      applyThreadUpdateLocal(updatedThread);
      return true;
    } catch (error) {
      toast.error(
        t("panel.toasts.updateOpenCodeConfigFailed", { error: String(error) }),
      );
      return false;
    }
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

    toast.success(t("panel.toasts.codexThreadCompactionStarted"));
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
              updateApprovalPolicy: false,
              approvalPolicy: null,
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
              updateApprovalPolicy: false,
              approvalPolicy: null,
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

    if (streaming) {
      if (!canSteerActiveTurn) {
        return false;
      }

      const activeThreadId = threadId ?? activeThread?.id ?? null;
      if (!activeThreadId) {
        return false;
      }

      const inputItems = await resolveCodexInputItems(text, "codex", currentReferences);
      const steered = await steer(text, {
        threadIdOverride: activeThreadId,
        // attachments: currentAttachments.length > 0 ? currentAttachments : undefined,
        attachments: transportAttachments.length > 0 ? transportAttachments : undefined,
        inputItems,
        planMode: submittedPlanMode,
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

    if (targetThreadId && !activeThreadEngineMatch) {
      try {
        const reconfiguredThread = await ipc.reconfigureUnstartedThreadRuntime(
          targetThreadId,
          submitEngineId,
          submitModelId,
          submitReasoningEffort,
          submitEngineId === "codex" && selectedServiceTier !== "inherit"
            ? selectedServiceTier
            : null,
        );
        applyThreadUpdateLocal(reconfiguredThread);
      } catch (error) {
        toast.error(String(error));
        return false;
      }
    }

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

    // 五项运行选择可能刚刚触发异步保存；发送必须等待同一线程最新任务完成。
    while (true) {
      const pendingRuntimeSelectionRequest =
        threadRuntimeSelectionRequestsRef.current[targetThreadId];
      if (!pendingRuntimeSelectionRequest) {
        break;
      }
      const runtimeSelectionSaved = await pendingRuntimeSelectionRequest;
      const latestRuntimeSelectionRequest =
        threadRuntimeSelectionRequestsRef.current[targetThreadId];
      if (
        latestRuntimeSelectionRequest &&
        latestRuntimeSelectionRequest !== pendingRuntimeSelectionRequest
      ) {
        continue;
      }
      if (!runtimeSelectionSaved) {
        return false;
      }
      break;
    }

    // 权限选择可能刚刚触发异步保存；发送必须等待同一线程最新任务完成。
    while (true) {
      const pendingPolicyRequest = threadExecutionPolicyRequestsRef.current[targetThreadId];
      if (!pendingPolicyRequest) {
        break;
      }
      let policySaved = false;
      try {
        policySaved = await pendingPolicyRequest;
      } catch {
        policySaved = false;
      }

      // 旧任务完成期间可能已经登记了新的权限保存任务；旧任务的失败不能阻止继续等待新任务。
      const latestPendingPolicyRequest =
        threadExecutionPolicyRequestsRef.current[targetThreadId];
      if (latestPendingPolicyRequest && latestPendingPolicyRequest !== pendingPolicyRequest) {
        continue;
      }

      // finally 可能已经清理当前任务的 ref：旧任务成功时结束等待，旧任务失败时阻止发送。
      if (!policySaved) {
        return false;
      }
      if (!latestPendingPolicyRequest || latestPendingPolicyRequest === pendingPolicyRequest) {
        break;
      }
    }

    while (true) {
      const pendingPermissionRequest = permissionSavePromisesRef.current[targetThreadId];
      if (!pendingPermissionRequest) break;
      const permissionSaved = await pendingPermissionRequest;
      const latestPendingPermissionRequest =
        permissionSavePromisesRef.current[targetThreadId];
      const permissionWaitAction = permissionSaveWaitAction(
        permissionSaved,
        pendingPermissionRequest,
        latestPendingPermissionRequest,
      );
      if (permissionWaitAction === "continue") {
        continue;
      }
      if (permissionWaitAction === "fail") return false;
      break;
    }

    // 等待权限保存完成后重新读取线程，避免使用等待前仍为 ask 的旧快照。
    const currentThread =
      useThreadStore.getState().threads.find((thread) => thread.id === targetThreadId) ??
      activeThread;

    setThreadMessageSendMode(targetThreadId, sessionMessageSendMode);
    clearPendingMessageSendMode(activeWorkspaceId);

    const inputItems = await resolveCodexInputItems(text, submitEngineId, currentReferences);


    if (!createdThread) {
      await ipc.setThreadReasoningEffort(
        targetThreadId,
        submitReasoningEffort,
        submitModelId,
      );
    }
    setThreadReasoningEffortLocal(targetThreadId, submitReasoningEffort);

    const needsNewThreadCodexConfig =
      submitEngineId === "codex" &&
      (selectedPersonality !== "inherit" ||
        outputSchemaText.trim().length > 0 ||
        customApprovalPolicyText.trim().length > 0);
    if (!createdThread || needsNewThreadCodexConfig) {
      if (!(await applyCodexConfigToThread(targetThreadId))) {
        return false;
      }
    }

    const needsNewThreadOpenCodeConfig =
      submitEngineId === "opencode" && selectedOpenCodeAgentRef.current !== "build";
    if (!createdThread || needsNewThreadOpenCodeConfig) {
      if (!(await applyOpenCodeConfigToThread(targetThreadId))) {
        return false;
      }
    }
    setThreadLastModelLocal(targetThreadId, submitModelId);

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
      sendMethod: sessionMessageSendMode,
      permissionValues: permissionComponent,
      /*
       * 旧实现的完整请求字段保留如下，不参与编译：
       *
       * permissionModeJson: JSON.stringify(
       *   readThreadExecutionPolicyState(currentThread ?? activeThread ?? null),
       * ),
       *
       * 旧值只包含 ChatPanel 的通用策略，不包含当前 CLI 的原始权限 JSON。
       */
      // 现由统一权限接口保存 CLI 原始 JSON，此处传 null 仅触发现有运行时选择持久化，后端忽略权限参数。
      permissionModeJson: null,
      remoteAttachmentUpload:
        activeWorkspace?.locationKind === "ssh" && currentAttachments.length > 0,
      referencedAuraCoderThreadId,
    });
    if (sent) {
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
    try {
      const accepted = await submitMessage(
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
      setPendingSubmission(null);
      isSubmittingRef.current = false;
      setIsSubmitting(false);
    }
  }

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

      if (!prompt.reasoningConfigured) {
        await ipc.setThreadReasoningEffort(prompt.threadId, prompt.effort, prompt.modelId);
      }
      setThreadReasoningEffortLocal(prompt.threadId, prompt.effort);
      if (!(await applyCodexConfigToThread(prompt.threadId, {
        engineId: prompt.engineId,
        personality: prompt.personality,
        serviceTier: prompt.serviceTier,
        outputSchemaText: prompt.outputSchemaText,
        customApprovalPolicyText: prompt.customApprovalPolicyText,
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
      setThreadLastModelLocal(prompt.threadId, prompt.modelId);

      const promptPlanMode = prompt.engineId === "opencode" ? false : prompt.planMode;
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

      pendingPlanImplementationThreadIdRef.current = promptPlanMode ? prompt.threadId : null;
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
      await ipc.setThreadReasoningEffort(currentThread.id, prompt.effort, prompt.modelId);
      setThreadReasoningEffortLocal(currentThread.id, prompt.effort);
      if (
        !(await applyCodexConfigToThread(currentThread.id, {
          engineId: prompt.engineId,
          personality: prompt.personality,
          serviceTier: prompt.serviceTier,
          outputSchemaText: prompt.outputSchemaText,
          customApprovalPolicyText: prompt.customApprovalPolicyText,
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
      setThreadLastModelLocal(currentThread.id, prompt.modelId);

      const sent = await send(implementationMessage, {
        threadIdOverride: currentThread.id,
        engineId: prompt.engineId,
        modelId: prompt.modelId,
        reasoningEffort: prompt.effort,
        planMode: false,
      });
      if (!sent) {
        setPlanMode(true);
        setPlanImplementationPrompt(prompt);
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

  async function onReasoningEffortChange(nextEffort: string) {
    setHasExplicitComposerRuntime(true);
    selectedEffortRef.current = nextEffort;
    setSelectedEffort(nextEffort);
    const targetThreadId = threadId ?? activeThread?.id ?? null;
    if (!targetThreadId) {
      return;
    }

    setThreadReasoningEffortLocal(targetThreadId, nextEffort);
    await ipc.setThreadReasoningEffort(
      targetThreadId,
      nextEffort,
      selectedModelIdRef.current,
    );
    await saveThreadRuntimeSelectionPatch(targetThreadId, {
      reasoningEffort: nextEffort,
    });
  }

  async function onWorkspaceTrustLevelChange(nextTrustLevel: TrustLevel) {
    if (activeWorkspaceId) {
      await useWorkspaceStore.getState().setWorkspaceTrustLevel(activeWorkspaceId, nextTrustLevel);
    }
  }

  /* 旧 PermissionPicker 的 CLI 权限状态合并和保存回调已停用，保留完整迁移记录。
  function onThreadExecutionPolicyChange(
    patch: ThreadExecutionPolicyPatch,
  ): Promise<boolean> {
    if (
      !activeThread ||
      (activeThread.engineId !== "codex" &&
        activeThread.engineId !== "claude" &&
        activeThread.engineId !== "opencode")
    ) {
      return Promise.resolve(false);
    }

    const currentThread =
      useThreadStore.getState().threads.find((thread) => thread.id === activeThread.id) ??
      activeThread;
    const isCodexThread = currentThread.engineId === "codex";
    const isOpenCodeThread = currentThread.engineId === "opencode";
    const nextState = {
      ...readThreadExecutionPolicyState(currentThread),
      ...patch,
    };
    const nextPatch: ThreadExecutionPolicyPatch = isOpenCodeThread
      ? { approvalPolicy: patch.approvalPolicy }
      : { ...patch };
    const currentStoredNetworkPolicy = isCodexThread
      ? readThreadStoredNetworkPolicyValue(currentThread)
      : "inherit";

    if (
      isCodexThread &&
      codexExternalSandboxActive &&
      (patch.sandboxMode === "read-only" || patch.sandboxMode === "workspace-write")
    ) {
      toast.error(
        t("panel.toasts.codexSandboxUnavailable"),
      );
      return Promise.resolve(false);
    }

    if (
      currentThread.engineId === "claude" &&
      patch.sandboxMode === "danger-full-access"
    ) {
      toast.error(t("panel.toasts.claudeSandboxUnsupported"));
      return Promise.resolve(false);
    }

    if (
      currentThread.engineId !== "codex" &&
      currentThread.engineId !== "claude" &&
      patch.sandboxMode !== undefined
    ) {
      toast.error(t("panel.toasts.codexOnlySandbox"));
      return Promise.resolve(false);
    }

    if (isOpenCodeThread && (patch.sandboxMode !== undefined || patch.networkPolicy !== undefined)) {
      return Promise.resolve(false);
    }

    if (isCodexThread && nextState.sandboxMode === "danger-full-access") {
      if (patch.networkPolicy !== undefined) {
        delete nextPatch.networkPolicy;
      }

      if (
        patch.networkPolicy === "restricted" ||
        (patch.sandboxMode === "danger-full-access" &&
          currentStoredNetworkPolicy === "restricted")
      ) {
        toast.warning(t("panel.toasts.fullAccessNetworkWarning"));
      }
    }

    applyThreadUpdateLocal(applyThreadExecutionPolicyPatch(currentThread, nextPatch));

    const requestId =
      (threadExecutionPolicyRequestIdsRef.current[currentThread.id] ?? 0) + 1;
    threadExecutionPolicyRequestIdsRef.current[currentThread.id] = requestId;

    const savePromise = (async (): Promise<boolean> => {
      try {
        const updatedThread = await ipc.setThreadExecutionPolicy(
          currentThread.id,
          toThreadExecutionPolicyRequest(nextPatch, isCodexThread),
        );

        if (threadExecutionPolicyRequestIdsRef.current[currentThread.id] !== requestId) {
          try {
            await refreshThreads(currentThread.workspaceId);
          } catch {
            // 最新权限任务仍会负责恢复真实状态；旧任务的刷新失败不能阻止它继续完成。
          }
          return true;
        }

        applyThreadUpdateLocal(updatedThread);
        return true;
      } catch (error) {
        if (threadExecutionPolicyRequestIdsRef.current[currentThread.id] !== requestId) {
          return false;
        }

        // If the backend rejects a sandbox override because Codex runs in
        // external sandbox mode, remember that and retry without the override
        // so presets keep working even when the health heuristic missed it.
        if (
          isCodexThread &&
          isCodexExternalSandboxWarning(String(error)) &&
          (nextPatch.sandboxMode === "read-only" || nextPatch.sandboxMode === "workspace-write")
        ) {
          setCodexExternalSandboxActive(true);
          return onThreadExecutionPolicyChange({ ...nextPatch, sandboxMode: "inherit" });
        }

        toast.error(t("panel.toasts.updateExecutionPolicyFailed", { error: String(error) }));
        try {
          await refreshThreads(currentThread.workspaceId);
        } catch {
          // 后端保存已经失败，刷新失败不能把本次任务伪装成成功。
        }
        return false;
      }
    })();
    let trackedPromise: Promise<boolean>;
    trackedPromise = savePromise.finally(() => {
      if (threadExecutionPolicyRequestsRef.current[currentThread.id] === trackedPromise) {
        delete threadExecutionPolicyRequestsRef.current[currentThread.id];
      }
    });
    threadExecutionPolicyRequestsRef.current[currentThread.id] = trackedPromise;
    return trackedPromise;
  }
  */

  /* 旧 PermissionPicker 的预设变更回调已停用，保留完整迁移记录。
  function onAutonomyPresetChange(preset: AutonomyPresetId) {
    if (!activeThreadAutonomyEngineId) {
      return;
    }
    void onThreadExecutionPolicyChange(
      autonomyPresetPatch(preset, activeThreadAutonomyEngineId, {
        codexExternalSandbox: codexExternalSandboxActive,
      }) as ThreadExecutionPolicyPatch,
    );
  }
  */

  async function onDefaultAutonomyPresetChange(preset: AutonomyPresetId | null) {
    const previous = defaultAutonomyPreset;
    setDefaultAutonomyPreset(preset);
    try {
      const saved = await ipc.setDefaultAutonomyPreset(preset);
      setDefaultAutonomyPreset(isAutonomyPresetId(saved) ? saved : null);
    } catch (error) {
      setDefaultAutonomyPreset(previous);
      toast.error(t("autonomy.defaultSaveFailed", { error: String(error) }));
    }
  }

  const batchApprovalInFlightRef = useRef(false);

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
      for (const approval of batchApprovableRows) {
        const details = approval.details ?? {};
        const accepted = await respondApproval(
          approval.approvalId,
          isPermissionsRequestApproval(details)
            ? buildPermissionApprovalResponseForEngine(engineId, details, "accept")
            : { decision: "accept" },
          targetThreadId,
        );
        if (!accepted) {
          toast.error(t("panel.toasts.approvalBatchFailed"));
          return;
        }
      }

      if (stopAsking) {
        /*
         * 旧实现通过 autonomyPresetExecutionPolicyRequest 按 CLI 生成
         * ThreadExecutionPolicyPatch，再调用 setThreadExecutionPolicy；该调用链已停用，
         * 旧代码保留如下作为迁移记录：
         *
         * const autonomyEngineId = activeThreadAutonomyEngineId;
         * if (stopAsking && autonomyEngineId) {
         *   const request = autonomyPresetExecutionPolicyRequest("full", autonomyEngineId, {
         *     codexExternalSandbox: codexExternalSandboxActive,
         *   });
         *   if (request) {
         *     const requestId =
         *       (threadExecutionPolicyRequestIdsRef.current[targetThreadId] ?? 0) + 1;
         *     threadExecutionPolicyRequestIdsRef.current[targetThreadId] = requestId;
         *     try {
         *       const updatedThread = await ipc.setThreadExecutionPolicy(targetThreadId, request);
         *       if (threadExecutionPolicyRequestIdsRef.current[targetThreadId] === requestId) {
         *         applyThreadUpdateLocal(updatedThread);
         *       }
         *     } catch (error) {
         *       if (threadExecutionPolicyRequestIdsRef.current[targetThreadId] === requestId) {
         *         toast.error(t("panel.toasts.updateExecutionPolicyFailed", { error: String(error) }));
         *       }
         *     }
         *   }
         * }
         *
         * 现在统一权限组件产生六键 JSON，由当前 CLI 实现类完成原始权限适配并保存。
         */
        const next = {
          ...permissionComponent,
          autonomyPreset: ["full"],
        } satisfies PermissionComponentJson;
        await onPermissionComponentChange(next);
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

  const onMessageRowHeightChange = useCallback(
    (messageId: string, height: number) => {
      const normalizedHeight = Math.max(56, Math.ceil(height));
      const previousHeight = messageHeightsRef.current.get(messageId);
      if (
        previousHeight !== undefined &&
        Math.abs(previousHeight - normalizedHeight) < 2
      ) {
        return;
      }

      messageHeightsRef.current.set(messageId, normalizedHeight);
      scheduleListLayoutVersionBump();
    },
    [scheduleListLayoutVersionBump],
  );

  const virtualizationEnabled =
    messages.length >= MESSAGE_VIRTUALIZATION_THRESHOLD;

  useEffect(() => {
    recordPerfMetric("chat.render.commit.ms", performance.now() - renderStartedAtRef.current, {
      threadId,
      messageCount: messages.length,
      virtualized: virtualizationEnabled,
      streaming,
    });
  }, [messages.length, streaming, threadId, virtualizationEnabled]);

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

  const virtualizedLayout = useMemo(() => {
    if (!virtualizationEnabled || messages.length === 0) {
      return null;
    }

    const rowCount = messages.length;
    const offsets = new Array<number>(rowCount + 1);
    offsets[0] = 0;

    for (let index = 0; index < rowCount; index += 1) {
      const messageId = messages[index].id;
      const measuredHeight = messageHeightsRef.current.get(messageId);
      const rowHeight = measuredHeight ?? MESSAGE_ESTIMATED_ROW_HEIGHT;
      offsets[index + 1] =
        offsets[index] + rowHeight + (index < rowCount - 1 ? MESSAGE_ROW_GAP : 0);
    }

    return {
      offsets,
      rowCount,
    };
  }, [messages, virtualizationEnabled, listLayoutVersion]);

  const virtualWindow = useMemo(() => {
    if (!virtualizedLayout) {
      return null;
    }

    const { offsets, rowCount } = virtualizedLayout;

    const visibleStart = Math.max(0, viewportScrollTop - MESSAGE_OVERSCAN_PX);
    const visibleEnd =
      viewportScrollTop + viewportHeight + MESSAGE_OVERSCAN_PX;

    // Binary search: find first row whose bottom edge (offsets[i+1]) >= visibleStart
    let lo = 0;
    let hi = rowCount;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (offsets[mid + 1] < visibleStart) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    const startIndex = lo;

    // Binary search: find first row whose top edge (offsets[i]) > visibleEnd
    lo = startIndex;
    hi = rowCount;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (offsets[mid] <= visibleEnd) {
        lo = mid + 1;
      } else {
        hi = mid;
      }
    }
    let endIndexExclusive = lo;

    if (endIndexExclusive <= startIndex) {
      endIndexExclusive = Math.min(rowCount, startIndex + 1);
    }

    return {
      startIndex,
      endIndexExclusive,
      topSpacerHeight: offsets[startIndex],
      bottomSpacerHeight: offsets[rowCount] - offsets[endIndexExclusive],
    };
  }, [
    virtualizedLayout,
    viewportHeight,
    viewportScrollTop,
  ]);

  const visibleMessages = useMemo(() => {
    if (!virtualizationEnabled || !virtualWindow) {
      return messages;
    }

    return messages.slice(virtualWindow.startIndex, virtualWindow.endIndexExclusive);
  }, [messages, virtualWindow, virtualizationEnabled]);

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
      {!embedded && (!focusMode || showSidebar) && (
        <div
          onMouseDown={handleDragMouseDown}
          onDoubleClick={handleDragDoubleClick}
          style={{
            height: "var(--panel-header-height)",
            padding: "0 16px",
            paddingLeft: showSidebar ? 16 : (customWindowFrame ? 16 : 80),
            display: "flex",
            alignItems: "center",
            gap: 8,
            borderBottom: "1px solid var(--border)",
            flexShrink: 0,
          }}
        >
          {/* Breadcrumb: workspace / thread title / +N files */}
          <div className="no-drag" style={{ flex: 1, display: "flex", alignItems: "center", gap: 0, minWidth: 0 }}>
            {workspaceName && (
              <>
                <span
                  style={{
                    fontSize: 12,
                    color: "var(--text-3)",
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {workspaceName}
                </span>
                <span style={{ fontSize: 12, color: "var(--border)", margin: "0 6px", flexShrink: 0 }}>/</span>
              </>
            )}
            {editingThreadTitle && activeThread ? (
              <input
                ref={titleInputRef}
                value={threadTitleDraft}
                onChange={(event) => setThreadTitleDraft(event.target.value)}
                onBlur={cancelThreadTitleEdit}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void saveThreadTitleEdit();
                    return;
                  }
                  if (event.key === "Escape") {
                    event.preventDefault();
                    cancelThreadTitleEdit();
                  }
                }}
                style={{
                  minWidth: 120,
                  width: "100%",
                  fontSize: 13.5,
                  fontWeight: 600,
                  letterSpacing: "-0.01em",
                  color: "var(--text-1)",
                  background: "var(--bg-3)",
                  border: "1px solid var(--border-active)",
                  borderRadius: "var(--radius-sm)",
                  padding: "4px 8px",
                }}
              />
            ) : (
              <button
                type="button"
                onClick={startThreadTitleEdit}
                disabled={!activeThread}
                title={activeThread ? t("panel.renameThread") : ""}
                style={{
                  border: "none",
                  background: "transparent",
                  padding: "2px 6px",
                  margin: 0,
                  fontSize: 13.5,
                  fontWeight: 600,
                  letterSpacing: "-0.01em",
                  color: "var(--text-1)",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  cursor: activeThread ? "text" : "default",
                  textAlign: "left",
                  borderRadius: "var(--radius-sm)",
                  transition: "background var(--duration-fast) var(--ease-out)",
                }}
                onMouseEnter={(e) => {
                  if (activeThread) e.currentTarget.style.background = "var(--wash-04)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "transparent";
                }}
              >
                {activeThread?.title || (
                  layoutMode === "terminal" ? t("panel.threadTitle.terminal")
                  : layoutMode === "editor" ? t("panel.threadTitle.fileEditor")
                  : layoutMode === "split" ? t("panel.threadTitle.newChat")
                  : t("panel.threadTitle.newChat")
                )}
              </button>
            )}
            {totalAdded > 0 && (
              <>
                <span style={{ fontSize: 12, color: "var(--border)", margin: "0 6px", flexShrink: 0 }}>/</span>
                <span
                  style={{
                    fontSize: 11,
                    fontFamily: '"Geist Mono", ui-monospace, monospace',
                    color: "var(--warning)",
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {t("panel.changedFiles", { count: totalAdded })}
                </span>
              </>
            )}
          </div>

          {/* Right-side action buttons */}
          {!embedded && (
          <div className="no-drag" style={{ display: "flex", alignItems: "center", gap: 4 }}>
            <div className="layout-mode-switcher">
              <button
                type="button"
                title={t("panel.layout.chatOnly")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "chat")}
                className={`layout-mode-btn ${isChatLayoutActive ? "active" : ""}`}
              >
                <MessageSquare size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.splitView")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "split")}
                className={`layout-mode-btn ${isSplitLayoutActive ? "active" : ""}`}
              >
                <Monitor size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.terminalOnly")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "terminal")}
                className={`layout-mode-btn ${isTerminalLayoutActive ? "active" : ""}`}
              >
                <SquareTerminal size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.fileEditor")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "editor")}
                className={`layout-mode-btn ${isEditorLayoutActive ? "active" : ""}`}
              >
                <FilePen size={12} />
              </button>
            </div>
          </div>
          )}
        </div>
      )}

      {showFocusModeHeader && (
        <div
          className="chat-focus-header"
          onMouseDown={handleDragMouseDown}
          onDoubleClick={handleDragDoubleClick}
        >
          <div
            className="chat-focus-header-leading"
            style={{ width: useTitlebarSafeInset ? 74 : 16 }}
          />

          <div className="chat-focus-header-content no-drag" style={{ flex: 1, display: "flex", alignItems: "center", gap: 0, minWidth: 0 }}>
            {workspaceName && (
              <>
                <span
                  style={{
                    fontSize: 12,
                    color: "var(--text-3)",
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {workspaceName}
                </span>
                <span style={{ fontSize: 12, color: "var(--border)", margin: "0 6px", flexShrink: 0 }}>/</span>
              </>
            )}
            {editingThreadTitle && activeThread ? (
              <input
                ref={titleInputRef}
                value={threadTitleDraft}
                onChange={(event) => setThreadTitleDraft(event.target.value)}
                onBlur={cancelThreadTitleEdit}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void saveThreadTitleEdit();
                    return;
                  }
                  if (event.key === "Escape") {
                    event.preventDefault();
                    cancelThreadTitleEdit();
                  }
                }}
                style={{
                  minWidth: 120,
                  width: "100%",
                  fontSize: 13.5,
                  fontWeight: 600,
                  letterSpacing: "-0.01em",
                  color: "var(--text-1)",
                  background: "var(--bg-3)",
                  border: "1px solid var(--border-active)",
                  borderRadius: "var(--radius-sm)",
                  padding: "4px 8px",
                }}
              />
            ) : (
              <button
                type="button"
                onClick={startThreadTitleEdit}
                disabled={!activeThread}
                title={activeThread ? t("panel.renameThread") : ""}
                style={{
                  border: "none",
                  background: "transparent",
                  padding: "2px 6px",
                  margin: 0,
                  fontSize: 13.5,
                  fontWeight: 600,
                  letterSpacing: "-0.01em",
                  color: "var(--text-1)",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  cursor: activeThread ? "text" : "default",
                  textAlign: "left",
                  borderRadius: "var(--radius-sm)",
                  transition: "background var(--duration-fast) var(--ease-out)",
                }}
                onMouseEnter={(e) => {
                  if (activeThread) e.currentTarget.style.background = "var(--wash-04)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "transparent";
                }}
              >
                {activeThread?.title || (
                  layoutMode === "split" ? t("panel.threadTitle.newChat")
                  : t("panel.threadTitle.newChat")
                )}
              </button>
            )}
            {totalAdded > 0 && (
              <>
                <span style={{ fontSize: 12, color: "var(--border)", margin: "0 6px", flexShrink: 0 }}>/</span>
                <span
                  style={{
                    fontSize: 11,
                    fontFamily: '"Geist Mono", ui-monospace, monospace',
                    color: "var(--warning)",
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {t("panel.changedFiles", { count: totalAdded })}
                </span>
              </>
            )}
          </div>

          {!embedded && (
          <div className="no-drag" style={{ display: "flex", alignItems: "center", gap: 4 }}>
            <div className="layout-mode-switcher">
              <button
                type="button"
                title={t("panel.layout.chatOnly")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "chat")}
                className={`layout-mode-btn ${isChatLayoutActive ? "active" : ""}`}
              >
                <MessageSquare size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.splitView")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "split")}
                className={`layout-mode-btn ${isSplitLayoutActive ? "active" : ""}`}
              >
                <Monitor size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.terminalOnly")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "terminal")}
                className={`layout-mode-btn ${isTerminalLayoutActive ? "active" : ""}`}
              >
                <SquareTerminal size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.fileEditor")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "editor")}
                className={`layout-mode-btn ${isEditorLayoutActive ? "active" : ""}`}
              >
                <FilePen size={12} />
              </button>
            </div>
          </div>
          )}
        </div>
      )}

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
            {isFileDropOver && (
              <div
                style={{
                  position: "absolute",
                  inset: 12,
                  borderRadius: "var(--radius-md)",
                  border: "1px solid var(--info-border)",
                  background: "var(--info-surface)",
                  color: "var(--text-1)",
                  fontSize: 13,
                  fontWeight: 600,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  pointerEvents: "none",
                  zIndex: 5,
                }}
              >
                {t("panel.dropFiles")}
              </div>
            )}
            {/* ── Messages ── */}
            {streaming && turnStartedAt !== null && (
              <WorkingDurationIndicator startedAt={turnStartedAt} />
            )}
            <div
              ref={viewportRef}
              onMouseUp={(event: ReactMouseEvent<HTMLDivElement>) => {
                if (!activeWorkspaceId || event.button !== 0) {
                  return;
                }
                const selection = window.getSelection();
                const selectedText = selection?.toString().trim() ?? "";
                const viewport = viewportRef.current;
                const target = event.target instanceof Element ? event.target : null;
                if (
                  !selection ||
                  selection.rangeCount === 0 ||
                  !selectedText ||
                  !viewport?.contains(selection.getRangeAt(0).commonAncestorContainer) ||
                  !target?.closest(".msg-row")
                ) {
                  setTextAnnotationPopover(null);
                  setTextAnnotationComment("");
                  return;
                }
                setTextAnnotationComment("");
                setTextAnnotationPopover({
                  selectedText,
                  left: Math.max(
                    12,
                    Math.min(event.clientX + 12, window.innerWidth - 292),
                  ),
                  top: Math.max(
                    12,
                    Math.min(event.clientY + 12, window.innerHeight - 152),
                  ),
                  stage: "actions",
                });
              }}
              style={{
                position: "relative",
                flex: 1,
                overflow: "auto",
                padding: "20px 24px",
              }}
            >
        {messages.length === 0 && !pendingSubmission ? (
          <div
            className="animate-fade-in"
            style={{
              height: "100%",
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              justifyContent: "center",
              gap: 14,
              color: "var(--text-3)",
              textAlign: "center",
            }}
          >
            <div className="chat-empty-tile">
              <MessageSquare size={18} />
            </div>
            <div>
              <p style={{ margin: "0 0 4px", fontSize: 14, fontWeight: 500, color: "var(--text-2)" }}>
                {t("panel.startConversation")}
              </p>
              <p style={{ margin: 0, fontSize: 12.5 }}>
                {activeWorkspaceId && gitStatus?.branch
                  ? t("panel.emptyScopeRepoBranch", { repo: workspaceName, branch: gitStatus.branch })
                  : t("panel.emptyHint")}
              </p>
            </div>
            {activeWorkspaceId && (
              <div className="chat-empty-suggestions">
                <button
                  type="button"
                  className="chat-empty-suggestion"
                  onClick={() => handleEditResend(t("panel.emptySuggestionSummarize"))}
                >
                  {t("panel.emptySuggestionSummarize")}
                </button>
                <button
                  type="button"
                  className="chat-empty-suggestion"
                  onClick={() => handleEditResend(t("panel.emptySuggestionTests"))}
                >
                  {t("panel.emptySuggestionTests")}
                </button>
              </div>
            )}
            <p style={{ margin: 0, fontSize: 11 }}>
              <span className="chat-empty-kbd">⌘K</span> {t("panel.emptyHintCommands")}
              <span style={{ opacity: 0.5, padding: "0 5px" }}>·</span>
              <span className="chat-empty-kbd">/</span> {t("panel.emptyHintSlash")}
            </p>
          </div>
        ) : virtualizationEnabled && virtualWindow ? (
          <div style={{ display: "flex", flexDirection: "column" }}>
            {virtualWindow.topSpacerHeight > 0 && (
              <div style={{ height: virtualWindow.topSpacerHeight }} />
            )}

            <div style={{ display: "flex", flexDirection: "column", gap: MESSAGE_ROW_GAP }}>
              {visibleMessages.map((message, relativeIndex) => {
                  const absoluteIndex = virtualWindow.startIndex + relativeIndex;
                  const assistantIdentity = assistantIdentityByMessageId.get(message.id);
                  return (
                    <MeasuredMessageRow
                      key={message.id}
                      messageId={message.id}
                      onHeightChange={onMessageRowHeightChange}
                    >
                      <MessageRow
                        message={message}
                        index={absoluteIndex}
                        isHighlighted={message.id === highlightedMessageId}
                        assistantLabel={assistantIdentity?.label ?? ""}
                        assistantEngineId={assistantIdentity?.engineId ?? ""}
                        assistantEngineName={assistantIdentity?.engineName ?? ""}
                        allowInitialPreparation={
                          !sessionReady && message.id === messages[messages.length - 1]?.id
                        }
                        allowTurnStartedThinking={
                          sessionReady && message.id === messages[messages.length - 1]?.id
                        }
                        preparingLabel={
                          activeWorkspace?.locationKind === "ssh" &&
                          preparingAttachments &&
                          message.id === messages[messages.length - 1]?.id
                            ? t("panel.uploadingRemoteAttachments")
                            : activeWorkspace?.locationKind === "ssh" &&
                                !sessionReady &&
                                preparingEngineId === "claude" &&
                                message.id === messages[messages.length - 1]?.id
                              ? t("panel.preparingRemoteEngine", { engine: "Claude Code" })
                              : undefined
                        }
                        onApproval={handleApproval}
                        onLoadActionOutput={handleLoadActionOutput}
                        onEditResend={handleEditResend}
                        onOpenDiffFile={handleOpenDiffFile}
                        onOpenImageAttachment={handleOpenImageAttachment}
                      />
                    </MeasuredMessageRow>
                  );
                })}
            </div>

            {virtualWindow.bottomSpacerHeight > 0 && (
              <div style={{ height: virtualWindow.bottomSpacerHeight }} />
            )}
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: MESSAGE_ROW_GAP }}>
            {visibleMessages.map((message, index) => {
              const assistantIdentity = assistantIdentityByMessageId.get(message.id);
              return (
                <MessageRow
                  key={message.id}
                  message={message}
                  index={index}
                  isHighlighted={message.id === highlightedMessageId}
                  assistantLabel={assistantIdentity?.label ?? ""}
                  assistantEngineId={assistantIdentity?.engineId ?? ""}
                  assistantEngineName={assistantIdentity?.engineName ?? ""}
                  allowInitialPreparation={
                    !sessionReady && message.id === messages[messages.length - 1]?.id
                  }
                  allowTurnStartedThinking={
                    sessionReady && message.id === messages[messages.length - 1]?.id
                  }
                  preparingLabel={
                    activeWorkspace?.locationKind === "ssh" &&
                    preparingAttachments &&
                    message.id === messages[messages.length - 1]?.id
                      ? t("panel.uploadingRemoteAttachments")
                      : activeWorkspace?.locationKind === "ssh" &&
                          !sessionReady &&
                          preparingEngineId === "claude" &&
                          message.id === messages[messages.length - 1]?.id
                        ? t("panel.preparingRemoteEngine", { engine: "Claude Code" })
                        : undefined
                  }
                  onApproval={handleApproval}
                  onLoadActionOutput={handleLoadActionOutput}
                  onEditResend={handleEditResend}
                  onOpenDiffFile={handleOpenDiffFile}
                  onOpenImageAttachment={handleOpenImageAttachment}
                />
              );
            })}
          </div>
        )}

        {hasPendingFlexibleMessages && !pendingSubmission && (
          <div
            style={{
              display: "flex",
              flexDirection: "column",
              marginTop: messages.length > 0 ? MESSAGE_ROW_GAP : 0,
            }}
          >
            <FlexibleMessageGroup
              messages={pendingFlexibleMessages}
              confirmDisabled={flexibleMessageConfirmDisabled}
              confirmTitle={flexibleMessageConfirmTitle}
              onConfirm={() => void submitFlexibleMessages()}
              onWithdraw={withdrawFlexibleMessage}
            />
          </div>
        )}

        {pendingSubmission && !streaming && (
          <div
            style={{
              display: "flex",
              flexDirection: "column",
              gap: 6,
              marginTop: messages.length > 0 ? MESSAGE_ROW_GAP : 0,
            }}
          >
            <MessageRow
              message={pendingSubmission}
              index={messages.length}
              isHighlighted={false}
              assistantLabel=""
              assistantEngineId=""
              assistantEngineName=""
              allowInitialPreparation={false}
              allowTurnStartedThinking={false}
              onApproval={handleApproval}
              onLoadActionOutput={handleLoadActionOutput}
              onOpenImageAttachment={handleOpenImageAttachment}
            />
            <div
              role="status"
              aria-live="polite"
              style={{
                display: "inline-flex",
                alignItems: "center",
                alignSelf: "flex-start",
                gap: 7,
                padding: "4px 14px 8px",
                color: "var(--text-3)",
                fontSize: 12,
              }}
            >
              <Loader2
                size={12}
                className="chat-send-spinner"
                aria-hidden="true"
                style={{ color: "var(--info)" }}
              />
              <span>{t("panel.sendingMessage")}</span>
            </div>
          </div>
        )}

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
            </div>

            {textAnnotationPopover && (
              <div
                ref={textAnnotationPopoverRef}
                className="chat-text-annotation-popover"
                style={{
                  left: textAnnotationPopover.left,
                  top: textAnnotationPopover.top,
                }}
                role="dialog"
                aria-label={t("panel.textAnnotations.dialogLabel")}
              >
                {textAnnotationPopover.stage === "actions" ? (
                  <button
                    type="button"
                    className="chat-text-annotation-add-button"
                    onClick={() => {
                      setTextAnnotationPopover((current) =>
                        current ? { ...current, stage: "comment" } : null,
                      );
                    }}
                  >
                    {t("panel.textAnnotations.addToChat")}
                  </button>
                ) : (
                  <form
                    className="chat-text-annotation-comment-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      const comment = textAnnotationComment.trim();
                      if (!comment) {
                        return;
                      }
                      setTextAnnotations((current) => [
                        ...current,
                        {
                          id: crypto.randomUUID(),
                          selectedText: textAnnotationPopover.selectedText,
                          comment,
                        },
                      ]);
                      setTextAnnotationPopover(null);
                      setTextAnnotationComment("");
                      window.getSelection()?.removeAllRanges();
                      inputRef.current?.focus();
                    }}
                  >
                    <input
                      ref={annotationCommentInputRef}
                      value={textAnnotationComment}
                      onChange={(event) => setTextAnnotationComment(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key !== "Escape") {
                          return;
                        }
                        event.preventDefault();
                        setTextAnnotationPopover(null);
                        setTextAnnotationComment("");
                      }}
                      placeholder={t("panel.textAnnotations.placeholder")}
                      aria-label={t("panel.textAnnotations.placeholder")}
                    />
                    <div className="chat-text-annotation-comment-actions">
                      <button
                        type="submit"
                        className="chat-text-annotation-confirm-button"
                        disabled={!textAnnotationComment.trim()}
                      >
                        {t("panel.textAnnotations.confirm")}
                      </button>
                      <button
                        type="button"
                        className="chat-text-annotation-cancel-button"
                        onClick={() => {
                          setTextAnnotationPopover(null);
                          setTextAnnotationComment("");
                        }}
                      >
                        {t("panel.textAnnotations.cancel")}
                      </button>
                    </div>
                  </form>
                )}
              </div>
            )}

            {/* ── Input Area ── */}
            <div className="chat-composer-surface">
        <form
          onSubmit={onSubmit}
          style={{
            display: "flex",
            flexDirection: "column",
            gap: 8,
          }}
        >
          {/* Pending approvals */}
          {pendingApprovalBannerRows.length > 0 && (
            <div className="chat-approval-banner">
              {showApprovalBannerHeader && (
                <div className="approval-header">
                  <span className="approval-header-icon">
                    <Shield size={11} />
                  </span>
                  <span className="approval-header-title">
                    {t("panel.approvalBannerTitleCount", {
                      count: pendingApprovalBannerRows.length,
                    })}
                  </span>
                  <span className="approval-header-spacer" />
                  {batchApprovableRows.length > 1 &&
                    activeThreadApprovalDecisionCapabilities.includes("accept") && (
                      <>
                        <button
                          type="button"
                          className="approval-batch-btn"
                          onClick={() => void allowAllPendingApprovals(false)}
                        >
                          {t("autonomy.allowAll")}
                        </button>
                        {Boolean(activeThread?.id) && (
                          <button
                            type="button"
                            className="approval-batch-btn"
                            onClick={() => void allowAllPendingApprovals(true)}
                            title={t("autonomy.presets.full.description")}
                          >
                            {t("autonomy.allowAllStopAsking")}
                          </button>
                        )}
                      </>
                    )}
                  {workspaceTrustLevel !== "trusted" && (
                    <button
                      type="button"
                      className="approval-trust-btn"
                      onClick={() => void onWorkspaceTrustLevelChange("trusted")}
                      title={t("panel.setWorkspaceTrusted")}
                    >
                      {t("panel.trustWorkspace")}
                    </button>
                  )}
                </div>
              )}

              <div className="approval-rows">
                {pendingApprovalBannerRows.slice(-3).map((approval) => {
                  const details = approval.details ?? {};
                  const isPermissionsRequest = isPermissionsRequestApproval(details);
                  const isToolInputRequest = isRequestUserInputApproval(details);
                  const isMcpElicitationRequest = isMcpElicitationApproval(details);
                  const requiresCustomPayload = requiresCustomApprovalPayload(details);
                  const canUseDecisionActions = canUseApprovalDecisionActions(
                    activeThread?.engineId,
                    details,
                  );
                  const toolInputQuestionCount = isToolInputRequest
                    ? parseToolInputQuestions(details).length
                    : 0;
                  const isClaudeApproval = activeThread?.engineId === "claude";
                  const supportsDecline =
                    canUseDecisionActions &&
                    activeThreadApprovalDecisionCapabilities.includes("decline");
                  const supportsCancel =
                    canUseDecisionActions &&
                    activeThreadApprovalDecisionCapabilities.includes("cancel");
                  const supportsSession =
                    activeThreadApprovalDecisionCapabilities.includes("accept_for_session");
                  const supportsAccept =
                    activeThreadApprovalDecisionCapabilities.includes("accept");
                  const proposedExecpolicyAmendment =
                    parseProposedExecpolicyAmendment(details);
                  const proposedNetworkPolicyAmendments =
                    parseProposedNetworkPolicyAmendments(details);
                  const hasUnsupportedClaudePayload = shouldShowClaudeUnsupportedApproval(
                    details,
                    true,
                    isClaudeApproval,
                  );
                  const canUseToolInputComposer =
                    isToolInputRequest &&
                    toolInputQuestionCount > 0 &&
                    (!isClaudeApproval || isSupportedClaudeToolInputApproval(details));
                  const showToolInputComposerHint = canUseToolInputComposer;
                  const canSelectToolInputComposer =
                    isClaudeApproval &&
                    canUseToolInputComposer &&
                    approval.approvalId !== pendingToolInputApproval?.approvalId;
                  const hidePositiveApprovalActions =
                    isToolInputRequest && toolInputQuestionCount === 0;
                  const command = parseApprovalCommand(details);
                  const reason = parseApprovalReason(details);

                  return (
                    <div
                      key={approval.approvalId}
                      className="chat-approval-row"
                    >
                      <div className="approval-row-info">
                        <div className="approval-row-head">
                          <span className="approval-row-icon">
                            {approvalRowIcon(approval.actionType)}
                          </span>
                          <div
                            className="approval-row-summary"
                            title={approval.summary}
                          >
                            {approval.summary}
                          </div>
                        </div>
                        {command && (
                          <div className="approval-row-command">{command}</div>
                        )}
                        {reason && (
                          <div className="approval-row-reason">{reason}</div>
                        )}
                      </div>

                      <div className="approval-actions">
                        {hasUnsupportedClaudePayload ? (
                          <>
                            <span className="approval-row-hint">
                              {t("panel.claudeApprovalUnsupported")}
                            </span>
                            {supportsDecline && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-deny"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    decision: "decline",
                                  })
                                }
                              >
                                {t("panel.approvalActions.deny")}
                              </button>
                            )}
                          </>
                        ) : showToolInputComposerHint || requiresCustomPayload ? (
                          <>
                            <span className="approval-row-hint">
                              {showToolInputComposerHint
                                ? t("panel.respondInCard")
                                : isMcpElicitationRequest
                                  ? t("panel.respondToMcpElicitation")
                                  : t("panel.respondInCustomCard")}
                            </span>
                            {canSelectToolInputComposer && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-allow"
                                onClick={() =>
                                  setSelectedPendingToolInputApprovalId(
                                    approval.approvalId,
                                  )
                                }
                              >
                                {t("panel.approvalActions.answerBelow")}
                              </button>
                            )}
                            {isMcpElicitationRequest && (
                              <>
                                <button
                                  type="button"
                                  className="approval-btn approval-btn-deny"
                                  onClick={() =>
                                    void respondApproval(approval.approvalId, {
                                      action: "decline",
                                    })
                                  }
                                >
                                  {t("panel.approvalActions.deny")}
                                </button>
                                <button
                                  type="button"
                                  className="approval-btn approval-btn-allow"
                                  onClick={() =>
                                    void respondApproval(
                                      approval.approvalId,
                                      buildMcpElicitationApprovalResponse(details, "accept"),
                                    )
                                  }
                                >
                                  {t("panel.approvalActions.approve")}
                                </button>
                              </>
                            )}
                          </>
                        ) : (
                          <>
                            {supportsDecline && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-deny"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    ...(isPermissionsRequest
                                      ? buildPermissionApprovalResponseForEngine(
                                          activeThread?.engineId,
                                          details,
                                          "decline",
                                        )
                                      : isToolInputRequest
                                        ? { action: "decline" }
                                        : { decision: "decline" }),
                                  })
                                }
                              >
                                {t("panel.approvalActions.deny")}
                              </button>
                            )}
                            {supportsCancel && !isPermissionsRequest && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-cancel"
                                onClick={() =>
                                  void respondApproval(
                                    approval.approvalId,
                                    isToolInputRequest
                                      ? { action: "cancel" }
                                      : { decision: "cancel" },
                                  )
                                }
                              >
                                {t("panel.approvalActions.cancel")}
                              </button>
                            )}
                            {!hidePositiveApprovalActions && (
                              <span className="approval-actions-gap" />
                            )}
                            {!hidePositiveApprovalActions && supportsSession && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-session"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    ...(isPermissionsRequest
                                      ? buildPermissionApprovalResponseForEngine(
                                          activeThread?.engineId,
                                          details,
                                          "accept_for_session",
                                        )
                                      : { decision: "accept_for_session" }),
                                  })
                                }
                              >
                                {t("panel.approvalActions.allowSession")}
                              </button>
                            )}
                            {!hidePositiveApprovalActions && !isClaudeApproval && !isPermissionsRequest && proposedExecpolicyAmendment.length > 0 && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-session"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    acceptWithExecpolicyAmendment: {
                                      execpolicy_amendment: proposedExecpolicyAmendment,
                                    },
                                  })
                                }
                              >
                                {t("panel.allowWithPolicy")}
                              </button>
                            )}
                            {!hidePositiveApprovalActions && !isClaudeApproval && !isPermissionsRequest && proposedNetworkPolicyAmendments.map((amendment) => (
                              <button
                                key={`${amendment.action}:${amendment.host}`}
                                type="button"
                                className="approval-btn approval-btn-session"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    applyNetworkPolicyAmendment: {
                                      network_policy_amendment: amendment,
                                    },
                                  })
                                }
                                title={t("panel.approvalActions.hostActionTitle", {
                                  action: amendment.action === "allow"
                                    ? t("panel.approvalActions.allow")
                                    : t("panel.approvalActions.block"),
                                  host: amendment.host,
                                })}
                              >
                                {amendment.action === "allow"
                                  ? t("panel.approvalActions.allowHost")
                                  : t("panel.approvalActions.blockHost")}
                              </button>
                            ))}
                            {!hidePositiveApprovalActions && supportsAccept && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-allow"
                                onClick={() =>
                                  void respondApproval(
                                    approval.approvalId,
                                    isPermissionsRequest
                                      ? buildPermissionApprovalResponseForEngine(
                                          activeThread?.engineId,
                                          details,
                                          "accept",
                                        )
                                      : { decision: "accept" },
                                  )
                                }
                              >
                                {t("panel.approvalActions.allow")}
                              </button>
                            )}
                          </>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Input container */}
          <div
            className={`chat-input-box ${activePlanMode && !showSpecialInputComposer ? "chat-input-box-plan" : ""} ${showSpecialInputComposer ? "chat-input-box-tool-input" : ""}`.trim()}
            onPaste={handleInputPaste}
          >
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

                <textarea
                  ref={inputRef}
                  rows={3}
                  value={input}
                  onChange={(e) => {
                    inputHistCursorRef.current = -1;
                    setInput(e.target.value);
                    handleSlashDetection(
                      e.target.value,
                      e.target.selectionStart ?? e.target.value.length,
                    );
                    handleThreadMentionDetection(
                      e.target.value,
                      e.target.selectionStart ?? e.target.value.length,
                    );
                  }}
                  onKeyDown={(e) => {
                    /* ── AuraCoder 会话 @ 菜单键盘导航 ── */
                    if (threadMentionMenuOpen && threadMentionCandidates.length > 0) {
                      if (e.key === "ArrowDown") {
                        e.preventDefault();
                        setThreadMentionActiveIndex((index) =>
                          Math.min(index + 1, threadMentionCandidates.length - 1),
                        );
                        return;
                      }
                      if (e.key === "ArrowUp") {
                        e.preventDefault();
                        setThreadMentionActiveIndex((index) => Math.max(index - 1, 0));
                        return;
                      }
                      if (e.key === "Enter" || e.key === "Tab") {
                        e.preventDefault();
                        const thread = threadMentionCandidates[
                          Math.min(threadMentionActiveIndex, threadMentionCandidates.length - 1)
                        ];
                        if (thread) handleThreadMentionSelect(thread);
                        return;
                      }
                      if (e.key === "Escape") {
                        e.preventDefault();
                        setThreadMentionMenuOpen(false);
                        return;
                      }
                    }
                    /* ── Slash menu keyboard nav ── */
                    if (slashMenuOpen) {
                      if (e.key === "ArrowDown") {
                        e.preventDefault();
                        setSlashMenuActiveIndex((i) =>
                          Math.min(i + 1, filteredSlashCommands.length - 1),
                        );
                        return;
                      }
                      if (e.key === "ArrowUp") {
                        e.preventDefault();
                        setSlashMenuActiveIndex((i) => Math.max(i - 1, 0));
                        return;
                      }
                      if (e.key === "Enter" || e.key === "Tab") {
                        e.preventDefault();
                        const cmd = filteredSlashCommands[Math.min(slashMenuActiveIndex, filteredSlashCommands.length - 1)];
                        if (cmd) handleSlashCommandSelect(cmd.id);
                        return;
                      }
                      if (e.key === "Escape") {
                        e.preventDefault();
                        setSlashMenuOpen(false);
                        return;
                      }
                    }
                    /* ── Command panel dismiss ── */
                    if (activeCommandPanel && e.key === "Escape") {
                      e.preventDefault();
                      setActiveCommandPanel(null);
                      setCommandPanelError(null);
                      return;
                    }
                    /* ── Input history cycling (Option+Up / Option+Down) ── */
                    if (e.altKey && (e.key === "ArrowUp" || e.key === "ArrowDown")) {
                      const history = inputHistoryRef.current;
                      if (history.length === 0) return;
                      e.preventDefault();
                      if (e.key === "ArrowUp") {
                        if (inputHistCursorRef.current === -1) {
                          inputLiveDraftRef.current = input;
                        }
                        const next = Math.min(inputHistCursorRef.current + 1, history.length - 1);
                        inputHistCursorRef.current = next;
                        setInput(history[next]);
                      } else {
                        const next = inputHistCursorRef.current - 1;
                        inputHistCursorRef.current = next;
                        if (next < 0) {
                          setInput(inputLiveDraftRef.current);
                        } else {
                          setInput(history[next]);
                        }
                      }
                      return;
                    }
                    if (shouldSubmitChatInput({
                      key: e.key,
                      ctrlKey: e.ctrlKey,
                      metaKey: e.metaKey,
                      shiftKey: e.shiftKey,
                      isComposing: e.nativeEvent.isComposing,
                    }, sendShortcut)) {
                      e.preventDefault();
                      if (streaming && !canSteerActiveTurn && sessionMessageSendMode !== "flexible") {
                        return;
                      }
                      void onSubmit(e);
                    }
                    if (e.shiftKey && e.key === "Tab") {
                      e.preventDefault();
                      if (activeWorkspaceId && !isOpenCodeEngine) {
                        const nextPlanMode = !planMode;
                        setPlanMode(nextPlanMode);
                        void saveThreadRuntimeSelectionPatch(
                          activeThread?.id ?? activeChatSessionId,
                          { planMode: nextPlanMode },
                        );
                      }
                    }
                  }}
                  placeholder={
                    activePlanMode
                      ? t("panel.placeholders.plan")
                      : t("panel.placeholders.chat")
                  }
                  disabled={!activeWorkspaceId}
                  style={{
                    width: "100%",
                    padding: "12px 14px",
                    background: "transparent",
                    color: "var(--text-1)",
                    fontSize: 13,
                    lineHeight: 1.6,
                    resize: "none",
                    fontFamily: "inherit",
                    caretColor: activePlanMode ? "var(--accent-2)" : "var(--accent)",
                  }}
                />

                <ChatThreadMentionMenu
                  visible={threadMentionMenuOpen && threadMentionCandidates.length > 0}
                  threads={threadMentionCandidates}
                  anchorRef={inputRef}
                  activeIndex={threadMentionActiveIndex}
                  onSelect={handleThreadMentionSelect}
                  onDismiss={() => setThreadMentionMenuOpen(false)}
                  onActiveChange={setThreadMentionActiveIndex}
                />

                {/* Slash command menu (portal) */}
                <ChatSlashMenu
                  visible={slashMenuOpen && filteredSlashCommands.length > 0}
                  commands={filteredSlashCommands}
                  anchorRef={inputRef}
                  activeIndex={slashMenuActiveIndex}
                  onSelect={handleSlashCommandSelect}
                  onDismiss={() => setSlashMenuOpen(false)}
                  onActiveChange={setSlashMenuActiveIndex}
                />
              </>
            )}

            {/* Input toolbar with selectors */}
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
                  (isCodexEngine || selectedEngineId === "claude") &&
                  usageLimits?.contextPercent != null && (
                  <button
                    type="button"
                    className={`chat-context-ring${
                      usageLimits.contextPercent <= 10
                        ? " chat-context-ring--critical"
                        : usageLimits.contextPercent <= 25
                          ? " chat-context-ring--warning"
                          : ""
                    }`}
                    onClick={openUsageLimitsModal}
                    title={t("status.contextRingTitle", {
                      percent: formatUsagePercent(usageLimits.contextPercent),
                    })}
                    aria-label={t("status.contextRingTitle", {
                      percent: formatUsagePercent(usageLimits.contextPercent),
                    })}
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
                          2 * Math.PI * 6 * (1 - Math.max(0, Math.min(100, usageLimits.contextPercent)) / 100)
                        }
                      />
                    </svg>
                    <span>{formatUsagePercent(usageLimits.contextPercent)}</span>
                  </button>
                )}

                {streaming && !showSpecialInputComposer && (
                  <button
                    type="button"
                    className="chat-stop-btn"
                    onClick={() => void cancel()}
                  >
                    <Square size={11} fill="currentColor" />
                    {t("panel.stop")}
                  </button>
                )}

                {(!streaming || canSteerActiveTurn || sessionMessageSendMode === "flexible") && !showSpecialInputComposer && (
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
          </div>

          {/* Bottom status bar with context usage */}
          <div className="chat-status-bar">
            {(isCodexEngine || selectedEngineId === "claude") && (
              usageLimits ? (
                <div className="chat-status-usage">
                  <button
                    type="button"
                    className="chat-context-section"
                    onClick={openUsageLimitsModal}
                    title={t("status.openUsageLimits")}
                  >
                    <Clock size={10} />
                    <span>{t("status.windowFiveHoursLeft")}</span>
                    <div className="chat-context-progress">
                      <div
                        className={`chat-context-progress-fill${usageProgressLevelClass(usageLimits.windowFiveHourPercent)}`}
                        style={{ width: usagePercentToWidth(usageLimits.windowFiveHourPercent) }}
                      />
                    </div>
                    <span className="chat-context-percent">
                      {formatUsagePercent(usageLimits.windowFiveHourPercent)}
                    </span>
                    {usageLimits.windowFiveHourResetsAt && (
                      <span className="chat-context-reset">
                        {t("status.resets", {
                          time: formatResetTime(t, usageLimits.windowFiveHourResetsAt),
                        })}
                      </span>
                    )}
                  </button>

                  <span className="chat-context-divider">&middot;</span>

                  <button
                    type="button"
                    className="chat-context-section"
                    onClick={openUsageLimitsModal}
                    title={t("status.openUsageLimits")}
                  >
                    <Clock size={10} />
                    <span>{t("status.windowWeeklyLeft")}</span>
                    <div className="chat-context-progress">
                      <div
                        className={`chat-context-progress-fill${usageProgressLevelClass(usageLimits.windowWeeklyPercent)}`}
                        style={{ width: usagePercentToWidth(usageLimits.windowWeeklyPercent) }}
                      />
                    </div>
                    <span className="chat-context-percent">
                      {formatUsagePercent(usageLimits.windowWeeklyPercent)}
                    </span>
                    {usageLimits.windowWeeklyResetsAt && (
                      <span className="chat-context-reset">
                        {t("status.resets", {
                          time: formatResetTime(t, usageLimits.windowWeeklyResetsAt),
                        })}
                      </span>
                    )}
                  </button>

                  {selectedClaudeWeeklyUsage && (
                    <>
                      <span className="chat-context-divider">&middot;</span>

                      <button
                        type="button"
                        className="chat-context-section"
                        onClick={openUsageLimitsModal}
                        title={t("status.openUsageLimits")}
                      >
                        <Clock size={10} />
                        <span>{selectedClaudeWeeklyUsage.label}</span>
                        <div className="chat-context-progress">
                          <div
                            className={`chat-context-progress-fill${usageProgressLevelClass(selectedClaudeWeeklyUsage.percent)}`}
                            style={{ width: usagePercentToWidth(selectedClaudeWeeklyUsage.percent) }}
                          />
                        </div>
                        <span className="chat-context-percent">
                          {formatUsagePercent(selectedClaudeWeeklyUsage.percent)}
                        </span>
                        {selectedClaudeWeeklyUsage.resetsAt && (
                          <span className="chat-context-reset">
                            {t("status.resets", {
                              time: formatResetTime(t, selectedClaudeWeeklyUsage.resetsAt),
                            })}
                          </span>
                        )}
                      </button>
                    </>
                  )}
                </div>
              ) : (
                <div className="chat-context-section">
                  <Clock size={10} />
                  <span>
                    {t(resolveUsageStatusKey(hasUserMessage, streaming || usageLimitsLoading))}
                  </span>
                </div>
              )
            )}

            {/* Branch */}
            {gitStatus?.branch && (
              <span className="chat-status-branch">
                <GitBranch size={11} />
                {gitStatus.branch}
              </span>
            )}
          </div>
        </form>

              {error && (
                <div className="msg-error-block" style={{ marginTop: 8, fontSize: 12 }}>
                  <AlertTriangle size={14} style={{ flexShrink: 0, marginTop: 2 }} />
                  {error}
                </div>
              )}
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

    </div>
  );
}
