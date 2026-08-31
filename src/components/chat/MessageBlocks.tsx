import {
  memo,
  useCallback,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { useTranslation } from "react-i18next";
import {
  CheckCircle2,
  Circle,
  AlertTriangle,
  AtSign,
  CornerDownRight,
  ChevronDown,
  ChevronRight,
  ChevronUp,
  DollarSign,
  ExternalLink,
  FileCode2,
  FileDiff,
  Maximize2,
  Minimize2,
  Terminal,
  Shield,
  Loader2,
  XCircle,
  Brain,
  Info,
  Layers,
  Copy,
  Check,
  MessageSquare,
} from "lucide-react";
import type {
  ActionBlock,
  ApprovalBlock,
  ApprovalResponse,
  AttachmentBlock,
  ClaudeBackgroundTask,
  ClaudeBackgroundTaskMetadata,
  ContentBlock,
  DiffBlock,
  MessageStatus,
  NoticeBlock,
  SteerBlock,
  ThinkingBlock,
} from "../../types";
import {
  // buildMcpElicitationApprovalResponse, // MCP 决策统一由 ChatPanel 底部授权栏提交。
  buildDynamicToolCallResponse,
  defaultAdvancedApprovalPayload,
  isDynamicToolCallApproval,
  isMcpElicitationApproval,
  isPermissionsRequestApproval,
  isRequestUserInputApproval,
  isSupportedClaudeToolInputApproval,
  parseApprovalCommand,
  parseApprovalReason,
  parseDynamicToolCallArguments,
  parseDynamicToolCallName,
  parseMcpElicitationMessage,
  parseMcpElicitationMode,
  parseMcpElicitationSchema,
  parseMcpElicitationServerName,
  parseMcpElicitationUrl,
  parseProposedExecpolicyAmendment,
  parseProposedNetworkPolicyAmendments,
  parseRequestedPermissions,
  parseToolInputQuestions,
  requiresCustomApprovalPayload,
} from "./toolInputApproval";
import {
  extractDiffFilename,
} from "../../lib/parseDiff";
import { getActionGroupId, getMessageBlockKey } from "./messageBlockKeys";
import {
  VirtualizedDiffBody,
  useParsedDiff,
} from "../shared/DiffViewer";
import MarkdownContent from "./MarkdownContent";
import { AttachmentChip } from "./AttachmentChip";
import { useChatFileContextMenu } from "./useChatFileContextMenu";
import {
  extractTextLinkMatches,
  getWorkspacePaneLeafIdFromEventTarget,
  navigateLinkTarget,
} from "../../lib/fileLinkNavigation";
import { shouldOpenLink } from "../../lib/linkOpenSettings";
import { useChatComposerStore } from "../../stores/chatComposerStore";
interface Props {
  messageId: string;
  blocks?: ContentBlock[];
  status?: MessageStatus;
  engineId?: string;
  onApproval: (approvalId: string, response: ApprovalResponse) => void;
  onLoadActionOutput?: (actionId: string) => Promise<void>;
  onOpenDiffFile?: (filePath: string) => void;
  onOpenImageAttachment?: (attachment: AttachmentBlock) => void;
}

function isBlockLike(value: unknown): value is { type: string } {
  return typeof value === "object" && value !== null && "type" in value;
}

function dedupeDiffBlocksByScope(blocks: ContentBlock[]): ContentBlock[] {
  const latestDiffIndexByScope = new Map<string, number>();
  blocks.forEach((block, index) => {
    if (block.type === "diff") {
      latestDiffIndexByScope.set(String(block.scope ?? "turn"), index);
    }
  });

  if (latestDiffIndexByScope.size === 0) {
    return blocks;
  }

  return blocks.filter((block, index) => {
    if (block.type !== "diff") {
      return true;
    }
    return latestDiffIndexByScope.get(String(block.scope ?? "turn")) === index;
  });
}

function CodeBlockCopyButton({ content }: { content: string }) {
  const [copied, setCopied] = useState(false);
  const handleCopy = useCallback(() => {
    navigator.clipboard.writeText(content).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }, [content]);
  return (
    <button
      type="button"
      onClick={handleCopy}
      style={{
        marginLeft: "auto", flexShrink: 0, cursor: "pointer",
        background: "none", border: "none", padding: "2px",
        color: copied ? "var(--success)" : "var(--text-3)",
        opacity: copied ? 1 : 0.5,
        transition: "color var(--duration-fast) var(--ease-out), opacity var(--duration-fast) var(--ease-out)",
      }}
      aria-label="Copy code"
    >
      {copied ? <Check size={12} /> : <Copy size={12} />}
    </button>
  );
}

function handleToggleKeyDown(e: React.KeyboardEvent, toggle: () => void) {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    toggle();
  }
}

function handlePlainTextLinkClick(
  event: ReactMouseEvent<HTMLAnchorElement>,
  target: string,
) {
  if (event.defaultPrevented || event.button !== 0) {
    return;
  }

  event.preventDefault();
  const linkOpenGesture = useChatComposerStore.getState().linkOpenGesture;
  if (!shouldOpenLink(event.shiftKey, linkOpenGesture)) {
    return;
  }

  event.stopPropagation();
  void navigateLinkTarget(target, {
    shiftKey: event.shiftKey,
    sourceLeafId: getWorkspacePaneLeafIdFromEventTarget(event.currentTarget),
  });
}

function LinkifiedPlainText({ text }: { text: string }) {
  const matches = useMemo(() => extractTextLinkMatches(text), [text]);
  const { openLocalFileContextMenu, contextMenu } = useChatFileContextMenu();
  if (matches.length === 0) {
    return <>{text}</>;
  }

  const nodes: ReactNode[] = [];
  let cursor = 0;
  for (const match of matches) {
    if (match.startIndex > cursor) {
      nodes.push(text.slice(cursor, match.startIndex));
    }
    nodes.push(
      <a
        key={`${match.startIndex}:${match.endIndex}:${match.text}`}
        href={match.text}
        className="chat-plain-link"
        rel="noreferrer noopener"
        onClick={(event) => handlePlainTextLinkClick(event, match.text)}
        onContextMenu={(event) => {
          openLocalFileContextMenu(
            event,
            match.text,
            getWorkspacePaneLeafIdFromEventTarget(event.currentTarget),
          );
        }}
      >
        {match.text}
      </a>,
    );
    cursor = match.endIndex;
  }

  if (cursor < text.length) {
    nodes.push(text.slice(cursor));
  }

  return <>{nodes}{contextMenu}</>;
}

interface MessageBlockHeaderProps {
  icon: ReactNode;
  label: ReactNode;
  meta?: ReactNode;
  expanded?: boolean;
  labelMono?: boolean;
  tileTone?: "neutral" | "violet" | "amber" | "info";
  onToggle?: () => void;
}

function MessageBlockHeader({
  icon,
  label,
  meta,
  expanded = false,
  labelMono = false,
  tileTone = "neutral",
  onToggle,
}: MessageBlockHeaderProps) {
  const interactive = onToggle != null;
  const tileToneClass = tileTone === "neutral" ? "" : ` msg-block-tile--${tileTone}`;

  return (
    <div
      className={`msg-block-header${interactive ? "" : " msg-block-header--static"}`}
      {...(interactive
        ? {
            role: "button" as const,
            tabIndex: 0,
            "aria-expanded": expanded,
            onClick: onToggle,
            onKeyDown: (event: React.KeyboardEvent) =>
              handleToggleKeyDown(event, onToggle),
          }
        : {})}
    >
      {interactive ? (
        <ChevronRight
          size={11}
          className={`msg-block-chevron${expanded ? " msg-block-chevron-open" : ""}`}
        />
      ) : (
        <span className="msg-block-chevron-spacer" aria-hidden="true" />
      )}
      <span className={`msg-block-tile${tileToneClass}`}>{icon}</span>
      <span
        className={`msg-block-label${labelMono ? " msg-block-label--mono" : ""}`}
      >
        {label}
      </span>
      {meta != null && <span className="msg-block-meta">{meta}</span>}
    </div>
  );
}

const actionIcons: Record<string, typeof Terminal> = {
  command: Terminal,
  file_write: FileCode2,
  file_edit: FileCode2,
  file_read: FileCode2,
  file_delete: FileCode2,
};

/* ── Action Group Segmentation ── */

const ACTION_GROUP_MIN_SIZE = 3;

type InnerSegment =
  | { kind: "single"; block: ContentBlock; index: number }
  | { kind: "action-group"; blocks: ActionBlock[]; indices: number[] };

type BlockSegment =
  | InnerSegment
  | { kind: "hook-group"; blocks: NoticeBlock[]; indices: number[] }
  // 旧逻辑保留，不执行，已由子代理来源分支替代：
  // | { kind: "action-card"; segments: InnerSegment[] };
  | { kind: "action-card"; segments: InnerSegment[] }
  | {
      kind: "subagent-card";
      /** 子代理线程标识。 */
      threadId: string;
      /** 子代理动作与 Hook 内容块。 */
      blocks: ContentBlock[];
      /** 内容块在原始消息中的索引。 */
      indices: number[];
    };

function getSubagentThreadId(block: ContentBlock): string | null {
  if (block.type === "action") {
    const value = block.details?.subagentThreadId;
    return typeof value === "string" && value.trim() ? value.trim() : null;
  }
  if (block.type !== "notice") {
    return null;
  }
  const marker = "::subagent::";
  const markerIndex = block.kind.lastIndexOf(marker);
  if (markerIndex < 0) {
    return null;
  }
  const threadId = block.kind.slice(markerIndex + marker.length).trim();
  return threadId || null;
}

function getSubagentActivityDetails(block: ContentBlock): Record<string, unknown> | null {
  if (block.type !== "action") {
    return null;
  }
  const activity = block.details?.subagentActivity;
  return typeof activity === "string" && activity.trim() ? block.details : null;
}

function isSubagentActivityBlock(block: ContentBlock): boolean {
  return getSubagentActivityDetails(block) != null;
}

