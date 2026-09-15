import hljs from "highlight.js/lib/common";
import { marked } from "marked";
import {
  DISALLOWED_LOCAL_PREFIX_CHAR_RE,
  TEXT_LINK_PATTERN,
  isLocalFileLinkSyntax,
  parseLocalAbsolutePathTarget,
  parseLocalUrlTarget,
  trimLinkText,
} from "../lib/localFileLinkPatterns";

interface FenceToken {
  placeholder: string;
  html: string;
}

interface FenceOpening {
  markerChar: "`" | "~";
  markerLength: number;
  info: string;
}

interface IndentInfo {
  width: number;
  nextIndex: number;
}

const SAFE_HTML_TAG_RE = /<(br|hr)\s*\/?>/gi;
const RAW_HTML_FRAGMENT_RE =
  /<!--[\s\S]*?-->|<!\[CDATA\[[\s\S]*?\]\]>|<![a-z][^>]*>|<\?[\s\S]*?\?>|<\/[a-z][a-z0-9-]*\s*>|<[a-z][a-z0-9-]*(?:\s+(?:"[^"]*"|'[^']*'|[^"'<>])*)?\s*\/?>/gi;

function formatLocalFileLinkPath(rawTarget: string): string {
  const parsed = parseLocalAbsolutePathTarget(rawTarget) ?? parseLocalUrlTarget(rawTarget);
  if (!parsed) {
    return rawTarget;
  }

  const path = parsed.path;
  if (/^\/?[A-Za-z]:[\\/]/.test(path)) {
    return path.replace(/^\/(?=[A-Za-z]:[\\/])/, "").replace(/\//g, "\\");
  }

  return path;
}

function escapeHtmlFragment(input: string): string {
  return input
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function escapeNonFenceHtml(input: string): string {
  // Preserve known safe self-closing HTML tags (<br>, <hr>) by replacing them
  // with placeholders before escaping, then restoring after.
  const placeholders: { placeholder: string; tag: string }[] = [];
  let idx = 0;
  const withPlaceholders = input.replace(SAFE_HTML_TAG_RE, (match) => {
    const placeholder = `\x00SAFE_TAG_${idx++}\x00`;
    placeholders.push({ placeholder, tag: match });
    return placeholder;
  });

  let escaped = withPlaceholders.replace(
    RAW_HTML_FRAGMENT_RE,
    (match) => escapeHtmlFragment(match),
  );

  for (const { placeholder, tag } of placeholders) {
    escaped = escaped.replace(placeholder, tag);
  }

  return escaped;
}

function sanitizeUrl(url: string, attrName: string): string {
  const trimmed = url.trim();
  if (!trimmed) {
    return "#";
  }

  if (attrName.toLowerCase() === "href" && isLocalFileLinkSyntax(trimmed)) {
    return formatLocalFileLinkPath(trimmed);
  }

  if (
    trimmed.startsWith("#") ||
    trimmed.startsWith("/") ||
    trimmed.startsWith("./") ||
    trimmed.startsWith("../")
  ) {
    return trimmed;
  }

  const lower = trimmed.toLowerCase();
  if (
    lower.startsWith("http://") ||
    lower.startsWith("https://") ||
    lower.startsWith("mailto:") ||
    lower.startsWith("tel:")
  ) {
    return trimmed;
  }

  return "#";
}

function sanitizeRenderedHtml(html: string): string {
  const withoutDangerousTags = html
    .replace(/<script[\s\S]*?>[\s\S]*?<\/script>/gi, "")
    .replace(/<style[\s\S]*?>[\s\S]*?<\/style>/gi, "")
    .replace(/<iframe[\s\S]*?>[\s\S]*?<\/iframe>/gi, "")
    .replace(/<object[\s\S]*?>[\s\S]*?<\/object>/gi, "")
    .replace(/<embed[\s\S]*?>[\s\S]*?<\/embed>/gi, "");

  const withoutEventHandlers = withoutDangerousTags.replace(
    /\s+on[a-z]+\s*=\s*(".*?"|'.*?'|[^\s>]+)/gi,
    "",
  );

  const withSafeLinks = withoutEventHandlers.replace(
    /\s(href|src)\s*=\s*("([^"]*)"|'([^']*)')/gi,
    (_full, attrName: string, _quoted: string, doubleValue: string, singleValue: string) => {
      const rawValue = typeof doubleValue === "string" ? doubleValue : singleValue;
      const trimmedValue = rawValue.trim();
      const localFileReference = attrName.toLowerCase() === "href" && isLocalFileLinkSyntax(trimmedValue);
      const safe = sanitizeUrl(rawValue, attrName);
      const referenceAttribute = localFileReference
        ? ` data-local-file-reference="${escapeHtmlAttribute(trimmedValue)}"`
        : "";
      return ` ${attrName}="${safe}"${referenceAttribute}`;
    },
  );

  return withSafeLinks.replace(
    /<a\b(?![^>]*\brel=)([^>]*)>/gi,
    "<a$1 rel=\"noreferrer noopener\">",
  );
}

function escapeHtmlAttribute(input: string): string {
  return input
    .replaceAll("&", "&amp;")
    .replaceAll("\"", "&quot;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;");
}

function linkifyLocalFileReferencesInText(text: string): string {
  return text.replace(TEXT_LINK_PATTERN, (raw, offset: number) => {
    const trimmed = trimLinkText(raw);
    if (
      !trimmed ||
      !isLocalFileLinkSyntax(trimmed) ||
      text.slice(Math.max(0, offset - 4), offset) === "&lt;" ||
      (offset > 0 && DISALLOWED_LOCAL_PREFIX_CHAR_RE.test(text[offset - 1] ?? ""))
    ) {
      return raw;
    }

    const trailing = raw.slice(trimmed.length);
    const safeHref = escapeHtmlAttribute(formatLocalFileLinkPath(trimmed));
    const rawReference = escapeHtmlAttribute(trimmed);
    return `<a href="${safeHref}" data-local-file-reference="${rawReference}" rel="noreferrer noopener">${trimmed}</a>${trailing}`;
  });
}

function linkifyLocalFileReferencesInHtml(html: string): string {
  const tagPattern = /<\/?([a-z][a-z0-9-]*)\b[^>]*>/gi;
  const skipTags = new Set(["a", "pre"]);
  let result = "";
  let lastIndex = 0;
  let skipDepth = 0;

  for (const match of html.matchAll(tagPattern)) {
    const tag = match[0];
    const tagName = (match[1] ?? "").toLowerCase();
    const index = match.index ?? 0;
    const text = html.slice(lastIndex, index);
    result += skipDepth > 0 ? text : linkifyLocalFileReferencesInText(text);
    result += tag;

    if (skipTags.has(tagName)) {
      const isClosing = tag.startsWith("</");
      const isSelfClosing = tag.endsWith("/>");
      if (isClosing) {
        skipDepth = Math.max(0, skipDepth - 1);
      } else if (!isSelfClosing) {
        skipDepth += 1;
      }
    }

    lastIndex = index + tag.length;
  }

  const remaining = html.slice(lastIndex);
  result += skipDepth > 0 ? remaining : linkifyLocalFileReferencesInText(remaining);
  return result;
}

function normalizeFenceLanguage(raw: string): string {
  const firstToken = raw.trim().split(/\s+/)[0] ?? "";
  return firstToken.toLowerCase();
}

function renderHighlightedFence(code: string, language: string): string {
  const normalizedLanguage = normalizeFenceLanguage(language);
  let highlighted: string;
  let languageClass = "";

  if (normalizedLanguage && hljs.getLanguage(normalizedLanguage)) {
    highlighted = hljs.highlight(code, { language: normalizedLanguage }).value;
    languageClass = ` language-${normalizedLanguage}`;
  } else {
    highlighted = hljs.highlightAuto(code).value;
  }

  return `<pre><code class="hljs${languageClass}">${highlighted}</code></pre>`;
}

function removeLineBreak(line: string): string {
  if (line.endsWith("\r\n")) {
    return line.slice(0, -2);
  }
  if (line.endsWith("\n")) {
    return line.slice(0, -1);
  }
  return line;
}

function readIndentInfo(line: string): IndentInfo {
  let width = 0;
  let index = 0;
  while (index < line.length) {
    const current = line[index];
    if (current === " ") {
      width += 1;
      index += 1;
      continue;
    }
    if (current === "\t") {
      width += 4 - (width % 4);
      index += 1;
      continue;
    }
    break;
  }

  return {
    width,
    nextIndex: index,
  };
}

function parseFenceOpening(line: string): FenceOpening | null {
  const content = removeLineBreak(line);
  const indent = readIndentInfo(content);
  if (indent.width > 3) {
    return null;
  }

  const markerChar = content[indent.nextIndex];
  if (markerChar !== "`" && markerChar !== "~") {
    return null;
  }

  let markerEnd = indent.nextIndex;
  while (content[markerEnd] === markerChar) {
    markerEnd += 1;
  }

  const markerLength = markerEnd - indent.nextIndex;
  if (markerLength < 3) {
    return null;
  }

  const info = content.slice(markerEnd).trim();
  if (markerChar === "`" && info.includes("`")) {
    return null;
  }

  return {
    markerChar,
    markerLength,
    info,
  };
}

function isFenceClosing(
  line: string,
  markerChar: FenceOpening["markerChar"],
  minMarkerLength: number,
): boolean {
  const content = removeLineBreak(line);
  const indent = readIndentInfo(content);
  if (indent.width > 3) {
    return false;
  }

  let markerEnd = indent.nextIndex;
  while (content[markerEnd] === markerChar) {
    markerEnd += 1;
  }

  const markerLength = markerEnd - indent.nextIndex;
  if (markerLength < minMarkerLength) {
    return false;
  }

  const suffix = content.slice(markerEnd);
  return /^[ \t]*$/.test(suffix);
}

function splitLinesWithEndings(markdown: string): string[] {
  return markdown.match(/[^\n]*\n|[^\n]+/g) ?? [];
}

function tokenizeFences(markdown: string): { source: string; fences: FenceToken[] } {
  const lines = splitLinesWithEndings(markdown);
  const fences: FenceToken[] = [];
  let source = "";
  let plainBuffer = "";
  let fenceIndex = 0;

  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const opening = parseFenceOpening(lines[lineIndex]);
    if (!opening) {
      plainBuffer += lines[lineIndex];
      continue;
    }

    let closingIndex = -1;
    let code = "";
    for (let scanIndex = lineIndex + 1; scanIndex < lines.length; scanIndex += 1) {
      if (isFenceClosing(lines[scanIndex], opening.markerChar, opening.markerLength)) {
        closingIndex = scanIndex;
        break;
      }
      code += lines[scanIndex];
    }

    if (closingIndex < 0) {
      plainBuffer += lines.slice(lineIndex).join("");
      break;
    }

    if (plainBuffer) {
      source += escapeNonFenceHtml(plainBuffer);
      plainBuffer = "";
    }

    const placeholder = `<div data-auracoder-fence="${fenceIndex}"></div>`;
    source += `\n${placeholder}\n`;

    fences.push({
      placeholder,
      html: renderHighlightedFence(code, opening.info),
    });

    fenceIndex += 1;
    lineIndex = closingIndex;
  }

  if (plainBuffer) {
    source += escapeNonFenceHtml(plainBuffer);
  }

  return { source, fences };
}

/**
 * 将完整 Markdown 文本解析为经过高亮、安全过滤和本地文件链接增强的 HTML。
 */
export function renderMarkdownToHtml(markdown: string): string {
  const { source, fences } = tokenizeFences(markdown);
  const html = marked.parse(source, {
    gfm: true,
    async: false,
  });

  let finalHtml = html;
  for (const fence of fences) {
    finalHtml = finalHtml.replace(fence.placeholder, fence.html);
  }

  return linkifyLocalFileReferencesInHtml(sanitizeRenderedHtml(finalHtml));
}

/**
 * 在流式消息中暂缓渲染尚未闭合的代码围栏，避免尾部文本被错误排版。
 */
function findUnclosedFenceStart(markdown: string): number | null {
  const lines = splitLinesWithEndings(markdown);
  let offset = 0;

  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const opening = parseFenceOpening(lines[lineIndex]);
    if (!opening) {
      offset += lines[lineIndex].length;
      continue;
    }

    let closingIndex = -1;
    for (let scanIndex = lineIndex + 1; scanIndex < lines.length; scanIndex += 1) {
      if (isFenceClosing(lines[scanIndex], opening.markerChar, opening.markerLength)) {
        closingIndex = scanIndex;
        break;
      }
    }

    if (closingIndex < 0) {
      return offset;
    }

    for (let consumedIndex = lineIndex; consumedIndex <= closingIndex; consumedIndex += 1) {
      offset += lines[consumedIndex].length;
    }
    lineIndex = closingIndex;
  }

  return null;
}

