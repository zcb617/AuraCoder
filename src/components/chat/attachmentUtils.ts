import type { TFunction } from "i18next";
import type { EngineModel } from "../../types";

const IMAGE_ATTACHMENT_EXTENSIONS = new Set([
  "png",
  "jpg",
  "jpeg",
  "gif",
  "webp",
  "bmp",
  "tif",
  "tiff",
  "svg",
]);
const TEXT_ATTACHMENT_EXTENSIONS = new Set([
  "txt",
  "md",
  "json",
  "js",
  "ts",
  "tsx",
  "jsx",
  "py",
  "rs",
  "go",
  "css",
  "html",
  "yaml",
  "yml",
  "toml",
  "xml",
  "sql",
  "sh",
  "csv",
]);
const CODEX_ATTACHMENT_EXTENSIONS = Array.from(
  new Set([...IMAGE_ATTACHMENT_EXTENSIONS, ...TEXT_ATTACHMENT_EXTENSIONS]),
);
const CLAUDE_TEXT_ATTACHMENT_EXTENSIONS = Array.from(
  new Set([...TEXT_ATTACHMENT_EXTENSIONS, "svg"]),
);
const CLAUDE_IMAGE_ATTACHMENT_EXTENSIONS = ["png", "jpg", "jpeg", "gif", "webp"];
const CLAUDE_ATTACHMENT_EXTENSIONS = Array.from(
  new Set([...CLAUDE_TEXT_ATTACHMENT_EXTENSIONS, ...CLAUDE_IMAGE_ATTACHMENT_EXTENSIONS]),
);
const PDF_ATTACHMENT_EXTENSIONS = ["pdf"];

export interface AttachmentFilterConfig {
  supportedExtensions: string[];
  textExtensions: string[];
  imageExtensions: string[];
  title: string;
  warningMessage: string;
  supportedLabel: string;
  imagesLabel: string;
  textFilesLabel: string;
}

export function attachmentExtensionsForModalities(modalities: string[]): {
  supportedExtensions: string[];
  textExtensions: string[];
  imageExtensions: string[];
} {
  const normalized = new Set(modalities.map((modality) => modality.trim().toLowerCase()));
  const textExtensions = normalized.has("text") ? [...TEXT_ATTACHMENT_EXTENSIONS] : [];
  const imageExtensions = normalized.has("image") ? [...IMAGE_ATTACHMENT_EXTENSIONS] : [];
  const pdfExtensions = normalized.has("pdf") ? PDF_ATTACHMENT_EXTENSIONS : [];

  return {
    supportedExtensions: Array.from(
      new Set([...textExtensions, ...imageExtensions, ...pdfExtensions]),
    ),
    textExtensions,
    imageExtensions,
  };
}

export function getAttachmentFilterConfig(
  t: TFunction<"chat">,
  engineId: string,
  model?: EngineModel | null,
): AttachmentFilterConfig | null {
  switch (engineId) {
    case "codex":
      return {
        supportedExtensions: CODEX_ATTACHMENT_EXTENSIONS,
        textExtensions: [...TEXT_ATTACHMENT_EXTENSIONS],
        imageExtensions: [...IMAGE_ATTACHMENT_EXTENSIONS],
        title: t("attachments.codexTitle"),
        warningMessage: t("attachments.codexWarning"),
        supportedLabel: t("attachments.filters.supportedFiles"),
        imagesLabel: t("attachments.filters.images"),
        textFilesLabel: t("attachments.filters.textFiles"),
      };
    case "claude":
      return {
        supportedExtensions: CLAUDE_ATTACHMENT_EXTENSIONS,
        textExtensions: CLAUDE_TEXT_ATTACHMENT_EXTENSIONS,
        imageExtensions: CLAUDE_IMAGE_ATTACHMENT_EXTENSIONS,
        title: t("attachments.claudeTitle"),
        warningMessage: t("attachments.claudeWarning"),
        supportedLabel: t("attachments.filters.supportedFiles"),
        imagesLabel: t("attachments.filters.images"),
        textFilesLabel: t("attachments.filters.textFiles"),
      };
    case "opencode": {
      const openCodeExtensions = attachmentExtensionsForModalities(
        model?.attachmentModalities ?? [],
      );
      return {
        supportedExtensions: openCodeExtensions.supportedExtensions,
        textExtensions: openCodeExtensions.textExtensions,
        imageExtensions: openCodeExtensions.imageExtensions,
        title: t("attachments.opencodeTitle"),
        warningMessage: t("attachments.opencodeWarning"),
        supportedLabel: t("attachments.filters.supportedFiles"),
        imagesLabel: t("attachments.filters.images"),
        textFilesLabel: t("attachments.filters.textFiles"),
      };
    }
    default:
      return null;
  }
}

