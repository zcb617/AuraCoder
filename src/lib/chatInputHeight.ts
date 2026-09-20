/** 聊天输入区高度的像素值边界，具体最小值和最大值由当前 DOM 布局动态提供。 */
export interface ChatInputHeightBounds {
  /** 当前普通聊天 textarea 至少一行文字所需的像素高度。 */
  min: number;
  /** 在保留消息区最小可见空间后允许的最大 textarea 像素高度。 */
  max: number;
}

/** 判断输入值是否为可持久化的聊天输入区高度。 */
export function isChatInputHeight(value?: number | null): value is number {
  return value !== null && value !== undefined && Number.isFinite(value) && Number.isInteger(value) && value > 0;
}

/** 将后端配置或拖动输入归一化为正整数像素高度，缺失或非法值保持未设置。 */
export function normalizeChatInputHeight(value?: number | null): number | null {
  return isChatInputHeight(value) ? value : null;
}

/** 将聊天输入区高度限制在当前窗口布局能够容纳的动态边界内。 */
export function clampChatInputHeight(value: number, bounds: ChatInputHeightBounds): number {
  const min = Math.max(1, Math.round(bounds.min));
  const max = Math.max(min, Math.round(bounds.max));
  return Math.min(Math.max(Math.round(value), min), max);
}