/**
 * 在流式尾部新增段中查找最后一个未闭合的行内定界符起点，用于暂缓半截行内结构渲染。
 */
function findUnclosedInlineDelimiterStart(
  markdown: string,
  searchFromIndex: number,
): number | null {
  if (markdown.length === 0 || searchFromIndex >= markdown.length) {
    return null;
  }

  const tailMarkdown = markdown.slice(searchFromIndex);
  const lines = splitLinesWithEndings(tailMarkdown);
  const fenceRanges: Array<[number, number]> = [];
  let lineOffset = 0;
  let unclosedFenceOffset: number | null = null;

  for (let lineIndex = 0; lineIndex < lines.length; lineIndex += 1) {
    const opening = parseFenceOpening(lines[lineIndex]);
    if (!opening) {
      lineOffset += lines[lineIndex].length;
      continue;
    }

    let closingIndex = -1;
    for (let scanIndex = lineIndex + 1; scanIndex < lines.length; scanIndex += 1) {
      if (isFenceClosing(lines[scanIndex], opening.markerChar, opening.markerLength)) {
        closingIndex = scanIndex;
        break;
      }
    }

    if (closingIndex < 0) {
      unclosedFenceOffset = lineOffset;
      break;
    }

    let closingEndOffset = lineOffset;
    for (let consumedIndex = lineIndex; consumedIndex <= closingIndex; consumedIndex += 1) {
      closingEndOffset += lines[consumedIndex].length;
    }
    fenceRanges.push([lineOffset, closingEndOffset]);
    lineOffset = closingEndOffset;
    lineIndex = closingIndex;
  }

  let backtickStart: number | null = null;
  let boldStart: number | null = null;
  const bracketStarts: number[] = [];
  const parenthesisStarts: number[] = [];
  const chineseParenthesisStarts: number[] = [];
  let fenceRangeIndex = 0;
  let index = 0;

  while (index < tailMarkdown.length) {
    const currentFenceRange = fenceRanges[fenceRangeIndex];
    if (currentFenceRange && index >= currentFenceRange[0]) {
      index = currentFenceRange[1];
      fenceRangeIndex += 1;
      continue;
    }
    if (unclosedFenceOffset !== null && index >= unclosedFenceOffset) {
      break;
    }

    const current = tailMarkdown[index];
    const absoluteIndex = searchFromIndex + index;
    if (current === "`") {
      backtickStart = backtickStart === null ? absoluteIndex : null;
      index += 1;
      continue;
    }
    if (current === "*" && tailMarkdown[index + 1] === "*") {
      boldStart = boldStart === null ? absoluteIndex : null;
      index += 2;
      continue;
    }
    if (current === "[") {
      bracketStarts.push(absoluteIndex);
    } else if (current === "]" && bracketStarts.length > 0) {
      bracketStarts.pop();
    } else if (current === "(") {
      parenthesisStarts.push(absoluteIndex);
    } else if (current === ")" && parenthesisStarts.length > 0) {
      parenthesisStarts.pop();
    } else if (current === "（") {
      chineseParenthesisStarts.push(absoluteIndex);
    } else if (current === "）" && chineseParenthesisStarts.length > 0) {
      chineseParenthesisStarts.pop();
    }
    index += 1;
  }

  const unclosedStarts: number[] = [];
  if (backtickStart !== null) {
    unclosedStarts.push(backtickStart);
  }
  if (boldStart !== null) {
    unclosedStarts.push(boldStart);
  }
  unclosedStarts.push(...bracketStarts, ...parenthesisStarts, ...chineseParenthesisStarts);
  if (unclosedFenceOffset !== null) {
    unclosedStarts.push(searchFromIndex + unclosedFenceOffset);
  }

  return unclosedStarts.length > 0 ? Math.min(...unclosedStarts) : null;
}

