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
/** 未创建线程时权限组件显示和发送使用的统一初始对象。 */
export const EMPTY_PERMISSION_COMPONENT: PermissionComponentJson = {
  /** 初始权限预设采用统一自动模式。 */
  autonomyPreset: ["automatic"],
  /** 初始信任设置采用自动继承。 */
  trust: ["automatic"],
  /** 初始审批设置采用自动继承。 */
  approval: ["automatic"],
  /** 初始沙箱设置采用自动继承。 */
  sandbox: ["automatic"],
  /** 初始网络设置采用自动继承。 */
  network: ["automatic"],
  /** 初始状态不覆盖新会话默认值。 */
  defaultForNewThreads: [],
};
