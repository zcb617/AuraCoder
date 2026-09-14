import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type MouseEvent as ReactMouseEvent,
} from "react";
import { recordPerfMetric } from "../../lib/perfTelemetry";
import {
  classifyLinkTarget,
  getWorkspacePaneLeafIdFromEventTarget,
  navigateLinkTarget,
} from "../../lib/fileLinkNavigation";
import { renderMarkdownToHtml } from "../../workers/markdownParserCore";
import { useChatFileContextMenu } from "./useChatFileContextMenu";
import type {
  MarkdownParseWorkerRequest,
  MarkdownParseWorkerResponse,
} from "../../workers/markdownParser.types";

const MARKDOWN_WORKER_THRESHOLD_CHARS = 1000;
// 流式回复期间控制 Markdown 全量解析频率，避免每个 delta 都占用渲染主线程。
const STREAM_PARSE_INTERVAL_MS = 300;
const MARKDOWN_CACHE_LIMIT = 280;
const MARKDOWN_CACHE_MAX_BYTES = 8 * 1024 * 1024;

const markdownHtmlCache = new Map<string, string>();
let markdownHtmlCacheBytes = 0;
let markdownWorkerInstance: Worker | null = null;
let markdownWorkerRequestSeq = 0;
const markdownWorkerCallbacks = new Map<
  number,
  {
    resolve: (value: string) => void;
    reject: (reason?: unknown) => void;
  }
>();

function computeCacheKey(content: string): string {
  let hash = 2166136261;
  for (let index = 0; index < content.length; index += 1) {
    hash ^= content.charCodeAt(index);
    hash = Math.imul(hash, 16777619);
  }
  return `${content.length}:${(hash >>> 0).toString(16)}`;
}

function readCachedMarkdownHtml(cacheKey: string): string | null {
  const html = markdownHtmlCache.get(cacheKey);
  if (html === undefined) {
    return null;
  }

  markdownHtmlCache.delete(cacheKey);
  markdownHtmlCache.set(cacheKey, html);
  return html;
}

function peekCachedMarkdownHtml(cacheKey: string): string | null {
  return markdownHtmlCache.get(cacheKey) ?? null;
}

function writeCachedMarkdownHtml(cacheKey: string, html: string) {
  const nextEntryBytes = estimateCacheEntryBytes(cacheKey, html);
  const existing = markdownHtmlCache.get(cacheKey);
  if (markdownHtmlCache.has(cacheKey)) {
    if (existing !== undefined) {
      markdownHtmlCacheBytes -= estimateCacheEntryBytes(cacheKey, existing);
    }
    markdownHtmlCache.delete(cacheKey);
  }
  markdownHtmlCache.set(cacheKey, html);
  markdownHtmlCacheBytes += nextEntryBytes;
  while (
    markdownHtmlCache.size > MARKDOWN_CACHE_LIMIT ||
    markdownHtmlCacheBytes > MARKDOWN_CACHE_MAX_BYTES
  ) {
    const oldestKey = markdownHtmlCache.keys().next().value;
    if (!oldestKey) {
      break;
    }
    const oldestHtml = markdownHtmlCache.get(oldestKey);
    if (oldestHtml !== undefined) {
      markdownHtmlCacheBytes -= estimateCacheEntryBytes(oldestKey, oldestHtml);
    }
    markdownHtmlCache.delete(oldestKey);
  }

  if (markdownHtmlCacheBytes < 0) {
    markdownHtmlCacheBytes = 0;
  }
}

function estimateCacheEntryBytes(cacheKey: string, html: string): number {
  return (cacheKey.length + html.length) * 2;
}

function ensureMarkdownWorker(): Worker | null {
  if (typeof Worker === "undefined") {
    return null;
  }
  if (!markdownWorkerInstance) {
    markdownWorkerInstance = new Worker(
      new URL("../../workers/markdownParser.worker.ts", import.meta.url),
      { type: "module" },
    );
    markdownWorkerInstance.onmessage = (
      event: MessageEvent<MarkdownParseWorkerResponse>,
    ) => {
      const payload = event.data;
      const callback = markdownWorkerCallbacks.get(payload.id);
      if (!callback) {
        return;
      }
      markdownWorkerCallbacks.delete(payload.id);
      if (payload.ok) {
        callback.resolve(payload.html);
      } else {
        callback.reject(new Error(payload.error));
      }
    };
    markdownWorkerInstance.onerror = (error) => {
      for (const callback of markdownWorkerCallbacks.values()) {
        callback.reject(error);
      }
      markdownWorkerCallbacks.clear();
      markdownWorkerInstance?.terminate();
      markdownWorkerInstance = null;
    };
  }
  return markdownWorkerInstance;
}