/**
 * 按流式状态解析 Markdown，并返回已稳定 HTML 与暂缓尾部原文。
 */
export function renderStableMarkdownToHtml(
  markdown: string,
  streaming: boolean,
): {
  /** 已完成 Markdown 块解析得到的静态 HTML。 */
  html: string;
  /** 尚未闭合、暂缓渲染的流式尾部原文。 */
  withheldTail: string;
} {
  if (!streaming) {
    return {
      html: renderMarkdownToHtml(markdown),
      withheldTail: "",
    };
  }

  // 先让 marked 负责块级词法分析；代码围栏的精确闭合判断由现有围栏扫描逻辑完成。
  const tokens = marked.lexer(markdown, { gfm: true });
  const lastNonSpaceToken = [...tokens]
    .reverse()
    .find((token) => token.type !== "space");
  const unclosedFenceStart = findUnclosedFenceStart(markdown);
  if (unclosedFenceStart === null || lastNonSpaceToken?.type !== "code") {
    return {
      html: renderMarkdownToHtml(markdown),
      withheldTail: "",
    };
  }

  return {
    html: renderMarkdownToHtml(markdown.slice(0, unclosedFenceStart)),
    withheldTail: markdown.slice(unclosedFenceStart),
  };
}

/**
 * 流式 Markdown 增量渲染器，锁定已完成块的 HTML，仅解析并追加新稳定片段。
 */
