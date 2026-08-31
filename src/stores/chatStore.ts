import { create } from "zustand";
import { ipc, listenThreadEvents } from "../lib/ipc";
import { recordPerfMetric } from "../lib/perfTelemetry";
import { useThreadStore } from "./threadStore";
import type {
  ApprovalResponse,
  ActionBlock,
  ApprovalBlock,
  AttachmentBlock,
  ChatAttachment,
  ChatInputItem,
  ChatProviderUsage,
  ContentBlock,
  ContextUsage,
  MentionBlock,
  Message,
  MessageWindowCursor,
  NoticeBlock,
  PermissionComponentJson,
  SkillBlock,
  SteerBlock,
  SteerDeliveryStatus,
  StreamEvent,
  ThreadStatus
} from "../types";

interface ChatState {
  threadId: string | null;
  /** 记录各线程是否已经完成 CLI 会话准备，仅存在于当前前端内存。 */
  sessionReadyByThread: Record<string, boolean>;
  messages: Message[];
  olderCursor: MessageWindowCursor | null;
  hasOlderMessages: boolean;
  loadingOlderMessages: boolean;
  olderLoadBlockedUntil: number;
  status: ThreadStatus;
  streaming: boolean;
  preparingEngineId: string | null;
  preparingAttachments: boolean;
  turnStartedAt: number | null;
  usageLimits: ContextUsage | null;
  usageLimitsLoading: boolean;
  error?: string;
  unlisten?: () => void;
  setActiveThread: (threadId: string | null) => Promise<void>;
  loadOlderMessages: () => Promise<void>;
  send: (
    message: string,
    options?: {
      threadIdOverride?: string;
      modelId?: string | null;
      engineId?: string | null;
      reasoningEffort?: string | null;
      attachments?: ChatAttachment[];
      inputItems?: ChatInputItem[];
      planMode?: boolean;
      remoteAttachmentUpload?: boolean;
      /** 发送方法（messageSendMode），发送时持久化到会话。 */
      sendMethod?: string | null;
      /** 权限状态的整包 JSON，前端给什么写什么、读时整个取回。 */
      permissionModeJson?: string | null;
      /** 发送前保存的当前 CLI 统一权限组件数据。 */
      permissionValues?: PermissionComponentJson;
      /** 发送时临时引用的 AuraCoder 会话 ID，不进入可见消息。 */
      referencedAuraCoderThreadId?: string | null;
    },
  ) => Promise<boolean>;
  steer: (
    message: string,
    options?: {
      threadIdOverride?: string;
      attachments?: ChatAttachment[];
      inputItems?: ChatInputItem[];
      planMode?: boolean;
      /** steer 时临时引用的 AuraCoder 会话 ID，不进入可见消息。 */
      referencedAuraCoderThreadId?: string | null;
    },
  ) => Promise<boolean>;
  cancel: () => Promise<void>;
  respondApproval: (
    approvalId: string,
    response: ApprovalResponse,
    threadIdOverride?: string,
  ) => Promise<boolean>;
  hydrateActionOutput: (messageId: string, actionId: string) => Promise<void>;
}

let activeThreadBindSeq = 0;
const STREAM_EVENT_BATCH_WINDOW_MS = 16;
const STREAM_EVENT_QUEUE_FLUSH_THRESHOLD = 500;

/**
 * Background listeners for threads that are still streaming when the user switches away.
 * Keeps the event stream alive so events are not lost. On switch-back, the DB state
 * will include all events that arrived while the thread was in the background.
 * Cleaned up on TurnCompleted or when the thread is explicitly cancelled.
 */
const backgroundStreamListeners = new Map<string, () => void>();

function cleanupBackgroundListener(threadId: string) {
  const cleanup = backgroundStreamListeners.get(threadId);
  if (cleanup) {
    cleanup();
    backgroundStreamListeners.delete(threadId);
  }
}
const MESSAGE_WINDOW_INITIAL_LIMIT = 80;
const OLDER_MESSAGES_RETRY_BACKOFF_MS = 2_000;
const MAX_FULLY_HYDRATED_MESSAGES = 80;
const ACTION_OUTPUT_MAX_CHARS = 80_000;
const ACTION_OUTPUT_TRIM_TARGET_CHARS = 48_000;
const ACTION_OUTPUT_MAX_CHUNKS = 160;

interface PendingTurnMeta {
  turnEngineId?: string | null;
  turnModelId?: string | null;
  turnReasoningEffort?: string | null;
  clientTurnId?: string | null;
  assistantMessageId?: string | null;
  startedAt: number;
  firstShellRecorded: boolean;
  firstContentRecorded: boolean;
  firstTextRecorded: boolean;
}

interface AssistantMessageTarget {
  clientTurnId?: string | null;
  assistantMessageId?: string | null;
}

const pendingTurnMetaByThread = new Map<string, PendingTurnMeta>();
const inflightActionOutputHydration = new Map<string, Promise<void>>();

/** 判断线程是否绑定了支持原生历史同步的 CLI 会话。 */
function isCliThreadAttached(thread: {
  engineId: string;
  engineThreadId: string | null;
} | undefined): boolean {
  return (
    (thread?.engineId === "codex" ||
      thread?.engineId === "opencode" ||
      thread?.engineId === "claude") &&
    Boolean(thread?.engineThreadId?.trim())
  );
}

function isThreadTurnActive(status: ThreadStatus): boolean {
  return status === "streaming" || status === "awaiting_approval";
}

function applyRuntimeStateFromEvent(
  status: ThreadStatus,
  streaming: boolean,
  event: StreamEvent,
): Pick<ChatState, "status" | "streaming"> {
  if (event.type === "UsageLimitsUpdated") {
    return { status, streaming };
  }

  if (event.type === "ApprovalRequested") {
    return { status: "awaiting_approval", streaming: true };
  }

  if (event.type === "ApprovalResolved") {
    return { status: "streaming", streaming: true };
  }

  if (event.type === "Error" && !event.recoverable) {
    return { status: "error", streaming: false };
  }

  if (event.type === "TurnCompleted") {
    const completionStatus = String(event.status ?? "completed");
    if (completionStatus === "failed") {
      return { status: "error", streaming: false };
    }
    if (completionStatus === "interrupted") {
      return { status: "idle", streaming: false };
    }
    return { status: "completed", streaming: false };
  }

  if (event.type === "TurnStarted") {
    return { status: "streaming", streaming: true };
  }

  // 终止事件后的通知仍需写入消息，但不能重新激活已经结束的回合。
  if (status === "error" || status === "completed") {
    return { status, streaming: false };
  }

  if (eventHasVisibleAssistantContent(event)) {
    return { status: "streaming", streaming: true };
  }

  return { status, streaming };
}

function recordPendingTurnMetric(
  threadId: string,
  flag: keyof Pick<
    PendingTurnMeta,
    "firstShellRecorded" | "firstContentRecorded" | "firstTextRecorded"
  >,
  metricName:
    | "chat.turn.first_shell.ms"
    | "chat.turn.first_content.ms"
    | "chat.turn.first_text.ms",
) {
  const pendingTurnMeta = pendingTurnMetaByThread.get(threadId);
  if (!pendingTurnMeta || pendingTurnMeta[flag]) {
    return;
  }

  pendingTurnMeta[flag] = true;
  recordPerfMetric(metricName, performance.now() - pendingTurnMeta.startedAt, {
    threadId,
    clientTurnId: pendingTurnMeta.clientTurnId ?? undefined,
    engineId: pendingTurnMeta.turnEngineId ?? undefined,
    modelId: pendingTurnMeta.turnModelId ?? undefined,
  });
}

function schedulePendingTurnShellMetric(threadId: string, clientTurnId: string) {
  const schedule = (() => {
    if (typeof globalThis.requestAnimationFrame === "function") {
      return globalThis.requestAnimationFrame.bind(globalThis);
    }
    return (callback: FrameRequestCallback) =>
      globalThis.setTimeout(() => callback(performance.now()), 0);
  })();

  schedule(() => {
    const pendingTurnMeta = pendingTurnMetaByThread.get(threadId);
    if (!pendingTurnMeta || pendingTurnMeta.clientTurnId !== clientTurnId) {
      return;
    }
    recordPendingTurnMetric(threadId, "firstShellRecorded", "chat.turn.first_shell.ms");
  });
}

function eventHasVisibleAssistantContent(event: StreamEvent): boolean {
  switch (event.type) {
    case "TextDelta":
      return String(event.content ?? "").length > 0;
    case "ThinkingDelta":
      return String(event.content ?? "").length > 0;
    case "ActionStarted":
    case "ActionBackgroundTaskAssigned":
    case "ActionOutputDelta":
    case "ActionCompleted":
    case "ApprovalRequested":
    case "DiffUpdated":
    case "ActionProgressUpdated":
    case "ModelRerouted":
    case "Notice":
    case "Error":
      return true;
    default:
      return false;
  }
}

function recordPendingTurnLatencyMetrics(threadId: string, event: StreamEvent) {
  if (eventHasVisibleAssistantContent(event)) {
    recordPendingTurnMetric(threadId, "firstContentRecorded", "chat.turn.first_content.ms");
  }

  if (event.type === "TextDelta" && String(event.content ?? "").length > 0) {
    recordPendingTurnMetric(threadId, "firstTextRecorded", "chat.turn.first_text.ms");
  }
}

function enqueueStreamEvent(queue: StreamEvent[], event: StreamEvent) {
  const previous = queue[queue.length - 1];
  if (!previous) {
    queue.push(event);
    return;
  }

  if (previous.type === "TextDelta" && event.type === "TextDelta") {
    queue[queue.length - 1] = {
      ...previous,
      content: `${previous.content}${event.content}`,
    };
    return;
  }

  if (previous.type === "ThinkingDelta" && event.type === "ThinkingDelta") {
    queue[queue.length - 1] = {
      ...previous,
      content: `${previous.content}${event.content}`,
    };
    return;
  }

  if (
    previous.type === "ActionOutputDelta" &&
    event.type === "ActionOutputDelta" &&
    previous.action_id === event.action_id &&
    previous.stream === event.stream
  ) {
    queue[queue.length - 1] = {
      ...previous,
      content: `${previous.content}${event.content}`,
    };
    return;
  }

  if (
    previous.type === "ActionProgressUpdated" &&
    event.type === "ActionProgressUpdated" &&
    previous.action_id === event.action_id
  ) {
    queue[queue.length - 1] = event;
    return;
  }

  if (
    previous.type === "DiffUpdated" &&
    event.type === "DiffUpdated" &&
    previous.scope === event.scope
  ) {
    queue[queue.length - 1] = event;
    return;
  }

  queue.push(event);
}

