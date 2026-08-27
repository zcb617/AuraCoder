<script setup lang="ts">
import { computed } from "vue";
import type { Message } from "../types";

interface TextTree {
  /** rich-text 文本节点固定类型。 */
  type: "text";
  /** rich-text 文本节点的显示内容。 */
  text: string;
}

interface RichTree {
  /** rich-text 元素名称。 */
  name: string;
  /** rich-text 元素的内联样式属性。 */
  attrs?: Record<string, string>;
  /** rich-text 元素包含的文本节点。 */
  children: TextTree[];
}

interface MarkdownSegment {
  /** 普通 Markdown 消息段类型。 */
  kind: "markdown";
  /** Markdown 文本经现有轻量解析后的富文本树。 */
  trees: RichTree[];
}

interface CodeSegment {
  /** 普通代码消息段类型。 */
  kind: "code";
  /** 代码块声明的语言名称。 */
  language?: string;
  /** 代码块原始内容。 */
  code: string;
}

interface MobileDiffSummary {
  /** Diff 涉及的文件数量。 */
  fileCount: number;
  /** Diff 中有效新增行数量。 */
  additions: number;
  /** Diff 中有效删除行数量。 */
  deletions: number;
}

interface ActionSegment {
  /** 工具执行消息段类型。 */
  kind: "action";
  /** 桌面工具执行摘要。 */
  summary: string;
  /** 规范化后的工具执行状态。 */
  status: "pending" | "running" | "done" | "error";
  /** 工具结果中的 Diff 汇总，可选。 */
  diffSummary?: MobileDiffSummary;
}

interface DiffSegment {
  /** Diff 汇总消息段类型。 */
  kind: "diff";
  /** 桌面完整 Diff 计算出的移动端汇总。 */
  summary: MobileDiffSummary;
}

type MessageSegment = MarkdownSegment | CodeSegment | ActionSegment | DiffSegment;

/*
Git 原版消息渲染实现保留在此注释块中，当前实现继续在下方处理完整 blocks。
interface TextNode {
  type: "text";
  text: string;
}

interface RichNode {
  name: string;
  attrs?: Record<string, string>;
  children: TextNode[];
}

interface MessageSegment {
  kind: "markdown" | "code";
  language?: string;
  code?: string;
  nodes?: RichNode[];
}

const props = defineProps<{ message: Message }>();

const segments = computed<MessageSegment[]>(() => {
  const content = props.message.content || props.message.blocks?.map((block) => {
    if (typeof block.content === "string") return block.content;
    if (typeof block.summary === "string") return `> ${block.summary}`;
    if (typeof block.message === "string") return block.message;
    return "";
  }).filter(Boolean).join("\n\n") || (props.message.status === "streaming" ? "正在生成…" : "");
  const result: MessageSegment[] = [];
  const expression = /```([^\n`]*)\n?([\s\S]*?)```/g;
  let position = 0;
  let match: RegExpExecArray | null;
  while ((match = expression.exec(content))) {
    if (match.index > position) result.push({ kind: "markdown", nodes: buildNodes(content.slice(position, match.index)) });
    result.push({ kind: "code", language: match[1].trim(), code: match[2].replace(/\n$/, "") });
    position = expression.lastIndex;
  }
  if (position < content.length || result.length === 0) result.push({ kind: "markdown", nodes: buildNodes(content.slice(position)) });
  return result;
});

function buildNodes(value: string): RichNode[] {
  // 旧写法给最后一段也追加 8px 下外边距，导致气泡内容下方明显大于上方；保留原写法以便追溯。
  // return value.split(/\n{2,}/).filter(Boolean).map((paragraph) => {
  const paragraphs = value.split(/\n{2,}/).filter(Boolean);
  return paragraphs.map((paragraph, index) => {
    const bottomMargin = index < paragraphs.length - 1 ? "8px" : "0";
    const heading = /^(#{1,6})\s+(.+)$/m.exec(paragraph);
    if (heading) return { name: "div", attrs: { style: `font-size:${18 - heading[1].length}px;font-weight:700;margin:5px 0 ${bottomMargin};` }, children: [{ type: "text", text: heading[2] }] };
    if (paragraph.startsWith("> ")) return { name: "div", attrs: { style: "padding-left:8px;border-left:3px solid #46d39a;color:#aeb8c7;" }, children: [{ type: "text", text: paragraph.slice(2) }] };
    return { name: "div", attrs: { style: `margin-bottom:${bottomMargin};white-space:pre-wrap;` }, children: [{ type: "text", text: paragraph }] };
  });
}
*/
const props = defineProps<{ message: Message }>();