export interface StreamingMarkdownAppender {
  /** 接收截至当前时刻的完整 Markdown 累积文本并返回已锁定的完整 HTML。 */
  push(fullMarkdownSoFar: string): { html: string };
  /** 清除当前消息的 Markdown 游标和已产出的 HTML。 */
  reset(): void;
}

/**
 * 创建跨渲染复用的流式 Markdown 增量渲染器，避免稳定前缀被重复解析。
 */
export function createStreamingMarkdownAppender(): StreamingMarkdownAppender {
  let lockedMarkdownLength = 0;
  let lockedHtml = "";
  let previousMarkdown = "";

  /**
   * 清空当前流式消息的解析游标、HTML 累积结果和上次输入快照。
   */
  const reset = (): void => {
    lockedMarkdownLength = 0;
    lockedHtml = "";
    previousMarkdown = "";
  };

  /**
   * 追加当前累积文本中已经形成完整块的部分，暂缓未闭合代码围栏和行内定界符尾部。
   */
  const push = (fullMarkdownSoFar: string): { html: string } => {
    if (
      fullMarkdownSoFar.length < previousMarkdown.length ||
      !fullMarkdownSoFar.startsWith(previousMarkdown)
    ) {
      reset();
    }

    const unclosedFenceStart = findUnclosedFenceStart(fullMarkdownSoFar);
    const unclosedInlineStart = findUnclosedInlineDelimiterStart(
      fullMarkdownSoFar,
      lockedMarkdownLength,
    );
    const candidates = [unclosedFenceStart, unclosedInlineStart].filter(
      (v): v is number => v !== null,
    );
    const stableMarkdownEnd =
      candidates.length > 0 ? Math.min(...candidates) : fullMarkdownSoFar.length;

    if (stableMarkdownEnd > lockedMarkdownLength) {
      const stableDelta = fullMarkdownSoFar.slice(
        lockedMarkdownLength,
        stableMarkdownEnd,
      );
      if (stableDelta) {
        lockedHtml += renderMarkdownToHtml(stableDelta);
      }
      lockedMarkdownLength = stableMarkdownEnd;
    }

    previousMarkdown = fullMarkdownSoFar;
    return { html: lockedHtml };
  };

  return { push, reset };
}

export const markdownParserCoreInternals = {
  parseFenceOpening,
  isFenceClosing,
  splitLinesWithEndings,
  tokenizeFences,
  findUnclosedFenceStart,
  // 提供行内定界符检测逻辑，供 Markdown 核心行为测试复用。
  findUnclosedInlineDelimiterStart,
  // 提供流式增量解析器，供 Markdown 核心行为测试复用。
  createStreamingMarkdownAppender,
};