function isCardSegment(seg: BlockSegment): seg is InnerSegment {
  if (seg.kind === "action-group") return true;
  if (
    seg.kind === "single" &&
    (
      seg.block.type === "action" ||
      seg.block.type === "diff" ||
      seg.block.type === "thinking" ||
      seg.block.type === "approval"
    )
  ) {
    return true;
  }
  return false;
}

function isCompletedActionSegment(
  segment: InnerSegment,
): segment is { kind: "single"; block: ActionBlock; index: number } {
  return (
    segment.kind === "single" &&
    segment.block.type === "action" &&
    segment.block.status !== "running" &&
    segment.block.status !== "pending"
  );
}

function groupCompletedActionsInCard(cardSegments: InnerSegment[]): InnerSegment[] {
  const actionBlocks: ActionBlock[] = [];
  const indices: number[] = [];
  for (const segment of cardSegments) {
    if (segment.kind === "action-group") {
      actionBlocks.push(...segment.blocks);
      indices.push(...segment.indices);
    } else if (isCompletedActionSegment(segment)) {
      actionBlocks.push(segment.block);
      indices.push(segment.index);
    }
  }

  if (actionBlocks.length < ACTION_GROUP_MIN_SIZE) {
    return cardSegments;
  }

  let insertedGroup = false;
  const groupedSegment: InnerSegment = {
    kind: "action-group",
    blocks: actionBlocks,
    indices,
  };

  const groupedSegments: InnerSegment[] = [];
  for (const segment of cardSegments) {
    if (segment.kind === "action-group" || isCompletedActionSegment(segment)) {
      if (insertedGroup) {
        continue;
      }
      insertedGroup = true;
      groupedSegments.push(groupedSegment);
      continue;
    }
    groupedSegments.push(segment);
  }
  return groupedSegments;
}

function getActionCardAnchorId(
  segment: InnerSegment,
  safeBlocks: ContentBlock[],
): string {
  if (segment.kind === "action-group") {
    return segment.blocks[0].actionId;
  }
  return getMessageBlockKey(segment.block, segment.index, safeBlocks);
}

export function buildBlockSegments(
  blocks: ContentBlock[],
  isStreaming?: boolean,
  engineId?: string,
): BlockSegment[] {
  type DisplayBlock = {
    /** 展示时使用的内容块。 */
    block: ContentBlock;
    /** 内容块在原始 blocks 数组中的索引。 */
    index: number;
  };

  // Codex 按回复边界重排 Hook，确保同一回复段的工具调用保持连续。
  const indexedBlocks: DisplayBlock[] = blocks
    .map((block, index) => ({ block, index }))
    // 已归属到后台任务的操作只在对应任务卡片中展示，不能再混入前台统一汇总。
    .filter(
      (entry) =>
        entry.block.type !== "action" ||
        !(entry.block as ActionBlock).backgroundTaskId,
    );
  const displayBlocks: DisplayBlock[] = [];
  const firstTextIndex = indexedBlocks.findIndex((entry) => entry.block.type === "text");
  if (engineId !== "codex" || firstTextIndex < 0) {
    displayBlocks.push(...indexedBlocks);
  } else {
    displayBlocks.push(...indexedBlocks.slice(0, firstTextIndex));
    let textIndex = firstTextIndex;
    while (textIndex < indexedBlocks.length) {
      displayBlocks.push(indexedBlocks[textIndex]);
      let nextTextIndex = textIndex + 1;
      while (
        nextTextIndex < indexedBlocks.length &&
        indexedBlocks[nextTextIndex].block.type !== "text"
      ) {
        nextTextIndex++;
      }

      const sectionParentHooks: DisplayBlock[] = [];
      const sectionSubagentHooks: DisplayBlock[] = [];
      const sectionOtherBlocks: DisplayBlock[] = [];
      for (const entry of indexedBlocks.slice(textIndex + 1, nextTextIndex)) {
        if (
          entry.block.type === "notice" &&
          (entry.block.kind.startsWith("hook_") ||
            entry.block.kind.startsWith("codex_hook_"))
        ) {
          if (getSubagentThreadId(entry.block)) {
            sectionSubagentHooks.push(entry);
          } else {
            sectionParentHooks.push(entry);
          }
        } else {
          sectionOtherBlocks.push(entry);
        }
      }
      displayBlocks.push(...sectionParentHooks, ...sectionSubagentHooks, ...sectionOtherBlocks);
      textIndex = nextTextIndex;
    }
  }

  // 子代理动作与带来源 Hook 在分段阶段先集中，保证后续普通 action/Hooks 逻辑不会拆散活动卡。
  const subagentGroups = new Map<
    string,
    { blocks: ContentBlock[]; indices: number[]; firstPosition: number }
  >();
  const subagentDisplayPositions = new Set<number>();
  displayBlocks.forEach((entry, position) => {
    const threadId = getSubagentThreadId(entry.block);
    if (!threadId) {
      return;
    }
    const group = subagentGroups.get(threadId);
    if (group) {
      group.blocks.push(entry.block);
      group.indices.push(entry.index);
      subagentDisplayPositions.add(position);
      return;
    }
    subagentGroups.set(threadId, {
      blocks: [entry.block],
      indices: [entry.index],
      firstPosition: position,
    });
  });

  // Phase 1: build flat inner segments
  const flat: BlockSegment[] = [];
  let i = 0;
  while (i < displayBlocks.length) {
    const firstSubagentGroup = [...subagentGroups.values()].find(
      (group) => group.firstPosition === i,
    );
    if (firstSubagentGroup) {
      const threadId = [...subagentGroups.entries()].find(
        ([, group]) => group === firstSubagentGroup,
      )?.[0];
      if (threadId) {
        flat.push({
          kind: "subagent-card",
          threadId,
          blocks: firstSubagentGroup.blocks,
          indices: firstSubagentGroup.indices,
        });
      }
      i++;
      continue;
    }
    if (subagentDisplayPositions.has(i)) {
      i++;
      continue;
    }
    const displayBlock = displayBlocks[i];
    const block = displayBlock.block;
    if (
      block.type === "notice" &&
      (block.kind.startsWith("hook_") || block.kind.startsWith("codex_hook_"))
    ) {
      const hookBlocks: NoticeBlock[] = [];
      const indices: number[] = [];
      while (i < displayBlocks.length) {
        const hookBlock = displayBlocks[i].block;
        // 子代理来源 Hook 必须交给后续子代理活动卡，避免被普通 Hooks 分组吞掉。
        if (getSubagentThreadId(hookBlock)) {
          break;
        }
        if (
          hookBlock.type !== "notice" ||
          (!hookBlock.kind.startsWith("hook_") &&
            !hookBlock.kind.startsWith("codex_hook_"))
        ) {
          break;
        }
        hookBlocks.push(hookBlock);
        indices.push(displayBlocks[i].index);
        i++;
      }
      flat.push({ kind: "hook-group", blocks: hookBlocks, indices });
      continue;
    }
    if (block.type !== "action") {
      flat.push({ kind: "single", block, index: displayBlock.index });
      i++;
      continue;
    }

    // 连续收集父代理 action；带合法子代理线程号的 action 是父代理工具卡边界。
    const runStart = i;
    while (
      i < displayBlocks.length &&
      displayBlocks[i].block.type === "action" &&
      getSubagentThreadId(displayBlocks[i].block) === null
    ) {
      i++;
    }
    const runEnd = i; // exclusive

    // Split the run: active (running/pending) actions break out as singles,
    // completed sub-runs of 3+ become groups
    let subStart = runStart;
    while (subStart < runEnd) {
      const actionBlock = displayBlocks[subStart].block as ActionBlock;
      if (actionBlock.status === "running" || actionBlock.status === "pending") {
        flat.push({
          kind: "single",
          block: actionBlock,
          index: displayBlocks[subStart].index,
        });
        subStart++;
        continue;
      }

      // Collect consecutive completed/error actions
      let subEnd = subStart;
      while (subEnd < runEnd) {
        const ab = displayBlocks[subEnd].block as ActionBlock;
        if (ab.status === "running" || ab.status === "pending") break;
        subEnd++;
      }

      const count = subEnd - subStart;
      if (!isStreaming && count >= ACTION_GROUP_MIN_SIZE) {
        const groupBlocks = displayBlocks
          .slice(subStart, subEnd)
          .map((entry) => entry.block) as ActionBlock[];
        const indices = displayBlocks
          .slice(subStart, subEnd)
          .map((entry) => entry.index);
        flat.push({ kind: "action-group", blocks: groupBlocks, indices });
      } else {
        for (let j = subStart; j < subEnd; j++) {
          flat.push({
            kind: "single",
            block: displayBlocks[j].block,
            index: displayBlocks[j].index,
          });
        }
      }
      subStart = subEnd;
    }
  }

  // Phase 2: wrap consecutive action segments into action-card containers
  const segments: BlockSegment[] = [];
  let j = 0;
  while (j < flat.length) {
    const seg = flat[j];
    if (!isCardSegment(seg)) {
      segments.push(seg);
      j++;
      continue;
    }
    const cardSegments: InnerSegment[] = [seg];
    j++;
    while (j < flat.length && isCardSegment(flat[j])) {
      cardSegments.push(flat[j] as InnerSegment);
      j++;
    }
    segments.push({
      kind: "action-card",
      segments: groupCompletedActionsInCard(cardSegments),
    });
  }
  return segments;
}

/* ── Diff Block ── */