function resolveApprovalDecision(response: ApprovalResponse): ApprovalBlock["decision"] {
  if ("decision" in response && typeof response.decision === "string") {
    const decision = String(response.decision).trim();
    if (decision === "deny") {
      return "decline";
    }
    if (decision === "acceptForSession") {
      return "accept_for_session";
    }
    return decision as ApprovalBlock["decision"];
  }

  if ("action" in response && typeof response.action === "string") {
    const action = String(response.action).trim();
    if (action === "accept" || action === "decline" || action === "cancel") {
      return action;
    }
    return "custom";
  }

  if ("permissions" in response) {
    const scope = response.scope;
    const permissions =
      typeof response.permissions === "object" &&
      response.permissions !== null &&
      !Array.isArray(response.permissions)
        ? (response.permissions as Record<string, unknown>)
        : null;
    const hasGrantedPermission =
      permissions !== null &&
      Object.values(permissions).some((value) => {
        if (Array.isArray(value)) {
          return value.length > 0;
        }
        if (typeof value === "object" && value !== null) {
          return Object.values(value as Record<string, unknown>).some((nested) => {
            if (Array.isArray(nested)) {
              return nested.length > 0;
            }
            if (typeof nested === "object" && nested !== null) {
              return Object.keys(nested as Record<string, unknown>).length > 0;
            }
            return nested === true || (typeof nested === "string" && nested.toLowerCase() !== "none");
          });
        }
        return value === true || (typeof value === "string" && value.toLowerCase() !== "none");
      });

    if (!hasGrantedPermission) {
      return "decline";
    }
    if (scope === "session") {
      return "accept_for_session";
    }
    return "accept";
  }

  return "custom";
}

function normalizeActionOutputStream(
  stream: unknown,
): ActionBlock["outputChunks"][number]["stream"] {
  return stream === "stderr" || stream === "stdin" ? stream : "stdout";
}

function resolveApprovalInMessages(
  messages: Message[],
  approvalId: string,
  decision?: ApprovalBlock["decision"],
  responseData?: Record<string, unknown>,
): Message[] {
  for (let messageIndex = 0; messageIndex < messages.length; messageIndex += 1) {
    const message = messages[messageIndex];
    const blocks = message.blocks;
    if (!blocks || blocks.length === 0) {
      continue;
    }

    const approvalIndex = blocks.findIndex(
      (block) => block.type === "approval" && block.approvalId === approvalId,
    );
    if (approvalIndex < 0) {
      continue;
    }

    const approvalBlock = blocks[approvalIndex] as ApprovalBlock;
    if (
      approvalBlock.status === "answered" &&
      (decision === undefined || approvalBlock.decision === decision)
    ) {
      return messages;
    }

    const nextBlocks = [...blocks];
    nextBlocks[approvalIndex] = {
      ...approvalBlock,
      status: "answered" as const,
      ...(decision !== undefined ? { decision } : {}),
      ...(responseData !== undefined ? { responseData } : {}),
    };

    const nextMessages = [...messages];
    nextMessages[messageIndex] = {
      ...message,
      blocks: nextBlocks,
    };
    return nextMessages;
  }

  return messages;
}

function restoreApprovalInMessages(
  messages: Message[],
  approvalId: string,
  previousApproval: ApprovalBlock,
): Message[] {
  for (let messageIndex = 0; messageIndex < messages.length; messageIndex += 1) {
    const message = messages[messageIndex];
    const blocks = message.blocks;
    if (!blocks || blocks.length === 0) {
      continue;
    }

    const approvalIndex = blocks.findIndex(
      (block) => block.type === "approval" && block.approvalId === approvalId,
    );
    if (approvalIndex < 0) {
      continue;
    }

    if (blocks[approvalIndex] === previousApproval) {
      return messages;
    }

    const nextBlocks = [...blocks];
    nextBlocks[approvalIndex] = previousApproval;
    const nextMessages = [...messages];
    nextMessages[messageIndex] = {
      ...message,
      blocks: nextBlocks,
    };
    return nextMessages;
  }

  return messages;
}

function trimActionOutputChunks(
  chunks: ActionBlock["outputChunks"],
): {
  chunks: ActionBlock["outputChunks"];
  truncated: boolean;
} {
  if (chunks.length === 0) {
    return { chunks, truncated: false };
  }

  let nextChunks = chunks;
  let truncated = false;

  if (nextChunks.length > ACTION_OUTPUT_MAX_CHUNKS) {
    nextChunks = nextChunks.slice(nextChunks.length - ACTION_OUTPUT_MAX_CHUNKS);
    truncated = true;
  }

  let totalChars = 0;
  for (const chunk of nextChunks) {
    totalChars += chunk.content.length;
  }

  if (totalChars <= ACTION_OUTPUT_MAX_CHARS) {
    return { chunks: nextChunks, truncated };
  }

  truncated = true;
  let charsToTrim = totalChars - ACTION_OUTPUT_TRIM_TARGET_CHARS;
  const trimmedChunks = [...nextChunks];
  let startIndex = 0;

  while (charsToTrim > 0 && startIndex < trimmedChunks.length) {
    const currentChunk = trimmedChunks[startIndex];
    const currentLength = currentChunk.content.length;
    if (currentLength <= charsToTrim) {
      charsToTrim -= currentLength;
      startIndex += 1;
      continue;
    }
    trimmedChunks[startIndex] = {
      ...currentChunk,
      content: currentChunk.content.slice(charsToTrim),
    };
    charsToTrim = 0;
  }

  return {
    chunks: trimmedChunks.slice(startIndex),
    truncated,
  };
}

function patchActionBlock(
  blocks: ContentBlock[],
  actionId: string,
  updater: (block: ActionBlock) => ActionBlock,
): ContentBlock[] {
  const blockIndex = blocks.findIndex(
    (block) => block.type === "action" && block.actionId === actionId,
  );
  if (blockIndex < 0) {
    return blocks;
  }

  const current = blocks[blockIndex] as ActionBlock;
  const nextBlock = updater(current);
  if (nextBlock === current) {
    return blocks;
  }

  const nextBlocks = [...blocks];
  nextBlocks[blockIndex] = nextBlock;
  return nextBlocks;
}

function createStreamingAssistantMessage(
  threadId: string,
  options?: {
    id?: string;
    clientTurnId?: string | null;
  },
): Message {
  const pendingTurnMeta = pendingTurnMetaByThread.get(threadId);
  return {
    id: options?.id ?? crypto.randomUUID(),
    threadId,
    role: "assistant",
    clientTurnId: options?.clientTurnId ?? pendingTurnMeta?.clientTurnId ?? null,
    turnEngineId: pendingTurnMeta?.turnEngineId ?? null,
    turnModelId: pendingTurnMeta?.turnModelId ?? null,
    turnReasoningEffort: pendingTurnMeta?.turnReasoningEffort ?? null,
    status: "streaming",
    schemaVersion: 1,
    blocks: [],
    createdAt: new Date().toISOString(),
    hydration: "full",
    hasDeferredContent: false,
  };
}

function createOptimisticUserMessage(
  threadId: string,
  message: string,
  options?: {
    attachments?: ChatAttachment[];
    inputItems?: ChatInputItem[];
    planMode?: boolean;
  },
): Message {
  const attachments = options?.attachments ?? [];
  const inputItems = options?.inputItems ?? [];
  const planMode = options?.planMode ?? false;
  const userBlocks: ContentBlock[] = [];

  for (const inputItem of inputItems) {
    if (inputItem.type === "skill") {
      userBlocks.push({
        type: "skill",
        name: inputItem.name,
        path: inputItem.path,
      });
    } else if (inputItem.type === "mention") {
      userBlocks.push({
        type: "mention",
        name: inputItem.name,
        path: inputItem.path,
      });
    }
  }

  for (const attachment of attachments) {
    userBlocks.push({
      type: "attachment",
      fileName: attachment.fileName,
      filePath: attachment.filePath,
      sizeBytes: attachment.sizeBytes,
      mimeType: attachment.mimeType,
      browserAnnotation: attachment.browserAnnotation,
    });
  }

  userBlocks.push({ type: "text", content: message, planMode: planMode || undefined });

  return {
    id: crypto.randomUUID(),
    threadId,
    role: "user",
    content: message,
    blocks: userBlocks,
    status: "completed",
    schemaVersion: 1,
    createdAt: new Date().toISOString(),
    hydration: "full",
    hasDeferredContent: false,
  };
}

function createSteerBlock(
  message: string,
  options?: {
    attachments?: ChatAttachment[];
    inputItems?: ChatInputItem[];
    planMode?: boolean;
    steerId?: string;
    deliveryStatus?: SteerDeliveryStatus;
  },
): SteerBlock {
  const attachments = (options?.attachments ?? []).map<AttachmentBlock>((attachment) => ({
    type: "attachment",
    fileName: attachment.fileName,
    filePath: attachment.filePath,
    sizeBytes: attachment.sizeBytes,
    mimeType: attachment.mimeType,
    browserAnnotation: attachment.browserAnnotation,
  }));
  const skills: SkillBlock[] = [];
  const mentions: MentionBlock[] = [];

  for (const inputItem of options?.inputItems ?? []) {
    if (inputItem.type === "skill") {
      skills.push({
        type: "skill",
        name: inputItem.name,
        path: inputItem.path,
      });
    } else if (inputItem.type === "mention") {
      mentions.push({
        type: "mention",
        name: inputItem.name,
        path: inputItem.path,
      });
    }
  }

  return {
    type: "steer",
    steerId: options?.steerId ?? crypto.randomUUID(),
    content: message,
    deliveryStatus: options?.deliveryStatus ?? "sending",
    planMode: options?.planMode || undefined,
    attachments: attachments.length > 0 ? attachments : undefined,
    skills: skills.length > 0 ? skills : undefined,
    mentions: mentions.length > 0 ? mentions : undefined,
  };
}

function createSteerBlockFromMessage(
  message: Message,
  deliveryStatus: SteerDeliveryStatus,
): SteerBlock {
  const blocks = Array.isArray(message.blocks) ? message.blocks : [];
  const content =
    typeof message.content === "string" && message.content.length > 0
      ? message.content
      : blocks
          .filter((block): block is Extract<ContentBlock, { type: "text" }> => block.type === "text")
          .map((block) => block.content)
          .join("\n");
  const attachments = blocks.filter(
    (block): block is AttachmentBlock => block.type === "attachment",
  );
  const skills = blocks.filter((block): block is SkillBlock => block.type === "skill");
  const mentions = blocks.filter((block): block is MentionBlock => block.type === "mention");
  const planMode = blocks.some(
    (block) => block.type === "text" && Boolean(block.planMode),
  );
  const clientSteerId = blocks.find(
    (block) => block.type === "text" && block.isSteer === true && block.clientSteerId,
  );

  return {
    type: "steer",
    steerId:
      clientSteerId?.type === "text" && clientSteerId.clientSteerId
        ? clientSteerId.clientSteerId
        : message.id,
    content,
    deliveryStatus,
    planMode: planMode || undefined,
    attachments: attachments.length > 0 ? attachments : undefined,
    skills: skills.length > 0 ? skills : undefined,
    mentions: mentions.length > 0 ? mentions : undefined,
  };
}

