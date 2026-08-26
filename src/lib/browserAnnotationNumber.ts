import type { ChatAttachment, Message } from "../types";

interface BrowserAnnotationNumberInput {
  threadId: string | null;
  messages: Array<Pick<Message, "threadId" | "blocks">>;
  pendingAttachments: Array<Pick<ChatAttachment, "browserAnnotation">>;
}

function highestAnnotationNumber(
  values: Array<Pick<ChatAttachment, "browserAnnotation">>,
): number {
  return values.reduce<number>((highest, value) => {
    const number = value.browserAnnotation?.number;
    return typeof number === "number" && Number.isSafeInteger(number) && number > highest
      ? number
      : highest;
  }, 0);
}

export function nextBrowserAnnotationNumber({
  threadId,
  messages,
  pendingAttachments,
}: BrowserAnnotationNumberInput): number {
  const sentAttachments = threadId
    ? messages
        .filter((message) => message.threadId === threadId)
        .flatMap((message) =>
          (message.blocks ?? []).flatMap((block) =>
            block.type === "attachment" ? [block] : [],
          ),
        )
    : [];

  return Math.max(
    highestAnnotationNumber(sentAttachments),
    highestAnnotationNumber(pendingAttachments),
  ) + 1;
}
