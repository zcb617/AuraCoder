<script setup lang="ts">
import { computed } from "vue";
import type { Message } from "../types";

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
</script>

<template>
  <view class="message-content">
    <template v-for="(segment, index) in segments" :key="index">
      <view v-if="segment.kind === 'code'" class="code-block"><text v-if="segment.language" class="code-language">{{ segment.language }}</text><text selectable class="code-text">{{ segment.code }}</text></view>
      <rich-text v-else :nodes="segment.nodes || []" selectable/>
    </template>
  </view>
</template>

<style scoped>
.message-content { display: block; overflow-wrap: anywhere; }.code-block { margin: 8px 0; padding: 10px; overflow-x: auto; border-radius: 9px; color: #dce7f5; background: #090b10; }.code-language { display: block; margin-bottom: 6px; color: #7f91aa; font-family: monospace; font-size: 9px; }.code-text { display: block; font-family: monospace; font-size: 11px; line-height: 1.6; white-space: pre; }
</style>
