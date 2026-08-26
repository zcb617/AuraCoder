import { create } from "zustand";
import {
  DEFAULT_CHAT_INPUT_MODE,
  DEFAULT_CHAT_INPUT_SEND_SHORTCUT,
  isChatInputMode,
  isChatInputSendShortcut,
  type ChatInputMode,
  type ChatInputSendShortcut,
} from "../lib/chatInputSettings";
import {
  DEFAULT_MESSAGE_SEND_MODE,
  isMessageSendMode,
  type MessageSendMode,
} from "../lib/chatInputSettings";
import type { ComposerRuntimeSnapshot } from "../lib/newThreadRuntime";
import type {
  ChatAttachment,
  ChatInputReference,
  ChatTextAnnotation,
} from "../types";
import {
  DEFAULT_LINK_OPEN_GESTURE,
  isLinkOpenGesture,
  type LinkOpenGesture,
} from "../lib/linkOpenSettings";

const CHAT_INPUT_SEND_SHORTCUT_STORAGE_KEY = "auracoder:chatInputSendShortcut";

const CHAT_INPUT_MODE_STORAGE_KEY = "auracoder:chatInputMode";

const MESSAGE_SEND_MODE_STORAGE_KEY = "auracoder:messageSendMode";

const THREAD_MESSAGE_SEND_MODES_STORAGE_KEY = "auracoder:threadMessageSendModes";

const LINK_OPEN_GESTURE_STORAGE_KEY = "auracoder:linkOpenGesture";

export interface PendingFlexibleMessage {
  /** 仅用于发送前定位 AuraCoder 会话，不持久化模型隐藏指令。 */
  auracoderThreadId: string | null;
  id: string;
  text: string;
  attachments: ChatAttachment[];
  references: ChatInputReference[];
}

/**
 * 计算聊天输入内容的会话范围键。
 * 正式会话使用 thread:<threadId>，尚未创建正式会话时使用 new:<workspaceId>。
 */
export function getChatComposerSessionKey(
  workspaceId: string,
  threadId: string | null | undefined,
): string {
  return threadId ? `thread:${threadId}` : `new:${workspaceId}`;
}

function readChatInputSendShortcut(): ChatInputSendShortcut {
  try {
    const stored = localStorage.getItem(CHAT_INPUT_SEND_SHORTCUT_STORAGE_KEY);
    return stored && isChatInputSendShortcut(stored)
      ? stored
      : DEFAULT_CHAT_INPUT_SEND_SHORTCUT;
  } catch {
    return DEFAULT_CHAT_INPUT_SEND_SHORTCUT;
  }
}

function persistChatInputSendShortcut(sendShortcut: ChatInputSendShortcut): void {
  try {
    localStorage.setItem(CHAT_INPUT_SEND_SHORTCUT_STORAGE_KEY, sendShortcut);
  } catch {
    // The shortcut still applies to this session if storage is unavailable.
  }
}

function readChatInputMode(): ChatInputMode {
  try {
    const stored = localStorage.getItem(CHAT_INPUT_MODE_STORAGE_KEY);
    return stored && isChatInputMode(stored)
      ? stored
      : DEFAULT_CHAT_INPUT_MODE;
  } catch {
    return DEFAULT_CHAT_INPUT_MODE;
  }
}

function persistChatInputMode(chatInputMode: ChatInputMode): void {
  try {
    localStorage.setItem(CHAT_INPUT_MODE_STORAGE_KEY, chatInputMode);
  } catch {
    // The mode still applies to this session if storage is unavailable.
  }
}

function readMessageSendMode(): MessageSendMode {
  try {
    const stored = localStorage.getItem(MESSAGE_SEND_MODE_STORAGE_KEY);
    return stored && isMessageSendMode(stored) ? stored : DEFAULT_MESSAGE_SEND_MODE;
  } catch {
    return DEFAULT_MESSAGE_SEND_MODE;
  }
}

