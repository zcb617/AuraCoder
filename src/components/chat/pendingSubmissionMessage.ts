import type {
  ChatAttachment,
  ChatInputReference,
  ContentBlock,
  Message,
} from "../../types";

export function createPendingSubmissionMessage(
  threadId: string,
  text: string,
  attachments: ChatAttachment[],
  references: ChatInputReference[],
  planMode: boolean,
): Message {
  const blocks: ContentBlock[] = [
    ...references.map((reference) => ({ ...reference })),
    ...attachments.map((attachment) => ({
      type: "attachment" as const,
      fileName: attachment.fileName,
      filePath: attachment.filePath,
      sizeBytes: attachment.sizeBytes,
      mimeType: attachment.mimeType,
      browserAnnotation: attachment.browserAnnotation,
    })),
  ];
  blocks.push({ type: "text", content: text, planMode: planMode || undefined });

  return {
    id: `pending-${crypto.randomUUID()}`,
    threadId,
    role: "user",
    content: text,
    blocks,
    status: "completed",
    schemaVersion: 1,
    createdAt: new Date().toISOString(),
    hydration: "full",
    hasDeferredContent: false,
  };
}
