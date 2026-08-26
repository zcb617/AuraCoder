import { describe, expect, it } from "vitest";
import type { ChatAttachment, Message } from "../types";
import { nextBrowserAnnotationNumber } from "./browserAnnotationNumber";

function browserAttachment(number: number): ChatAttachment {
  return {
    id: `annotation-${number}`,
    fileName: `annotation-${number}.png`,
    filePath: `C:/tmp/annotation-${number}.png`,
    sizeBytes: 1,
    mimeType: "image/png",
    browserAnnotation: { comment: "说明", number },
  };
}

function message(threadId: string, attachments: ChatAttachment[]): Pick<Message, "threadId" | "blocks"> {
  return {
    threadId,
    blocks: attachments.map((attachment) => ({
      type: "attachment" as const,
      fileName: attachment.fileName,
      filePath: attachment.filePath,
      sizeBytes: attachment.sizeBytes,
      mimeType: attachment.mimeType,
      browserAnnotation: attachment.browserAnnotation,
    })),
  };
}

describe("nextBrowserAnnotationNumber", () => {
  it("starts a new conversation at one", () => {
    expect(nextBrowserAnnotationNumber({
      threadId: "thread-new",
      messages: [message("thread-other", [browserAttachment(4)])],
      pendingAttachments: [],
    })).toBe(1);
  });

  it("continues numbering from annotations already sent in the current conversation", () => {
    expect(nextBrowserAnnotationNumber({
      threadId: "thread-current",
      messages: [message("thread-current", [browserAttachment(1), browserAttachment(2)])],
      pendingAttachments: [],
    })).toBe(3);
  });

  it("continues numbering from annotations waiting in the current composer", () => {
    expect(nextBrowserAnnotationNumber({
      threadId: null,
      messages: [],
      pendingAttachments: [browserAttachment(1)],
    })).toBe(2);
  });
});
