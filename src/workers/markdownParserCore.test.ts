import { describe, expect, it } from "vitest";
import {
  markdownParserCoreInternals,
  renderMarkdownToHtml,
} from "./markdownParserCore";

const { createStreamingMarkdownAppender } = markdownParserCoreInternals;

describe("renderMarkdownToHtml 代码块排版", () => {
  it("代码块输出为独立 <pre>，不被 <p> 包裹，前后段落各自成段", () => {
    const html = renderMarkdownToHtml("前文\n\n```js\nconst a = 1\n```\n\n后文\n\n");
    expect(html).toContain("<p>前文</p>");
    expect(html).toContain("<pre><code class=\"hljs language-js\">");
    expect(html).toContain("<p>后文</p>");
    expect(html).not.toContain("<p><pre>");
    expect(html).not.toContain("</pre>\n后文</p>");
  });
});

describe("createStreamingMarkdownAppender 纯 append 流式解析", () => {
  it("完整块锁死后，新增内容只解析增量并 append，已锁死 HTML 不被重算", () => {
    const appender = createStreamingMarkdownAppender();

    // 第一次：完整第一段。
    const first = appender.push("第一段\n\n");
    expect(first.html).toContain("第一段");

    // 第二次：完整第一段 + 完整第二段。已锁死的第一段 HTML 必须原样保留在结果前缀里。
    const second = appender.push("第一段\n\n第二段\n\n");
    expect(second.html).toContain("第一段");
    expect(second.html).toContain("第二段");
    // append 语义：第二次的 HTML 以第一次的 HTML 为前缀（已锁死部分不变）。
    expect(second.html.startsWith(first.html)).toBe(true);
  });

  it("未闭合代码围栏时尾巴憋住，闭合后增量 append", () => {
    const appender = createStreamingMarkdownAppender();

    // 推入未闭合围栏：稳定前缀是空，整段尾巴被憋住。
    const open = appender.push("前文\n\n```js\nconst a = 1\n");
    expect(open.html).toContain("前文");
    expect(open.html).not.toContain("const a = 1");

    // 闭合围栏后：代码块作为增量 append 上来（hljs 高亮后含 language-js class）。
    const closed = appender.push("前文\n\n```js\nconst a = 1\n```\n\n后文\n\n");
    expect(closed.html).toContain("language-js");
    expect(closed.html).toContain("后文");
    // 已锁死的"前文"部分仍是前缀。
    expect(closed.html.startsWith(open.html)).toBe(true);
  });

  it("输入不再增长（换消息）时 reset 重新解析", () => {
    const appender = createStreamingMarkdownAppender();
    appender.push("AAAA\n\nBBBB\n\n");
    const fresh = appender.push("全新的内容\n\n");
    expect(fresh.html).toContain("全新的内容");
    expect(fresh.html).not.toContain("AAAA");
  });

  it("重复 push 相同内容时不重复解析", () => {
    const appender = createStreamingMarkdownAppender();
    const first = appender.push("固定内容\n\n");
    const second = appender.push("固定内容\n\n");
    expect(second.html).toBe(first.html);
  });
});