function persistMessageSendMode(messageSendMode: MessageSendMode): void {
  try {
    localStorage.setItem(MESSAGE_SEND_MODE_STORAGE_KEY, messageSendMode);
  } catch {
    // The mode still applies to this session if storage is unavailable.
  }
}

function readThreadMessageSendModes(): Record<string, MessageSendMode> {
  try {
    const stored = localStorage.getItem(THREAD_MESSAGE_SEND_MODES_STORAGE_KEY);
    if (!stored) {
      return {};
    }

    const parsed: unknown = JSON.parse(stored);
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
      return {};
    }

    const messageSendModes: Record<string, MessageSendMode> = {};
    for (const [threadId, messageSendMode] of Object.entries(parsed)) {
      if (isMessageSendMode(messageSendMode)) {
        messageSendModes[threadId] = messageSendMode;
      }
    }
    return messageSendModes;
  } catch {
    return {};
  }
}

function persistThreadMessageSendModes(
  messageSendModesByThread: Record<string, MessageSendMode>,
): void {
  try {
    localStorage.setItem(
      THREAD_MESSAGE_SEND_MODES_STORAGE_KEY,
      JSON.stringify(messageSendModesByThread),
    );
  } catch {
    // The thread override still applies to this session if storage is unavailable.
  }
}

function readLinkOpenGesture(): LinkOpenGesture {
  try {
    const stored = localStorage.getItem(LINK_OPEN_GESTURE_STORAGE_KEY);
    return stored && isLinkOpenGesture(stored)
      ? stored
      : DEFAULT_LINK_OPEN_GESTURE;
  } catch {
    return DEFAULT_LINK_OPEN_GESTURE;
  }
}

function persistLinkOpenGesture(linkOpenGesture: LinkOpenGesture): void {
  try {
    localStorage.setItem(LINK_OPEN_GESTURE_STORAGE_KEY, linkOpenGesture);
  } catch {
    // The setting still applies to this session if storage is unavailable.
  }
}

interface ChatComposerState {
  runtimeByWorkspace: Record<string, ComposerRuntimeSnapshot>;
  /** 当前会话的未发送文字草稿。 */
  draftBySession: Record<string, string>;
  /** 当前会话的未发送附件。 */
  attachmentsBySession: Record<string, ChatAttachment[]>;
  /** 当前会话的未发送文本标注。 */
  textAnnotationsBySession: Record<string, ChatTextAnnotation[]>;
  /** 当前会话的未发送引用。 */
  referencesBySession: Record<string, ChatInputReference[]>;
  pendingFlexibleMessagesBySession: Record<string, PendingFlexibleMessage[]>;
  pendingMessageSendModeByWorkspace: Record<string, MessageSendMode>;
  sendShortcut: ChatInputSendShortcut;
  chatInputMode: ChatInputMode;
  messageSendMode: MessageSendMode;
  messageSendModeByThread: Record<string, MessageSendMode>;
  linkOpenGesture: LinkOpenGesture;
  setWorkspaceRuntime: (
    workspaceId: string,
    runtime: ComposerRuntimeSnapshot,
  ) => void;
  clearWorkspaceRuntime: (workspaceId: string) => void;
  /** 写入或清除指定会话范围的未发送文字。 */
  setSessionDraft: (sessionKey: string, draft: string) => void;
  /** 清除指定会话范围的未发送文字。 */
  clearSessionDraft: (sessionKey: string) => void;
  /** 写入或清除指定会话范围的未发送附件。 */
  setSessionAttachments: (sessionKey: string, attachments: ChatAttachment[]) => void;
  /** 清除指定会话范围的未发送附件。 */
  clearSessionAttachments: (sessionKey: string) => void;
  /** 写入或清除指定会话范围的未发送文本标注。 */
  setSessionTextAnnotations: (
    sessionKey: string,
    annotations: ChatTextAnnotation[],
  ) => void;
  /** 清除指定会话范围的未发送文本标注。 */
  clearSessionTextAnnotations: (sessionKey: string) => void;
  /** 写入或清除指定会话范围的未发送引用。 */
  setSessionReferences: (
    sessionKey: string,
    references: ChatInputReference[],
  ) => void;
  /** 清除指定会话范围的未发送引用。 */
  clearSessionReferences: (sessionKey: string) => void;
  addPendingFlexibleMessage: (
    sessionKey: string,
    message: PendingFlexibleMessage,
  ) => void;
  removePendingFlexibleMessage: (sessionKey: string, messageId: string) => void;
  clearPendingFlexibleMessages: (sessionKey: string) => void;
  setPendingMessageSendMode: (workspaceId: string, messageSendMode: MessageSendMode) => void;
  clearPendingMessageSendMode: (workspaceId: string) => void;
  setSendShortcut: (sendShortcut: ChatInputSendShortcut) => void;
  setChatInputMode: (chatInputMode: ChatInputMode) => void;
  setMessageSendMode: (messageSendMode: MessageSendMode) => void;
  setThreadMessageSendMode: (threadId: string, messageSendMode: MessageSendMode) => void;
  setLinkOpenGesture: (linkOpenGesture: LinkOpenGesture) => void;
}