function parseMarkdownInWorker(markdown: string): Promise<string> {
  const worker = ensureMarkdownWorker();
  if (!worker) {
    return Promise.reject(new Error("worker-unavailable"));
  }

  return new Promise((resolve, reject) => {
    markdownWorkerRequestSeq += 1;
    const requestId = markdownWorkerRequestSeq;
    markdownWorkerCallbacks.set(requestId, { resolve, reject });
    const payload: MarkdownParseWorkerRequest = {
      id: requestId,
      markdown,
    };
    worker.postMessage(payload);
  });
}

interface MarkdownContentProps {
  content: string;
  className?: string;
  style?: CSSProperties;
  streaming?: boolean;
  enableFileContextMenu?: boolean;
}

interface MarkdownWorkerPlaceholderOptions {
  hasStreamed: boolean;
  streaming: boolean;
  workerEligible: boolean;
  workerError: boolean;
  workerHtml: string | null;
}

export function shouldRenderMarkdownWorkerPlaceholder({
  hasStreamed,
  streaming,
  workerEligible,
  workerError,
  workerHtml,
}: MarkdownWorkerPlaceholderOptions): boolean {
  return (
    workerEligible &&
    !streaming &&
    !hasStreamed &&
    !workerError &&
    workerHtml === null
  );
}

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