function messageHasSteerMarker(message: Message): boolean {
  return (message.blocks ?? []).some(
    (block) => block.type === "text" && block.isSteer === true,
  );
}

function appendSteerBlockToAssistantMessage(message: Message, steerBlock: SteerBlock): Message {
  const existingBlocks = message.blocks ?? [];
  if (
    existingBlocks.some(
      (block) => block.type === "steer" && block.steerId === steerBlock.steerId,
    )
  ) {
    return message;
  }

  const blocks = [...existingBlocks, steerBlock];
  return {
    ...message,
    blocks,
    hydration: "full",
    hasDeferredContent: hasDeferredActionOutput(blocks),
  };
}

function resolveActiveAssistantTarget(threadId: string): AssistantMessageTarget {
  const pendingTurnMeta = pendingTurnMetaByThread.get(threadId);
  return {
    clientTurnId: pendingTurnMeta?.clientTurnId ?? null,
    assistantMessageId: pendingTurnMeta?.assistantMessageId ?? null,
  };
}

function appendSteerBlockToActiveAssistant(
  messages: Message[],
  threadId: string,
  steerBlock: SteerBlock,
): Message[] {
  const { messages: ensuredMessages, assistantIndex } = ensureAssistantMessage(
    messages,
    threadId,
    resolveActiveAssistantTarget(threadId),
  );
  const assistant = ensuredMessages[assistantIndex];
  const nextAssistant = appendSteerBlockToAssistantMessage(assistant, steerBlock);
  if (nextAssistant === assistant) {
    return ensuredMessages;
  }

  return [
    ...ensuredMessages.slice(0, assistantIndex),
    nextAssistant,
    ...ensuredMessages.slice(assistantIndex + 1),
  ];
}

/*
 * 旧逻辑在 steer RPC 失败时直接删除乐观插入的块。保留这段代码作为行为变更记录：
 * function removeSteerBlock(messages: Message[], steerId: string): Message[] {
 *   ...通过 filter 删除对应 steer block...
 * }
 *
 * 新逻辑保留用户输入，并把块标记为 failed，避免用户误以为追加消息已经丢失。
 */
function updateSteerBlockDelivery(
  messages: Message[],
  steerId: string | null,
  deliveryStatus: SteerDeliveryStatus,
  details?: {
    expectedTurnId?: string;
    acceptedAt?: string;
    failureReason?: string;
  },
): Message[] {
  let nextMessages = messages;
  for (let index = 0; index < messages.length; index += 1) {
    const message = messages[index];
    const blocks = message.blocks ?? [];
    let blocksChanged = false;
    const nextBlocks = blocks.map((block) => {
      if (
        block.type !== "steer" ||
        (steerId !== null && block.steerId !== steerId) ||
        (steerId === null &&
          (block.deliveryStatus === "failed" || block.deliveryStatus === "settled")) ||
        (deliveryStatus === "accepted" &&
          (block.deliveryStatus === "applied" || block.deliveryStatus === "settled")) ||
        (block.deliveryStatus === "applied" && deliveryStatus === "failed")
      ) {
        return block;
      }

      blocksChanged = true;
      return {
        ...block,
        deliveryStatus,
        expectedTurnId: details?.expectedTurnId ?? block.expectedTurnId,
        acceptedAt: details?.acceptedAt ?? block.acceptedAt,
        failureReason:
          deliveryStatus === "failed" ? details?.failureReason ?? block.failureReason : undefined,
      };
    });
    if (!blocksChanged) {
      continue;
    }

    if (nextMessages === messages) {
      nextMessages = [...messages];
    }
    nextMessages[index] = {
      ...message,
      blocks: nextBlocks,
      hydration: "full",
      hasDeferredContent: hasDeferredActionOutput(nextBlocks),
    };
  }

  return nextMessages;
}

function settlePendingSteerBlocks(blocks: ContentBlock[] | undefined): ContentBlock[] | undefined {
  if (!blocks) {
    return blocks;
  }

  let changed = false;
  const nextBlocks = blocks.map((block) => {
    if (
      block.type !== "steer" ||
      block.deliveryStatus === "failed" ||
      block.deliveryStatus === "settled"
    ) {
      return block;
    }
    changed = true;
    return { ...block, deliveryStatus: "settled" as const };
  });
  return changed ? nextBlocks : blocks;
}

function collapseTrailingSteerMessages(messages: Message[]): Message[] {
  let changed = false;
  const collapsed: Message[] = [];

  for (let index = 0; index < messages.length; index += 1) {
    const message = messages[index];
    const previous = collapsed[collapsed.length - 1];
    const nextMessage = messages[index + 1];

    const isSteerCandidate =
      message.role === "user" &&
      previous?.role === "assistant" &&
      messageHasSteerMarker(message) &&
      nextMessage?.role !== "assistant";

    if (isSteerCandidate) {
      collapsed[collapsed.length - 1] = appendSteerBlockToAssistantMessage(
        previous,
        createSteerBlockFromMessage(
          message,
          previous.status === "streaming" ? "accepted" : "settled",
        ),
      );
      changed = true;
      continue;
    }

    collapsed.push(message);
  }

  return changed ? collapsed : messages;
}

function isThreadStatusStreaming(status: ThreadStatus): boolean {
  return isThreadTurnActive(status);
}

function resolveRestoredTurnStartedAt(messages: Message[]): number | null {
  const lastMessage = messages[messages.length - 1];
  const startedAt = lastMessage ? Date.parse(lastMessage.createdAt) : Number.NaN;
  return Number.isFinite(startedAt) ? startedAt : null;
}

function hasRenderableAssistantContent(message: Message): boolean {
  if (typeof message.content === "string" && message.content.trim().length > 0) {
    return true;
  }

  const blocks = message.blocks;
  if (!Array.isArray(blocks) || blocks.length === 0) {
    return false;
  }

  return blocks.some((block) => {
    if (block.type === "text" || block.type === "thinking") {
      return Boolean(block.content?.trim());
    }
    return true;
  });
}

function resolveAssistantMessageIndex(
  messages: Message[],
  target: AssistantMessageTarget,
): number {
  if (target.assistantMessageId) {
    const byIdIndex = messages.findIndex((message) => message.id === target.assistantMessageId);
    if (byIdIndex >= 0) {
      return byIdIndex;
    }
  }

  if (target.clientTurnId) {
    for (let index = messages.length - 1; index >= 0; index -= 1) {
      const message = messages[index];
      if (message.role === "assistant" && message.clientTurnId === target.clientTurnId) {
        return index;
      }
    }
  }

  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index];
    if (message.role === "assistant" && message.status === "streaming") {
      return index;
    }
  }

  return -1;
}

function compactTrailingStreamingAssistantMessages(
  messages: Message[],
  target: AssistantMessageTarget,
): Message[] {
  if (messages.length < 2) {
    return messages;
  }

  let trailingStart = messages.length;
  while (trailingStart > 0) {
    const message = messages[trailingStart - 1];
    if (message.role !== "assistant" || message.status !== "streaming") {
      break;
    }
    trailingStart -= 1;
  }

  const trailingCount = messages.length - trailingStart;
  if (trailingCount <= 1) {
    return messages;
  }

  const trailingMessages = messages.slice(trailingStart);
  let keepIndex = -1;

  if (target.assistantMessageId) {
    keepIndex = trailingMessages.findIndex((message) => message.id === target.assistantMessageId);
  }
  if (keepIndex < 0 && target.clientTurnId) {
    keepIndex = trailingMessages.findIndex(
      (message) => message.clientTurnId === target.clientTurnId,
    );
  }
  if (keepIndex < 0) {
    keepIndex = trailingMessages.length - 1;
    for (let index = trailingMessages.length - 1; index >= 0; index -= 1) {
      if (hasRenderableAssistantContent(trailingMessages[index])) {
        keepIndex = index;
        break;
      }
    }
  }

  return [...messages.slice(0, trailingStart), trailingMessages[keepIndex]];
}

function ensureAssistantMessage(
  messages: Message[],
  threadId: string,
  target: AssistantMessageTarget,
): { messages: Message[]; assistantIndex: number } {
  const compactedMessages = compactTrailingStreamingAssistantMessages(messages, target);
  const existingIndex = resolveAssistantMessageIndex(compactedMessages, target);
  if (existingIndex >= 0) {
    return {
      messages: compactedMessages,
      assistantIndex: existingIndex,
    };
  }

  const assistantMessage = createStreamingAssistantMessage(threadId, {
    id: target.assistantMessageId ?? undefined,
    clientTurnId: target.clientTurnId ?? null,
  });
  return {
    messages: [...compactedMessages, assistantMessage],
    assistantIndex: compactedMessages.length,
  };
}

function upsertBlock(blocks: ContentBlock[], block: ContentBlock): ContentBlock[] {
  if (block.type === "action") {
    const idx = blocks.findIndex(
      (b) => b.type === "action" && (b as ActionBlock).actionId === block.actionId
    );
    if (idx >= 0) {
      const next = [...blocks];
      next[idx] = block;
      return next;
    }
  }

  if (block.type === "approval") {
    const idx = blocks.findIndex(
      (b) => b.type === "approval" && (b as ApprovalBlock).approvalId === block.approvalId
    );
    if (idx >= 0) {
      const next = [...blocks];
      next[idx] = block;
      return next;
    }
  }

  return [...blocks, block];
}

function upsertNoticeBlock(blocks: ContentBlock[], block: NoticeBlock): ContentBlock[] {
  const idx = blocks.findIndex(
    (candidate) =>
      candidate.type === "notice" &&
      (candidate as NoticeBlock).kind === block.kind,
  );
  if (idx >= 0) {
    const next = [...blocks];
    next[idx] = block;
    return next;
  }

  if (block.kind.startsWith("hook_") || block.kind.startsWith("codex_hook_")) {
    return [...blocks, block];
  }

  return [block, ...blocks];
}