/*
 * 旧项目级输入实现已由会话级实现替代，完整旧代码保留在此注释中，不参与编译：
 *
 * interface ChatComposerState {
 *   draftByWorkspace: Record<string, string>;
 *   attachmentsByWorkspace: Record<string, ChatAttachment[]>;
 *   textAnnotationsByWorkspace: Record<string, ChatTextAnnotation[]>;
 *   referencesByWorkspace: Record<string, ChatInputReference[]>;
 *   setWorkspaceDraft: (workspaceId: string, draft: string) => void;
 *   clearWorkspaceDraft: (workspaceId: string) => void;
 *   setWorkspaceAttachments: (workspaceId: string, attachments: ChatAttachment[]) => void;
 *   clearWorkspaceAttachments: (workspaceId: string) => void;
 *   setWorkspaceTextAnnotations: (
 *     workspaceId: string,
 *     annotations: ChatTextAnnotation[],
 *   ) => void;
 *   clearWorkspaceTextAnnotations: (workspaceId: string) => void;
 *   setWorkspaceReferences: (
 *     workspaceId: string,
 *     references: ChatInputReference[],
 *   ) => void;
 *   clearWorkspaceReferences: (workspaceId: string) => void;
 * }
 *
 * draftByWorkspace: {},
 * attachmentsByWorkspace: {},
 * textAnnotationsByWorkspace: {},
 * referencesByWorkspace: {},
 * setWorkspaceDraft: (workspaceId, draft) =>
 *   set((state) => {
 *     if (!draft) {
 *       const { [workspaceId]: _removed, ...rest } = state.draftByWorkspace;
 *       return { draftByWorkspace: rest };
 *     }
 *     return {
 *       draftByWorkspace: {
 *         ...state.draftByWorkspace,
 *         [workspaceId]: draft,
 *       },
 *     };
 *   }),
 * clearWorkspaceDraft: (workspaceId) =>
 *   set((state) => {
 *     const { [workspaceId]: _removed, ...rest } = state.draftByWorkspace;
 *     return { draftByWorkspace: rest };
 *   }),
 * setWorkspaceAttachments: (workspaceId, attachments) =>
 *   set((state) => {
 *     if (attachments.length === 0) {
 *       const { [workspaceId]: _removed, ...rest } = state.attachmentsByWorkspace;
 *       return { attachmentsByWorkspace: rest };
 *     }
 *     return {
 *       attachmentsByWorkspace: {
 *         ...state.attachmentsByWorkspace,
 *         [workspaceId]: [...attachments],
 *       },
 *     };
 *   }),
 * clearWorkspaceAttachments: (workspaceId) =>
 *   set((state) => {
 *     const { [workspaceId]: _removed, ...rest } = state.attachmentsByWorkspace;
 *     return { attachmentsByWorkspace: rest };
 *   }),
 * setWorkspaceTextAnnotations: (workspaceId, annotations) =>
 *   set((state) => {
 *     if (annotations.length === 0) {
 *       const { [workspaceId]: _removed, ...rest } = state.textAnnotationsByWorkspace;
 *       return { textAnnotationsByWorkspace: rest };
 *     }
 *     return {
 *       textAnnotationsByWorkspace: {
 *         ...state.textAnnotationsByWorkspace,
 *         [workspaceId]: [...annotations],
 *       },
 *     };
 *   }),
 * clearWorkspaceTextAnnotations: (workspaceId) =>
 *   set((state) => {
 *     const { [workspaceId]: _removed, ...rest } = state.textAnnotationsByWorkspace;
 *     return { textAnnotationsByWorkspace: rest };
 *   }),
 * setWorkspaceReferences: (workspaceId, references) =>
 *   set((state) => {
 *     if (references.length === 0) {
 *       const { [workspaceId]: _removed, ...rest } = state.referencesByWorkspace;
 *       return { referencesByWorkspace: rest };
 *     }
 *     return {
 *       referencesByWorkspace: {
 *         ...state.referencesByWorkspace,
 *         [workspaceId]: [...references],
 *       },
 *     };
 *   }),
 * clearWorkspaceReferences: (workspaceId) =>
 *   set((state) => {
 *     const { [workspaceId]: _removed, ...rest } = state.referencesByWorkspace;
 *     return { referencesByWorkspace: rest };
 *   }),
 *
 * 新实现统一使用 getChatComposerSessionKey(workspaceId, threadId) 生成 sessionKey，
 * 从而保证同一工作区的不同会话互不覆盖。
 */

