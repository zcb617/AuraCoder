import { useLayoutEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { ipc } from "../../lib/ipc";
import MarkdownContent from "../chat/MarkdownContent";

interface Props {
  /** Markdown 文件内容。 */
  content: string;
  /** 文件所属的工作区根目录。 */
  rootPath: string;
  /** Markdown 文件在根目录内的路径。 */
  filePath: string;
  /** 文件所属工作区标识。 */
  workspaceId: string | null;
}

/** 展示 Markdown 文件并加载其相对路径图片。 */
export function MarkdownPreviewPanel({ content, rootPath, filePath, workspaceId }: Props) {
  const containerRef = useRef<HTMLDivElement>(null);
  const { t } = useTranslation();

  useLayoutEffect(() => {
    // 当前预览轮次共享图片读取结果，并在切换文件时丢弃旧结果。
    const container = containerRef.current;
    if (!container) {
      return;
    }
    let cancelled = false;
    const images = new Map<string, Promise<string>>();
    // 图片读取或解码失败时显示原始说明和具体原因。
    const showImageError = (img: HTMLImageElement, source: string, error: unknown) => {
      console.error("Markdown 图片加载失败", error);
      if (!cancelled && container.contains(img)) {
        const message = document.createElement("span");
        const alt = img.getAttribute("alt") || "";
        message.textContent = `${alt}（${t("editor.markdownImageLoadFailed")}）`;
        message.title = `${source}: ${String(error)}`;
        img.replaceWith(message);
      }
    };
    container.querySelectorAll("img").forEach((img) => {
      const source = img.dataset.markdownImageSource ?? img.getAttribute("src");
      if (!source || source.startsWith("#") || source.startsWith("//") || /^[a-z][a-z0-9+.-]*:/i.test(source)) {
        return;
      }
      img.dataset.markdownImageSource = source;
      img.removeAttribute("src");
      let image = images.get(source);
      if (!image) {
        image = ipc.readMarkdownImage(rootPath, filePath, source, workspaceId);
        images.set(source, image);
      }
      void image.then((dataUrl) => {
        if (!cancelled && container.contains(img)) {
          img.onerror = () => showImageError(img, source, new Error("图片解码失败"));
          img.src = dataUrl;
        }
      }).catch((error: unknown) => {
        showImageError(img, source, error);
      });
    });
    return () => {
      cancelled = true;
    };
  }, [content, rootPath, filePath, workspaceId, t]);

  return (
    <div className="editor-markdown-preview">
      <div className="editor-markdown-preview-scroll" ref={containerRef}>
        <MarkdownContent
          key={JSON.stringify([workspaceId, rootPath, filePath])}
          content={content}
          className="prose editor-markdown-preview-prose"
        />
      </div>
    </div>
  );
}
