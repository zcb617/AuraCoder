import { reactive } from "vue";
import { deleteBatchAttachments, uploadAttachmentBatch } from "../attachments";
import type { AttachmentBatchItem, AttachmentBatchState, ChatAttachment, Message, MessageWindow, MessageWindowCursor, RemoteEvent } from "../types";
import { auracoderConnectionManager } from "./auracoder-connection";
import { auracoderDeviceStore } from "./auracoder-device";
// 旧的整段会话快照会同时携带项目列表信息；该快照推送已停用。
// import { projectStore } from "./project";

interface ConversationState {
  messages: Message[];
  // 旧模板仍会读取这两个字段；完整取数后始终为空和 false，不再触发分页。
  // nextCursor: null;
  // 服务端游标；null 表示没有更早历史。
  nextCursor: MessageWindowCursor | null;
  draft: string;
  attachments: ChatAttachment[];
  // 点击发送后冻结的批次；编辑区后续变化不修改该快照。
  pendingBatch: AttachmentBatchState | null;
  loading: boolean;
  loadingOlder: boolean;
  sending: boolean;
  messageRevision: number;
}

interface UnreadConversationState {
  // 未读完成消息数量。
  count: number;
  // 最后一条完成消息摘要。
  summary: string;
  // 最后一条完成消息状态。
  status: Message["status"];
  // 最后一条完成消息标识。
  lastMessageId: string;
  // 最后一条完成消息到达时间。
  updatedAt: string;
}

const stateByConversation = reactive<Record<string, ConversationState>>({});
const unreadMap = reactive<Record<string, UnreadConversationState>>({});
// 手机端页面实例会在返回上一页时卸载；该运行期 Map 跨实例保存会话滚动位置，不写入持久化存储。
export const conversationScrollTopMap = new Map<string, number>();
const activeConversation = reactive({
  // 当前可见会话所属的 AuraCoder。
  auracoderId: "",
  // 当前可见会话 ID。
  threadId: "",
  // 当前会话是否停在底部。
  atBottom: true,
});

function conversationKey(auracoderId: string, threadId: string) {
  return `${auracoderId}:${threadId}`;
}

function createState(): ConversationState {
  return {
    messages: [],
    nextCursor: null,
    draft: "",
    attachments: [],
    pendingBatch: null,
    loading: false,
    loadingOlder: false,
    sending: false,
    messageRevision: 0,
  };
}

/* 重构初版直接用实时快照覆盖本地历史，且首次只请求 50 条；保留原实现以便追溯。
function applyWindow(state: ConversationState, window: MessageWindow) {
  state.messages = window.messages;
  state.nextCursor = window.nextCursor;
}
*/
/*
function mergeWindow(state: ConversationState, window: MessageWindow, replaceCursor: boolean) {
  const hadMessages = state.messages.length > 0;
  const byId = new Map(state.messages.map((message) => [message.id, message]));
  window.messages.forEach((message) => byId.set(message.id, message));
  state.messages = [...byId.values()].sort((left, right) => {
    const createdAt = left.createdAt.localeCompare(right.createdAt);
    return createdAt || left.id.localeCompare(right.id);
  });
  if (replaceCursor || !hadMessages) state.nextCursor = window.nextCursor;
}
*/
/*
type MessageWindowSource = "initial" | "older" | "snapshot";

function mergeWindow(state: ConversationState, window: MessageWindow, source: MessageWindowSource) {
  if (source === "initial") {
    state.messages = window.messages.slice();
    state.nextCursor = window.nextCursor;
    return;
  }

  const incomingIds = new Set(window.messages.map((message) => message.id));
  if (source === "older") {
    // 服务端已按 created_at、rowid 返回稳定顺序；不能再按 UUID 重排。
    state.messages = [...window.messages, ...state.messages.filter((message) => !incomingIds.has(message.id))];
    state.nextCursor = window.nextCursor;
    return;
  }

  const incomingById = new Map(window.messages.map((message) => [message.id, message]));
  const knownIds = new Set(state.messages.map((message) => message.id));
  state.messages = [
    ...state.messages.map((message) => incomingById.get(message.id) || message),
    ...window.messages.filter((message) => !knownIds.has(message.id)),
  ];
  if (state.messages.length === window.messages.length) state.nextCursor = window.nextCursor;
}
*/

