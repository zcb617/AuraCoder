import { useEffect, useState } from "react";
import { File, FileText, Image, X } from "lucide-react";
import { ipc } from "../../lib/ipc";
import { useChatFileContextMenu } from "./useChatFileContextMenu";

interface AttachmentChipData {
  fileName: string;
  filePath: string;
  previewFilePath?: string;
  sizeBytes?: number;
  mimeType?: string;
  isRemote?: boolean;
}

interface AttachmentChipProps {
  attachment: AttachmentChipData;
  compact?: boolean;
  showSize?: boolean;
  removeLabel?: string;
  onRemove?: () => void;
  onOpen?: () => void;
}

function getFileExtension(fileName: string): string {
  const lastDot = fileName.lastIndexOf(".");
  return lastDot >= 0 ? fileName.slice(lastDot + 1).toLowerCase() : "";
}

function guessAttachmentMimeType(fileName: string): string | undefined {
  switch (getFileExtension(fileName)) {
    case "png":
      return "image/png";
    case "jpg":
    case "jpeg":
      return "image/jpeg";
    case "gif":
      return "image/gif";
    case "webp":
      return "image/webp";
    case "bmp":
      return "image/bmp";
    case "tif":
    case "tiff":
      return "image/tiff";
    case "svg":
      return "image/svg+xml";
    default:
      return undefined;
  }
}

function getEffectiveMimeType(attachment: AttachmentChipData): string | undefined {
  const guessedMimeType = guessAttachmentMimeType(attachment.fileName);
  if (isImageAttachment(guessedMimeType) && !isImageAttachment(attachment.mimeType)) {
    return guessedMimeType;
  }
  return attachment.mimeType || guessedMimeType;
}

function isImageAttachment(mimeType?: string): boolean {
  return Boolean(mimeType?.toLowerCase().startsWith("image/"));
}

function getAttachmentIcon(mimeType?: string) {
  if (!mimeType) return File;
  const normalized = mimeType.toLowerCase();
  if (normalized.startsWith("image/")) return Image;
  if (
    normalized.startsWith("text/") ||
    normalized.includes("json") ||
    normalized.includes("javascript") ||
    normalized.includes("typescript")
  ) {
    return FileText;
  }
  return File;
}

export function AttachmentChip({
  attachment,
  compact = false,
  showSize = false,
  removeLabel,
  onRemove,
  onOpen,
}: AttachmentChipProps) {
  const effectiveMimeType = getEffectiveMimeType(attachment);
  const [thumbnailSrc, setThumbnailSrc] = useState<string | null>(null);
  const [thumbnailFailed, setThumbnailFailed] = useState(false);
  const { openLocalFileContextMenu, contextMenu } = useChatFileContextMenu();

  useEffect(() => {
    let cancelled = false;
    setThumbnailSrc(null);
    setThumbnailFailed(false);

    const hasLocalPreviewSource = Boolean(
      attachment.previewFilePath || (!attachment.isRemote && attachment.filePath),
    );
    if (!isImageAttachment(effectiveMimeType) || !hasLocalPreviewSource) {
      return () => {
        cancelled = true;
      };
    }

    ipc.readAttachmentPreview(
      attachment.filePath,
      effectiveMimeType,
      attachment.previewFilePath,
    )
      .then((preview) => {
        if (cancelled || !preview) {
          return;
        }
        setThumbnailSrc(`data:${preview.mimeType};base64,${preview.dataBase64}`);
      })
      .catch((error) => {
        console.error(error);
        if (!cancelled) {
          setThumbnailFailed(true);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [
    attachment.filePath,
    attachment.isRemote,
    attachment.previewFilePath,
    effectiveMimeType,
  ]);

  const IconComponent = getAttachmentIcon(effectiveMimeType);
  const sizeBytes = attachment.sizeBytes ?? 0;
  const canOpen = Boolean(onOpen && isImageAttachment(effectiveMimeType));
  const className = [
    "chat-attachment-chip",
    compact ? "chat-attachment-chip-compact" : "",
    thumbnailSrc && !thumbnailFailed ? "chat-attachment-chip-image" : "",
    canOpen ? "chat-attachment-chip-openable" : "",
  ]
    .filter(Boolean)
    .join(" ");

  return (
    <div
      className={className}
      role={canOpen ? "button" : undefined}
      tabIndex={canOpen ? 0 : undefined}
      onClick={canOpen ? onOpen : undefined}
      onKeyDown={canOpen ? (event) => {
        if (event.target !== event.currentTarget) {
          return;
        }
        if (event.key === "Enter" || event.key === " ") {
          event.preventDefault();
          onOpen?.();
        }
      } : undefined}
      onContextMenu={(event) => {
        if (!attachment.isRemote) {
          openLocalFileContextMenu(
            event,
            attachment.filePath,
            null,
            canOpen ? onOpen ?? null : null,
          );
        }
      }}
    >
      {thumbnailSrc && !thumbnailFailed ? (
        <img
          src={thumbnailSrc}
          alt=""
          className="chat-attachment-thumbnail"
          draggable={false}
          onError={() => setThumbnailFailed(true)}
        />
      ) : (
        <IconComponent size={compact ? 10 : 12} />
      )}
      <span className="chat-attachment-chip-name">{attachment.fileName}</span>
      {showSize && sizeBytes > 0 && (
        <span className="chat-attachment-chip-size">{formatFileSize(sizeBytes)}</span>
      )}
      {onRemove && (
        <button
          type="button"
          className="chat-attachment-chip-remove"
          onClick={(event) => {
            event.stopPropagation();
            onRemove();
          }}
          title={removeLabel}
          aria-label={removeLabel}
        >
          <X size={10} />
        </button>
      )}
      {contextMenu}
    </div>
  );
}

function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