function normalizeBlocks(blocks?: ContentBlock[]): ContentBlock[] | undefined {
  if (!Array.isArray(blocks)) {
    return blocks;
  }

  const normalized: ContentBlock[] = [];
  for (const block of blocks) {
    const last = normalized[normalized.length - 1];
    if (block.type === "text" && last?.type === "text") {
      normalized[normalized.length - 1] = {
        ...last,
        content: `${last.content}${block.content ?? ""}`
      };
      continue;
    }
    if (block.type === "thinking" && last?.type === "thinking") {
      normalized[normalized.length - 1] = {
        ...last,
        content: `${last.content}${block.content ?? ""}`
      };
      continue;
    }
    normalized.push(block);
  }

  return normalized;
}

function hasDeferredActionOutput(blocks?: ContentBlock[]): boolean {
  if (!Array.isArray(blocks)) {
    return false;
  }
  return blocks.some((block) => block.type === "action" && block.outputDeferred === true);
}

function markMessageAsFullyHydrated(message: Message): Message {
  const hasDeferredContent = hasDeferredActionOutput(message.blocks);
  if (message.hydration === "full" && message.hasDeferredContent === hasDeferredContent) {
    return message;
  }

  return {
    ...message,
    hydration: "full",
    hasDeferredContent,
  };
}

function summarizeActionBlockForMemory(block: ActionBlock): ActionBlock {
  const hasOutput =
    block.outputChunks.length > 0 ||
    (typeof block.result?.output === "string" && block.result.output.length > 0) ||
    block.outputDeferred === true;
  if (!hasOutput) {
    return block;
  }

  let nextResult = block.result;
  if (block.result && typeof block.result.output === "string") {
    nextResult = {
      ...block.result,
      output: undefined,
    };
  }

  if (
    block.outputDeferred === true &&
    block.outputDeferredLoaded === false &&
    block.outputChunks.length === 0 &&
    nextResult === block.result
  ) {
    return block;
  }

  return {
    ...block,
    outputChunks: [],
    outputDeferred: true,
    outputDeferredLoaded: false,
    result: nextResult,
  };
}

function summarizeMessageForMemory(message: Message): Message {
  const sourceBlocks = message.blocks;
  let nextBlocks = sourceBlocks;

  if (Array.isArray(sourceBlocks) && sourceBlocks.length > 0) {
    for (let index = 0; index < sourceBlocks.length; index += 1) {
      const block = sourceBlocks[index];
      if (block.type !== "action") {
        continue;
      }

      const summarizedBlock = summarizeActionBlockForMemory(block);
      if (summarizedBlock === block) {
        continue;
      }

      if (nextBlocks === sourceBlocks) {
        nextBlocks = [...sourceBlocks];
      }
      (nextBlocks as ContentBlock[])[index] = summarizedBlock;
    }
  }

  const hasDeferredContent = hasDeferredActionOutput(nextBlocks);
  if (
    nextBlocks === sourceBlocks &&
    message.hydration === "summary" &&
    message.hasDeferredContent === hasDeferredContent
  ) {
    return message;
  }

  return {
    ...message,
    hydration: "summary",
    hasDeferredContent,
    blocks: nextBlocks,
  };
}

function applyHydrationWindow(messages: Message[]): Message[] {
  if (messages.length === 0) {
    return messages;
  }

  const summarizeUntil = Math.max(0, messages.length - MAX_FULLY_HYDRATED_MESSAGES);
  let nextMessages = messages;
  for (let index = 0; index < messages.length; index += 1) {
    const message = messages[index];
    const nextMessage =
      index < summarizeUntil
        ? summarizeMessageForMemory(message)
        : message.hydration === "summary"
          ? message
          : markMessageAsFullyHydrated(message);
    if (nextMessage !== message) {
      if (nextMessages === messages) {
        nextMessages = [...messages];
      }
      nextMessages[index] = nextMessage;
    }
  }

  return nextMessages;
}

function normalizeMessages(
  messages: Message[],
  options?: { collapseTrailingSteers?: boolean },
): Message[] {
  let nextMessages = messages;
  for (let index = 0; index < nextMessages.length; index += 1) {
    const message = nextMessages[index];
    const normalizedBlocks = normalizeBlocks(message.blocks);
    const normalizedContent =
      message.role === "user" && normalizedBlocks && typeof message.content === "string"
        ? undefined
        : message.content;
    const normalizedMessage =
      normalizedBlocks === message.blocks && normalizedContent === message.content
        ? message
        : {
            ...message,
            content: normalizedContent,
            blocks: normalizedBlocks,
          };
    const nextMessage = markMessageAsFullyHydrated(normalizedMessage);
    if (nextMessage !== message) {
      if (nextMessages === messages) {
        nextMessages = [...messages];
      }
      nextMessages[index] = nextMessage;
    }
  }

  return options?.collapseTrailingSteers === false
    ? nextMessages
    : collapseTrailingSteerMessages(nextMessages);
}

function toIsoTimestamp(value: number | null | undefined): string | null {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    return null;
  }

  const normalized = value < 10_000_000_000 ? value * 1000 : value;
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) {
    return null;
  }
  return date.toISOString();
}

const CONTEXT_WINDOW_BASELINE_TOKENS = 12_000;

function calculateContextPercentRemaining(
  currentTokens: number | null,
  maxContextTokens: number | null,
): number | null {
  if (
    typeof currentTokens !== "number" ||
    !Number.isFinite(currentTokens) ||
    typeof maxContextTokens !== "number" ||
    !Number.isFinite(maxContextTokens)
  ) {
    return null;
  }

  if (maxContextTokens <= CONTEXT_WINDOW_BASELINE_TOKENS) {
    return 0;
  }

  const effectiveWindow = maxContextTokens - CONTEXT_WINDOW_BASELINE_TOKENS;
  const usedTokens = Math.max(0, currentTokens - CONTEXT_WINDOW_BASELINE_TOKENS);
  const remainingTokens = Math.max(0, effectiveWindow - usedTokens);

  return Math.max(
    0,
    Math.min(100, Math.round((remainingTokens / effectiveWindow) * 100)),
  );
}

function mapUsageLimitsFromEvent(event: Extract<StreamEvent, { type: "UsageLimitsUpdated" }>): ContextUsage | null {
  const usage = event.usage ?? {};
  const currentTokensRaw = usage.current_tokens;
  const maxContextTokensRaw = usage.max_context_tokens;
  const contextPercentRaw = usage.context_window_percent;
  const fiveHourPercentRaw = usage.five_hour_percent;
  const weeklyPercentRaw = usage.weekly_percent;
  const fableWeeklyPercentRaw = usage.fable_weekly_percent;
  const opusWeeklyPercentRaw = usage.opus_weekly_percent;
  const sonnetWeeklyPercentRaw = usage.sonnet_weekly_percent;

  const currentTokens =
    typeof currentTokensRaw === "number" ? Math.max(0, Math.round(currentTokensRaw)) : null;
  const maxContextTokens =
    typeof maxContextTokensRaw === "number" ? Math.max(0, Math.round(maxContextTokensRaw)) : null;
  const hasContextMetrics = currentTokens !== null || maxContextTokens !== null;

  let contextPercent = calculateContextPercentRemaining(currentTokens, maxContextTokens);
  if (contextPercent === null && typeof contextPercentRaw === "number") {
    contextPercent = Math.round(contextPercentRaw);
  }
  if (contextPercent !== null && !Number.isFinite(contextPercent)) {
    contextPercent = null;
  }

  const hasAnyMetric =
    hasContextMetrics ||
    typeof contextPercentRaw === "number" ||
    typeof fiveHourPercentRaw === "number" ||
    typeof weeklyPercentRaw === "number" ||
    typeof fableWeeklyPercentRaw === "number" ||
    typeof opusWeeklyPercentRaw === "number" ||
    typeof sonnetWeeklyPercentRaw === "number";
  if (!hasAnyMetric) {
    return null;
  }

  // Codex reports `usedPercent`; UI shows remaining budget.
  const toRemainingPercent = (
    usedPercent: number | null | undefined,
  ): number | null => {
    if (typeof usedPercent !== "number" || !Number.isFinite(usedPercent)) {
      return null;
    }
    const used = Math.max(0, Math.min(100, Math.round(usedPercent)));
    return 100 - used;
  };

  return {
    currentTokens,
    maxContextTokens,
    contextPercent:
      contextPercent === null ? null : Math.max(0, Math.min(100, contextPercent)),
    windowFiveHourPercent: toRemainingPercent(fiveHourPercentRaw),
    windowWeeklyPercent: toRemainingPercent(weeklyPercentRaw),
    windowFableWeeklyPercent: toRemainingPercent(fableWeeklyPercentRaw),
    windowOpusWeeklyPercent: toRemainingPercent(opusWeeklyPercentRaw),
    windowSonnetWeeklyPercent: toRemainingPercent(sonnetWeeklyPercentRaw),
    windowFiveHourResetsAt: toIsoTimestamp(usage.five_hour_resets_at),
    windowWeeklyResetsAt: toIsoTimestamp(usage.weekly_resets_at),
    windowFableWeeklyResetsAt: toIsoTimestamp(usage.fable_weekly_resets_at),
    windowOpusWeeklyResetsAt: toIsoTimestamp(usage.opus_weekly_resets_at),
    windowSonnetWeeklyResetsAt: toIsoTimestamp(usage.sonnet_weekly_resets_at),
  };
}

function mapProviderUsage(provider: ChatProviderUsage | undefined): ContextUsage | null {
  if (!provider?.available || provider.windows.length === 0) {
    return null;
  }

  const usage: ContextUsage = {
    currentTokens: null,
    maxContextTokens: null,
    contextPercent: null,
    windowFiveHourPercent: null,
    windowWeeklyPercent: null,
    windowFableWeeklyPercent: null,
    windowOpusWeeklyPercent: null,
    windowSonnetWeeklyPercent: null,
    windowFiveHourResetsAt: null,
    windowWeeklyResetsAt: null,
    windowFableWeeklyResetsAt: null,
    windowOpusWeeklyResetsAt: null,
    windowSonnetWeeklyResetsAt: null,
  };

  for (const window of provider.windows) {
    const remainingPercent = Math.max(0, Math.min(100, 100 - Math.round(window.usedPercent)));
    const resetsAt = toIsoTimestamp(window.resetsAt);
    switch (window.kind) {
      case "five_hour":
        usage.windowFiveHourPercent = remainingPercent;
        usage.windowFiveHourResetsAt = resetsAt;
        break;
      case "weekly":
        usage.windowWeeklyPercent = remainingPercent;
        usage.windowWeeklyResetsAt = resetsAt;
        break;
      case "fable_weekly":
        usage.windowFableWeeklyPercent = remainingPercent;
        usage.windowFableWeeklyResetsAt = resetsAt;
        break;
      case "opus_weekly":
        usage.windowOpusWeeklyPercent = remainingPercent;
        usage.windowOpusWeeklyResetsAt = resetsAt;
        break;
      case "sonnet_weekly":
        usage.windowSonnetWeeklyPercent = remainingPercent;
        usage.windowSonnetWeeklyResetsAt = resetsAt;
        break;
    }
  }

  return usage;
}

