import { useEffect, useLayoutEffect, useRef, useState, type MouseEvent as ReactMouseEvent } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { ChevronRight, ClipboardCopy, Copy, FolderOpen, Save } from "lucide-react";
import { copyTextToClipboard } from "../../lib/clipboard";
import { normalizeAbsolutePath } from "../../lib/fileRootUtils";
import {
  classifyLinkTarget,
  navigateLinkTarget,
  resolveActiveWorkspaceLocalFileLinkTarget,
} from "../../lib/fileLinkNavigation";
import { ipc } from "../../lib/ipc";
import { parseLocalAbsolutePathTarget, parseLocalUrlTarget } from "../../lib/localFileLinkPatterns";
import { useChatComposerStore } from "../../stores/chatComposerStore";
import { toast } from "../../stores/toastStore";
import { getActionMenuPosition } from "../git/actionMenuPosition";

interface LocalFileContextMenuState {
  rawTarget: string;
  path: string;
  sourceLeafId: string | null;
  openInApp: (() => void) | null;
  triggerRect: {
    top: number;
    bottom: number;
    right: number;
  };
  top: number;
  left: number;
  submenuOnLeft: boolean;
}

export function useChatFileContextMenu() {
  const { t } = useTranslation("chat");
  const [fileContextMenu, setFileContextMenu] = useState<LocalFileContextMenuState | null>(null);
  const [openWithMenuVisible, setOpenWithMenuVisible] = useState(false);
  const [defaultFileOpenTarget, setDefaultFileOpenTarget] = useState<
    Awaited<ReturnType<typeof ipc.getDefaultFileOpenTarget>> | null
  >(null);
  const [defaultFileOpenTargetLoading, setDefaultFileOpenTargetLoading] = useState(false);
  const fileContextMenuRef = useRef<HTMLDivElement>(null);

  const closeFileContextMenu = () => {
    setFileContextMenu(null);
    setOpenWithMenuVisible(false);
  };

  useEffect(() => {
    if (!fileContextMenu) {
      return;
    }

    function closeOnOutsidePointerDown(event: PointerEvent) {
      if (fileContextMenuRef.current?.contains(event.target as Node)) {
        return;
      }
      closeFileContextMenu();
    }

    function closeOnContextMenu(event: MouseEvent) {
      if (fileContextMenuRef.current?.contains(event.target as Node)) {
        return;
      }
      closeFileContextMenu();
    }

    function closeOnEscape(event: KeyboardEvent) {
      if (event.key !== "Escape") {
        return;
      }
      event.stopPropagation();
      closeFileContextMenu();
    }

    document.addEventListener("pointerdown", closeOnOutsidePointerDown, true);
    document.addEventListener("contextmenu", closeOnContextMenu, true);
    document.addEventListener("keydown", closeOnEscape, true);
    document.addEventListener("scroll", closeFileContextMenu, true);
    return () => {
      document.removeEventListener("pointerdown", closeOnOutsidePointerDown, true);
      document.removeEventListener("contextmenu", closeOnContextMenu, true);
      document.removeEventListener("keydown", closeOnEscape, true);
      document.removeEventListener("scroll", closeFileContextMenu, true);
    };
  }, [fileContextMenu]);

  useLayoutEffect(() => {
    if (!fileContextMenu || !fileContextMenuRef.current) {
      return;
    }

    const position = getActionMenuPosition({
      triggerRect: fileContextMenu.triggerRect,
      menuWidth: fileContextMenuRef.current.offsetWidth,
      menuHeight: fileContextMenuRef.current.offsetHeight,
      viewportWidth: window.innerWidth,
      viewportHeight: window.innerHeight,
      horizontalPlacement: "after",
      verticalPlacement: "clamp",
    });
    if (position.top === fileContextMenu.top && position.left === fileContextMenu.left) {
      return;
    }
    setFileContextMenu((current) => current ? { ...current, ...position } : null);
  }, [fileContextMenu]);

  const openLocalFileContextMenu = (
    event: ReactMouseEvent<HTMLElement>,
    rawTarget: string,
    sourceLeafId: string | null,
    openInApp: (() => void) | null = null,
  ) => {
    if (classifyLinkTarget(rawTarget) !== "local") {
      return;
    }

    const localTarget = resolveActiveWorkspaceLocalFileLinkTarget(rawTarget);
    const directTarget = parseLocalAbsolutePathTarget(rawTarget) ?? parseLocalUrlTarget(rawTarget);
    const path = localTarget?.absolutePath ?? (directTarget
      ? normalizeAbsolutePath(directTarget.path)
      : null);
    if (!path) {
      return;
    }

    event.preventDefault();
    event.stopPropagation();
    const triggerRect = {
      top: event.clientY,
      bottom: event.clientY,
      right: event.clientX,
    };
    const position = getActionMenuPosition({
      triggerRect,
      menuWidth: 228,
      menuHeight: 264,
      viewportWidth: window.innerWidth,
      viewportHeight: window.innerHeight,
      horizontalPlacement: "after",
      verticalPlacement: "clamp",
    });
    setFileContextMenu({
      rawTarget,
      path,
      sourceLeafId,
      openInApp,
      triggerRect,
      top: position.top,
      left: position.left,
      submenuOnLeft: position.left + 448 > window.innerWidth,
    });
    setOpenWithMenuVisible(false);
    setDefaultFileOpenTarget(null);
    setDefaultFileOpenTargetLoading(true);
    void ipc.getDefaultFileOpenTarget().then(
      (target) => setDefaultFileOpenTarget(target),
      () => setDefaultFileOpenTarget(null),
    ).finally(() => setDefaultFileOpenTargetLoading(false));
  };

  const selectedApplication = defaultFileOpenTarget?.selectedEditorId
    ? defaultFileOpenTarget.applications.find(
        (application) => application.id === defaultFileOpenTarget.selectedEditorId,
      ) ?? null
    : null;
  const defaultOpenTargetName = selectedApplication?.name ?? t("fileContextMenu.systemDefaultApp");

  const contextMenu = fileContextMenu && createPortal(
    <div
      ref={fileContextMenuRef}
      className="git-action-menu chat-file-context-menu"
      style={{
        position: "fixed",
        top: fileContextMenu.top,
        left: fileContextMenu.left,
        minWidth: 228,
      }}
    >
      <button
        type="button"
        className="git-action-menu-item chat-file-context-menu-item"
        onClick={() => {
          const { rawTarget, sourceLeafId, openInApp } = fileContextMenu;
          closeFileContextMenu();
          if (openInApp) {
            openInApp();
            return;
          }
          void navigateLinkTarget(rawTarget, {
            shiftKey: useChatComposerStore.getState().linkOpenGesture === "shift-click",
            sourceLeafId,
          }).catch(() => toast.error(t("fileContextMenu.toasts.openFailed")));
        }}
      >
        <FolderOpen size={14} />
        {t("fileContextMenu.openFile")}
      </button>
      <button
        type="button"
        className="git-action-menu-item chat-file-context-menu-item"
        onClick={() => {
          const { path } = fileContextMenu;
          closeFileContextMenu();
          void ipc.openPathWithDefaultApp(path).catch(() => {
            toast.error(t("fileContextMenu.toasts.openFailed"));
          });
        }}
      >
        <FolderOpen size={14} />
        {t("fileContextMenu.openInTarget", { target: defaultOpenTargetName })}
      </button>
      <div
        className="chat-file-context-menu-open-with"
        onPointerLeave={() => setOpenWithMenuVisible(false)}
      >
        <button
          type="button"
          className="git-action-menu-item chat-file-context-menu-item"
          onPointerEnter={() => setOpenWithMenuVisible(true)}
          onClick={() => setOpenWithMenuVisible((visible) => !visible)}
        >
          {t("fileContextMenu.openWith")}
          <ChevronRight size={14} />
        </button>
        {openWithMenuVisible && (
          <div
            className="git-action-menu chat-file-context-menu-submenu"
            style={fileContextMenu.submenuOnLeft ? { right: "calc(100% - 3px)" } : undefined}
          >
            <button
              type="button"
              className="git-action-menu-item"
              onClick={() => {
                const { path } = fileContextMenu;
                closeFileContextMenu();
                void ipc.openPathWithTextEditor(path, null).catch(() => {
                  toast.error(t("fileContextMenu.toasts.openFailed"));
                });
              }}
            >
              {t("fileContextMenu.systemDefaultApp")}
            </button>
            {defaultFileOpenTargetLoading && (
              <button type="button" className="git-action-menu-item" disabled>
                {t("fileContextMenu.loadingTargets")}
              </button>
            )}
            {defaultFileOpenTarget?.applications.map((application) => (
              <button
                key={application.id}
                type="button"
                className="git-action-menu-item"
                onClick={() => {
                  const { path } = fileContextMenu;
                  closeFileContextMenu();
                  void ipc.openPathWithTextEditor(path, application.id).catch(() => {
                    toast.error(t("fileContextMenu.toasts.openFailed"));
                  });
                }}
              >
                {application.name}
              </button>
            ))}
          </div>
        )}
      </div>
      <div className="git-action-menu-divider" />
      <button
        type="button"
        className="git-action-menu-item chat-file-context-menu-item"
        onClick={() => {
          const { path } = fileContextMenu;
          closeFileContextMenu();
          void import("@tauri-apps/plugin-dialog").then(async ({ save }) => {
            const fileName = path.split(/[\\/]/).pop() || "file";
            const destinationPath = await save({
              title: t("fileContextMenu.saveDialogTitle"),
              defaultPath: fileName,
            });
            if (!destinationPath) {
              return;
            }
            await ipc.saveFileAs(path, destinationPath);
            toast.success(t("fileContextMenu.toasts.saved"));
          }).catch(() => toast.error(t("fileContextMenu.toasts.saveFailed")));
        }}
      >
        <Save size={14} />
        {t("fileContextMenu.saveAs")}
      </button>
      <button
        type="button"
        className="git-action-menu-item chat-file-context-menu-item"
        onClick={() => {
          const { path } = fileContextMenu;
          closeFileContextMenu();
          void copyTextToClipboard(path).then(
            () => toast.success(t("fileContextMenu.toasts.pathCopied")),
            () => toast.error(t("fileContextMenu.toasts.copyFailed")),
          );
        }}
      >
        <Copy size={14} />
        {t("fileContextMenu.copyPath")}
      </button>
      <button
        type="button"
        className="git-action-menu-item chat-file-context-menu-item"
        onClick={() => {
          const { path } = fileContextMenu;
          closeFileContextMenu();
          void ipc.readTextFileForClipboard(path).then(async (content) => {
            if (content === null) {
              return;
            }
            await copyTextToClipboard(content);
            toast.success(t("fileContextMenu.toasts.contentCopied"));
          }).catch(() => toast.error(t("fileContextMenu.toasts.copyFailed")));
        }}
      >
        <ClipboardCopy size={14} />
        {t("fileContextMenu.copyFileContents")}
      </button>
      <button
        type="button"
        className="git-action-menu-item chat-file-context-menu-item"
        onClick={() => {
          const { path } = fileContextMenu;
          closeFileContextMenu();
          void ipc.openContainingDirectory(path).catch(() => {
            toast.error(t("fileContextMenu.toasts.revealFailed"));
          });
        }}
      >
        <FolderOpen size={14} />
        {t("fileContextMenu.revealInFileExplorer")}
      </button>
    </div>,
    document.body,
  );

  return { openLocalFileContextMenu, contextMenu };
}
