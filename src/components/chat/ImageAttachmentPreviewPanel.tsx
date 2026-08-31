import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type MouseEvent as ReactMouseEvent,
  type WheelEvent as ReactWheelEvent,
} from "react";
import { Check, Crosshair, Loader2, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { ipc } from "../../lib/ipc";
import {
  getChatComposerSessionKey,
  useChatComposerStore,
} from "../../stores/chatComposerStore";
import { useThreadStore } from "../../stores/threadStore";
import { useUiStore } from "../../stores/uiStore";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import type {
  AttachmentBlock,
  ChatAttachment,
  ImageAttachmentAnnotation,
} from "../../types";
import {
  fitAspectRatioWithinBox,
  imagePointFromClientPosition,
  isImageChatAttachment,
} from "./imageAttachmentAnnotations";

type PreviewRatio = "original" | "16:9" | "4:3" | "1:1";
type PreviewZoom = "fit" | number;
const PRESET_ZOOM_VALUES = [50, 75, 100, 125, 150, 200] as const;
const MIN_PREVIEW_ZOOM = 10;
const MAX_PREVIEW_ZOOM = 800;

/**
 * 将图片预览缩放比例规范为有限的整数百分比，并限制在业务允许的缩放范围内。
 */
function clampPreviewZoom(value: number): number {
  if (!Number.isFinite(value)) {
    return 100;
  }
  return Math.min(MAX_PREVIEW_ZOOM, Math.max(MIN_PREVIEW_ZOOM, Math.round(value)));
}

const RATIO_VALUE: Record<Exclude<PreviewRatio, "original">, number> = {
  "16:9": 16 / 9,
  "4:3": 4 / 3,
  "1:1": 1,
};
const EMPTY_CHAT_ATTACHMENTS: ChatAttachment[] = [];

interface AnnotationEditorState {
  annotationId: string | null;
  number: number;
  xPercent: number;
  yPercent: number;
  comment: string;
}

export function ImageAttachmentPreviewPanel() {
  const { t } = useTranslation("chat");
  const activeWorkspaceId = useWorkspaceStore((state) => state.activeWorkspaceId);
  /*
   * 旧项目级图片附件读取代码保留如下，不参与编译；当前实现按聊天会话范围读取：
   * const attachments = useChatComposerStore((state) =>
   *   activeWorkspaceId
   *     ? state.attachmentsByWorkspace[activeWorkspaceId] ?? EMPTY_CHAT_ATTACHMENTS
   *     : EMPTY_CHAT_ATTACHMENTS,
   * );
   * const setWorkspaceAttachments = useChatComposerStore(
   *   (state) => state.setWorkspaceAttachments,
   * );
   */
  const activeThread = useThreadStore((state) =>
    state.threads.find((thread) => thread.id === state.activeThreadId) ?? null,
  );
  const composerSessionKey = activeWorkspaceId
    ? getChatComposerSessionKey(
        activeWorkspaceId,
        activeThread?.workspaceId === activeWorkspaceId ? activeThread.id : null,
      )
    : null;
  const previewTarget = useUiStore((state) => state.imageAttachmentPreview);
  const attachments = useChatComposerStore((state) =>
    composerSessionKey
      ? state.attachmentsBySession[composerSessionKey] ?? EMPTY_CHAT_ATTACHMENTS
      : EMPTY_CHAT_ATTACHMENTS,
  );
  const setSessionAttachments = useChatComposerStore(
    (state) => state.setSessionAttachments,
  );
  const draftAttachment =
    previewTarget?.source === "draft" && previewTarget.workspaceId === activeWorkspaceId
      ? attachments.find((candidate) => candidate.id === previewTarget.attachmentId) ?? null
      : null;
  const messageAttachment: AttachmentBlock | null =
    previewTarget?.source === "message" && previewTarget.workspaceId === activeWorkspaceId
      ? previewTarget.attachment
      : null;
  const attachment = draftAttachment ?? messageAttachment;
  const isDraftPreview = draftAttachment !== null;
  const viewportRef = useRef<HTMLDivElement>(null);
  const cancelEditorRef = useRef(false);
  const [previewSrc, setPreviewSrc] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [previewError, setPreviewError] = useState<string | null>(null);
  const [naturalSize, setNaturalSize] = useState({ width: 0, height: 0 });
  const [viewportSize, setViewportSize] = useState({ width: 0, height: 0 });
  const [ratio, setRatio] = useState<PreviewRatio>("original");
  const [zoom, setZoom] = useState<PreviewZoom>("fit");
  const [annotationEnabled, setAnnotationEnabled] = useState(false);
  const [annotationEditor, setAnnotationEditor] = useState<AnnotationEditorState | null>(null);

  useEffect(() => {
    const viewport = viewportRef.current;
    if (!viewport) {
      return;
    }
    const observer = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect) {
        setViewportSize({ width: rect.width, height: rect.height });
      }
    });
    observer.observe(viewport);
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    let cancelled = false;
    setPreviewSrc(null);
    setPreviewError(null);
    setNaturalSize({ width: 0, height: 0 });
    setAnnotationEditor(null);
    setAnnotationEnabled(false);

    if (!attachment || !isImageChatAttachment(attachment)) {
      setLoading(false);
      return () => {
        cancelled = true;
      };
    }

    if (messageAttachment?.isRemote && !messageAttachment.previewFilePath) {
      setLoading(false);
      setPreviewError(t("imagePreview.unavailable"));
      return () => {
        cancelled = true;
      };
    }

    setLoading(true);
    void ipc.readAttachmentPreview(
      attachment.filePath,
      attachment.mimeType,
      messageAttachment?.previewFilePath,
    ).then(
      (preview) => {
        if (cancelled) {
          return;
        }
        if (!preview) {
          setPreviewError(t("imagePreview.unavailable"));
          return;
        }
        setPreviewSrc(`data:${preview.mimeType};base64,${preview.dataBase64}`);
      },
      (error) => {
        console.error(error);
        if (!cancelled) {
          setPreviewError(t("imagePreview.loadFailed"));
        }
      },
    ).finally(() => {
      if (!cancelled) {
        setLoading(false);
      }
    });

    return () => {
      cancelled = true;
    };
  }, [
    attachment?.filePath,
    attachment?.mimeType,
    messageAttachment?.previewFilePath,
    messageAttachment?.isRemote,
    previewTarget?.source,
    t,
  ]);

  const naturalRatio = naturalSize.width > 0 && naturalSize.height > 0
    ? naturalSize.width / naturalSize.height
    : 1;
  const selectedRatio = ratio === "original" ? naturalRatio : RATIO_VALUE[ratio];
  const frameSize = useMemo(
    () => fitAspectRatioWithinBox(
      Math.max(0, viewportSize.width - 24),
      Math.max(0, viewportSize.height - 24),
      selectedRatio,
    ),
    [selectedRatio, viewportSize.height, viewportSize.width],
  );
  const fittedImageSize = fitAspectRatioWithinBox(
    frameSize.width,
    frameSize.height,
    naturalRatio,
  );
  const fittedZoom = naturalSize.width > 0
    ? (fittedImageSize.width / naturalSize.width) * 100
    : 100;
  const imageSize = zoom === "fit"
    ? fittedImageSize
    : {
        width: naturalSize.width * (zoom / 100),
        height: naturalSize.height * (zoom / 100),
      };
  const annotations = draftAttachment?.imageAnnotations ?? [];
  const visibleAnnotations: ImageAttachmentAnnotation[] = annotationEditor?.annotationId === null
    ? [
        ...annotations,
        {
          id: "pending",
          number: annotationEditor.number,
          xPercent: annotationEditor.xPercent,
          yPercent: annotationEditor.yPercent,
          comment: annotationEditor.comment,
        },
      ]
    : annotations;

  const commitAnnotationEditor = () => {
    if (!annotationEditor || !draftAttachment || !activeWorkspaceId) {
      return;
    }
    const comment = annotationEditor.comment.trim();
    if (!comment) {
      setAnnotationEditor(null);
      return;
    }
    if (!composerSessionKey) {
      return;
    }
    /*
     * 旧项目级图片标注读写完整代码保留如下，不参与编译：
     * const currentAttachments =
     *   useChatComposerStore.getState().attachmentsByWorkspace[activeWorkspaceId] ?? [];
     * setWorkspaceAttachments(
     *   activeWorkspaceId,
     *   currentAttachments.map((candidate) => {
     *     if (candidate.id !== attachment.id) {
     *       return candidate;
     *     }
     *     const currentAnnotations = candidate.imageAnnotations ?? [];
     *     const nextAnnotation: ImageAttachmentAnnotation = {
     *       id: annotationEditor.annotationId ?? crypto.randomUUID(),
     *       number: annotationEditor.number,
     *       xPercent: annotationEditor.xPercent,
     *       yPercent: annotationEditor.yPercent,
     *       comment,
     *     };
     *     return {
     *       ...candidate,
     *       imageAnnotations: annotationEditor.annotationId
     *         ? currentAnnotations.map((annotation) =>
     *             annotation.id === annotationEditor.annotationId ? nextAnnotation : annotation,
     *           )
     *         : [...currentAnnotations, nextAnnotation],
     *     };
     *   }),
     * );
     */
    const currentAttachments =
      useChatComposerStore.getState().attachmentsBySession[composerSessionKey] ?? [];
    // 旧项目级附件读写已改为当前聊天会话范围，图片标注仍保持原有数据结构。
    setSessionAttachments(
      composerSessionKey,
      currentAttachments.map((candidate) => {
        if (candidate.id !== draftAttachment.id) {
          return candidate;
        }
        const currentAnnotations = candidate.imageAnnotations ?? [];
        const nextAnnotation: ImageAttachmentAnnotation = {
          id: annotationEditor.annotationId ?? crypto.randomUUID(),
          number: annotationEditor.number,
          xPercent: annotationEditor.xPercent,
          yPercent: annotationEditor.yPercent,
          comment,
        };
        return {
          ...candidate,
          imageAnnotations: annotationEditor.annotationId
            ? currentAnnotations.map((annotation) =>
                annotation.id === annotationEditor.annotationId ? nextAnnotation : annotation,
              )
            : [...currentAnnotations, nextAnnotation],
        };
      }),
    );
    setAnnotationEditor(null);
  };

  const handleImageClick = (event: ReactMouseEvent<HTMLDivElement>) => {
    if (!annotationEnabled || !isDraftPreview) {
      return;
    }
    if ((event.target as Element).closest("[data-image-annotation-control]")) {
      return;
    }
    const point = imagePointFromClientPosition(
      event.currentTarget.getBoundingClientRect(),
      event.clientX,
      event.clientY,
    );
    setAnnotationEditor({
      annotationId: null,
      number: annotations.reduce(
        (highest, annotation) => Math.max(highest, annotation.number),
        0,
      ) + 1,
      ...point,
      comment: "",
    });
  };

  /**
   * 仅处理图片预览区域内的 Ctrl+滚轮缩放，保留普通滚轮的原有浏览行为。
   */
  const handlePreviewWheel = (event: ReactWheelEvent<HTMLDivElement>) => {
    if (!event.ctrlKey) {
      return;
    }
    if (event.deltaY === 0) {
      return;
    }
    event.preventDefault();
    const currentZoom = zoom === "fit" ? fittedZoom : zoom;
    const step = Math.max(1, Math.round(currentZoom * 0.1));
    const nextZoom = currentZoom + (event.deltaY < 0 ? step : -step);
    setZoom(clampPreviewZoom(nextZoom));
  };

  return (
    <section className="image-attachment-preview-panel" aria-label={t("imagePreview.title")}>
      <header className="image-attachment-preview-toolbar">
        {isDraftPreview && (
          <button
            type="button"
            className={`image-attachment-preview-mark${annotationEnabled ? " active" : ""}`}
            onClick={() => {
              setAnnotationEnabled((enabled) => !enabled);
              setAnnotationEditor(null);
            }}
            aria-pressed={annotationEnabled}
            disabled={!previewSrc}
          >
            <Crosshair size={13} />
            {t("imagePreview.mark")}
          </button>
        )}
        <label className="image-attachment-preview-control">
          <span>{t("imagePreview.ratio")}</span>
          <select
            value={ratio}
            onChange={(event) => setRatio(event.target.value as PreviewRatio)}
          >
            <option value="original">{t("imagePreview.ratios.original")}</option>
            <option value="16:9">16:9</option>
            <option value="4:3">4:3</option>
            <option value="1:1">1:1</option>
          </select>
        </label>
        <label className="image-attachment-preview-control">
          <span>{t("imagePreview.zoom")}</span>
          <select
            value={String(zoom)}
            onChange={(event) => {
              const value = event.target.value;
              setZoom(value === "fit" ? "fit" : clampPreviewZoom(Number(value)));
            }}
          >
            <option value="fit">{t("imagePreview.fit")}</option>
            {zoom !== "fit" && !PRESET_ZOOM_VALUES.includes(zoom as (typeof PRESET_ZOOM_VALUES)[number]) ? (
              <option value={zoom}>{zoom}%</option>
            ) : null}
            {PRESET_ZOOM_VALUES.map((value) => (
              <option key={value} value={value}>{value}%</option>
            ))}
          </select>
        </label>
      </header>

      <div ref={viewportRef} className="image-attachment-preview-viewport">
        {!attachment ? (
          <div className="image-attachment-preview-empty">{t("imagePreview.empty")}</div>
        ) : loading ? (
          <div className="image-attachment-preview-empty">
            <Loader2 size={16} className="animate-spin" />
            {t("imagePreview.loading")}
          </div>
        ) : previewError ? (
          <div className="image-attachment-preview-empty image-attachment-preview-error">
            {previewError}
          </div>
        ) : previewSrc ? (
          <div
            className="image-attachment-preview-frame"
            style={{ width: frameSize.width, height: frameSize.height }}
            onWheel={handlePreviewWheel}
          >
            <div
              className="image-attachment-preview-stage"
              style={{
                width: Math.max(frameSize.width, imageSize.width),
                height: Math.max(frameSize.height, imageSize.height),
              }}
            >
              <div
                className={`image-attachment-preview-canvas${annotationEnabled ? " marking" : ""}`}
                style={{ width: imageSize.width, height: imageSize.height }}
                onClick={handleImageClick}
              >
                <img
                  src={previewSrc}
                  alt={attachment.fileName}
                  draggable={false}
                  onLoad={(event) => {
                    setNaturalSize({
                      width: event.currentTarget.naturalWidth,
                      height: event.currentTarget.naturalHeight,
                    });
                  }}
                  onError={() => setPreviewError(t("imagePreview.unsupported"))}
                />
                {visibleAnnotations.map((annotation) => (
                  <button
                    key={annotation.id}
                    type="button"
                    className="image-attachment-preview-marker"
                    style={{ left: `${annotation.xPercent}%`, top: `${annotation.yPercent}%` }}
                    data-image-annotation-control
                    onClick={(event) => {
                      event.stopPropagation();
                      const saved = annotations.find((candidate) => candidate.id === annotation.id);
                      if (saved) {
                        setAnnotationEditor({
                          annotationId: saved.id,
                          number: saved.number,
                          xPercent: saved.xPercent,
                          yPercent: saved.yPercent,
                          comment: saved.comment,
                        });
                      }
                    }}
                    aria-label={t("imagePreview.editMark", { number: annotation.number })}
                  >
                    {annotation.number}
                  </button>
                ))}
                {annotationEditor ? (
                  <form
                    className={`image-attachment-preview-editor${annotationEditor.xPercent > 58 ? " align-right" : ""}${annotationEditor.yPercent > 82 ? " align-top" : ""}`}
                    style={{
                      left: `${annotationEditor.xPercent}%`,
                      top: `${annotationEditor.yPercent}%`,
                    }}
                    data-image-annotation-control
                    onSubmit={(event) => {
                      event.preventDefault();
                      commitAnnotationEditor();
                    }}
                    onBlur={(event) => {
                      if (event.currentTarget.contains(event.relatedTarget as Node | null)) {
                        return;
                      }
                      if (cancelEditorRef.current) {
                        cancelEditorRef.current = false;
                        return;
                      }
                      commitAnnotationEditor();
                    }}
                  >
                    <input
                      autoFocus
                      value={annotationEditor.comment}
                      onChange={(event) => setAnnotationEditor({
                        ...annotationEditor,
                        comment: event.target.value,
                      })}
                      onKeyDown={(event) => {
                        if (event.key === "Escape") {
                          event.preventDefault();
                          cancelEditorRef.current = true;
                          setAnnotationEditor(null);
                        }
                      }}
                      placeholder={t("imagePreview.commentPlaceholder")}
                      aria-label={t("imagePreview.commentLabel", {
                        number: annotationEditor.number,
                      })}
                    />
                    <button type="submit" aria-label={t("imagePreview.saveMark")}>
                      <Check size={12} />
                    </button>
                    <button
                      type="button"
                      aria-label={t("imagePreview.cancelMark")}
                      onMouseDown={() => {
                        cancelEditorRef.current = true;
                      }}
                      onClick={() => setAnnotationEditor(null)}
                    >
                      <X size={12} />
                    </button>
                  </form>
                ) : null}
              </div>
            </div>
          </div>
        ) : null}
      </div>
    </section>
  );
}