export function getFileExtension(fileName: string): string {
  const lastDot = fileName.lastIndexOf(".");
  return lastDot >= 0 ? fileName.slice(lastDot + 1).toLowerCase() : "";
}

export function fileNameFromPath(filePath: string): string {
  return filePath.split("/").pop() ?? filePath.split("\\").pop() ?? filePath;
}

export function isSupportedAttachmentName(fileName: string, supportedExtensions: ReadonlySet<string>): boolean {
  const extension = getFileExtension(fileName);
  return supportedExtensions.has(extension);
}

export function guessMimeType(fileName: string): string | undefined {
  const ext = getFileExtension(fileName);
  const mimeMap: Record<string, string> = {
    txt: "text/plain",
    md: "text/markdown",
    json: "application/json",
    js: "text/javascript",
    ts: "text/typescript",
    tsx: "text/typescript",
    jsx: "text/javascript",
    py: "text/x-python",
    rs: "text/x-rust",
    go: "text/x-go",
    css: "text/css",
    html: "text/html",
    svg: "image/svg+xml",
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    gif: "image/gif",
    webp: "image/webp",
    pdf: "application/pdf",
    yaml: "text/yaml",
    yml: "text/yaml",
    toml: "text/toml",
    xml: "text/xml",
    sql: "text/x-sql",
    sh: "text/x-shellscript",
    csv: "text/csv",
  };
  return mimeMap[ext];
}

export function imageExtensionForMimeType(mimeType: string): string | null {
  switch (mimeType.toLowerCase()) {
    case "image/png":
      return "png";
    case "image/jpeg":
    case "image/jpg":
      return "jpg";
    case "image/gif":
      return "gif";
    case "image/webp":
      return "webp";
    case "image/bmp":
      return "bmp";
    case "image/tiff":
      return "tiff";
    case "image/svg+xml":
      return "svg";
    default:
      return null;
  }
}

export function fileNameForPastedImage(file: File, index: number): string {
  if (file.name.trim()) {
    return file.name.trim();
  }
  const extension = imageExtensionForMimeType(file.type) ?? "png";
  return `pasted-image-${index + 1}.${extension}`;
}

export function pastedImageFileSupported(file: File, supportedExtensions: ReadonlySet<string>): boolean {
  const mimeExtension = file.type ? imageExtensionForMimeType(file.type) : null;
  if (file.type && !mimeExtension) {
    return false;
  }
  const fileName = fileNameForPastedImage(file, 0);
  const extension = getFileExtension(fileName) || mimeExtension;
  return Boolean(extension && supportedExtensions.has(extension));
}

export function clipboardImageFiles(clipboardData: DataTransfer): File[] {
  const files: File[] = [];
  for (const item of Array.from(clipboardData.items)) {
    if (item.kind !== "file" || !item.type.toLowerCase().startsWith("image/")) {
      continue;
    }
    const file = item.getAsFile();
    if (file) {
      files.push(file);
    }
  }
  if (files.length > 0) {
    return files;
  }
  return Array.from(clipboardData.files).filter((file) =>
    file.type.toLowerCase().startsWith("image/"),
  );
}

export function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => {
      const result = typeof reader.result === "string" ? reader.result : "";
      const [, base64 = ""] = result.split(",", 2);
      resolve(base64);
    };
    reader.onerror = () => reject(reader.error ?? new Error("Failed to read image data."));
    reader.readAsDataURL(blob);
  });
}
