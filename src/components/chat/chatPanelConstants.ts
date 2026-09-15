import type { PendingFlexibleMessage } from "../../stores/chatComposerStore";
import type {
  ChatAttachment,
  ChatInputReference,
  ChatTextAnnotation,
  PermissionComponentJson,
} from "../../types";

export const MESSAGE_ROW_GAP = 12;
export const EMPTY_CHAT_ATTACHMENTS: ChatAttachment[] = [];
export const EMPTY_CHAT_INPUT_REFERENCES: ChatInputReference[] = [];
export const EMPTY_CHAT_TEXT_ANNOTATIONS: ChatTextAnnotation[] = [];
export const EMPTY_PENDING_FLEXIBLE_MESSAGES: PendingFlexibleMessage[] = [];
export const EMPTY_PERMISSION_COMPONENT: PermissionComponentJson = {
  autonomyPreset: ["automatic"],
  trust: ["automatic"],
  approval: ["automatic"],
  sandbox: ["automatic"],
  network: ["automatic"],
  defaultForNewThreads: [],
};

/** 已接入统一权限组件的 CLI 引擎标识；权限取数据库只依赖该标识，与 CLI 环境加载状态无关。 */
export const PERMISSION_COMPONENT_ENGINE_IDS: ReadonlySet<string> = new Set([
  "codex",
  "opencode",
  "claude",
]);
