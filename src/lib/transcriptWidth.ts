/** 聊天内容宽度设置允许的三档业务值。 */
export const TRANSCRIPT_WIDTH_PREFERENCES = ["narrow", "medium", "wide"] as const;

/** 聊天内容宽度设置的类型定义。 */
export type TranscriptWidth = (typeof TRANSCRIPT_WIDTH_PREFERENCES)[number];

/** 判断持久化或外部输入是否为合法的聊天内容宽度。 */
export function isTranscriptWidth(value?: string | null): value is TranscriptWidth {
  return TRANSCRIPT_WIDTH_PREFERENCES.includes(value as TranscriptWidth);
}

/** 将聊天内容宽度输入归一化为合法值，缺失或非法值使用中等宽度。 */
export function normalizeTranscriptWidth(value?: string | null): TranscriptWidth {
  return isTranscriptWidth(value) ? value : "medium";
}

/** 聊天内容宽度档位对应的消息内容最大宽度。 */
const TRANSCRIPT_WIDTH_CSS: Record<TranscriptWidth, string> = {
  /** 窄档限制为聊天面板宽度的百分之六十。 */
  narrow: "60%",
  /** 中档限制为聊天面板宽度的百分之八十。 */
  medium: "80%",
  /** 宽档铺满聊天面板。 */
  wide: "100%",
};

/** 将聊天内容宽度即时应用到聊天消息区域的 CSS 自定义属性。 */
export function applyTranscriptWidth(width: TranscriptWidth) {
  if (typeof document !== "undefined") {
    document.documentElement.style.setProperty("--chat-content-max-width", TRANSCRIPT_WIDTH_CSS[width]);
  }
}
