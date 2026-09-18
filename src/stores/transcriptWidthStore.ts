import { create } from "zustand";
import { ipc } from "../lib/ipc";
import {
  applyTranscriptWidth,
  normalizeTranscriptWidth,
  type TranscriptWidth,
} from "../lib/transcriptWidth";

/** 管理聊天内容宽度加载、持久化和即时样式应用状态。 */
interface TranscriptWidthStoreState {
  /** 当前聊天消息文字显示区域的宽度档位。 */
  transcriptWidth: TranscriptWidth;
  /** 是否已经完成后端聊天内容宽度配置加载。 */
  loaded: boolean;
  /** 从后端读取并应用聊天内容宽度配置。 */
  load: () => Promise<TranscriptWidth>;
  /** 保存聊天内容宽度配置并立即应用到聊天区域。 */
  setTranscriptWidth: (transcriptWidth: TranscriptWidth) => Promise<boolean>;
}

/** 聊天内容宽度配置的全局状态仓库。 */
export const useTranscriptWidthStore = create<TranscriptWidthStoreState>((set) => ({
  // 未加载后端配置前使用中等宽度，保证首次渲染符合默认业务值。
  transcriptWidth: "medium",
  // 标记后端配置是否已经完成首次读取。
  loaded: false,

  // 启动时读取后端配置并同步聊天消息区域的 CSS 宽度。
  load: async () => {
    try {
      const saved = await ipc.getTranscriptWidth();
      const normalized = normalizeTranscriptWidth(saved);
      set({ transcriptWidth: normalized, loaded: true });
      applyTranscriptWidth(normalized);
      return normalized;
    } catch {
      const fallback = normalizeTranscriptWidth();
      set({ transcriptWidth: fallback, loaded: true });
      applyTranscriptWidth(fallback);
      return fallback;
    }
  },

  // 保存用户选择并在成功后立即同步聊天消息区域的 CSS 宽度。
  setTranscriptWidth: async (transcriptWidth) => {
    try {
      const saved = await ipc.setTranscriptWidth(transcriptWidth);
      const normalized = normalizeTranscriptWidth(saved);
      set({ transcriptWidth: normalized });
      applyTranscriptWidth(normalized);
      return true;
    } catch {
      return false;
    }
  },
}));