function MessageDiffBlock({
  block,
  onOpenDiffFile,
}: {
  block: DiffBlock;
  onOpenDiffFile?: (filePath: string) => void;
}) {
  const { t } = useTranslation("chat");
  const [expanded, setExpanded] = useState(false);
  const raw = String(block.diff ?? "");
  const fallbackFilename = useMemo(() => extractDiffFilename(raw), [raw]);
  const {
    parseResult,
    loading: loadingParse,
    parseAttempted,
  } = useParsedDiff(raw, {
    enabled: expanded,
  });
  const filename = parseResult?.filename ?? fallbackFilename;
  const adds = parseResult?.adds ?? 0;
  const dels = parseResult?.dels ?? 0;

  const toggleExpanded = useCallback(() => setExpanded((v) => !v), []);
  return (
    <div>
      <MessageBlockHeader
        icon={<FileDiff size={11} />}
        label={
          <LinkifiedPlainText text={filename ?? t("messageBlocks.diffFallback", { scope: String(block.scope ?? "turn") })} />
        }
        labelMono
        expanded={expanded}
        onToggle={toggleExpanded}
        meta={
          <>
          {loadingParse && <span>{t("messageBlocks.parsing")}</span>}
          {(adds > 0 || dels > 0) && (
            <span style={{ display: "inline-flex", gap: 5 }}>
              {adds > 0 && <span style={{ color: "var(--success)" }}>+{adds}</span>}
              {dels > 0 && <span style={{ color: "var(--danger)" }}>-{dels}</span>}
            </span>
          )}
          {onOpenDiffFile && filename && (
            <button
              type="button"
              className="msg-row-action-btn"
              onClick={(event) => {
                event.stopPropagation();
                onOpenDiffFile(filename);
              }}
              title={t("messageBlocks.openInEditor")}
              aria-label={t("messageBlocks.openInEditor")}
            >
              <ExternalLink size={11} />
            </button>
          )}
          </>
        }
      />
      {expanded && (
        !parseResult && (loadingParse || !parseAttempted) ? (
          <div style={{ padding: "4px 14px", fontSize: 11.5, color: "var(--text-3)" }}>
            {t("messageBlocks.parsingDiff")}
          </div>
        ) : parseResult && parseResult.parsed.length > 0 ? (
          <div style={{
            margin: "2px 12px 4px",
            borderRadius: "var(--radius-sm)",
            border: "1px solid var(--border)",
            background: "var(--code-bg)",
          }}>
            <VirtualizedDiffBody parsed={parseResult.parsed} foldContext />
          </div>
        ) : (
          <div style={{ padding: "4px 14px", fontSize: 11.5, color: "var(--text-3)" }}>
            {t("messageBlocks.noChanges")}
          </div>
        )
      )}
    </div>
  );
}

/* ── Thinking Block ── */

function ThinkingBlockView({ block, isStreaming }: { block: ThinkingBlock; isStreaming: boolean }) {
  const { t } = useTranslation("chat");
  const [expanded, setExpanded] = useState(false);
  const content = String(block.content ?? "");

  const durationSec = block.durationMs != null ? Math.round(block.durationMs / 1000) : null;
  const thinkingLabel = isStreaming
    ? `${t("messageBlocks.thinking")}\u2026`
    : t("messageBlocks.thought");
  const toggleExpanded = useCallback(() => setExpanded((v) => !v), []);

  return (
    <div>
      <MessageBlockHeader
        icon={<Brain size={11} />}
        label={<span className={isStreaming ? "msg-shimmer" : undefined}>{thinkingLabel}</span>}
        tileTone="violet"
        expanded={expanded}
        onToggle={toggleExpanded}
        meta={
          !isStreaming && durationSec != null && durationSec > 0
            ? t("messageBlocks.thinkingDuration", { seconds: durationSec })
            : undefined
        }
      />
      {expanded && (
        <div className="msg-block-body">
          <MarkdownContent
            content={content}
            streaming={isStreaming}
            enableFileContextMenu
            className="prose"
            style={{
              fontSize: 12.5,
              color: "var(--text-2)",
              minWidth: 0,
            }}
          />
        </div>
      )}
    </div>
  );
}

/** 判断 Notice 元数据是否符合 Claude 后台任务卡片的数据契约。 */
function isClaudeBackgroundTaskMetadata(
  value: unknown,
): value is ClaudeBackgroundTaskMetadata {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }
  const metadata = value as Record<string, unknown>;
  const activeTaskCount = metadata.activeTaskCount;
  if (
    !Array.isArray(metadata.backgroundTasks) ||
    typeof activeTaskCount !== "number" ||
    !Number.isInteger(activeTaskCount) ||
    activeTaskCount < 0
  ) {
    return false;
  }

  return metadata.backgroundTasks.every((candidate) => {
    if (typeof candidate !== "object" || candidate === null || Array.isArray(candidate)) {
      return false;
    }
    const task = candidate as Record<string, unknown>;
    const validStatus =
      task.status === "running" ||
      task.status === "completed" ||
      task.status === "failed" ||
      task.status === "stopped";
    return (
      typeof task.taskId === "string" &&
      typeof task.taskType === "string" &&
      typeof task.description === "string" &&
      validStatus &&
      typeof task.startedAt === "number" &&
      Number.isFinite(task.startedAt) &&
      (task.summary === undefined || typeof task.summary === "string") &&
      (task.finishedAt === undefined ||
        (typeof task.finishedAt === "number" && Number.isFinite(task.finishedAt)))
    );
  });
}

/** 将后台任务持续时间格式化为用户可读的分钟和秒。 */
function formatClaudeBackgroundElapsed(startedAt: number, endedAt: number): string {
  const elapsedSeconds = Math.max(0, Math.floor((endedAt - startedAt) / 1000));
  const minutes = Math.floor(elapsedSeconds / 60);
  const seconds = String(elapsedSeconds % 60).padStart(2, "0");
  return `${minutes}:${seconds}`;
}