export const useChatComposerStore = create<ChatComposerState>((set) => ({
  runtimeByWorkspace: {},
  draftBySession: {},
  attachmentsBySession: {},
  textAnnotationsBySession: {},
  referencesBySession: {},
  pendingFlexibleMessagesBySession: {},
  pendingMessageSendModeByWorkspace: {},
  sendShortcut: readChatInputSendShortcut(),
  chatInputMode: readChatInputMode(),
  messageSendMode: readMessageSendMode(),
  messageSendModeByThread: readThreadMessageSendModes(),
  linkOpenGesture: readLinkOpenGesture(),
  setWorkspaceRuntime: (workspaceId, runtime) =>
    set((state) => ({
      runtimeByWorkspace: {
        ...state.runtimeByWorkspace,
        [workspaceId]: runtime,
      },
    })),
  clearWorkspaceRuntime: (workspaceId) =>
    set((state) => {
      const { [workspaceId]: _removed, ...rest } = state.runtimeByWorkspace;
      return {
        runtimeByWorkspace: rest,
      };
    }),
  setSessionDraft: (sessionKey, draft) =>
    set((state) => {
      if (!draft) {
        const { [sessionKey]: _removed, ...rest } = state.draftBySession;
        return { draftBySession: rest };
      }
      return {
        draftBySession: {
          ...state.draftBySession,
          [sessionKey]: draft,
        },
      };
    }),
  clearSessionDraft: (sessionKey) =>
    set((state) => {
      const { [sessionKey]: _removed, ...rest } = state.draftBySession;
      return { draftBySession: rest };
    }),
  setSessionAttachments: (sessionKey, attachments) =>
    set((state) => {
      if (attachments.length === 0) {
        const { [sessionKey]: _removed, ...rest } = state.attachmentsBySession;
        return { attachmentsBySession: rest };
      }
      return {
        attachmentsBySession: {
          ...state.attachmentsBySession,
          [sessionKey]: [...attachments],
        },
      };
    }),
  clearSessionAttachments: (sessionKey) =>
    set((state) => {
      const { [sessionKey]: _removed, ...rest } = state.attachmentsBySession;
      return { attachmentsBySession: rest };
    }),
  setSessionTextAnnotations: (sessionKey, annotations) =>
    set((state) => {
      if (annotations.length === 0) {
        const { [sessionKey]: _removed, ...rest } = state.textAnnotationsBySession;
        return { textAnnotationsBySession: rest };
      }
      return {
        textAnnotationsBySession: {
          ...state.textAnnotationsBySession,
          [sessionKey]: [...annotations],
        },
      };
    }),
  clearSessionTextAnnotations: (sessionKey) =>
    set((state) => {
      const { [sessionKey]: _removed, ...rest } = state.textAnnotationsBySession;
      return { textAnnotationsBySession: rest };
    }),
  setSessionReferences: (sessionKey, references) =>
    set((state) => {
      if (references.length === 0) {
        const { [sessionKey]: _removed, ...rest } = state.referencesBySession;
        return { referencesBySession: rest };
      }
      return {
        referencesBySession: {
          ...state.referencesBySession,
          [sessionKey]: [...references],
        },
      };
    }),
  clearSessionReferences: (sessionKey) =>
    set((state) => {
      const { [sessionKey]: _removed, ...rest } = state.referencesBySession;
      return { referencesBySession: rest };
    }),
  addPendingFlexibleMessage: (sessionKey, message) =>
    set((state) => ({
      pendingFlexibleMessagesBySession: {
        ...state.pendingFlexibleMessagesBySession,
        [sessionKey]: [
          ...(state.pendingFlexibleMessagesBySession[sessionKey] ?? []),
          message,
        ],
      },
    })),
  removePendingFlexibleMessage: (sessionKey, messageId) =>
    set((state) => {
      const messages = state.pendingFlexibleMessagesBySession[sessionKey];
      if (!messages) {
        return state;
      }

      const remainingMessages = messages.filter((message) => message.id !== messageId);
      if (remainingMessages.length === messages.length) {
        return state;
      }
      if (remainingMessages.length === 0) {
        const { [sessionKey]: _removed, ...rest } = state.pendingFlexibleMessagesBySession;
        return { pendingFlexibleMessagesBySession: rest };
      }

      return {
        pendingFlexibleMessagesBySession: {
          ...state.pendingFlexibleMessagesBySession,
          [sessionKey]: remainingMessages,
        },
      };
    }),
  clearPendingFlexibleMessages: (sessionKey) =>
    set((state) => {
      const { [sessionKey]: _removed, ...rest } = state.pendingFlexibleMessagesBySession;
      return { pendingFlexibleMessagesBySession: rest };
    }),
  setPendingMessageSendMode: (workspaceId, messageSendMode) =>
    set((state) => ({
      pendingMessageSendModeByWorkspace: {
        ...state.pendingMessageSendModeByWorkspace,
        [workspaceId]: messageSendMode,
      },
    })),
  clearPendingMessageSendMode: (workspaceId) =>
    set((state) => {
      const { [workspaceId]: _removed, ...rest } = state.pendingMessageSendModeByWorkspace;
      return { pendingMessageSendModeByWorkspace: rest };
    }),
  setSendShortcut: (sendShortcut) => {
    persistChatInputSendShortcut(sendShortcut);
    set({ sendShortcut });
  },
  setChatInputMode: (chatInputMode) => {
    persistChatInputMode(chatInputMode);
    set({ chatInputMode });
  },
  setMessageSendMode: (messageSendMode) => {
    persistMessageSendMode(messageSendMode);
    set({ messageSendMode });
  },
  setThreadMessageSendMode: (threadId, messageSendMode) =>
    set((state) => {
      const messageSendModeByThread = {
        ...state.messageSendModeByThread,
        [threadId]: messageSendMode,
      };
      persistThreadMessageSendModes(messageSendModeByThread);
      return { messageSendModeByThread };
    }),
  setLinkOpenGesture: (linkOpenGesture) => {
    persistLinkOpenGesture(linkOpenGesture);
    set({ linkOpenGesture });
  },
}));