type MessageWindowSource = "initial" | "older";

function mergeWindow(state: ConversationState, window: MessageWindow, source: MessageWindowSource) {
  if (source === "initial") {
    state.messages = window.messages.slice();
    state.nextCursor = window.nextCursor;
    return;
  }
  const incomingIds = new Set(window.messages.map((message) => message.id));
  // 服务端已按 created_at、rowid 返回稳定顺序；不能再按 UUID 重排。
  state.messages = [...window.messages, ...state.messages.filter((message) => !incomingIds.has(message.id))];
  state.nextCursor = window.nextCursor;
}

/** 为一批消息并发加载历史图片预览；两路并发限制避免占满手机和隧道内存。 */
async function loadImagePreviews(auracoderId: string, threadId: string, state: ConversationState, messages: Message[]) {
  const previewTargets = messages.flatMap((message) => (message.attachments || [])
    .filter((attachment) => attachment.source === "image" && Number.isInteger(attachment.remoteAttachmentIndex))
    .map((attachment) => ({
      messageId: message.id,
      attachmentId: attachment.id,
      attachmentIndex: attachment.remoteAttachmentIndex as number,
    })));
  for (let start = 0; start < previewTargets.length; start += 2) {
    await Promise.all(previewTargets.slice(start, start + 2).map(async (target) => {
      try {
        const preview = await auracoderConnectionManager.request<{ mimeType?: string; dataBase64?: string } | null>(
          auracoderId,
          "message.attachment.preview",
          {
            thread_id: threadId,
            message_id: target.messageId,
            attachment_index: target.attachmentIndex,
          },
        );
        if (!preview?.mimeType || !preview.dataBase64) return;
        const currentMessage = state.messages.find((message) => message.id === target.messageId);
        const currentAttachment = currentMessage?.attachments?.find((attachment) => attachment.id === target.attachmentId);
        if (!currentAttachment || currentAttachment.remoteAttachmentIndex !== target.attachmentIndex) return;
        currentAttachment.previewUrl = `data:${preview.mimeType};base64,${preview.dataBase64}`;
        state.messageRevision += 1;
      } catch {
        // 单个历史文件已不存在或无法读取时，仍保留其文字和附件名称，不中断整个会话加载。
      }
    }));
  }
}

auracoderConnectionManager.subscribe((auracoderId, event: RemoteEvent) => {
  if (event.event !== "thread.message.completed") return;
  const payload = event.payload as { threadId?: unknown; messageId?: unknown; message?: unknown };
  const message = payload.message as Message | undefined;
  if (!message?.id || message.role !== "assistant" || typeof payload.threadId !== "string" || payload.threadId !== message.threadId) return;
  if (typeof payload.messageId === "string" && payload.messageId !== message.id) return;
  // getState 会创建占位状态，因此目标会话尚未打开时也能保留完成消息。
  const state = conversationStore.getState(auracoderId, message.threadId);
  const index = state.messages.findIndex((item) => item.id === message.id);
  const isNewMessage = index < 0;
  if (index >= 0) state.messages.splice(index, 1, message);
  else state.messages.push(message);
  state.messageRevision += 1;
  const active = activeConversation.auracoderId === auracoderId && activeConversation.threadId === message.threadId;
  if (!active && isNewMessage) {
    const key = conversationKey(auracoderId, message.threadId);
    const existing = unreadMap[key];
    const content = typeof message.content === "string" ? message.content.trim() : "";
    unreadMap[key] = {
      count: (existing?.count || 0) + 1,
      summary: content ? content.slice(0, 120) : "助手已完成一条消息",
      status: message.status,
      lastMessageId: message.id,
      updatedAt: message.createdAt || new Date().toISOString(),
    };
  }
});