const segments = computed<MessageSegment[]>(() => {
  const result: MessageSegment[] = [];
  const blocks = props.message.blocks;
  if (blocks?.length) {
    for (const block of blocks) {
      const blockType = readString(block, "type");
      if (blockType === "action") {
        const summary = readString(block, "summary");
        if (summary !== undefined) {
          const status = readActionStatus(readString(block, "status"));
          const resultObject = readRecord(block, "result");
          const diffSummary = resultObject ? readMobileDiffSummary(resultObject) : undefined;
          result.push({ kind: "action", summary, status, diffSummary });
        }
        continue;
      }
      if (blockType === "diff") {
        const summary = readMobileDiffSummary(block);
        if (summary) result.push({ kind: "diff", summary });
        continue;
      }
      if (blockType === "code") {
        const code = readString(block, "content");
        if (code !== undefined) result.push({ kind: "code", language: readString(block, "language"), code });
        continue;
      }
      if (blockType === "text" || blockType === "thinking" || blockType === "notice" || blockType === "error") {
        const text = readString(block, "content") ?? readString(block, "message");
        if (text !== undefined) appendContentSegments(result, text);
      }
    }
    if (result.length > 0) return result;
  }

  const content = props.message.content || (blocks?.map((block) => {
    if (typeof block.content === "string") return block.content;
    if (typeof block.summary === "string") return `> ${block.summary}`;
    if (typeof block.message === "string") return block.message;
    return "";
  }).filter(Boolean).join("\n\n") || (props.message.status === "streaming" ? "正在生成…" : ""));
  appendContentSegments(result, content);
  return result;
});

/** 判断消息协议字段是否为可安全读取的对象。 */
function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 读取已验证的字符串字段，非法字段按缺失处理。 */
function readString(value: Record<string, unknown>, key: string): string | undefined {
  const field = value[key];
  return typeof field === "string" ? field : undefined;
}

/** 读取已验证的嵌套对象字段，避免异常协议数据进入渲染流程。 */
function readRecord(value: Record<string, unknown>, key: string): Record<string, unknown> | undefined {
  const field = value[key];
  return isRecord(field) ? field : undefined;
}

/** 将移动端 Diff 摘要字段归一化为非负整数，并拒绝空摘要。 */
function readMobileDiffSummary(value: Record<string, unknown>): MobileDiffSummary | undefined {
  const summaryValue = value.mobileSummary;
  if (!isRecord(summaryValue)) return undefined;
  const fileCount = readNonNegativeInteger(summaryValue.fileCount);
  const additions = readNonNegativeInteger(summaryValue.additions);
  const deletions = readNonNegativeInteger(summaryValue.deletions);
  if (fileCount === undefined || additions === undefined || deletions === undefined) return undefined;
  if (fileCount === 0 && additions === 0 && deletions === 0) return undefined;
  return { fileCount, additions, deletions };
}

/** 将协议数字字段限制为有限、非负整数，防止非法值显示到用户界面。 */
function readNonNegativeInteger(value: unknown): number | undefined {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) return undefined;
  return Math.floor(value);
}

/** 将桌面工具状态转换为手机端固定中文状态和值域。 */
function readActionStatus(value: string | undefined): ActionSegment["status"] {
  if (value === "pending" || value === "running" || value === "done" || value === "error") return value;
  return "done";
}

