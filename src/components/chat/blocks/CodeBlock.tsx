import {
  useCallback,
  useMemo,
  useState,
  type MouseEvent as ReactMouseEvent,
  type ReactNode,
} from "react";
import { Check, Copy, FileCode2 } from "lucide-react";
import {
  extractTextLinkMatches,
  getWorkspacePaneLeafIdFromEventTarget,
  navigateLinkTarget,
} from "../../../lib/fileLinkNavigation";
import { shouldOpenLink } from "../../../lib/linkOpenSettings";
import { useChatComposerStore } from "../../../stores/chatComposerStore";
import { useChatFileContextMenu } from "../useChatFileContextMenu";

/**
 * 聊天代码块的渲染参数，负责展示代码、文件名链接和复制入口。
 */
export interface CodeBlockProps {
  /** 待展示及复制的代码文本。 */
  content: string;
  /** 代码语言标识，用于保留原有语言 class。 */
  language?: string;
  /** 代码块头部显示的文件名或本地文件路径。 */
  filename?: string;
}

/**
 * 处理代码块文件名本地链接的左键导航，保持聊天链接打开策略。
 */
function handlePlainTextLinkClick(
  event: ReactMouseEvent<HTMLAnchorElement>,
  target: string,
): void {
  if (event.defaultPrevented || event.button !== 0) {
    return;
  }

  event.preventDefault();
  const linkOpenGesture = useChatComposerStore.getState().linkOpenGesture;
  if (!shouldOpenLink(event.shiftKey, linkOpenGesture)) {
    return;
  }

  event.stopPropagation();
  void navigateLinkTarget(target, {
    shiftKey: event.shiftKey,
    sourceLeafId: getWorkspacePaneLeafIdFromEventTarget(event.currentTarget),
  });
}

/**
 * 将代码块头部的文件名文本转换为可点击的本地文件链接。
 */
function LinkifiedPlainText({ text }: { text: string }) {
  const matches = useMemo(() => extractTextLinkMatches(text), [text]);
  const { openLocalFileContextMenu, contextMenu } = useChatFileContextMenu();
  if (matches.length === 0) {
    return <>{text}</>;
  }

  const nodes: ReactNode[] = [];
  let cursor = 0;
  for (const match of matches) {
    if (match.startIndex > cursor) {
      nodes.push(text.slice(cursor, match.startIndex));
    }
    nodes.push(
      <a
        key={`${match.startIndex}:${match.endIndex}:${match.text}`}
        href={match.text}
        className="chat-plain-link"
        rel="noreferrer noopener"
        onClick={(event) => handlePlainTextLinkClick(event, match.text)}
        onContextMenu={(event) => {
          openLocalFileContextMenu(
            event,
            match.text,
            getWorkspacePaneLeafIdFromEventTarget(event.currentTarget),
          );
        }}
      >
        {match.text}
      </a>,
    );
    cursor = match.endIndex;
  }

  if (cursor < text.length) {
    nodes.push(text.slice(cursor));
  }

  return <>{nodes}{contextMenu}</>;
}

/**
 * 执行代码复制动作并短暂显示复制成功状态。
 */
function CodeBlockCopyButton({ content }: { content: string }) {
  const [copied, setCopied] = useState(false);
  const handleCopy = useCallback(() => {
    void navigator.clipboard.writeText(content).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }, [content]);

  return (
    <button
      type="button"
      onClick={handleCopy}
      style={{
        marginLeft: "auto", flexShrink: 0, cursor: "pointer",
        background: "none", border: "none", padding: "2px",
        color: copied ? "var(--success)" : "var(--text-3)",
        opacity: copied ? 1 : 0.5,
        transition: "color var(--duration-fast) var(--ease-out), opacity var(--duration-fast) var(--ease-out)",
      }}
      aria-label="Copy code"
    >
      {copied ? <Check size={12} /> : <Copy size={12} />}
    </button>
  );
}

/**
 * 渲染聊天代码块的固定结构，保持文件名链接和复制交互不变。
 */
export function CodeBlock({ content, language, filename }: CodeBlockProps) {
  const lang = String(language ?? "text");
  const displayName = filename || lang;

  return (
    <div
      style={{
        borderRadius: "var(--radius-sm)",
        border: "1px solid var(--border)",
        overflow: "hidden",
        background: "var(--code-bg)",
      }}
    >
      <div
        style={{
          display: "flex",
          alignItems: "center",
          gap: 6,
          padding: "6px 12px",
          borderBottom: "1px solid var(--border)",
          fontSize: 11,
          color: "var(--text-3)",
          fontFamily: '"Geist Mono", ui-monospace, monospace',
        }}
      >
        <FileCode2 size={12} style={{ opacity: 0.5 }} />
        <span style={{ flex: 1 }}>
          <LinkifiedPlainText text={displayName} />
        </span>
        <CodeBlockCopyButton content={content} />
      </div>
      <pre
        style={{
          margin: 0,
          padding: "12px 14px",
          fontSize: 12.5,
          lineHeight: 1.6,
          fontFamily: '"Geist Mono", ui-monospace, monospace',
          whiteSpace: "pre-wrap",
          wordBreak: "break-word",
          overflow: "auto",
          maxHeight: 400,
        }}
      >
        <code className={`language-${lang}`}>{content}</code>
      </pre>
    </div>
  );
}
