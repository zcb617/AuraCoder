import { describe, expect, it } from "vitest";
import { formatTextAnnotationsForSubmission } from "./textAnnotations";

describe("formatTextAnnotationsForSubmission", () => {
  it("keeps ordinary input unchanged when no annotation is pending", () => {
    expect(formatTextAnnotationsForSubmission("  请继续处理  ", [])).toBe("请继续处理");
  });

  it("sends selected text, its annotation, and the typed message together", () => {
    expect(
      formatTextAnnotationsForSubmission("继续修复这个问题", [
        {
          id: "annotation-1",
          selectedText: "runId: abc123",
          comment: "这里的 runId 不应该写死",
        },
      ]),
    ).toBe(
      "以下是我对当前聊天内容的标注：\n\n" +
        "[聊天标注 1]\n选中内容：\nrunId: abc123\n\n" +
        "标注说明：\n这里的 runId 不应该写死\n[/聊天标注 1]\n\n" +
        "我的补充：\n继续修复这个问题",
    );
  });
});