/** 发送批次 ID 使用标准 UUID；旧 WebView 没有 randomUUID 时采用 RFC 4122 v4 回退。 */
function createBatchId() {
  const cryptoObject = (globalThis as { crypto?: { randomUUID?: () => string } }).crypto;
  const randomUUID = cryptoObject?.randomUUID?.();
  if (randomUUID && randomUUID.length <= 128) return randomUUID;
  return "xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx".replace(/[xy]/g, (char) => {
    const random = Math.floor(Math.random() * 16);
    const value = char === "x" ? random : (random & 0x3) | 0x8;
    return value.toString(16);
  });
}

function toBatchAttachment(item: ChatAttachment): AttachmentBatchItem {
  if (!item.localPath || !item.source) throw new Error(`附件 ${item.fileName} 缺少手机端来源信息`);
  return {
    ...item,
    localPath: item.localPath,
    source: item.source,
    filePath: item.filePath || "",
    uploading: false,
    failed: false,
    error: undefined,
  };
}

/** 去掉手机端字段后构造 message.send 的远端附件引用。 */
function toRemoteAttachment(item: AttachmentBatchItem) {
  if (!item.attachmentKey) throw new Error(`附件 ${item.fileName} 缺少服务端附件键`);
  return {
    // HTTP 上传成功后返回的附件键。
    attachment_key: item.attachmentKey,
    // 服务端确认的文件名。
    file_name: item.fileName,
    // 服务端确认的文件字节数。
    size_bytes: item.sizeBytes,
    // 服务端确认的 MIME 类型。
    mime_type: item.mimeType,
  };
}

/** 为本地回显构造用户消息，不在上传失败时提前插入。 */
function createLocalMessage(threadId: string, message: string, attachments: ChatAttachment[]): Message {
  return {
    // 手机端临时消息标识。
    id: `mobile-user-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`,
    // 本地消息所属会话。
    threadId,
    // 消息角色。
    role: "user",
    // 发送时冻结正文。
    content: message,
    // 发送成功后本地回显附件，页面按图片、普通附件分区渲染在文字上方。
    attachments: attachments.map((attachment) => ({
      ...attachment,
      uploading: false,
      failed: false,
      error: undefined,
    })),
    // 发送成功后等待桌面最终消息替换。
    status: "completed",
    // 本地创建时间。
    createdAt: new Date().toISOString(),
    // 标记为尚未与桌面历史合并。
    localOnly: true,
  };
}

/** 发送一个冻结批次；失败时仅保留 pendingBatch，不创建或保留用户消息。 */
async function transmitBatch(
  auracoderId: string,
  state: ConversationState,
  batch: AttachmentBatchState,
): Promise<void> {
  await uploadAttachmentBatch(auracoderId, batch.batchId, batch.attachments, () => batch.cancelled === true);
  if (batch.cancelled) throw new Error("发送已取消");
  batch.status = "sending";
  const device = auracoderDeviceStore.getDevice(auracoderId);
  if (!device?.deviceId) throw new Error("设备未完成绑定");
  const result = await auracoderConnectionManager.request<{ message?: Message }>(auracoderId, "message.send", {
    // 协议要求消息和附件属于同一个批次。
    batch_id: batch.batchId,
    // WSS message.send 与 HTTP 上传使用同一个绑定设备 ID。
    device_id: device.deviceId,
    // 发送目标会话。
    thread_id: batch.threadId,
    // 冻结后的正文。
    message: batch.message,
    // 空字符串不应覆盖桌面默认模型。
    model_id: batch.modelId || undefined,
    // 空字符串不应覆盖桌面默认思考强度。
    reasoning_effort: batch.reasoningEffort || undefined,
    // 仅序列化桌面端附件字段，过滤 localPath/source/uploading 等本地字段。
    attachments: batch.attachments.map(toRemoteAttachment),
  });
  const localAttachments = batch.attachments.map((attachment) => ({
    ...attachment,
    uploading: false,
    failed: false,
    error: undefined,
  }));
  const localMessage = createLocalMessage(batch.threadId, batch.message, localAttachments);
  if (result?.message?.id) state.messages.push({ ...result.message, attachments: localAttachments, localOnly: false });
  else state.messages.push(localMessage);
  state.messageRevision += 1;
  // 发送成功后只清理仍对应本批次的编辑区项；发送期间新选附件不会被误删。
  const batchIds = new Set(batch.attachments.map((item) => item.id));
  state.attachments = state.attachments.filter((item) => !batchIds.has(item.id));
  if (state.draft.trim() === batch.message) state.draft = "";
  state.pendingBatch = null;
}

