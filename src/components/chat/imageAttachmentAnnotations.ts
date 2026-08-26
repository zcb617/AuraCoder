import type { ChatAttachment } from "../../types";

const IMAGE_EXTENSION: Record<string, true> = {
  png: true,
  jpg: true,
  jpeg: true,
  gif: true,
  webp: true,
  bmp: true,
  tif: true,
  tiff: true,
  svg: true,
};

interface ImageBounds {
  left: number;
  top: number;
  width: number;
  height: number;
}

export function fitAspectRatioWithinBox(
  availableWidth: number,
  availableHeight: number,
  aspectRatio: number,
): { width: number; height: number } {
  if (availableWidth <= 0 || availableHeight <= 0 || aspectRatio <= 0) {
    return { width: 0, height: 0 };
  }
  if (availableWidth / availableHeight > aspectRatio) {
    return { width: availableHeight * aspectRatio, height: availableHeight };
  }
  return { width: availableWidth, height: availableWidth / aspectRatio };
}

function roundPercent(value: number): number {
  return Math.round(Math.min(100, Math.max(0, value)) * 10) / 10;
}

function formatPercent(value: number): string {
  return value.toFixed(1).replace(/\.0$/, "");
}

export function isImageChatAttachment(
  attachment: Pick<ChatAttachment, "fileName" | "mimeType">,
): boolean {
  if (attachment.mimeType?.toLowerCase().startsWith("image/")) {
    return true;
  }
  const dotIndex = attachment.fileName.lastIndexOf(".");
  return dotIndex >= 0
    && IMAGE_EXTENSION[attachment.fileName.slice(dotIndex + 1).toLowerCase()] === true;
}

export function imagePointFromClientPosition(
  bounds: ImageBounds,
  clientX: number,
  clientY: number,
): { xPercent: number; yPercent: number } {
  if (bounds.width <= 0 || bounds.height <= 0) {
    return { xPercent: 0, yPercent: 0 };
  }
  return {
    xPercent: roundPercent(((clientX - bounds.left) / bounds.width) * 100),
    yPercent: roundPercent(((clientY - bounds.top) / bounds.height) * 100),
  };
}

export function hasImageAttachmentAnnotations(
  attachments: ReadonlyArray<ChatAttachment>,
): boolean {
  return attachments.some((attachment) =>
    attachment.imageAnnotations?.some((annotation) => annotation.comment.trim().length > 0),
  );
}

export function stripImageAttachmentAnnotationsForSubmission(
  attachments: ReadonlyArray<ChatAttachment>,
): ChatAttachment[] {
  return attachments.map((attachment) => {
    const { imageAnnotations: _imageAnnotations, ...submissionAttachment } = attachment;
    return submissionAttachment;
  });
}

export function formatImageAttachmentAnnotationsForSubmission(
  input: string,
  attachments: ReadonlyArray<ChatAttachment>,
): string {
  const typedText = input.trim();
  const sections: string[] = [];
  let imageNumber = 0;

  for (const attachment of attachments) {
    if (!isImageChatAttachment(attachment)) {
      continue;
    }
    imageNumber += 1;
    const annotations = (attachment.imageAnnotations ?? []).filter(
      (annotation) => annotation.comment.trim().length > 0,
    );
    if (annotations.length === 0) {
      continue;
    }
    sections.push(
      `图像 ${imageNumber}:\n${annotations
        .map(
          (annotation) =>
            `${annotation.number}.(x:${formatPercent(annotation.xPercent)}%,y:${formatPercent(annotation.yPercent)}%)${annotation.comment.trim()}`,
        )
        .join("\n")}`,
    );
  }

  if (sections.length === 0) {
    return typedText;
  }
  return typedText ? `${typedText}\n\n${sections.join("\n\n")}` : sections.join("\n\n");
}
