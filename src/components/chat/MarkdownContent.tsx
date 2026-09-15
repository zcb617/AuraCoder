import type { CSSProperties } from "react";
import { TextBlock } from "./blocks/TextBlock";

/**
 * MarkdownContent 对外兼容参数，保持聊天 Markdown 调用方现有接口不变。
 */
interface MarkdownContentProps {
  /** 待解析的 Markdown 文本内容。 */
  content: string;
  /** Markdown 外层容器的 CSS 类名。 */
  className?: string;
  /** Markdown 外层容器的行内样式。 */
  style?: CSSProperties;
  /** 标记内容是否正在流式接收。 */
  streaming?: boolean;
  /** 是否启用本地文件链接右键菜单。 */
  enableFileContextMenu?: boolean;
}

/**
 * 兼容旧调用方的 Markdown 薄封装，实际文本解析和链接交互由 TextBlock 负责。
 */
export default function MarkdownContent(props: MarkdownContentProps) {
  return <TextBlock {...props} />;
}