export const conversationStore = {
  stateByConversation,
  unreadMap,
  activeConversation,
  getState(auracoderId: string, threadId: string) {
    const key = conversationKey(auracoderId, threadId);
    if (!stateByConversation[key]) stateByConversation[key] = createState();
    return stateByConversation[key];
  },
  setActive(auracoderId: string, threadId: string, atBottom = true) {
    activeConversation.auracoderId = auracoderId;
    activeConversation.threadId = threadId;
    activeConversation.atBottom = atBottom;
  },
  setActiveViewport(atBottom: boolean) {
    activeConversation.atBottom = atBottom;
  },
  clearActive(auracoderId: string, threadId: string) {
    if (activeConversation.auracoderId !== auracoderId || activeConversation.threadId !== threadId) return;
    activeConversation.auracoderId = "";
    activeConversation.threadId = "";
    activeConversation.atBottom = true;
  },
  getUnread(auracoderId: string, threadId: string) {
    return unreadMap[conversationKey(auracoderId, threadId)] || null;
  },
  getUnreadCount(auracoderId: string, threadId: string) {
    return unreadMap[conversationKey(auracoderId, threadId)]?.count || 0;
  },
  getUnreadTotal(auracoderId: string) {
    return Object.entries(unreadMap)
      .filter(([key]) => key.startsWith(`${auracoderId}:`))
      .reduce((total, [, item]) => total + item.count, 0);
  },
  clearUnreadAfterSync(auracoderId: string, threadId: string) {
    delete unreadMap[conversationKey(auracoderId, threadId)];
  },
  async open(auracoderId: string, threadId: string) {
    const state = this.getState(auracoderId, threadId);
    state.loading = true;
    try {
      // 旧版页面级订阅已停用；保留旧调用说明，设备级连接始终接收事件。
      // await auracoderConnectionManager.request(auracoderId, "thread.subscribe", { thread_id: threadId });
      // message.list 是进入会话后清除未读的全量同步边界。
      const result = await auracoderConnectionManager.request<MessageWindow>(auracoderId, "message.list", { thread_id: threadId, limit: 50 });
      const incoming = Array.isArray(result.messages) ? result.messages : [];
      /*
      旧实现把本地回显混入重新打开时的服务端完整历史。message.send 的响应不返回用户消息，
      因而本地回显的临时 ID 不会出现在 incomingIds 中，同一条用户消息会重复显示；本地图片
      也会错误地掩盖服务端历史没有可访问预览的问题。保留旧逻辑以便追溯，不再执行。
      const incomingIds = new Set(incoming.map((message) => message.id));
      const preserved = state.messages.filter((message) => !incomingIds.has(message.id));
      const localById = new Map(state.messages.map((message) => [message.id, message]));
      const mergedIncoming = incoming.map((message) => {
        const local = localById.get(message.id);
        // 桌面历史暂未返回附件时保留刚发送的本地回显，避免刷新会话后图片和附件区闪失。
        return local?.attachments?.length && !message.attachments?.length
          ? { ...message, attachments: local.attachments }
          : message;
      });
      // 后台已经按 created_at、rowid 返回稳定顺序；手机端必须原样显示，不能用 UUID 或其他字段二次排序。
      // preserved 只可能是本次请求期间新收到、尚未出现在返回结果中的消息，应追加在后台列表之后。
      state.messages = [...mergedIncoming, ...preserved];
      */
      // 重新进入会话时，消息列表只采用本次 message.list 返回的服务端历史；不拼接任何本地回显。
      // state.messages = incoming;
      // 恢复分页加载后，initial 模式用最新一页替换本地历史并保存服务端游标。
      mergeWindow(state, { messages: incoming, nextCursor: result.nextCursor ?? null }, "initial");
      state.messageRevision += 1;
      this.clearUnreadAfterSync(auracoderId, threadId);
      /*
      图片预览两路并发加载已抽取为模块级 loadImagePreviews，供 open 与 loadOlder 复用；保留原内联实现以便追溯。
      // 图片预览由桌面 AuraCoder 按消息附件返回；两路并发限制可避免包含大量历史图片时占满手机和隧道内存。
      const previewTargets = incoming.flatMap((message) => (message.attachments || [])
        .filter((attachment) => attachment.source === "image" && Number.isInteger(attachment.remoteAttachmentIndex))
        .map((attachment) => ({
          messageId: message.id,
          attachmentId: attachment.id,
          attachmentIndex: attachment.remoteAttachmentIndex as number,
        })));
      for (let start = 0; start < previewTargets.length; start += 2) {
        await Promise.all(previewTargets.slice(start, start + 2).map(async (target) => {
          try {
            const preview = await auracoderConnectionManager.request<{ mimeType?: string; dataBase64?: string } | null>(
              auracoderId,
              "message.attachment.preview",
              {
                thread_id: threadId,
                message_id: target.messageId,
                attachment_index: target.attachmentIndex,
              },
            );
            if (!preview?.mimeType || !preview.dataBase64) return;
            const currentMessage = state.messages.find((message) => message.id === target.messageId);
            const currentAttachment = currentMessage?.attachments?.find((attachment) => attachment.id === target.attachmentId);
            if (!currentAttachment || currentAttachment.remoteAttachmentIndex !== target.attachmentIndex) return;
            currentAttachment.previewUrl = `data:${preview.mimeType};base64,${preview.dataBase64}`;
            state.messageRevision += 1;
          } catch {
            // 单个历史文件已不存在或无法读取时，仍保留其文字和附件名称，不中断整个会话加载。
          }
        }));
      }
      */
      await loadImagePreviews(auracoderId, threadId, state, incoming);
      /*
      const loadedCursors = new Set<string>();
      while (state.nextCursor) {
        const cursorKey = `${state.nextCursor.createdAt}:${state.nextCursor.rowId ?? ""}:${state.nextCursor.id}`;
        if (loadedCursors.has(cursorKey)) throw new Error("会话历史分页游标重复");
        loadedCursors.add(cursorKey);
        const olderWindow = await auracoderConnectionManager.request<MessageWindow>(auracoderId, "message.list", {
          thread_id: threadId,
          cursor: state.nextCursor,
          limit: 200,
        });
        mergeWindow(state, olderWindow, "older");
      }
      */
      return state;
    } catch (error) {
      throw error;
    } finally {
      state.loading = false;
    }
  },
  /*
  async loadOlder(auracoderId: string, threadId: string) {
    const state = this.getState(auracoderId, threadId);
    if (!state.nextCursor || state.loadingOlder) return;
    state.loadingOlder = true;
    try {
      const window = await auracoderConnectionManager.request<MessageWindow>(auracoderId, "message.list", {
        thread_id: threadId,
        cursor: state.nextCursor,
        limit: 50,
      });
      mergeWindow(state, window, "older");
    } finally {
      state.loadingOlder = false;
    }
  },
  */
  /*
  已恢复游标分页加载；保留旧存根以便追溯。
  async loadOlder(_auracoderId: string, _threadId: string) {
    // 会话已在首次打开时完整取得；保留旧模板入口但不再发起请求。
  },
  */
  async loadOlder(auracoderId: string, threadId: string) {
    const state = this.getState(auracoderId, threadId);
    if (!state.nextCursor || state.loadingOlder) return;
    state.loadingOlder = true;
    try {
      const window = await auracoderConnectionManager.request<MessageWindow>(auracoderId, "message.list", {
        thread_id: threadId,
        cursor: state.nextCursor,
        limit: 50,
      });
      const incoming = Array.isArray(window.messages) ? window.messages : [];
      mergeWindow(state, { messages: incoming, nextCursor: window.nextCursor ?? null }, "older");
      state.messageRevision += 1;
      await loadImagePreviews(auracoderId, threadId, state, incoming);
    } finally {
      state.loadingOlder = false;
    }
  },
  async send(auracoderId: string, threadId: string, modelId: string, reasoningEffort: string) {
    const state = this.getState(auracoderId, threadId);
    if (state.sending || state.pendingBatch) return;
    if (!auracoderDeviceStore.getDevice(auracoderId)?.deviceId) throw new Error("设备未完成绑定");
    const message = state.draft.trim();
    if (!message && state.attachments.length === 0) return;
    const batch: AttachmentBatchState = {
      // 每次点击发送生成唯一批次 UUID。
      batchId: createBatchId(),
      // 冻结目标会话。
      threadId,
      // 冻结正文快照。
      message,
      // 冻结模型选择。
      modelId,
      // 冻结思考强度选择。
      reasoningEffort,
      // 深复制附件，发送后的编辑不会改变批次。
      attachments: state.attachments.map(toBatchAttachment),
      // 先进入上传阶段。
      status: "uploading",
    };
    state.pendingBatch = batch;
    state.sending = true;
    try {
      await transmitBatch(auracoderId, state, batch);
    } catch (error) {
      batch.status = "failed";
      batch.error = error instanceof Error ? error.message : String(error);
      throw error;
    } finally {
      state.sending = false;
    }
  },
  async retryBatch(auracoderId: string, threadId: string) {
    const state = this.getState(auracoderId, threadId);
    const batch = state.pendingBatch;
    if (!batch || batch.status !== "failed" || state.sending) return;
    batch.cancelled = false;
    batch.error = undefined;
    batch.status = "uploading";
    state.sending = true;
    try {
      await transmitBatch(auracoderId, state, batch);
    } catch (error) {
      batch.status = "failed";
      batch.error = error instanceof Error ? error.message : String(error);
      throw error;
    } finally {
      state.sending = false;
    }
  },
  async abortBatch(auracoderId: string, threadId: string) {
    const state = this.getState(auracoderId, threadId);
    const batch = state.pendingBatch;
    if (!batch) return;
    batch.cancelled = true;
    try {
      await deleteBatchAttachments(auracoderId, batch.attachments);
    } finally {
      state.pendingBatch = null;
      state.sending = false;
    }
  },
  async close(auracoderId: string, threadId: string) {
    this.clearActive(auracoderId, threadId);
    // 旧版 thread.unsubscribe 已停用；设备级接收通道不随页面离开而取消注册。
    // await auracoderConnectionManager.request(auracoderId, "thread.unsubscribe", { thread_id: threadId });
  },
  clear(auracoderId: string) {
    Object.keys(stateByConversation)
      .filter((key) => key.startsWith(`${auracoderId}:`))
      .forEach((key) => delete stateByConversation[key]);
    Object.keys(unreadMap)
      .filter((key) => key.startsWith(`${auracoderId}:`))
      .forEach((key) => delete unreadMap[key]);
    // 清理 AuraCoder 会话缓存时同步丢弃该 AuraCoder 的滚动位置，避免重新配对后恢复旧会话位置。
    [...conversationScrollTopMap.keys()]
      .filter((key) => key.startsWith(`${auracoderId}:`))
      .forEach((key) => conversationScrollTopMap.delete(key));
    if (activeConversation.auracoderId === auracoderId) {
      activeConversation.auracoderId = "";
      activeConversation.threadId = "";
      activeConversation.atBottom = true;
    }
  },
};
