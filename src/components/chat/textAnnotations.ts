import type { ChatTextAnnotation } from "../../types";

export function formatTextAnnotationsForSubmission(
  input: string,
  annotations: ReadonlyArray<ChatTextAnnotation>,
): string {
  const typedText = input.trim();
  if (annotations.length === 0) {
    return typedText;
  }

  const formattedAnnotations = annotations
    .map((annotation, index) =>
      "[聊天标注 " +
      String(index + 1) +
      "]\n选中内容：\n" +
      annotation.selectedText +
      "\n\n标注说明：\n" +
      annotation.comment +
      "\n[/聊天标注 " +
      String(index + 1) +
      "]",
    )
    .join("\n\n");

  if (!typedText) {
    return "以下是我对当前聊天内容的标注：\n\n" + formattedAnnotations;
  }

  return (
    "以下是我对当前聊天内容的标注：\n\n" +
    formattedAnnotations +
    "\n\n我的补充：\n" +
    typedText
  );
}
