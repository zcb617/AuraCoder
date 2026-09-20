import { create } from "zustand";
import { ipc } from "../lib/ipc";
import { normalizeChatInputHeight } from "../lib/chatInputHeight";

/** 管理聊天输入区高度的后端读取、保存和回读确认状态。 */
interface ChatInputHeightStoreState {
  /** 已持久化的普通聊天 textarea 高度；缺失时保持自然布局。 */
  chatInputHeight: number | null;
  /** 是否已经完成后端聊天输入区高度配置加载。 */
  loaded: boolean;
  /** 从后端读取聊天输入区高度配置。 */
  load: () => Promise<number | null>;
  /** 保存聊天输入区高度，并使用后端回读值更新状态。 */
  setChatInputHeight: (height: number) => Promise<boolean>;
}

/** 聊天输入区高度的全局配置状态仓库。 */
export const useChatInputHeightStore = create<ChatInputHeightStoreState>((set) => ({
  // 缺少配置时不设置 inline height，由 textarea rows=3 提供自然默认布局。
  chatInputHeight: null,
  // 标记后端配置是否已经完成首次读取。
  loaded: false,

  // 启动时读取后端聊天输入区高度配置。
  load: async () => {
    try {
      const saved = await ipc.getChatInputHeight();
      const normalized = normalizeChatInputHeight(saved);
      set({ chatInputHeight: normalized, loaded: true });
      return normalized;
    } catch {
      set({ chatInputHeight: null, loaded: true });
      return null;
    }
  },

  // 保存用户拖动结束后的高度，并使用后端回读值确认持久化结果。
  setChatInputHeight: async (height) => {
    const normalized = normalizeChatInputHeight(height);
    if (normalized === null) {
      return false;
    }
    try {
      const saved = await ipc.setChatInputHeight(normalized);
      const persisted = normalizeChatInputHeight(saved);
      if (persisted === null) {
        return false;
      }
      set({ chatInputHeight: persisted });
      return true;
    } catch {
      return false;
    }
  },
}));