export default function MarkdownContent({
  content,
  className,
  style,
  streaming = false,
  enableFileContextMenu = false,
}: MarkdownContentProps) {
  const [workerHtml, setWorkerHtml] = useState<string | null>(null);
  const [workerError, setWorkerError] = useState(false);
  // 通过版本号触发到点后的流式 Markdown 重新渲染。
  const [streamParseVersion, setStreamParseVersion] = useState(0);
  const parseStartedAtRef = useRef(0);
  // 保存流式窗口内最近一次全量解析得到的 HTML，供节流间隔内复用。
  const streamParsedHtmlRef = useRef<string | null>(null);
  // 记录最近一次流式全量解析对应的内容，避免把旧 HTML 写入新内容缓存。
  const streamParsedContentRef = useRef<string | null>(null);
  // 记录最近一次流式 Markdown 全量解析的时间，用于计算下一次刷新时间。
  const streamLastParsedAtRef = useRef<number | null>(null);
  // 保存流式解析定时器，保证组件卸载或流式结束时能够清理。
  const streamParseTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const hasStreamedRef = useRef(streaming);
  const { openLocalFileContextMenu, contextMenu } = useChatFileContextMenu();

  const workerEligible = content.length >= MARKDOWN_WORKER_THRESHOLD_CHARS;
  const cacheKey = useMemo(() => computeCacheKey(content), [content]);
  const hasStreamed = hasStreamedRef.current || streaming;
  const cachedHtml = useMemo(() => peekCachedMarkdownHtml(cacheKey), [cacheKey]);
  const showWorkerPlaceholder = shouldRenderMarkdownWorkerPlaceholder({
    hasStreamed,
    streaming,
    workerEligible,
    workerError,
    workerHtml,
  }) && cachedHtml === null;

  const immediateHtml = useMemo(() => {
    if (cachedHtml !== null) {
      return cachedHtml;
    }
    if (showWorkerPlaceholder) {
      return null;
    }
    if (!streaming) {
      streamParsedHtmlRef.current = null;
      streamParsedContentRef.current = null;
      streamLastParsedAtRef.current = null;
      return renderMarkdownToHtml(content);
    }

    const now = performance.now();
    const lastParsedAt = streamLastParsedAtRef.current;
    if (
      lastParsedAt === null ||
      now - lastParsedAt >= STREAM_PARSE_INTERVAL_MS
    ) {
      const html = renderMarkdownToHtml(content);
      streamParsedHtmlRef.current = html;
      streamParsedContentRef.current = content;
      streamLastParsedAtRef.current = now;
      return html;
    }

    return streamParsedHtmlRef.current;
  }, [cachedHtml, content, showWorkerPlaceholder, streaming, streamParseVersion]);

  // 流式期间安排下一次节流刷新，并在流式结束或组件卸载时清理定时器。
  useEffect(() => {
    if (!streaming) {
      streamParsedHtmlRef.current = null;
      streamParsedContentRef.current = null;
      streamLastParsedAtRef.current = null;
    }
    if (!streaming || cachedHtml !== null || showWorkerPlaceholder) {
      if (streamParseTimerRef.current !== null) {
        clearTimeout(streamParseTimerRef.current);
        streamParseTimerRef.current = null;
      }
      return;
    }

    const lastParsedAt = streamLastParsedAtRef.current;
    const elapsed = lastParsedAt === null
      ? STREAM_PARSE_INTERVAL_MS
      : performance.now() - lastParsedAt;
    const delay = Math.max(0, STREAM_PARSE_INTERVAL_MS - elapsed);
    if (delay === 0) {
      setStreamParseVersion((version) => version + 1);
      return;
    }

    streamParseTimerRef.current = setTimeout(() => {
      streamParseTimerRef.current = null;
      setStreamParseVersion((version) => version + 1);
    }, delay);

    return () => {
      if (streamParseTimerRef.current !== null) {
        clearTimeout(streamParseTimerRef.current);
        streamParseTimerRef.current = null;
      }
    };
  }, [cachedHtml, content, showWorkerPlaceholder, streaming, streamParseVersion]);

  useEffect(() => {
    if (!streaming) {
      return;
    }
    hasStreamedRef.current = true;
  }, [streaming]);

  useEffect(() => {
    if (
      immediateHtml === null ||
      (streaming && streamParsedContentRef.current !== content)
    ) {
      return;
    }
    writeCachedMarkdownHtml(cacheKey, immediateHtml);
  }, [cacheKey, content, immediateHtml, streaming]);

  useEffect(() => {
    if (!workerEligible || streaming || hasStreamed) {
      setWorkerHtml(null);
      setWorkerError(false);
      return;
    }

    const cached = readCachedMarkdownHtml(cacheKey);
    if (cached !== null) {
      setWorkerHtml(cached);
      setWorkerError(false);
      return;
    }

    let disposed = false;
    setWorkerHtml(null);
    setWorkerError(false);
    parseStartedAtRef.current = performance.now();

    parseMarkdownInWorker(content)
      .then((html) => {
        if (disposed) {
          return;
        }
        writeCachedMarkdownHtml(cacheKey, html);
        setWorkerHtml(html);
        recordPerfMetric("chat.markdown.worker.ms", performance.now() - parseStartedAtRef.current, {
          chars: content.length,
          cached: false,
        });
      })
      .catch(() => {
        if (disposed) {
          return;
        }
        setWorkerError(true);
      });

    return () => {
      disposed = true;
    };
  }, [cacheKey, content, hasStreamed, streaming, workerEligible]);

  const handleMarkdownLinkContextMenu = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (!enableFileContextMenu) {
      return;
    }

    const rawTarget = getMarkdownLinkTarget(event.target);
    if (!rawTarget) {
      return;
    }
    openLocalFileContextMenu(
      event,
      rawTarget,
      getWorkspacePaneLeafIdFromEventTarget(event.currentTarget),
    );
  };

  if (showWorkerPlaceholder) {
    return (
      <div className={className} style={style}>
        <pre
          style={{
            margin: 0,
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
            fontFamily: "inherit",
          }}
        >
          {content}
        </pre>
      </div>
    );
  }

  const html = workerEligible && !streaming && !hasStreamed && workerHtml !== null
    ? workerHtml
    : immediateHtml;

  if (html === null) {
    return (
      <div className={className} style={style}>
        <pre
          style={{
            margin: 0,
            whiteSpace: "pre-wrap",
            wordBreak: "break-word",
            fontFamily: "inherit",
          }}
        >
          {content}
        </pre>
      </div>
    );
  }

  return (
    <>
      <div
        className={className}
        style={style}
        onClickCapture={handleMarkdownLinkClick}
        onContextMenu={enableFileContextMenu ? handleMarkdownLinkContextMenu : undefined}
        dangerouslySetInnerHTML={{ __html: html }}
      />
      {contextMenu}
    </>
  );
}