function mergeUsageLimits(
  previous: ContextUsage | null,
  update: ContextUsage | null,
): ContextUsage | null {
  if (!update) {
    return previous;
  }
  if (!previous) {
    return update;
  }

  return {
    currentTokens: update.currentTokens ?? previous.currentTokens,
    maxContextTokens: update.maxContextTokens ?? previous.maxContextTokens,
    contextPercent: update.contextPercent ?? previous.contextPercent,
    windowFiveHourPercent:
      update.windowFiveHourPercent ?? previous.windowFiveHourPercent,
    windowWeeklyPercent: update.windowWeeklyPercent ?? previous.windowWeeklyPercent,
    windowFableWeeklyPercent:
      update.windowFableWeeklyPercent ?? previous.windowFableWeeklyPercent,
    windowOpusWeeklyPercent:
      update.windowOpusWeeklyPercent ?? previous.windowOpusWeeklyPercent,
    windowSonnetWeeklyPercent:
      update.windowSonnetWeeklyPercent ?? previous.windowSonnetWeeklyPercent,
    windowFiveHourResetsAt:
      update.windowFiveHourResetsAt ?? previous.windowFiveHourResetsAt,
    windowWeeklyResetsAt:
      update.windowWeeklyResetsAt ?? previous.windowWeeklyResetsAt,
    windowFableWeeklyResetsAt:
      update.windowFableWeeklyResetsAt ?? previous.windowFableWeeklyResetsAt,
    windowOpusWeeklyResetsAt:
      update.windowOpusWeeklyResetsAt ?? previous.windowOpusWeeklyResetsAt,
    windowSonnetWeeklyResetsAt:
      update.windowSonnetWeeklyResetsAt ?? previous.windowSonnetWeeklyResetsAt,
  };
}

function resolveAssistantTargetFromEvent(
  threadId: string,
  event: StreamEvent,
): AssistantMessageTarget {
  const pendingTurnMeta = pendingTurnMetaByThread.get(threadId);
  const eventClientTurnId =
    event.type === "TurnStarted" && typeof event.client_turn_id === "string"
      ? event.client_turn_id
      : null;

  return {
    clientTurnId: eventClientTurnId ?? pendingTurnMeta?.clientTurnId ?? null,
    assistantMessageId:
      eventClientTurnId && pendingTurnMeta?.clientTurnId === eventClientTurnId
        ? pendingTurnMeta.assistantMessageId ?? null
        : pendingTurnMeta?.assistantMessageId ?? null,
  };
}

function applyStreamEvent(messages: Message[], event: StreamEvent, threadId: string): Message[] {
  if (event.type === "UsageLimitsUpdated") {
    return messages;
  }

  if (event.type === "ApprovalResolved") {
    return resolveApprovalInMessages(messages, String(event.approval_id ?? ""));
  }

  const assistantTarget = resolveAssistantTargetFromEvent(threadId, event);
  const { messages: ensuredMessages, assistantIndex } = ensureAssistantMessage(
    messages,
    threadId,
    assistantTarget,
  );
  let next = ensuredMessages;
  const currentAssistant = next[assistantIndex];
  const assistant: Message = { ...currentAssistant };
  const existingBlocks = currentAssistant.blocks ?? [];
  let trailingPendingSteers: SteerBlock[] = [];
  if (event.type === "TurnCompleted") {
    assistant.blocks = existingBlocks;
  } else {
    trailingPendingSteers = existingBlocks.filter(
      (block): block is SteerBlock =>
        block.type === "steer" &&
        (block.deliveryStatus === "sending" || block.deliveryStatus === "accepted"),
    );
    assistant.blocks =
      trailingPendingSteers.length > 0
        ? existingBlocks.filter(
            (block) =>
              block.type !== "steer" ||
              (block.deliveryStatus !== "sending" && block.deliveryStatus !== "accepted"),
          )
        : existingBlocks;
  }

  if (event.type === "TurnStarted" && typeof event.client_turn_id === "string") {
    assistant.clientTurnId = event.client_turn_id;
  }

  // Stamp durationMs on the last thinking block when a non-thinking event arrives
  if (event.type !== "ThinkingDelta") {
    const blocks = assistant.blocks ?? [];
    const last = blocks[blocks.length - 1];
    if (last?.type === "thinking" && last.startedAt != null && last.durationMs == null) {
      assistant.blocks = [
        ...blocks.slice(0, -1),
        { ...last, durationMs: Date.now() - last.startedAt },
      ];
    }
  }

  if (event.type === "TextDelta") {
    const blocks = assistant.blocks ?? [];
    const delta = String(event.content ?? "");
    if (!delta) {
      return next;
    }
    const last = blocks[blocks.length - 1];
    if (last?.type === "text") {
      assistant.blocks = [
        ...blocks.slice(0, -1),
        {
          ...last,
          content: `${last.content}${delta}`,
        },
      ];
    } else {
      assistant.blocks = [...blocks, { type: "text", content: delta }];
    }
  }

  if (event.type === "ThinkingDelta") {
    const blocks = assistant.blocks ?? [];
    const delta = String(event.content ?? "");
    if (!delta) {
      return next;
    }
    const last = blocks[blocks.length - 1];
    if (last?.type === "thinking") {
      assistant.blocks = [
        ...blocks.slice(0, -1),
        {
          ...last,
          content: `${last.content}${delta}`,
        },
      ];
    } else {
      assistant.blocks = [...blocks, { type: "thinking" as const, content: delta, startedAt: Date.now() }];
    }
  }

  if (event.type === "SteerApplied") {
    const clientSteerId = String(event.client_steer_id ?? "");
    const pendingIndex = trailingPendingSteers.findIndex(
      (block) => block.steerId === clientSteerId,
    );
    const pendingBlock =
      pendingIndex >= 0 ? trailingPendingSteers[pendingIndex] : undefined;
    if (pendingIndex >= 0) {
      trailingPendingSteers = trailingPendingSteers.filter(
        (_block, index) => index !== pendingIndex,
      );
    }

    const blocks = assistant.blocks ?? [];
    const existingIndex = blocks.findIndex(
      (block) => block.type === "steer" && block.steerId === clientSteerId,
    );
    if (existingIndex >= 0) {
      assistant.blocks = blocks.map((block, index) =>
        index === existingIndex && block.type === "steer"
          ? { ...block, deliveryStatus: "applied" as const }
          : block,
      );
    } else {
      const appliedBlock = pendingBlock
        ? { ...pendingBlock, deliveryStatus: "applied" as const }
        : createSteerBlock(String(event.content ?? ""), {
            steerId: clientSteerId,
            deliveryStatus: "applied",
            planMode: Boolean(event.plan_mode),
            attachments: Array.isArray(event.attachments) ? event.attachments : [],
            inputItems: Array.isArray(event.input_items) ? event.input_items : [],
          });
      assistant.blocks = [...blocks, appliedBlock];
    }
  }

  if (event.type === "ActionStarted") {
    const blocks = assistant.blocks ?? [];
    assistant.blocks = upsertBlock(blocks, {
      type: "action",
      actionId: String(event.action_id),
      engineActionId: event.engine_action_id as string | undefined,
      actionType: String(event.action_type ?? "other") as ActionBlock["actionType"],
      summary: String(event.summary ?? ""),
      details: (event.details as Record<string, unknown>) ?? {},
      outputChunks: [],
      outputDeferred: false,
      outputDeferredLoaded: true,
      status: "running"
    });
  }

  if (event.type === "ActionBackgroundTaskAssigned") {
    const actionId = String(event.action_id ?? "");
    const taskId = String(event.task_id ?? "");
    if (actionId && taskId) {
      const blocks = assistant.blocks ?? [];
      assistant.blocks = patchActionBlock(blocks, actionId, (block) => {
        if (block.backgroundTaskId === taskId) {
          return block;
        }
        return { ...block, backgroundTaskId: taskId };
      });
    }
  }

  if (event.type === "ActionOutputDelta") {
    const actionId = String(event.action_id ?? "");
    const stream = normalizeActionOutputStream(event.stream);
    const content = String(event.content ?? "");
    if (actionId && content) {
      const blocks = assistant.blocks ?? [];
      assistant.blocks = patchActionBlock(blocks, actionId, (block) => {
        const details = (block.details ?? {}) as Record<string, unknown>;
        const previousChunk = block.outputChunks[block.outputChunks.length - 1];
        const mergedChunks =
          previousChunk && previousChunk.stream === stream
            ? [
                ...block.outputChunks.slice(0, -1),
                {
                  ...previousChunk,
                  content: `${previousChunk.content}${content}`,
                },
              ]
            : [
                ...block.outputChunks,
                {
                  stream,
                  content,
                },
              ];
        const { chunks: nextOutputChunks, truncated } = trimActionOutputChunks(mergedChunks);
        const shouldMarkTruncated =
          truncated &&
          !("outputTruncated" in details && details.outputTruncated === true);
        const nextDetails = shouldMarkTruncated
          ? {
              ...details,
              outputTruncated: true,
            }
          : details;

        if (nextOutputChunks === block.outputChunks && nextDetails === block.details) {
          return block;
        }

        return {
          ...block,
          outputChunks: nextOutputChunks,
          details: nextDetails,
          outputDeferred: false,
          outputDeferredLoaded: true,
        };
      });
    }
  }

  if (event.type === "ActionProgressUpdated") {
    const actionId = String(event.action_id ?? "");
    const progressMessage = String(event.message ?? "");
    if (actionId && progressMessage) {
      const blocks = assistant.blocks ?? [];
      assistant.blocks = patchActionBlock(blocks, actionId, (block) => {
        const details = (block.details ?? {}) as Record<string, unknown>;
        if (
          details.progressKind === "mcp" &&
          typeof details.progressMessage === "string" &&
          details.progressMessage === progressMessage
        ) {
          return block;
        }

        return {
          ...block,
          details: {
            ...details,
            progressKind: "mcp",
            progressMessage,
          },
        };
      });
    }
  }

  if (event.type === "ActionCompleted") {
    const blocks = assistant.blocks ?? [];
    const actionId = String(event.action_id ?? "");
    assistant.blocks = patchActionBlock(blocks, actionId, (block) => {
      const result = (event.result as Record<string, unknown> | undefined) ?? {};
      return {
        ...block,
        status: result.success ? "done" : "error",
        result: {
          success: Boolean(result.success),
          output: result.output as string | undefined,
          error: result.error as string | undefined,
          diff: result.diff as string | undefined,
          durationMs: Number(result.durationMs ?? result.duration_ms ?? 0)
        }
      };
    });
  }

  if (event.type === "ApprovalRequested") {
    const blocks = assistant.blocks ?? [];
    assistant.blocks = upsertBlock(blocks, {
      type: "approval",
      approvalId: String(event.approval_id),
      actionType: String(event.action_type ?? "other") as ApprovalBlock["actionType"],
      summary: String(event.summary ?? ""),
      details: (event.details as Record<string, unknown>) ?? {},
      status: "pending"
    });
  }

  if (event.type === "DiffUpdated") {
    const blocks = assistant.blocks ?? [];
    const scope = String(event.scope ?? "turn") as "turn" | "file" | "workspace";
    const diff = String(event.diff ?? "");

    let existingDiffIndex = -1;
    for (let index = blocks.length - 1; index >= 0; index -= 1) {
      const block = blocks[index];
      if (block.type === "diff" && block.scope === scope) {
        existingDiffIndex = index;
        break;
      }
    }

    if (existingDiffIndex >= 0) {
      const existingBlock = blocks[existingDiffIndex];
      if (existingBlock.type === "diff") {
        const nextBlocks: ContentBlock[] = [];
        blocks.forEach((block, index) => {
          if (block.type === "diff" && block.scope === scope) {
            if (index === existingDiffIndex) {
              nextBlocks.push({
                ...existingBlock,
                diff,
              });
            }
            return;
          }
          nextBlocks.push(block);
        });
        if (nextBlocks.length !== blocks.length || existingBlock.diff !== diff) {
          assistant.blocks = nextBlocks;
        }
      }
    } else {
      assistant.blocks = [
        ...blocks,
        {
          type: "diff",
          diff,
          scope,
        },
      ];
    }
  }

  if (event.type === "ModelRerouted") {
    const fromModel = String(event.from_model ?? "").trim();
    const toModel = String(event.to_model ?? "").trim();
    const reason = String(event.reason ?? "").trim();
    if (toModel) {
      assistant.turnModelId = toModel;
      assistant.blocks = upsertNoticeBlock(assistant.blocks ?? [], {
        type: "notice",
        kind: "model_rerouted",
        level: "info",
        title: "Model rerouted",
        message: `Switched from ${fromModel || "the requested model"} to ${toModel}${reason ? ` (${reason})` : ""}.`,
      });
    }
  }

  if (event.type === "Notice") {
    assistant.blocks = upsertNoticeBlock(assistant.blocks ?? [], {
      type: "notice",
      kind: String(event.kind ?? "notice"),
      level:
        event.level === "warning" || event.level === "error"
          ? event.level
          : "info",
      title: String(event.title ?? "Notice"),
      message: String(event.message ?? ""),
      // Notice 元数据只透传到内容块，由专用 Claude 卡片决定是否解释。
      ...(event.metadata !== undefined ? { metadata: event.metadata } : {}),
    });
  }

  if (event.type === "Error") {
    const blocks = assistant.blocks ?? [];
    const errorBlocks: ContentBlock[] = [
      ...blocks,
      { type: "error", message: String(event.message ?? "Unknown error") },
    ];
    assistant.blocks = event.recoverable
      ? errorBlocks
      : settlePendingSteerBlocks(errorBlocks);
    if (!event.recoverable) {
      assistant.status = "error";
    }
  }

  if (event.type === "TurnCompleted") {
    const status = String(event.status ?? "completed");
    if (status === "failed") {
      assistant.status = "error";
    } else if (status === "interrupted") {
      assistant.status = "interrupted";
    } else {
      assistant.status = "completed";
    }

    const actionSucceeded = status === "completed";
    const actionError =
      status === "interrupted"
        ? "Action did not report completion before the turn was interrupted."
        : status === "failed"
          ? "Action did not report completion because the turn failed."
          : undefined;
    const currentBlocks = assistant.blocks ?? [];
    const blocks = settlePendingSteerBlocks(currentBlocks) ?? [];
    let finalizedBlocksChanged = blocks !== currentBlocks;
    const finalizedBlocks = blocks.map((block) => {
      if (
        block.type !== "action" ||
        (block.status !== "running" && block.status !== "pending")
      ) {
        return block;
      }

      finalizedBlocksChanged = true;
      return {
        ...block,
        status: (actionSucceeded ? "done" : "error") as ActionBlock["status"],
        ...(actionSucceeded
          ? {}
          : {
              result: {
                success: false,
                error: actionError,
                durationMs: 0,
              },
            }),
      };
    });
    if (finalizedBlocksChanged) {
      assistant.blocks = finalizedBlocks;
    }
  }


  if (trailingPendingSteers.length > 0) {
    assistant.blocks = [...(assistant.blocks ?? []), ...trailingPendingSteers];
  }

  assistant.hydration = "full";
  assistant.hasDeferredContent = hasDeferredActionOutput(assistant.blocks);

  const blocksChanged = assistant.blocks !== existingBlocks;
  const statusChanged = assistant.status !== currentAssistant.status;
  const metadataChanged =
    assistant.clientTurnId !== currentAssistant.clientTurnId ||
    assistant.turnEngineId !== currentAssistant.turnEngineId ||
    assistant.turnModelId !== currentAssistant.turnModelId ||
    assistant.turnReasoningEffort !== currentAssistant.turnReasoningEffort ||
    assistant.hydration !== currentAssistant.hydration ||
    assistant.hasDeferredContent !== currentAssistant.hasDeferredContent;

  if (!blocksChanged && !statusChanged && !metadataChanged) {
    return next;
  }

  next = [
    ...next.slice(0, assistantIndex),
    assistant,
    ...next.slice(assistantIndex + 1),
  ];
  return next;
}

