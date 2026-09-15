import { useRef, type CSSProperties, type MouseEvent as ReactMouseEvent } from "react";
import {
  classifyLinkTarget,
  getWorkspacePaneLeafIdFromEventTarget,
  navigateLinkTarget,
} from "../../../lib/fileLinkNavigation";
import { ipc } from "../../../lib/ipc";
import {
  createStreamingMarkdownAppender,
  renderMarkdownToHtml,
} from "../../../workers/markdownParserCore";
import { useChatFileContextMenu } from "../useChatFileContextMenu";

/**
 * 聊天文本块的渲染参数，负责把 Markdown 文本转换为静态 HTML 并接管文件链接交互。
 */
export interface TextBlockProps {
  /** 待展示的聊天 Markdown 文本内容。 */
  content: string;
  /** 标记文本是否仍在流式接收，用于暂缓未闭合代码围栏。 */
  streaming?: boolean;
  /** 文本块外层容器的 CSS 类名。 */
  className?: string;
  /** 文本块外层容器的行内样式。 */
  style?: CSSProperties;
  /** 是否启用本地文件链接右键菜单。 */
  enableFileContextMenu?: boolean;
}

/**
 * 从事件目标中取得 Markdown 链接的本地文件引用或安全 href。
 */
function getMarkdownLinkTarget(eventTarget: EventTarget | null): string | null {
  const element = eventTarget instanceof Element
    ? eventTarget
    : eventTarget instanceof Node
      ? eventTarget.parentElement
      : null;
  if (!element) {
    return null;
  }

  const anchor = element.closest("a");
  if (!(anchor instanceof HTMLAnchorElement)) {
    return null;
  }

  return anchor.dataset.localFileReference ?? anchor.getAttribute("href");
}

/**
 * 处理文本块中的链接左键动作，按链接类型导航到本地文件或外部地址。
 */
function handleMarkdownLinkClick(event: ReactMouseEvent<HTMLDivElement>): void {
  if (event.defaultPrevented || event.button !== 0) {
    return;
  }

  const rawTarget = getMarkdownLinkTarget(event.target);
  if (!rawTarget) {
    return;
  }

  const targetKind = classifyLinkTarget(rawTarget);
  if (targetKind === "other") {
    return;
  }

  event.preventDefault();
  if (targetKind === "local") {
    event.stopPropagation();
  }
  void navigateLinkTarget(rawTarget, {
    shiftKey: event.shiftKey,
    sourceLeafId: getWorkspacePaneLeafIdFromEventTarget(event.currentTarget),
  });
}

/**
 * 处理文本块中的本地文件链接右键动作，打开聊天文件上下文菜单。
 */
function handleMarkdownLinkContextMenu(
  event: ReactMouseEvent<HTMLDivElement>,
  openLocalFileContextMenu: ReturnType<typeof useChatFileContextMenu>["openLocalFileContextMenu"],
): void {
  const rawTarget = getMarkdownLinkTarget(event.target);
  if (!rawTarget) {
    return;
  }

  openLocalFileContextMenu(
    event,
    rawTarget,
    getWorkspacePaneLeafIdFromEventTarget(event.currentTarget),
  );
}

/**
 * 将聊天文本转换为静态 HTML，保留链接事件委托和本地文件上下文菜单能力。
 */
export function TextBlock({
  content,
  streaming = false,
  className,
  style,
  enableFileContextMenu = false,
}: TextBlockProps) {
  const { openLocalFileContextMenu, contextMenu } = useChatFileContextMenu();
  const appenderRef = useRef<ReturnType<typeof createStreamingMarkdownAppender> | null>(null);
  const previousContentRef = useRef<string | null>(null);

  if (appenderRef.current === null) {
    appenderRef.current = createStreamingMarkdownAppender();
  }

  let html: string;
  if (streaming) {
    const previousContent = previousContentRef.current;
    if (previousContent !== null && !content.startsWith(previousContent)) {
      appenderRef.current.reset();
    }
    html = appenderRef.current.push(content).html;
    // === 临时调试：抓流式中途每帧累积文本与解析结果，落进 auracoder.log。定位"中途竖排、结束横排"。调试验证完后整段注释。 ===
    void ipc.appendStreamRenderDebug(
      JSON.stringify({
        len: content.length,
        contentTail: content.slice(-100),
        htmlTail: html.slice(-200),
        pCount: (html.match(/<p[ >]/g) ?? []).length,
      }),
    ).catch(() => undefined);
    previousContentRef.current = content;
  } else {
    appenderRef.current.reset();
    html = renderMarkdownToHtml(content);
    previousContentRef.current = null;
  }

  return (
    <>
      <div
        className={className}
        style={style}
        onClickCapture={handleMarkdownLinkClick}
        onContextMenu={enableFileContextMenu
          ? (event) => handleMarkdownLinkContextMenu(event, openLocalFileContextMenu)
          : undefined}
        dangerouslySetInnerHTML={{ __html: html }}
      />
      {contextMenu}
    </>
  );
}