/** 渲染 Claude 后台任务生命周期卡片，并保留用户手动控制的折叠状态。 */
function ClaudeBackgroundTasksCard({
  metadata,
  actions,
  onLoadActionOutput,
}: {
  /** Claude 后台任务卡片的结构化展示数据。 */
  metadata: ClaudeBackgroundTaskMetadata;
  /** 当前消息中已确认归属到后台任务的操作。 */
  actions: ActionBlock[];
  /** 延迟加载某条操作完整结果的回调。 */
  onLoadActionOutput?: (actionId: string) => Promise<void>;
}) {
  const tasks = metadata.backgroundTasks;
  const hasRunningTask = tasks.some((task) => task.status === "running");
  const hasFailedTask = tasks.some((task) => task.status === "failed");
  const hasStoppedTask = tasks.some((task) => task.status === "stopped");
  const [now, setNow] = useState(() => Date.now());
  const [expanded, setExpanded] = useState(hasRunningTask || hasFailedTask);
  const previousStatusesRef = useRef(
    new Map<string, ClaudeBackgroundTask["status"]>(
      tasks.map((task) => [task.taskId, task.status]),
    ),
  );

  useEffect(() => {
    if (!hasRunningTask) {
      return undefined;
    }
    const timer = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(timer);
  }, [hasRunningTask]);

  useEffect(() => {
    const previousStatuses = previousStatusesRef.current;
    const newlyFailed = tasks.some(
      (task) =>
        task.status === "failed" &&
        previousStatuses.has(task.taskId) &&
        previousStatuses.get(task.taskId) !== "failed",
    );
    if (newlyFailed) {
      setExpanded(true);
    }
    previousStatusesRef.current = new Map(
      tasks.map((task) => [task.taskId, task.status]),
    );
  }, [tasks]);

  const statusMeta = metadata.activeTaskCount > 0
    ? (
      <span className="claude-background-tasks-status claude-background-tasks-status--running">
        <Loader2 size={11} style={{ animation: "spin 1s linear infinite" }} />
        执行中 · {metadata.activeTaskCount} 项
      </span>
    )
    : (
      <span
        className={`claude-background-tasks-status${hasFailedTask
          ? " claude-background-tasks-status--failed"
          : hasStoppedTask
            ? " claude-background-tasks-status--stopped"
            : ""}`}
      >
        {hasFailedTask ? "有失败任务" : hasStoppedTask ? "已停止" : "已完成"}
      </span>
    );

  return (
    <div className="msg-action-card claude-background-tasks-card">
      <MessageBlockHeader
        icon={<Layers size={11} />}
        label="Claude 后台任务"
        tileTone="info"
        expanded={expanded}
        onToggle={() => setExpanded((current) => !current)}
        meta={statusMeta}
      />
      {expanded && (
        <div className="claude-background-tasks-body">
          {tasks.map((task) => {
            const taskStatusLabel = task.status === "running"
              ? "执行中"
              : task.status === "completed"
                ? "已完成"
                : task.status === "failed"
                  ? "失败"
                  : "已停止";
            const taskStatusIcon = task.status === "running"
              ? <Loader2 size={11} style={{ animation: "spin 1s linear infinite" }} />
              : task.status === "completed"
                ? <CheckCircle2 size={11} />
                : task.status === "failed"
                  ? <XCircle size={11} />
                  : <Circle size={11} />;
            const taskStatusClass = task.status === "running"
              ? "claude-background-task-status--running"
              : task.status === "completed"
                ? "claude-background-task-status--completed"
                : task.status === "failed"
                  ? "claude-background-task-status--failed"
                  : "claude-background-task-status--stopped";
            const elapsedEnd = task.status === "running"
              ? now
              : task.finishedAt ?? now;
            const taskActions = actions.filter((action) => action.backgroundTaskId === task.taskId);
            const failedActionCount = taskActions.filter((action) => action.status === "error").length;
            return (
              <div key={task.taskId} style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                <div className="claude-background-task-row">
                  <span className="claude-background-task-icon" aria-hidden="true">
                    {taskStatusIcon}
                  </span>
                  <div className="claude-background-task-content">
                    <div className="claude-background-task-heading">
                      <span
                        className="claude-background-task-description"
                        title={task.description}
                      >
                        {task.description}
                      </span>
                      <span className={`claude-background-task-status ${taskStatusClass}`}>
                        {taskStatusIcon}
                        {taskStatusLabel}
                      </span>
                    </div>
                    {task.summary && (
                      <div
                        className="claude-background-task-summary"
                        title={task.summary}
                      >
                        {task.summary}
                      </div>
                    )}
                  </div>
                  <span className="claude-background-task-elapsed">
                    {formatClaudeBackgroundElapsed(task.startedAt, elapsedEnd)}
                  </span>
                </div>
                {taskActions.length > 0 && (
                  <div className="action-group-body action-group-body--expanded">
                    <div className="action-group-body-inner" style={{ display: "flex", flexDirection: "column", gap: 2 }}>
                      <div className="msg-block-meta" style={{ padding: "3px 12px 1px" }}>
                        {taskActions.length} 个操作{failedActionCount > 0 ? ` · ${failedActionCount} 个错误` : ""}
                      </div>
                      {taskActions.map((action) => (
                        <ActionBlockView
                          key={action.actionId}
                          block={action}
                          onLoadDeferredOutput={
                            onLoadActionOutput ? () => onLoadActionOutput(action.actionId) : undefined
                          }
                        />
                      ))}
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

function NoticeBlockView({
  block,
  backgroundTaskActions = [],
  onLoadActionOutput,
}: {
  block: NoticeBlock;
  backgroundTaskActions?: ActionBlock[];
  onLoadActionOutput?: (actionId: string) => Promise<void>;
}) {
  if (
    block.kind === "claude_background_tasks" &&
    isClaudeBackgroundTaskMetadata(block.metadata)
  ) {
    return (
      <ClaudeBackgroundTasksCard
        metadata={block.metadata}
        actions={backgroundTaskActions}
        onLoadActionOutput={onLoadActionOutput}
      />
    );
  }

  return (
    <div className="msg-notice">
      <span className="msg-block-tile msg-block-tile--info">
        <Info size={11} />
      </span>
      <div className="msg-notice-content">
        <div className="msg-notice-title">{block.title}</div>
        <div className="msg-notice-message">{block.message}</div>
      </div>
    </div>
  );
}

function SteerBlockView({
  block,
  onOpenImageAttachment,
}: {
  block: SteerBlock;
  onOpenImageAttachment?: (attachment: AttachmentBlock) => void;
}) {
  const { t } = useTranslation("chat");
  const attachmentBlocks = block.attachments ?? [];
  const skillBlocks = block.skills ?? [];
  const mentionBlocks = block.mentions ?? [];
  const hasContent = block.content.trim().length > 0;
  const deliveryStatus = block.deliveryStatus ?? "settled";

  return (
    <div className="msg-notice msg-notice--steer">
      <span className="msg-block-tile msg-block-tile--accent">
        <CornerDownRight size={11} />
      </span>
      <div className="msg-notice-content">
        {hasContent && (
          <div className="msg-notice-message">
            {block.content}
          </div>
        )}

        {(skillBlocks.length > 0 || mentionBlocks.length > 0 || attachmentBlocks.length > 0) && (
          <div style={{ display: "flex", flexWrap: "wrap", gap: 6 }}>
            {skillBlocks.map((skill) => (
              <span
                key={`skill:${skill.path}`}
                className="chat-attachment-chip chat-attachment-chip--skill"
                style={{ display: "inline-flex" }}
              >
                <DollarSign size={10} />
                <span className="chat-attachment-chip-name">{skill.name}</span>
              </span>
            ))}
            {mentionBlocks.map((mention) => (
              <span
                key={`mention:${mention.path}`}
                className="chat-attachment-chip chat-attachment-chip--mention"
                style={{ display: "inline-flex" }}
              >
                <AtSign size={10} />
                <span className="chat-attachment-chip-name">{mention.name}</span>
              </span>
            ))}
            {attachmentBlocks.map((attachment) => {
              return (
                <AttachmentChip
                  key={`attachment:${attachment.filePath}:${attachment.fileName}`}
                  attachment={attachment}
                  onOpen={onOpenImageAttachment
                    ? () => onOpenImageAttachment(attachment)
                    : undefined}
                />
              );
            })}
          </div>
        )}
        {deliveryStatus !== "settled" && (
          <div
            className={`msg-steer-delivery${deliveryStatus === "failed" ? " msg-steer-delivery--failed" : ""}`}
            role="status"
            aria-live="polite"
            title={block.failureReason}
          >
            {deliveryStatus === "sending" ? (
              <Loader2 size={11} className="chat-send-spinner" />
            ) : deliveryStatus === "failed" ? (
              <XCircle size={11} />
            ) : (
              <CheckCircle2 size={11} />
            )}
            <span>{t(`messageBlocks.steerDelivery.${deliveryStatus}`)}</span>
          </div>
        )}
      </div>
    </div>
  );
}

/* ── Action Block ── */

function ActionStatusBadge({ status }: { status: string }) {
  const { t } = useTranslation("chat");
  if (status === "done") {
    return (
      <span className="msg-block-status">
        <CheckCircle2 size={11} />
      </span>
    );
  }
  if (status === "running") {
    return (
      <span className="msg-block-status msg-block-status--warning">
        <Loader2 size={11} style={{ animation: "spin 1s linear infinite" }} />
        {t("messageBlocks.actionStatus.running")}
      </span>
    );
  }
  if (status === "error") {
    return (
      <span className="msg-block-status msg-block-status--danger">
        <XCircle size={11} />
        {t("messageBlocks.actionStatus.error")}
      </span>
    );
  }
  return (
    <span className="msg-block-status">
      <Circle size={11} />
    </span>
  );
}

function ActionBlockView({
  block,
  onLoadDeferredOutput,
}: {
  block: ActionBlock;
  onLoadDeferredOutput?: () => Promise<void>;
}) {
  const { t } = useTranslation("chat");
  const outputChunks = Array.isArray(block.outputChunks) ? block.outputChunks : [];
  const outputDeferred = block.outputDeferred === true;
  const resultOutput = typeof block.result?.output === "string" ? block.result.output : "";
  const hasResultOutput = resultOutput.trim().length > 0;
  const outputText = useMemo(
    () => {
      let raw: string;
      if (outputChunks.length === 0) {
        // 旧逻辑保留，不执行，已由子代理来源分支替代：return "";
        raw = resultOutput;
      } else if (outputChunks.length === 1) {
        // 旧逻辑保留，不执行，已由子代理来源分支替代：
        // if (outputChunks.length === 1) {
        //   const firstContent = outputChunks[0].content;
        //   raw = typeof firstContent === "string" ? firstContent : String(firstContent ?? "");
        // } else {
        //   raw = outputChunks.map((chunk) => String(chunk.content ?? "")).join("");
        // }
        const firstContent = outputChunks[0].content;
        raw = typeof firstContent === "string" ? firstContent : String(firstContent ?? "");
      } else {
        raw = outputChunks.map((chunk) => String(chunk.content ?? "")).join("");
      }
      // Unescape literal \n and \t sequences that come from JSON-encoded engine output
      if (raw.includes("\\n") || raw.includes("\\t")) {
        raw = raw.replace(/\\n/g, "\n").replace(/\\t/g, "\t");
      }
      return raw;
    },
    // 旧逻辑保留，不执行，已由子代理来源分支替代：[outputChunks],
    [outputChunks, resultOutput],
  );
  const Icon = actionIcons[block.actionType] ?? Terminal;
  const isRunning = block.status === "running";
  const isPending = block.status === "pending";
  // 旧逻辑保留，不执行，已由子代理来源分支替代：
  // const hasBody = outputChunks.length > 0 || Boolean(block.result?.error) || outputDeferred;
  const hasBody = outputChunks.length > 0 || hasResultOutput || Boolean(block.result?.error) || outputDeferred;
  const actionDetails = (block.details ?? {}) as Record<string, unknown>;
  const outputTruncated =
    "outputTruncated" in actionDetails && actionDetails.outputTruncated === true;
  const progressMessage =
    actionDetails.progressKind === "mcp" && typeof actionDetails.progressMessage === "string"
      ? actionDetails.progressMessage
      : null;
  const [expanded, setExpanded] = useState(false);
  const [outputExpandedFully, setOutputExpandedFully] = useState(false);
  const [outputCopied, setOutputCopied] = useState(false);
  const [loadingDeferredOutput, setLoadingDeferredOutput] = useState(false);
  const [deferredOutputError, setDeferredOutputError] = useState<string | null>(null);
  const deferredOutputRequestedRef = useRef(false);
  const canToggle = hasBody;

  const handleCopyOutput = useCallback(() => {
    if (!outputText) return;
    navigator.clipboard.writeText(outputText).then(() => {
      setOutputCopied(true);
      setTimeout(() => setOutputCopied(false), 1500);
    });
  }, [outputText]);

  const requestDeferredOutput = useCallback(() => {
    if (!onLoadDeferredOutput || deferredOutputRequestedRef.current) {
      return;
    }

    deferredOutputRequestedRef.current = true;
    setLoadingDeferredOutput(true);
    setDeferredOutputError(null);
    onLoadDeferredOutput()
      .catch((error) => {
        deferredOutputRequestedRef.current = false;
        setDeferredOutputError(String(error));
      })
      .finally(() => {
        setLoadingDeferredOutput(false);
      });
  }, [onLoadDeferredOutput]);

  useEffect(() => {
    if (!expanded || !outputDeferred || outputChunks.length > 0) {
      return;
    }
    requestDeferredOutput();
  }, [expanded, outputDeferred, outputChunks.length, requestDeferredOutput]);

  useEffect(() => {
    if (!outputDeferred || outputChunks.length > 0) {
      deferredOutputRequestedRef.current = false;
    }
  }, [outputDeferred, outputChunks.length]);

  const toggleExpanded = useCallback(() => setExpanded((v) => !v), []);
  return (
    <div>
      <MessageBlockHeader
        icon={<Icon size={11} />}
        label={block.summary}
        labelMono={block.actionType === "command"}
        expanded={expanded}
        onToggle={canToggle ? toggleExpanded : undefined}
        meta={
          <>
          {block.result?.durationMs != null && block.status === "done" && (
            <span>
              {block.result.durationMs < 1000
                ? `${block.result.durationMs}ms`
                : `${(block.result.durationMs / 1000).toFixed(1)}s`}
            </span>
          )}
          <ActionStatusBadge status={block.status} />
          </>
        }
      />

      {progressMessage && (
        <div
          style={{
            padding: "0 12px 6px 30px",
            fontSize: 11,
            color: "var(--text-3)",
            lineHeight: 1.5,
          }}
        >
          {progressMessage}
        </div>
      )}

      {/* 旧逻辑保留，不执行，已由子代理来源分支替代：
          expanded && (outputChunks.length > 0 || block.result?.error || outputDeferred)
      */}
      {expanded && (outputChunks.length > 0 || hasResultOutput || block.result?.error || outputDeferred) && (
        <div style={{
          margin: "2px 12px 4px",
          borderRadius: "var(--radius-sm)",
          border: "1px solid var(--border)",
          overflow: "hidden",
        }}>
          {outputDeferred && outputChunks.length === 0 && (
            <div
              style={{
                margin: 0,
                padding: "8px 12px",
                background: "var(--code-bg)",
                fontSize: 11.5,
                lineHeight: 1.5,
                color: "var(--text-3)",
                display: "flex",
                alignItems: "center",
                gap: 6,
                justifyContent: "space-between",
              }}
            >
              <span style={{ display: "inline-flex", alignItems: "center", gap: 6 }}>
                {loadingDeferredOutput && (
                  <Loader2 size={12} style={{ animation: "spin 1s linear infinite" }} />
                )}
                {loadingDeferredOutput
                  ? t("messageBlocks.deferredOutput.loadingFull")
                  : deferredOutputError
                    ? t("messageBlocks.deferredOutput.failed")
                    : t("messageBlocks.deferredOutput.loading")}
              </span>
              {!loadingDeferredOutput && deferredOutputError && onLoadDeferredOutput && (
                <button
                  type="button"
                  onClick={(event) => {
                    event.stopPropagation();
                    deferredOutputRequestedRef.current = false;
                    requestDeferredOutput();
                  }}
                  style={{
                    border: "1px solid var(--border)",
                    borderRadius: "var(--radius-xs)",
                    padding: "3px 8px",
                    background: "var(--bg-2)",
                    color: "var(--text-2)",
                    fontSize: 10.5,
                    cursor: "pointer",
                  }}
                >
                  {t("messageBlocks.deferredOutput.retry")}
                </button>
              )}
            </div>
          )}

          {/* 旧逻辑保留，不执行，已由子代理来源分支替代：outputChunks.length > 0 */}
          {(outputChunks.length > 0 || hasResultOutput) && (
            <>
              <div className="action-output-well-header">
                <span>{t("messageBlocks.output")}</span>
                <span className="action-output-well-actions">
                  <button
                    type="button"
                    onClick={(event) => {
                      event.stopPropagation();
                      handleCopyOutput();
                    }}
                    title={t("messageBlocks.copyOutput")}
                    aria-label={t("messageBlocks.copyOutput")}
                    style={outputCopied ? { color: "var(--success)" } : undefined}
                  >
                    {outputCopied ? <Check size={11} /> : <Copy size={11} />}
                  </button>
                  <button
                    type="button"
                    onClick={(event) => {
                      event.stopPropagation();
                      setOutputExpandedFully((v) => !v);
                    }}
                    title={
                      outputExpandedFully
                        ? t("messageBlocks.collapseOutput")
                        : t("messageBlocks.expandOutput")
                    }
                    aria-label={
                      outputExpandedFully
                        ? t("messageBlocks.collapseOutput")
                        : t("messageBlocks.expandOutput")
                    }
                  >
                    {outputExpandedFully ? <Minimize2 size={11} /> : <Maximize2 size={11} />}
                  </button>
                </span>
              </div>
              <pre
                className="action-output-pre"
                style={{ maxHeight: outputExpandedFully ? undefined : 260 }}
              >
                <LinkifiedPlainText text={outputText} />
              </pre>
            </>
          )}

          {outputTruncated && (
            <div style={{
              margin: 0, padding: "5px 12px",
              // 旧逻辑保留，不执行，已由子代理来源分支替代：
              // borderTop: outputChunks.length > 0 ? "1px solid var(--border)" : undefined,
              borderTop: outputChunks.length > 0 || hasResultOutput ? "1px solid var(--border)" : undefined,
              background: "var(--neutral-surface)",
              fontSize: 10.5, color: "var(--text-3)",
            }}>
              {t("messageBlocks.outputTruncated")}
            </div>
          )}

          {block.result?.error && (
            <pre
              className="action-output-error"
              style={{
                // 旧逻辑保留，不执行，已由子代理来源分支替代：
                // borderTop: outputChunks.length > 0 || outputTruncated ? "1px solid var(--danger-border)" : undefined,
                borderTop: outputChunks.length > 0 || hasResultOutput || outputTruncated
                  ? "1px solid var(--danger-border)" : undefined,
              }}
            >
              <LinkifiedPlainText text={String(block.result.error)} />
            </pre>
          )}
        </div>
      )}
    </div>
  );
}

function SubagentActivityRow({ block }: { block: ActionBlock }) {
  return (
    <div className="msg-notice">
      <span className="msg-block-tile msg-block-tile--info">
        <Info size={11} />
      </span>
      <div className="msg-notice-content">
        <div className="msg-notice-title">{block.summary}</div>
        <div className="msg-notice-message">
          <ActionStatusBadge status={block.status} />
        </div>
      </div>
    </div>
  );
}

function getSubagentCardStatus(blocks: ContentBlock[]): "running" | "error" | "done" {
  if (
    blocks.some(
      (block) =>
        (block.type === "action" && block.status === "error") ||
        (block.type === "notice" && block.level === "error"),
    )
  ) {
    return "error";
  }
  if (
    blocks.some(
      (block) => block.type === "action" && (block.status === "running" || block.status === "pending"),
    )
  ) {
    return "running";
  }
  return "done";
}

export function getSubagentCardTitle(blocks: ContentBlock[], threadId: string): string {
  for (const block of blocks) {
    const details = getSubagentActivityDetails(block);
    const agentPath = details?.agentPath;
    if (typeof agentPath === "string" && agentPath.trim()) {
      const normalizedPath = agentPath.trim();
      // 子代理标题优先显示路径末段；协议未提供可关联任务标题时，以此作为稳定回退名称。
      // return `子代理：${agentPath.trim()}`;
      const agentName = normalizedPath.split("/").filter(Boolean).at(-1);
      if (agentName) {
        return `子代理：${agentName}`;
      }
    }
  }
  return `子代理：${threadId.slice(0, 8)}`;
}

function SubagentCardView({
  threadId,
  blocks,
  indices,
  onLoadActionOutput,
}: {
  /** 子代理线程标识。 */
  threadId: string;
  /** 该子代理的动作和 Hook 块。 */
  blocks: ContentBlock[];
  /** 对应原消息块索引。 */
  indices: number[];
  /** 延迟加载动作完整输出的回调。 */
  onLoadActionOutput?: (actionId: string) => Promise<void>;
}) {
  const { t } = useTranslation("chat");
  const status = getSubagentCardStatus(blocks);
  const [expanded, setExpanded] = useState(status !== "done");
  const [hooksExpanded, setHooksExpanded] = useState(false);
  const hooksContentId = useId();
  const title = getSubagentCardTitle(blocks, threadId);
  const actionCount = blocks.filter((block) => block.type === "action").length;
  const hookEntries = blocks.flatMap((block, blockIndex) =>
    block.type === "notice" ? [{ block, index: indices[blockIndex] }] : [],
  );
  const hookCount = hookEntries.length;
  const statusLabel = status === "error" ? "错误" : status === "running" ? "执行中" : "已完成";

  return (
    <div className="msg-action-card">
      {/* 局部动作或 Hook 错误不将整张子代理卡标为警示色；保留原选择逻辑供追溯：tileTone={status === "error" ? "amber" : "info"}。 */}
      <MessageBlockHeader
        icon={<Layers size={11} />}
        label={title}
        expanded={expanded}
        onToggle={() => setExpanded((value) => !value)}
        tileTone="info"
        meta={
          <>
            <span>{statusLabel}</span>
            <span>{actionCount} 个动作 · {hookCount} 个 Hook</span>
          </>
        }
      />
      {expanded && (
        <div className="action-group-body action-group-body--expanded">
          <div className="action-group-body-inner" style={{ display: "flex", flexDirection: "column", gap: 2 }}>
            {blocks.map((block) => {
              if (block.type !== "action") return null;
              if (isSubagentActivityBlock(block)) {
                return <SubagentActivityRow key={block.actionId} block={block} />;
              }
              return (
                <ActionBlockView
                  key={block.actionId}
                  block={block}
                  onLoadDeferredOutput={
                    onLoadActionOutput ? () => onLoadActionOutput(block.actionId) : undefined
                  }
                />
              );
            })}
            {hookEntries.length > 0 && (
              <div className="msg-hook-notices">
                <button
                  type="button"
                  className="msg-hooks-toggle"
                  aria-expanded={hooksExpanded}
                  aria-controls={hooksContentId}
                  onClick={() => setHooksExpanded((value) => !value)}
                >
                  <span>{t("messageBlocks.hooks", { count: hookEntries.length })}</span>
                  {hooksExpanded ? <ChevronUp size={13} /> : <ChevronDown size={13} />}
                </button>
                {hooksExpanded && (
                  <div id={hooksContentId} className="msg-hooks-content">
                    {hookEntries.map(({ block, index }) => (
                      <NoticeBlockView
                        key={`${threadId}-hook-${index}`}
                        block={block}
                      />
                    ))}
                  </div>
                )}
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

/* ── Action Group ── */

const actionTypeLabels: Record<string, string> = {
  command: "command",
  file_read: "file_read",
  file_write: "file_write",
  file_edit: "file_edit",
  file_delete: "file_delete",
  git: "git",
  search: "search",
  other: "other",
};

function ActionGroupView({
  blocks,
  expanded,
  onToggle,
  onLoadActionOutput,
}: {
  blocks: ActionBlock[];
  expanded: boolean;
  onToggle: () => void;
  onLoadActionOutput?: (actionId: string) => Promise<void>;
}) {
  const { t } = useTranslation("chat");

  const typeCounts = useMemo(() => {
    const counts: Record<string, number> = {};
    for (const b of blocks) {
      counts[b.actionType] = (counts[b.actionType] ?? 0) + 1;
    }
    return counts;
  }, [blocks]);

  const errorCount = useMemo(
    () => blocks.filter((b) => b.status === "error").length,
    [blocks],
  );
  const hasAnyError = errorCount > 0;
  const allErrored = errorCount === blocks.length;

  const typeBreakdown = useMemo(() => {
    return Object.entries(typeCounts)
      .map(([type, count]) =>
        t(`messageBlocks.actionGroup.types.${actionTypeLabels[type] ?? "other"}`, { count }),
      )
      .join(" · ");
  }, [typeCounts, t]);

  const baseSummary = t("messageBlocks.actionGroup.summary", { count: blocks.length });
  const summaryText = hasAnyError
    ? `${baseSummary}, ${t("messageBlocks.actionGroup.errorCount", { count: errorCount })}`
    : baseSummary;

  return (
    <div className="animate-slide-up">
      <MessageBlockHeader
        icon={<Layers size={11} />}
        label={summaryText}
        expanded={expanded}
        onToggle={onToggle}
        meta={
          <>
          <span>{typeBreakdown}</span>
          {allErrored ? (
            <XCircle size={11} style={{ color: "var(--danger)", flexShrink: 0 }} />
          ) : hasAnyError ? (
            <AlertTriangle size={11} style={{ color: "var(--text-3)", flexShrink: 0 }} />
          ) : (
            <CheckCircle2 size={11} style={{ color: "var(--text-3)", flexShrink: 0 }} />
          )}
          </>
        }
      />
      <div className={`action-group-body${expanded ? " action-group-body--expanded" : ""}`}>
        <div
          className="action-group-body-inner"
          style={{
            background: expanded ? "var(--wash-02)" : undefined,
            borderRadius: "var(--radius-sm)",
            display: "flex",
            flexDirection: "column",
            gap: 2,
          }}
        >
          {expanded &&
            blocks.map((block) => (
              <ActionBlockView
                key={block.actionId}
                block={block}
                onLoadDeferredOutput={
                  onLoadActionOutput ? () => onLoadActionOutput(block.actionId) : undefined
                }
              />
            ))}
        </div>
      </div>
    </div>
  );
}

/* ── Approval Card ── */

const APPROVAL_INTERNAL_KEYS = new Set([
  "_serverMethod",
  "_rawRequestId",
  "_raw_request_id",
  "threadId",
  "thread_id",
  "turnId",
  "turn_id",
  "itemId",
  "item_id",
  "proposedExecpolicyAmendment",
  "proposed_execpolicy_amendment",
  "proposedNetworkPolicyAmendments",
  "proposed_network_policy_amendments",
  "networkApprovalContext",
  "network_approval_context",
  "questions",
  "command",
  "reason",
  "commandActions",
  "callId",
  "call_id",
  "arguments",
  "tool",
  "name",
  "permissions",
  "serverName",
  "server_name",
  "message",
  "mode",
  "url",
  "requestedSchema",
  "requested_schema",
  "elicitationId",
  "elicitation_id",
]);

function extractApprovalDetails(details: Record<string, unknown>) {
  const command = parseApprovalCommand(details);
  const reason = parseApprovalReason(details);
  const commandActions = Array.isArray(details.commandActions) ? details.commandActions : [];
  const commandActionCount = commandActions.length;
  const remainingDetails: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(details)) {
    if (!APPROVAL_INTERNAL_KEYS.has(k)) remainingDetails[k] = v;
  }
  const hasRemainingDetails = Object.keys(remainingDetails).length > 0;
  return { command, reason, commandActionCount, remainingDetails, hasRemainingDetails };
}

function extractAnswerText(raw: unknown): string | null {
  if (typeof raw === "string") return raw;
  if (typeof raw === "object" && raw !== null && !Array.isArray(raw)) {
    const obj = raw as Record<string, unknown>;
    // Shape from buildToolInputResponseFromSelections: { answers: string[] }
    if (Array.isArray(obj.answers) && obj.answers.length > 0) {
      return obj.answers.map(String).join(", ");
    }
    if (typeof obj.label === "string") return obj.label;
    if (typeof obj.value === "string") return obj.value;
  }
  if (Array.isArray(raw) && raw.length > 0) {
    return raw.map(String).join(", ");
  }
  return null;
}

function ToolInputApprovalCard({
  block,
  questions,
  isPending,
}: {
  block: ApprovalBlock;
  questions: { id: string; question: string }[];
  isPending: boolean;
}) {
  const { t } = useTranslation("chat");
  if (questions.length <= 0) return null;

  const rawAnswers = block.responseData?.answers;
  const answers = typeof rawAnswers === "object" && rawAnswers !== null && !Array.isArray(rawAnswers)
    ? rawAnswers as Record<string, unknown>
    : undefined;
  const isAnswered = !isPending && block.decision;
  const hasAnswers = isAnswered && answers;
  const [expanded, setExpanded] = useState(false);
  const toggleExpanded = useCallback(() => setExpanded((v) => !v), []);

  return (
    <div>
      <MessageBlockHeader
        icon={<MessageSquare size={11} />}
        tileTone={isPending ? "info" : "neutral"}
        expanded={expanded}
        onToggle={hasAnswers ? toggleExpanded : undefined}
        label={
          isPending
            ? t("messageBlocks.approval.pendingQuestions", { count: questions.length })
            : t("messageBlocks.approval.answeredQuestions", { count: questions.length })
        }
      />
      {hasAnswers && expanded && (
        <div className="tool-input-qa-body">
          {questions.map((q) => {
            const text = extractAnswerText(answers[q.id]);
            if (!text) return null;
            return (
              <div key={q.id} className="tool-input-qa-row">
                {q.question} → <strong>{text}</strong>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}

export function shouldShowClaudeUnsupportedApproval(
  details: Record<string, unknown>,
  isPending: boolean,
  isClaudeThread: boolean,
): boolean {
  if (!isPending || !isClaudeThread) {
    return false;
  }

  const isToolInputRequest = isRequestUserInputApproval(details);
  const proposedExecpolicyAmendment = parseProposedExecpolicyAmendment(details);
  const proposedNetworkPolicyAmendments = parseProposedNetworkPolicyAmendments(details);

  return (
    (isToolInputRequest && !isSupportedClaudeToolInputApproval(details)) ||
    (!isToolInputRequest &&
      (isDynamicToolCallApproval(details) ||
        isMcpElicitationApproval(details) ||
        requiresCustomApprovalPayload(details))) ||
    proposedExecpolicyAmendment.length > 0 ||
    proposedNetworkPolicyAmendments.length > 0
  );
}

function ApprovalCard({
  block,
  engineId,
  onApproval,
}: {
  block: ApprovalBlock;
  engineId?: string;
  onApproval: (approvalId: string, response: ApprovalResponse) => void;
}) {
  const { t } = useTranslation("chat");
  const isPending = block.status === "pending";
  const isClaudeThread = engineId === "claude";
  const details = block.details ?? {};
  const isToolInputRequest = isRequestUserInputApproval(details);
  const isDynamicToolCall = isDynamicToolCallApproval(details);
  const isPermissionsRequest = isPermissionsRequestApproval(details);
  const isMcpElicitation = isMcpElicitationApproval(details);
  const requiresCustomPayload = requiresCustomApprovalPayload(details);
  const toolInputQuestions = isToolInputRequest ? parseToolInputQuestions(details) : [];
  const requiresAdvancedJsonFallback =
    requiresCustomPayload || (isToolInputRequest && toolInputQuestions.length === 0);
  const proposedExecpolicyAmendment = parseProposedExecpolicyAmendment(details);
  const proposedNetworkPolicyAmendments = parseProposedNetworkPolicyAmendments(details);
  const requestedPermissions = isPermissionsRequest ? parseRequestedPermissions(details) : null;
  const showClaudeUnsupportedApproval = shouldShowClaudeUnsupportedApproval(
    details,
    isPending,
    isClaudeThread,
  );
  const dynamicToolName = parseDynamicToolCallName(details);
  const dynamicToolArguments = parseDynamicToolCallArguments(details);
  const mcpServerName = parseMcpElicitationServerName(details);
  const mcpMessage = parseMcpElicitationMessage(details);
  const mcpMode = parseMcpElicitationMode(details);
  const mcpUrl = parseMcpElicitationUrl(details);
  const mcpSchema = parseMcpElicitationSchema(details);

  const { command, reason, commandActionCount, remainingDetails, hasRemainingDetails } =
    extractApprovalDetails(details);
  const displayReason = isMcpElicitation ? mcpMessage ?? reason : reason;

  const defaultAdvancedPayload = useMemo(
    () => JSON.stringify(defaultAdvancedApprovalPayload(details), null, 2),
    [details],
  );
  const [advancedJsonPayload, setAdvancedJsonPayload] = useState(defaultAdvancedPayload);
  const [advancedJsonError, setAdvancedJsonError] = useState<string | null>(null);
  const [showRemainingDetails, setShowRemainingDetails] = useState(false);
  const [showAdvancedMcpElicitation, setShowAdvancedMcpElicitation] = useState(false);
  const [dynamicToolSuccess, setDynamicToolSuccess] = useState(true);
  const [dynamicToolText, setDynamicToolText] = useState("");
  const [dynamicToolImageUrl, setDynamicToolImageUrl] = useState("");

  useEffect(() => {
    setAdvancedJsonPayload(defaultAdvancedPayload);
  }, [defaultAdvancedPayload, block.approvalId]);

  useEffect(() => {
    setDynamicToolSuccess(true);
    setDynamicToolText("");
    setDynamicToolImageUrl("");
  }, [block.approvalId]);

  useEffect(() => {
    setShowAdvancedMcpElicitation(false);
  }, [block.approvalId]);

  let decisionLabel = t("messageBlocks.approval.decision.answered");
  let decisionStatusClass = "msg-block-status";
  let DecisionIcon = CheckCircle2;
  if (block.decision === "decline") {
    decisionLabel = t("messageBlocks.approval.decision.denied");
    decisionStatusClass = "msg-block-status msg-block-status--danger";
    DecisionIcon = XCircle;
  } else if (block.decision === "cancel") {
    decisionLabel = t("messageBlocks.approval.decision.canceled");
    DecisionIcon = XCircle;
  } else if (block.decision === "accept" || block.decision === "accept_for_session") {
    decisionLabel = t("messageBlocks.approval.decision.approved");
    decisionStatusClass = "msg-block-status msg-block-status--success";
  }

  if (isToolInputRequest && toolInputQuestions.length > 0 && !showClaudeUnsupportedApproval) {
    return (
      <div>
        <ToolInputApprovalCard
          block={block}
          questions={toolInputQuestions}
          isPending={isPending}
        />
      </div>
    );
  }

  function submitAdvancedJsonPayload() {
    let parsedPayload: unknown;
    try {
      parsedPayload = JSON.parse(advancedJsonPayload);
    } catch (error) {
      setAdvancedJsonError(
        t("messageBlocks.approval.invalidJson", { error: String(error) }),
      );
      return;
    }

    if (
      typeof parsedPayload !== "object" ||
      parsedPayload === null ||
      Array.isArray(parsedPayload)
    ) {
      setAdvancedJsonError(t("messageBlocks.approval.payloadMustBeObject"));
      return;
    }

    setAdvancedJsonError(null);
    onApproval(block.approvalId, parsedPayload as ApprovalResponse);
  }

  function submitDynamicToolResponse() {
    onApproval(
      block.approvalId,
      buildDynamicToolCallResponse(dynamicToolText, dynamicToolSuccess, dynamicToolImageUrl),
    );
  }

  // MCP 授权决策已统一由输入框上方的固定授权栏处理，避免同一 approvalId
  // 在消息卡片和固定授权栏中各出现一组“拒绝/批准”按钮。
  /*
  function respondMcpElicitation(
    action: "accept" | "decline",
  ) {
    onApproval(
      block.approvalId,
      buildMcpElicitationApprovalResponse(details, action),
    );
  }
  */

  return (
    <div className="msg-approval-block">
      <MessageBlockHeader
        icon={<Shield size={11} />}
        label={block.summary}
        tileTone="amber"
        meta={
          <>
            <span>{block.actionType}</span>
            {isPending ? (
              <span className="msg-block-status msg-block-status--warning">
                <Circle size={11} />
                {t("messageBlocks.actionStatus.pending")}
              </span>
            ) : block.decision ? (
              <span className={decisionStatusClass}>
                <DecisionIcon size={11} />
                {decisionLabel}
              </span>
            ) : null}
          </>
        }
      />

      {/* Details — collapsed for resolved approvals */}
      {!isToolInputRequest && (command || displayReason || commandActionCount > 0 || requestedPermissions || mcpUrl || mcpSchema || hasRemainingDetails) && (isPending || !block.decision) && (
        <div className="acard-details">
          {command && (
            <pre className="acard-command">{command}</pre>
          )}
          {!command && displayReason && (
            <p className="acard-reason">{displayReason}</p>
          )}
          {isMcpElicitation && mcpServerName && (
            <p className="acard-meta">{mcpServerName}</p>
          )}
          {isMcpElicitation && mcpMode === "url" && mcpUrl && (
            <pre className="acard-command">{mcpUrl}</pre>
          )}
          {isPermissionsRequest && requestedPermissions && (
            <pre className="acard-remaining-pre">
              {JSON.stringify(requestedPermissions, null, 2)}
            </pre>
          )}
          {isMcpElicitation && mcpMode === "form" && mcpSchema && (
            <pre className="acard-remaining-pre">
              {JSON.stringify(mcpSchema, null, 2)}
            </pre>
          )}
          {commandActionCount > 0 && (
            <p className="acard-meta">
              {t("messageBlocks.approval.actionCount", { count: commandActionCount })}
            </p>
          )}
          {proposedExecpolicyAmendment.length > 0 && (
            <p className="acard-meta">
              {t("messageBlocks.approval.execPolicyAmendment", {
                value: proposedExecpolicyAmendment.join(" "),
              })}
            </p>
          )}
          {proposedNetworkPolicyAmendments.length > 0 && (
            <p className="acard-meta">
              {t("messageBlocks.approval.networkAmendment", {
                value: proposedNetworkPolicyAmendments
                  .map((amendment) => `${amendment.action} ${amendment.host}`)
                  .join(", "),
              })}
            </p>
          )}
          {isDynamicToolCall && dynamicToolName && (
            <p className="acard-meta">
              {t("messageBlocks.approval.dynamicTool", { name: dynamicToolName })}
            </p>
          )}
          {hasRemainingDetails && (
            <div className="acard-remaining">
              <button
                type="button"
                className="acard-toggle"
                onClick={() => setShowRemainingDetails((v) => !v)}
              >
                {showRemainingDetails
                  ? t("messageBlocks.approval.hideDetails")
                  : t("messageBlocks.approval.showDetails")}
              </button>
              {showRemainingDetails && (
                <pre className="acard-remaining-pre">
                  {JSON.stringify(remainingDetails, null, 2)}
                </pre>
              )}
            </div>
          )}
        </div>
      )}
      {showClaudeUnsupportedApproval && (
        <div className="acard-section">
          <p className="acard-reason">
            {t("messageBlocks.approval.claudeUnsupported")}
          </p>
          <div className="acard-advanced-footer">
            <button
              type="button"
              className="approval-btn approval-btn-deny"
              onClick={() => onApproval(block.approvalId, { decision: "decline" })}
            >
              {t("panel.approvalActions.deny")}
            </button>
          </div>
        </div>
      )}

      {isPending && !isClaudeThread && isDynamicToolCall && (
        <div className="acard-section">
          <div className="acard-advanced" style={{ gap: 10 }}>
            <p className="acard-reason">
              {t("messageBlocks.approval.dynamicToolPrompt")}
            </p>
            {dynamicToolArguments && (
              <pre className="acard-remaining-pre">
                {JSON.stringify(dynamicToolArguments, null, 2)}
              </pre>
            )}
            <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
              <button
                type="button"
                className={`approval-btn ${dynamicToolSuccess ? "approval-btn-allow" : "approval-btn-deny"}`}
                onClick={() => setDynamicToolSuccess((current) => !current)}
              >
                {dynamicToolSuccess
                  ? t("messageBlocks.approval.dynamicToolSuccess")
                  : t("messageBlocks.approval.dynamicToolFailure")}
              </button>
            </div>
            <textarea
              className="acard-textarea"
              value={dynamicToolText}
              onChange={(event) => setDynamicToolText(event.target.value)}
              rows={4}
              placeholder={t("messageBlocks.approval.toolResponsePlaceholder")}
            />
            <input
              className="acard-textarea"
              value={dynamicToolImageUrl}
              onChange={(event) => setDynamicToolImageUrl(event.target.value)}
              placeholder={t("messageBlocks.approval.imageUrlPlaceholder")}
            />
            <div className="acard-advanced-footer">
              <button
                type="button"
                className="approval-btn approval-btn-allow"
                onClick={submitDynamicToolResponse}
              >
                {t("messageBlocks.approval.sendToolResponse")}
              </button>
            </div>
          </div>
        </div>
      )}

      {isPending && !isClaudeThread && isMcpElicitation && (
        <div className="acard-section">
          <p className="acard-reason">
            {t("messageBlocks.approval.mcpElicitationPrompt")}
          </p>
          {/* MCP 授权按钮统一显示在底部固定授权栏；卡片继续承载请求详情和高级 JSON。
          <div className="acard-advanced-footer">
            <button
              type="button"
              className="approval-btn approval-btn-deny"
              onClick={() => respondMcpElicitation("decline")}
            >
              {t("panel.approvalActions.deny")}
            </button>
            <button
              type="button"
              className="approval-btn approval-btn-allow"
              onClick={() => respondMcpElicitation("accept")}
            >
              {t("panel.approvalActions.approve")}
            </button>
          </div>
          */}
          <button
            type="button"
            className="acard-toggle"
            onClick={() => setShowAdvancedMcpElicitation((value) => !value)}
          >
            {showAdvancedMcpElicitation
              ? t("messageBlocks.approval.hideAdvancedJson")
              : t("messageBlocks.approval.showAdvancedJson")}
          </button>
        </div>
      )}

      {isPending && !isClaudeThread && requiresAdvancedJsonFallback && !isMcpElicitation && (
        <div className="acard-section">
          <p className="acard-reason">
            {t("messageBlocks.approval.customPayloadHint")}
          </p>
        </div>
      )}

      {/* Standard approval — no inline buttons; the approval banner handles it */}

      {/* Advanced JSON — for custom payload requests and malformed tool-input fallbacks */}
      {isPending &&
        !isClaudeThread &&
        requiresAdvancedJsonFallback &&
        (!isMcpElicitation || showAdvancedMcpElicitation) && (
        <div className="acard-section">
          <div className="acard-advanced">
            <textarea
              className="acard-textarea"
              value={advancedJsonPayload}
              onChange={(event) => {
                setAdvancedJsonPayload(event.target.value);
                if (advancedJsonError) {
                  setAdvancedJsonError(null);
                }
              }}
              rows={6}
            />
            {advancedJsonError && (
              <p className="acard-error">{advancedJsonError}</p>
            )}
            <div className="acard-advanced-footer">
              <button
                type="button"
                className="approval-btn approval-btn-allow"
                onClick={submitAdvancedJsonPayload}
              >
                {t("messageBlocks.approval.sendPayload")}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

/* ── Main Component ── */

function renderSingleBlock(
  block: ContentBlock,
  index: number,
  safeBlocks: ContentBlock[],
  status: MessageStatus | undefined,
  engineId: string | undefined,
  onApproval: (approvalId: string, response: ApprovalResponse) => void,
  onLoadActionOutput: ((actionId: string) => Promise<void>) | undefined,
  backgroundTaskActions: ActionBlock[],
  onOpenDiffFile: ((filePath: string) => void) | undefined,
  onOpenImageAttachment: ((attachment: AttachmentBlock) => void) | undefined,
) {
  const blockKey = getMessageBlockKey(block, index, safeBlocks);

  /* ── Text ── */
  if (block.type === "text") {
    const textContent = String(block.content ?? "");
    const isLastBlock = index === safeBlocks.length - 1;
    const isStreamingText = status === "streaming" && isLastBlock;

    if (isStreamingText) {
      return (
        <MarkdownContent
          key={blockKey}
          content={textContent}
          streaming
          enableFileContextMenu
          className="prose"
          style={{ fontSize: 13, padding: "6px 14px" }}
        />
      );
    }

    return (
      <MarkdownContent
        key={blockKey}
        content={textContent}
        enableFileContextMenu
        className="prose"
        style={{ fontSize: 13, padding: "6px 14px" }}
      />
    );
  }

  /* ── Code ── */
  if (block.type === "code") {
    const lang = String(block.language ?? "text");
    return (
      <div
        key={blockKey}
        style={{
          borderRadius: "var(--radius-sm)",
          border: "1px solid var(--border)",
          overflow: "hidden",
          background: "var(--code-bg)",
        }}
      >
        <div
          style={{
            display: "flex",
            alignItems: "center",
            gap: 6,
            padding: "6px 12px",
            borderBottom: "1px solid var(--border)",
            fontSize: 11,
            color: "var(--text-3)",
            fontFamily: '"Geist Mono", ui-monospace, monospace',
          }}
        >
          <FileCode2 size={12} style={{ opacity: 0.5 }} />
          <span style={{ flex: 1 }}>
            <LinkifiedPlainText text={block.filename || lang} />
          </span>
          <CodeBlockCopyButton content={String(block.content ?? "")} />
        </div>
        <pre
          style={{
            margin: 0,
            padding: "12px 14px",
            fontSize: 12.5,
            lineHeight: 1.6,
            fontFamily: '"Geist Mono", ui-monospace, monospace',
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
            overflow: "auto",
            maxHeight: 400,
          }}
        >
          <code className={`language-${lang}`}>{String(block.content ?? "")}</code>
        </pre>
      </div>
    );
  }

  /* ── Diff ── */
  if (block.type === "diff") {
    return (
      <div key={blockKey} className="msg-action-card">
        <MessageDiffBlock block={block} onOpenDiffFile={onOpenDiffFile} />
      </div>
    );
  }

  /* ── Notice ── */
  if (block.type === "notice") {
    return (
      <NoticeBlockView
        key={blockKey}
        block={block}
        backgroundTaskActions={backgroundTaskActions}
        onLoadActionOutput={onLoadActionOutput}
      />
    );
  }

  /* ── Steer ── */
  if (block.type === "steer") {
    return (
      <SteerBlockView
        key={blockKey}
        block={block}
        onOpenImageAttachment={onOpenImageAttachment}
      />
    );
  }

  /* ── Action ── */
  if (block.type === "action") {
    return (
      <div key={blockKey} >
        <ActionBlockView
          block={block}
          onLoadDeferredOutput={
            onLoadActionOutput ? () => onLoadActionOutput(block.actionId) : undefined
          }
        />
      </div>
    );
  }

  /* ── Approval ── */
  if (block.type === "approval") {
    return (
      <ApprovalCard
        key={blockKey}
        block={block}
        engineId={engineId}
        onApproval={onApproval}
      />
    );
  }

  /* ── Thinking ── */
  if (block.type === "thinking") {
    const isLastBlock = index === safeBlocks.length - 1;
    const thinkingActive = status === "streaming" && isLastBlock;
    return (
      <div key={blockKey} >
        <ThinkingBlockView block={block} isStreaming={thinkingActive} />
      </div>
    );
  }

  /* ── Attachment ── */
  if (block.type === "attachment") {
    const attachmentBlock = block as AttachmentBlock;
    return (
      <div key={blockKey} style={{ margin: "2px 12px", display: "inline-flex" }}>
        <AttachmentChip
          attachment={attachmentBlock}
          onOpen={onOpenImageAttachment
            ? () => onOpenImageAttachment(attachmentBlock)
            : undefined}
        />
      </div>
    );
  }

  /* ── Error ── */
  if (block.type === "error") {
    return (
      <div key={blockKey} className="msg-error-block">
        <AlertTriangle size={14} style={{ flexShrink: 0, marginTop: 2 }} />
        {block.message}
      </div>
    );
  }

  return null;
}

function MessageBlocksView({
  messageId,
  blocks = [],
  status,
  engineId,
  onApproval,
  onLoadActionOutput,
  onOpenDiffFile,
  onOpenImageAttachment,
}: Props) {
  const { t } = useTranslation("chat");
  const [expandedActionGroups, setExpandedActionGroups] = useState<Record<string, boolean>>({});
  const [expandedHookGroups, setExpandedHookGroups] = useState<Record<string, boolean>>({});

  const toggleActionGroup = useCallback((groupId: string) => {
    setExpandedActionGroups((current) => ({
      ...current,
      [groupId]: !(current[groupId] ?? false),
    }));
  }, []);

  const safeBlocks = useMemo(
    () => dedupeDiffBlocksByScope(
      (Array.isArray(blocks) ? blocks : []).filter(isBlockLike) as ContentBlock[],
    ),
    [blocks],
  );

  const backgroundTaskActions = useMemo(
    () =>
      safeBlocks.filter(
        (block): block is ActionBlock =>
          block.type === "action" && typeof block.backgroundTaskId === "string",
      ),
    [safeBlocks],
  );
  const isStreaming = status === "streaming";
  const blockSegments = useMemo(
    () => buildBlockSegments(safeBlocks, isStreaming, engineId),
    [safeBlocks, isStreaming, engineId],
  );

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
      {blockSegments.map((segment, segIdx) => {
        if (segment.kind === "subagent-card") {
          return (
            <SubagentCardView
              key={`subagent-card-${messageId}-${segment.threadId}`}
              threadId={segment.threadId}
              blocks={segment.blocks}
              indices={segment.indices}
              onLoadActionOutput={onLoadActionOutput}
            />
          );
        }
        if (segment.kind === "hook-group") {
          const groupKey = getMessageBlockKey(
            segment.blocks[0],
            segment.indices[0],
            safeBlocks,
          );
          const groupId = `message-hooks-${messageId}-${encodeURIComponent(groupKey)}`;
          const hooksExpanded = expandedHookGroups[groupId] ?? false;
          return (
            <div key={groupId} className="msg-hook-notices">
              <button
                type="button"
                className="msg-hooks-toggle"
                aria-expanded={hooksExpanded}
                aria-controls={groupId}
                onClick={() =>
                  setExpandedHookGroups((current) => ({
                    ...current,
                    [groupId]: !(current[groupId] ?? false),
                  }))
                }
              >
                <span>{t("messageBlocks.hooks", { count: segment.blocks.length })}</span>
                {hooksExpanded ? <ChevronUp size={13} /> : <ChevronDown size={13} />}
              </button>
              {hooksExpanded && (
                <div id={groupId} className="msg-hooks-content">
                  {segment.blocks.map((block, index) => (
                    <NoticeBlockView
                      key={getMessageBlockKey(block, segment.indices[index], safeBlocks)}
                      block={block}
                    />
                  ))}
                </div>
              )}
            </div>
          );
        }

        if (segment.kind === "action-card") {
          const groupAnchorId = getActionCardAnchorId(segment.segments[0], safeBlocks);
          return (
            <div key={`action-card-${segIdx}`} className="msg-action-card">
              {segment.segments.map((inner, innerIdx) => {
                if (inner.kind === "action-group") {
                  const groupId = getActionGroupId(messageId, groupAnchorId);
                  return (
                    <ActionGroupView
                      key={groupId}
                      blocks={inner.blocks}
                      expanded={expandedActionGroups[groupId] ?? false}
                      onToggle={() => toggleActionGroup(groupId)}
                      onLoadActionOutput={onLoadActionOutput}
                    />
                  );
                }
                if (inner.block.type === "thinking") {
                  const thinkingBlock = inner.block as ThinkingBlock;
                  const isLastBlock = inner.index === safeBlocks.length - 1;
                  const thinkingActive = status === "streaming" && isLastBlock;
                  return (
                    <ThinkingBlockView
                      key={getMessageBlockKey(inner.block, inner.index, safeBlocks)}
                      block={thinkingBlock}
                      isStreaming={thinkingActive}
                    />
                  );
                }
                if (inner.block.type === "approval") {
                  return (
                    <ApprovalCard
                      key={(inner.block as ApprovalBlock).approvalId}
                      block={inner.block as ApprovalBlock}
                      engineId={engineId}
                      onApproval={onApproval}
                    />
                  );
                }
                if (inner.block.type === "diff") {
                  return (
                    <MessageDiffBlock
                      key={getMessageBlockKey(inner.block, inner.index, safeBlocks)}
                      block={inner.block as DiffBlock}
                      onOpenDiffFile={onOpenDiffFile}
                    />
                  );
                }
                return (
                  <ActionBlockView
                    key={inner.block.type === "action" ? (inner.block as ActionBlock).actionId : `inner-${innerIdx}`}
                    block={inner.block as ActionBlock}
                    onLoadDeferredOutput={
                      onLoadActionOutput ? () => onLoadActionOutput((inner.block as ActionBlock).actionId) : undefined
                    }
                  />
                );
              })}
            </div>
          );
        }

        if (segment.kind === "action-group") {
          const first = segment.blocks[0];
          const groupId = getActionGroupId(messageId, first.actionId);
          return (
            <ActionGroupView
              key={groupId}
              blocks={segment.blocks}
              expanded={expandedActionGroups[groupId] ?? false}
              onToggle={() => toggleActionGroup(groupId)}
              onLoadActionOutput={onLoadActionOutput}
            />
          );
        }

        return renderSingleBlock(
          segment.block,
          segment.index,
          safeBlocks,
          status,
          engineId,
          onApproval,
          onLoadActionOutput,
          backgroundTaskActions,
          onOpenDiffFile,
          onOpenImageAttachment,
        );
      })}
    </div>
  );
}

export const MessageBlocks = memo(
  MessageBlocksView,
  (prev, next) =>
    prev.messageId === next.messageId &&
    prev.blocks === next.blocks &&
    prev.status === next.status &&
    prev.engineId === next.engineId &&
    prev.onApproval === next.onApproval &&
    prev.onLoadActionOutput === next.onLoadActionOutput &&
    prev.onOpenDiffFile === next.onOpenDiffFile &&
    prev.onOpenImageAttachment === next.onOpenImageAttachment,
);