export const useChatStore = create<ChatState>((set, get) => ({
  threadId: null,
  sessionReadyByThread: {},
  messages: [],
  olderCursor: null,
  hasOlderMessages: false,
  loadingOlderMessages: false,
  olderLoadBlockedUntil: 0,
  status: "idle",
  streaming: false,
  preparingEngineId: null,
  preparingAttachments: false,
  turnStartedAt: null,
  usageLimits: null,
  usageLimitsLoading: false,
  setActiveThread: async (threadId) => {
    const currentThreadId = get().threadId;
    const currentUnlisten = get().unlisten;

    activeThreadBindSeq += 1;
    const bindSeq = activeThreadBindSeq;

    // Tear down the current listener. If the thread was still streaming,
    // install a lightweight background listener that watches for TurnCompleted
    // so the thread status updates correctly when the user switches back.
    if (currentUnlisten) {
      currentUnlisten();
    }
    if (currentThreadId && get().streaming) {
      cleanupBackgroundListener(currentThreadId);
      listenThreadEvents(currentThreadId, (event) => {
        if (event.type === "TurnStarted") {
          markThreadSessionReady(currentThreadId);
        }
        if (event.type === "TurnCompleted") {
          cleanupBackgroundListener(currentThreadId!);
        }
      }).then((unsub) => {
        // If the user already switched back to this thread, don't register
        if (useChatStore.getState().threadId === currentThreadId) {
          unsub();
          return;
        }
        const existing = backgroundStreamListeners.get(currentThreadId!);
        if (existing) {
          // Another background listener was set up in the meantime
          unsub();
        } else {
          backgroundStreamListeners.set(currentThreadId!, unsub);
        }
      });
    }

    if (!threadId) {
      if (bindSeq !== activeThreadBindSeq) {
        return;
      }

      set({
        threadId: null,
        messages: [],
        olderCursor: null,
        hasOlderMessages: false,
        loadingOlderMessages: false,
        olderLoadBlockedUntil: 0,
        streaming: false,
        preparingEngineId: null,
        preparingAttachments: false,
        turnStartedAt: null,
        status: "idle",
        usageLimits: null,
        usageLimitsLoading: false,
        unlisten: undefined,
      });
      return;
    }

    try {
      // Clean up any background listener for this thread before re-subscribing
      cleanupBackgroundListener(threadId);
      if (bindSeq !== activeThreadBindSeq) {
        return;
      }
      set({
        threadId,
        messages: [],
        olderCursor: null,
        hasOlderMessages: false,
        loadingOlderMessages: false,
        olderLoadBlockedUntil: 0,
        unlisten: undefined,
        streaming: false,
        preparingEngineId: null,
        preparingAttachments: false,
        turnStartedAt: null,
        status: "idle",
        usageLimits: null,
        usageLimitsLoading: false,
        error: undefined,
      });

      const threadState = useThreadStore.getState();
      let activeThread = threadState.threads.find((thread) => thread.id === threadId);
      // A CLI thread can advance while AuraCoder is closed or while it is being
      // used directly in its native CLI. Refresh every attached thread on activation;
      // the engine-specific sync flag only describes a transient in-progress remote turn.
      if (activeThread && isCliThreadAttached(activeThread)) {
        try {
          const syncedThread = await ipc.syncThreadFromEngine(threadId);
          threadState.applyThreadUpdateLocal(syncedThread);
          activeThread = syncedThread;
        } catch (error) {
          console.warn(`Failed to sync CLI thread ${threadId}:`, error);
        }
      }

      const messageWindow = await ipc.getThreadMessagesWindow(
        threadId,
        null,
        MESSAGE_WINDOW_INITIAL_LIMIT,
      );
      let messages = normalizeMessages(messageWindow.messages);
      const olderCursor = messageWindow.nextCursor;
      messages = applyHydrationWindow(messages);
      if (bindSeq !== activeThreadBindSeq) {
        return;
      }

      const hasEngineThreadId =
        typeof activeThread?.engineThreadId === "string" &&
        activeThread.engineThreadId.trim().length > 0;
      if (hasEngineThreadId) {
        markThreadSessionReady(threadId);
      }

      const queuedStreamEvents: StreamEvent[] = [];
      let streamFlushTimer: ReturnType<typeof setTimeout> | null = null;
      let streamFlushInProgress = false;
      let eventRateWindowStartedAt = performance.now();
      let eventRateWindowCount = 0;

      const emitEventRateMetric = (now: number) => {
        const elapsedMs = now - eventRateWindowStartedAt;
        if (elapsedMs <= 0 || eventRateWindowCount <= 0) {
          eventRateWindowStartedAt = now;
          eventRateWindowCount = 0;
          return;
        }
        const eventsPerSecond = (eventRateWindowCount * 1000) / elapsedMs;
        recordPerfMetric("chat.stream.events_per_sec", eventsPerSecond, {
          threadId,
          events: eventRateWindowCount,
          windowMs: elapsedMs,
        });
        eventRateWindowStartedAt = now;
        eventRateWindowCount = 0;
      };

      const flushQueuedStreamEvents = () => {
        if (streamFlushInProgress) {
          return;
        }
        if (streamFlushTimer !== null) {
          globalThis.clearTimeout(streamFlushTimer);
          streamFlushTimer = null;
        }
        if (queuedStreamEvents.length === 0) {
          return;
        }

        streamFlushInProgress = true;
        const batch = queuedStreamEvents.splice(0, queuedStreamEvents.length);
        const flushStartedAt = performance.now();
        try {
          set((state) => {
            if (bindSeq !== activeThreadBindSeq || state.threadId !== threadId) {
              return state;
            }

            let nextMessages = state.messages;
            let nextStreaming = state.streaming;
            let nextStatus = state.status;
            let nextTurnStartedAt = state.turnStartedAt;
            let nextUsageLimits = state.usageLimits;
            let nextUsageLimitsLoading = state.usageLimitsLoading;
            let hydrationRecalcRequired = false;
            for (const queuedEvent of batch) {
              if (queuedEvent.type === "UsageLimitsUpdated") {
                nextUsageLimits = mergeUsageLimits(
                  nextUsageLimits,
                  mapUsageLimitsFromEvent(queuedEvent),
                );
                nextUsageLimitsLoading = false;
                continue;
              }
              const previousLength = nextMessages.length;
              nextMessages = applyStreamEvent(nextMessages, queuedEvent, state.threadId);
              if (nextMessages.length !== previousLength) {
                hydrationRecalcRequired = true;
              }
              const nextRuntimeState = applyRuntimeStateFromEvent(
                nextStatus,
                nextStreaming,
                queuedEvent,
              );
              nextStatus = nextRuntimeState.status;
              nextStreaming = nextRuntimeState.streaming;
              if (
                queuedEvent.type === "TurnCompleted" ||
                (queuedEvent.type === "Error" && !queuedEvent.recoverable)
              ) {
                pendingTurnMetaByThread.delete(threadId);
                nextTurnStartedAt = null;
              }
            }
            if (hydrationRecalcRequired) {
              nextMessages = applyHydrationWindow(nextMessages);
            }

            if (
              nextMessages === state.messages &&
              nextStatus === state.status &&
              nextStreaming === state.streaming &&
              nextTurnStartedAt === state.turnStartedAt &&
              nextUsageLimits === state.usageLimits &&
              nextUsageLimitsLoading === state.usageLimitsLoading
            ) {
              return state;
            }

            return {
              ...state,
              messages: nextMessages,
              status: nextStatus,
              streaming: nextStreaming,
              turnStartedAt: nextTurnStartedAt,
              usageLimits: nextUsageLimits,
              usageLimitsLoading: nextUsageLimitsLoading,
            };
          });
        } finally {
          streamFlushInProgress = false;
        }
        recordPerfMetric("chat.stream.flush.ms", performance.now() - flushStartedAt, {
          threadId,
          batchSize: batch.length,
        });

        if (queuedStreamEvents.length > 0) {
          scheduleStreamFlush();
        }
      };

      const scheduleStreamFlush = () => {
        if (streamFlushTimer !== null) {
          return;
        }
        streamFlushTimer = globalThis.setTimeout(() => {
          streamFlushTimer = null;
          flushQueuedStreamEvents();
        }, STREAM_EVENT_BATCH_WINDOW_MS);
      };

      const unlistenStream = await listenThreadEvents(threadId, (event) => {
        if (bindSeq !== activeThreadBindSeq) {
          return;
        }
        if (event.type === "TurnStarted") {
          markThreadSessionReady(threadId);
        }
        const pendingTurnMeta = pendingTurnMetaByThread.get(threadId);
        if (
          pendingTurnMeta &&
          event.type === "TurnStarted" &&
          typeof event.client_turn_id === "string" &&
          event.client_turn_id.length > 0
        ) {
          pendingTurnMeta.clientTurnId = event.client_turn_id;
        }
        if (pendingTurnMeta && event.type === "ModelRerouted") {
          const reroutedModelId =
            typeof event.to_model === "string" ? event.to_model.trim() : "";
          if (reroutedModelId) {
            pendingTurnMeta.turnModelId = reroutedModelId;
          }
        }
        recordPendingTurnLatencyMetrics(threadId, event);
        enqueueStreamEvent(queuedStreamEvents, event);
        eventRateWindowCount += 1;
        const now = performance.now();
        if (now - eventRateWindowStartedAt >= 1000) {
          emitEventRateMetric(now);
        }
        if (event.type === "TurnCompleted") {
          flushQueuedStreamEvents();
          emitEventRateMetric(performance.now());
          return;
        }
        if (queuedStreamEvents.length >= STREAM_EVENT_QUEUE_FLUSH_THRESHOLD) {
          flushQueuedStreamEvents();
          return;
        }
        scheduleStreamFlush();
      });

      const unlisten = () => {
        if (streamFlushTimer !== null) {
          globalThis.clearTimeout(streamFlushTimer);
          streamFlushTimer = null;
        }
        queuedStreamEvents.length = 0;
        emitEventRateMetric(performance.now());
        unlistenStream();
      };

      if (bindSeq !== activeThreadBindSeq) {
        unlisten();
        return;
      }

      const threadStatus = activeThread?.status ?? "idle";
      const currentState = get();
      if (currentState.threadId === threadId && currentState.streaming) {
        if (currentState.unlisten) {
          unlisten();
        }
        set({
          olderCursor,
          hasOlderMessages: olderCursor !== null,
          loadingOlderMessages: false,
          olderLoadBlockedUntil: 0,
          unlisten: currentState.unlisten ?? unlisten,
          error: undefined,
        });
        return;
      }

      const restoredEngineId = activeThread?.engineId ??
        messages.find((message) => message.turnEngineId)?.turnEngineId ??
        null;
      const shouldRefreshUsageLimits =
        messages.some((message) => message.role === "user") &&
        (restoredEngineId === "codex" || restoredEngineId === "claude");

      set({
        threadId,
        messages,
        olderCursor,
        hasOlderMessages: olderCursor !== null,
        loadingOlderMessages: false,
        olderLoadBlockedUntil: 0,
        unlisten,
        error: undefined,
        streaming: isThreadStatusStreaming(threadStatus),
        preparingEngineId: null,
        preparingAttachments: false,
        turnStartedAt: isThreadStatusStreaming(threadStatus)
          ? resolveRestoredTurnStartedAt(messages)
          : null,
        status: threadStatus,
        usageLimits: null,
        usageLimitsLoading: shouldRefreshUsageLimits,
      });

      if (shouldRefreshUsageLimits) {
        const refreshRestoredUsageLimits = async () => {
          try {
            const providers = await ipc.getChatProviderUsage(
              activeThread?.workspaceId ?? null,
              restoredEngineId,
            );
            const providerUsage = mapProviderUsage(
              providers.find((provider) => provider.engineId === restoredEngineId),
            );
            if (bindSeq !== activeThreadBindSeq || get().threadId !== threadId) {
              return;
            }
            set((state) => ({
              usageLimits: mergeUsageLimits(state.usageLimits, providerUsage),
              usageLimitsLoading: false,
            }));
          } catch {
            if (bindSeq === activeThreadBindSeq && get().threadId === threadId) {
              set({ usageLimitsLoading: false });
            }
          }
        };
        void refreshRestoredUsageLimits();
      }
    } catch (error) {
      if (bindSeq !== activeThreadBindSeq) {
        return;
      }
      set({
        threadId,
        messages: [],
        olderCursor: null,
        hasOlderMessages: false,
        loadingOlderMessages: false,
        olderLoadBlockedUntil: 0,
        usageLimits: null,
        usageLimitsLoading: false,
        turnStartedAt: null,
        error: String(error),
      });
    }
  },
  loadOlderMessages: async () => {
    const state = get();
    const threadId = state.threadId;
    const cursor = state.olderCursor;
    if (
      !threadId ||
      !cursor ||
      state.loadingOlderMessages ||
      state.olderLoadBlockedUntil > Date.now()
    ) {
      return;
    }

    set((current) => {
      if (
        current.threadId !== threadId ||
        current.loadingOlderMessages ||
        current.olderCursor !== cursor
      ) {
        return current;
      }
      return {
        ...current,
        loadingOlderMessages: true,
      };
    });

    try {
      const olderWindow = await ipc.getThreadMessagesWindow(
        threadId,
        cursor,
        MESSAGE_WINDOW_INITIAL_LIMIT,
      );
      const olderMessages = normalizeMessages(olderWindow.messages, {
        collapseTrailingSteers: false,
      }).map((message) =>
        summarizeMessageForMemory(message),
      );
      set((current) => {
        if (current.threadId !== threadId) {
          return current;
        }
        const nextCursor = olderWindow.nextCursor;
        const mergedMessages = collapseTrailingSteerMessages([
          ...olderMessages,
          ...current.messages,
        ]);
        return {
          ...current,
          messages: applyHydrationWindow(mergedMessages),
          olderCursor: nextCursor,
          hasOlderMessages: nextCursor !== null,
          loadingOlderMessages: false,
          olderLoadBlockedUntil: 0,
        };
      });
    } catch (error) {
      const retryAt = Date.now() + OLDER_MESSAGES_RETRY_BACKOFF_MS;
      set((current) => {
        if (current.threadId !== threadId) {
          return current;
        }
        return {
          ...current,
          loadingOlderMessages: false,
          olderLoadBlockedUntil: retryAt,
          error: String(error),
        };
      });
    }
  },
  send: async (message, options) => {
    const state = get();
    if (state.streaming) {
      set({ error: "A turn is already in progress for this thread." });
      return false;
    }

    const threadId = options?.threadIdOverride ?? state.threadId;
    if (!threadId) {
      set({ error: "No active thread selected" });
      return false;
    }
    const startedAt = performance.now();
    const clientTurnId = crypto.randomUUID();
    const optimisticAssistantMessageId = crypto.randomUUID();
    pendingTurnMetaByThread.set(threadId, {
      turnEngineId: options?.engineId ?? null,
      turnModelId: options?.modelId ?? null,
      turnReasoningEffort: options?.reasoningEffort ?? null,
      clientTurnId,
      assistantMessageId: optimisticAssistantMessageId,
      startedAt,
      firstShellRecorded: false,
      firstContentRecorded: false,
      firstTextRecorded: false,
    });

    const attachments = options?.attachments ?? [];
    const inputItems = options?.inputItems ?? [];
    const planMode = options?.planMode ?? false;
    const userMessage = createOptimisticUserMessage(threadId, message, {
      attachments,
      inputItems,
      planMode,
    });
    const optimisticAssistantMessage = createStreamingAssistantMessage(threadId, {
      id: optimisticAssistantMessageId,
      clientTurnId,
    });

    set((state) => ({
      messages: applyHydrationWindow([
        ...state.messages,
        userMessage,
        optimisticAssistantMessage,
      ]),
      status: "streaming",
      streaming: true,
      preparingEngineId: options?.engineId ?? null,
      preparingAttachments: Boolean(options?.remoteAttachmentUpload),
      turnStartedAt: Date.now(),
      error: undefined
    }));
    schedulePendingTurnShellMetric(threadId, clientTurnId);

    try {
      // 先持久化本轮底部运行选择，再保存统一权限，最后发送消息。
      // 旧逻辑还要求 permissionModeJson 存在，现已改为只判断五项运行选择是否完整；旧条件保留如下：
      // options?.engineId && options?.modelId && options?.sendMethod !== undefined && options?.permissionModeJson !== undefined
      if (
        options?.engineId &&
        options?.modelId &&
        options?.sendMethod !== undefined
      ) {
        await ipc.updateThreadRuntimeSelection(threadId, {
          engineId: options.engineId,
          modelId: options.modelId,
          planMode,
          sendMethod: options.sendMethod ?? null,
          reasoningEffort: options.reasoningEffort ?? null,
          permissionMode: options.permissionModeJson ?? null,
        });
        if (options.permissionValues) {
          await ipc.setThreadPermissions(threadId, options.permissionValues);
        }
      }

      await ipc.sendMessage(
        threadId,
        message,
        options?.modelId ?? null,
        options?.reasoningEffort ?? null,
        attachments.length > 0 ? attachments : null,
        inputItems.length > 0 ? inputItems : null,
        planMode,
        clientTurnId,
        options?.referencedAuraCoderThreadId ?? null,
      );
      set({ preparingEngineId: null, preparingAttachments: false });

      // 旧逻辑在 sendMessage 成功后才保存运行选择，并吞掉保存异常；已停用，保留如下追踪：
      // if (options?.engineId && options?.modelId && options?.sendMethod !== undefined && options?.permissionModeJson !== undefined) {
      //   await ipc.updateThreadRuntimeSelection(threadId, runtimeSelection);
      // }
      return true;
    } catch (error) {
      pendingTurnMetaByThread.delete(threadId);
      set((state) => ({
        messages: state.messages.filter(
          (item) => item.id !== userMessage.id && item.id !== optimisticAssistantMessage.id,
        ),
        status: "error",
        streaming: false,
        preparingEngineId: null,
        preparingAttachments: false,
        turnStartedAt: null,
        error: String(error),
      }));
      return false;
    }
  },
  steer: async (message, options) => {
    const state = get();
    if (!state.streaming) {
      set({ error: "No turn is currently in progress for this thread." });
      return false;
    }

    const threadId = options?.threadIdOverride ?? state.threadId;
    if (!threadId) {
      set({ error: "No active thread selected" });
      return false;
    }

    if (options?.threadIdOverride && options.threadIdOverride !== state.threadId) {
      set({ error: "Cannot steer a thread that is not currently active" });
      return false;
    }

    const attachments = options?.attachments ?? [];
    const inputItems = options?.inputItems ?? [];
    const planMode = options?.planMode ?? false;
    const steerBlock = createSteerBlock(message, {
      attachments,
      inputItems,
      planMode,
      deliveryStatus: "sending",
    });

    set((current) => ({
      messages: applyHydrationWindow(
        appendSteerBlockToActiveAssistant(current.messages, threadId, steerBlock),
      ),
      error: undefined,
    }));

    try {
      const receipt = await ipc.steerMessage(
        threadId,
        message,
        attachments.length > 0 ? attachments : null,
        inputItems.length > 0 ? inputItems : null,
        planMode,
        steerBlock.steerId,
        options?.referencedAuraCoderThreadId ?? null,
      );
      if (receipt.clientSteerId !== steerBlock.steerId) {
        throw new Error(
          `Steer receipt mismatch: expected ${steerBlock.steerId}, received ${receipt.clientSteerId}`,
        );
      }
      set((current) => ({
        messages: applyHydrationWindow(
          updateSteerBlockDelivery(
            current.messages,
            receipt.clientSteerId,
            "accepted",
            {
              expectedTurnId: receipt.expectedTurnId,
              acceptedAt: receipt.acceptedAt,
            },
          ),
        ),
      }));
      return true;
    } catch (error) {
      set((current) => ({
        messages: applyHydrationWindow(
          updateSteerBlockDelivery(current.messages, steerBlock.steerId, "failed", {
            failureReason: String(error),
          }),
        ),
        error: String(error),
      }));
      return false;
    }
  },
  cancel: async () => {
    const threadId = get().threadId;
    if (!threadId) {
      return;
    }

    try {
      await ipc.cancelTurn(threadId);
      pendingTurnMetaByThread.delete(threadId);
      const currentState = get();
      if (currentState.threadId !== threadId || !currentState.streaming) {
        return;
      }
      // Remove the trailing assistant message if it has no meaningful content
      // (e.g. only thinking blocks with no text, or completely empty)
      const messages = updateSteerBlockDelivery(
        currentState.messages,
        null,
        "settled",
      );
      const interruptedMessages = applyStreamEvent(
        messages,
        { type: "TurnCompleted", status: "interrupted" },
        threadId,
      );
      const last = interruptedMessages[interruptedMessages.length - 1];
      const lastHasContent = last?.role === "assistant" && (last.blocks ?? []).some((b) => {
        if (b.type === "text") return Boolean(b.content?.trim());
        if (b.type === "action" || b.type === "diff" || b.type === "code" || b.type === "approval" || b.type === "steer") return true;
        return false;
      });
      const nextMessages = last?.role === "assistant" && !lastHasContent
        ? interruptedMessages.slice(0, -1)
        : interruptedMessages;
      set({ status: "idle", streaming: false, turnStartedAt: null, messages: nextMessages });
    } catch (error) {
      set({ error: String(error) });
    }
  },
  // 业务说明：等待 sidecar 审批回执成功后，再结算可见线程中的审批状态。
  respondApproval: async (approvalId, response, threadIdOverride) => {
    const threadId = threadIdOverride ?? get().threadId;
    if (!threadId) {
      set({ error: "No active thread selected" });
      return false;
    }

    // Only inspect the visible transcript when it belongs to the target thread.
    const decision = resolveApprovalDecision(response);
    const responseData = typeof response === "object" && response !== null && !Array.isArray(response)
      ? response as Record<string, unknown>
      : undefined;
    let previousApproval: ApprovalBlock | null = null;
    if (get().threadId === threadId) {
      set((state) => {
        for (const message of state.messages) {
          const approval = message.blocks?.find(
            (block): block is ApprovalBlock =>
              block.type === "approval" && block.approvalId === approvalId,
          );
          if (approval) {
            previousApproval = approval;
            break;
          }
        }

        // 旧逻辑会在 IPC 调用前乐观标记 answered，现保留迁移留痕但不再执行：
        // const nextMessages = resolveApprovalInMessages(state.messages, approvalId, decision, responseData);
        // if (nextMessages === state.messages) {
        //   return state;
        // }
        // return { ...state, messages: nextMessages };
        return state;
      });
    }

    try {
      await ipc.respondApproval(threadId, approvalId, response);
      if (get().threadId === threadId) {
        set((state) => {
          if (state.threadId !== threadId) {
            return state;
          }
          const nextMessages = resolveApprovalInMessages(
            state.messages,
            approvalId,
            decision,
            responseData,
          );
          return {
            ...state,
            messages: nextMessages,
            status: "streaming",
            streaming: true,
          };
        });
      }
      return true;
    } catch (error) {
      set((state) => ({
        ...(previousApproval && state.threadId === threadId
          ? { messages: restoreApprovalInMessages(state.messages, approvalId, previousApproval) }
          : {}),
        error: String(error),
      }));
      return false;
    }
  },
  hydrateActionOutput: async (messageId, actionId) => {
    const requestKey = `${messageId}::${actionId}`;
    const existingRequest = inflightActionOutputHydration.get(requestKey);
    if (existingRequest) {
      await existingRequest;
      return;
    }

    const request = (async () => {
      const payload = await ipc.getActionOutput(messageId, actionId);
      if (!payload.found) {
        throw new Error("Action output not found.");
      }

      const normalizedChunks: ActionBlock["outputChunks"] = payload.outputChunks.map((chunk) => ({
        stream: normalizeActionOutputStream(chunk.stream),
        content: String(chunk.content ?? ""),
      }));
      const { chunks: trimmedChunks, truncated: trimmedByFrontend } =
        trimActionOutputChunks(normalizedChunks);

      set((state) => {
        const messageIndex = state.messages.findIndex((message) => message.id === messageId);
        if (messageIndex < 0) {
          return state;
        }

        const message = state.messages[messageIndex];
        const blocks = message.blocks;
        if (!blocks || blocks.length === 0) {
          return state;
        }

        const nextBlocks = patchActionBlock(blocks, actionId, (block) => {
          if (
            block.outputDeferred !== true &&
            block.outputDeferredLoaded === true &&
            block.outputChunks.length > 0
          ) {
            return block;
          }

          const details = (block.details ?? {}) as Record<string, unknown>;
          const shouldMarkTruncated =
            (payload.truncated || trimmedByFrontend) &&
            !("outputTruncated" in details && details.outputTruncated === true);
          const nextDetails = shouldMarkTruncated
            ? {
                ...details,
                outputTruncated: true,
              }
            : details;

          return {
            ...block,
            details: nextDetails,
            outputChunks: trimmedChunks,
            outputDeferred: false,
            outputDeferredLoaded: true,
          };
        });

        if (nextBlocks === blocks) {
          return state;
        }

        const nextMessages = [...state.messages];
        nextMessages[messageIndex] = {
          ...message,
          blocks: nextBlocks,
          hasDeferredContent: hasDeferredActionOutput(nextBlocks),
        };

        return {
          ...state,
          messages: nextMessages,
        };
      });
    })();

    inflightActionOutputHydration.set(requestKey, request);
    try {
      await request;
    } finally {
      inflightActionOutputHydration.delete(requestKey);
    }
  },
}));

/**
 * 标记指定线程已经收到会话启动事件，供消息面板关闭后续首轮占位提示。
 * 已经标记的线程直接复用原状态，避免重复创建状态对象。
 */
function markThreadSessionReady(threadId: string | null): void {
  if (!threadId) {
    return;
  }

  useChatStore.setState((state) => {
    if (state.sessionReadyByThread[threadId] === true) {
      return state;
    }
    return {
      ...state,
      sessionReadyByThread: {
        ...state.sessionReadyByThread,
        [threadId]: true,
      },
    };
  });
}