/** 按消息块顺序复用原有 Markdown 与 fenced code 的显示语义。 */
function appendContentSegments(result: MessageSegment[], content: string) {
  const expression = /\`\`\`([^\n\`]*)\n?([\s\S]*?)\`\`\`/g;
  let position = 0;
  let match: RegExpExecArray | null;
  while ((match = expression.exec(content))) {
    if (match.index > position) result.push({ kind: "markdown", trees: buildRichTrees(content.slice(position, match.index)) });
    result.push({ kind: "code", language: match[1].trim(), code: match[2].replace(/\n$/, "") });
    position = expression.lastIndex;
  }
  if (position < content.length || result.length === 0) result.push({ kind: "markdown", trees: buildRichTrees(content.slice(position)) });
}

/** 将 Markdown 段落转换为手机 rich-text 所需的轻量富文本树。 */
function buildRichTrees(value: string): RichTree[] {
  const paragraphs = value.split(/\n{2,}/).filter(Boolean);
  return paragraphs.map((paragraph, index) => {
    const bottomMargin = index < paragraphs.length - 1 ? "8px" : "0";
    const heading = /^(#{1,6})\s+(.+)$/m.exec(paragraph);
    if (heading) return { name: "div", attrs: { style: `font-size:${18 - heading[1].length}px;font-weight:700;margin:5px 0 ${bottomMargin};` }, children: [{ type: "text", text: heading[2] }] };
    if (paragraph.startsWith("> ")) return { name: "div", attrs: { style: "padding-left:8px;border-left:3px solid #46d39a;color:#aeb8c7;" }, children: [{ type: "text", text: paragraph.slice(2) }] };
    return { name: "div", attrs: { style: `margin-bottom:${bottomMargin};white-space:pre-wrap;` }, children: [{ type: "text", text: paragraph }] };
  });
}
</script>

<template>
  <view class="message-content">
    <template v-for="(segment, index) in segments" :key="index">
      <view v-if="segment.kind === 'code'" class="code-block"><text v-if="segment.language" class="code-language">{{ segment.language }}</text><text selectable class="code-text">{{ segment.code }}</text></view>
      <rich-text v-else-if="segment.kind === 'markdown'" :nodes="segment.trees" selectable/>
      <view v-else-if="segment.kind === 'action'" class="action-card"><view class="action-heading"><text class="action-summary">{{ segment.summary }}</text><text class="action-status" :class="`action-status-${segment.status}`">{{ segment.status === 'pending' ? '等待执行' : segment.status === 'running' ? '执行中' : segment.status === 'error' ? '执行失败' : '已完成' }}</text></view><view v-if="segment.diffSummary" class="diff-summary"><text v-if="segment.diffSummary.fileCount > 0" class="diff-files">{{ segment.diffSummary.fileCount }} 个文件已更改</text><text v-if="segment.diffSummary.additions > 0" class="diff-additions">+{{ segment.diffSummary.additions }}</text><text v-if="segment.diffSummary.deletions > 0" class="diff-deletions">-{{ segment.diffSummary.deletions }}</text></view></view>
      <view v-else-if="segment.kind === 'diff'" class="diff-card"><text v-if="segment.summary.fileCount > 0" class="diff-files">{{ segment.summary.fileCount }} 个文件已更改</text><text v-if="segment.summary.additions > 0" class="diff-additions">+{{ segment.summary.additions }}</text><text v-if="segment.summary.deletions > 0" class="diff-deletions">-{{ segment.summary.deletions }}</text></view>
    </template>
  </view>
</template>

<style scoped>
.message-content { display: block; overflow-wrap: anywhere; }.code-block { margin: 8px 0; padding: 10px; overflow-x: auto; border-radius: 9px; color: #dce7f5; background: #090b10; }.code-language { display: block; margin-bottom: 6px; color: #7f91aa; font-family: monospace; font-size: 9px; }.code-text { display: block; font-family: monospace; font-size: 11px; line-height: 1.6; white-space: pre; }
.action-card { display: block; min-width: 0; margin: 8px 0; padding: 10px 11px; overflow: hidden; border: 1px solid var(--line); border-radius: 9px; background: var(--raised); }.action-heading { display: flex; min-width: 0; align-items: center; gap: 8px; }.action-summary { min-width: 0; overflow: hidden; color: var(--text); font-size: 12px; line-height: 18px; text-overflow: ellipsis; white-space: nowrap; }.action-status { flex-shrink: 0; font-size: 10px; }.action-status-pending { color: #f7c06e; }.action-status-running { color: var(--accent); }.action-status-done { color: var(--muted); }.action-status-error { color: #ff7f88; }.diff-card, .diff-summary { display: flex; min-width: 0; flex-wrap: wrap; align-items: center; gap: 7px; }.diff-card { margin: 8px 0; padding: 9px 11px; border: 1px solid var(--line); border-radius: 9px; background: var(--raised); }.diff-summary { margin-top: 8px; }.diff-files { min-width: 0; overflow: hidden; color: var(--text); font-size: 11px; text-overflow: ellipsis; white-space: nowrap; }.diff-additions { color: #46d39a; font-size: 11px; }.diff-deletions { color: #ff7f88; font-size: 11px; }
</style>
