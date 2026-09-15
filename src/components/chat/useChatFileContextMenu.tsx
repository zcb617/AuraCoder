import { useState, type MouseEvent as ReactMouseEvent } from "react";
import { useTranslation } from "react-i18next";
import { ClipboardCopy, Copy, FolderOpen, Save } from "lucide-react";
import { ContextMenu, type ContextMenuConfig, type ContextMenuItem } from "../common/ContextMenu";
import type { ContextMenuTriggerRect } from "../common/contextMenuTypes";
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

interface LocalFileContextMenuState {
  /** 原始本地文件链接，用于执行默认的打开文件行为。 */
  rawTarget: string;
  /** 解析后的本地文件绝对路径，用于文件系统相关操作。 */
  path: string;
  /** 触发菜单的工作区叶子节点标识，用于保持打开位置语义。 */
  sourceLeafId: string | null;
  /** 调用方提供的应用内打开文件行为。 */
  openInApp: (() => void) | null;
  /** 右键触发点的视口坐标，用于菜单定位。 */
  triggerRect: ContextMenuTriggerRect;
}

/**
 * 管理聊天本地文件链接的右键菜单，向现有调用方保持原有打开方法和菜单节点接口。
 */
export function useChatFileContextMenu() {
  const { t } = useTranslation("chat");
  const [fileContextMenu, setFileContextMenu] = useState<LocalFileContextMenuState | null>(null);
  const [defaultFileOpenTarget, setDefaultFileOpenTarget] = useState<
    Awaited<ReturnType<typeof ipc.getDefaultFileOpenTarget>> | null
  >(null);
  const [defaultFileOpenTargetLoading, setDefaultFileOpenTargetLoading] = useState(false);

  /** 关闭当前文件菜单并交由通用菜单骨架卸载 Portal。 */
  const closeFileContextMenu = () => {
    setFileContextMenu(null);
  };

  /** 解析本地文件链接并打开文件菜单，同时异步加载可选的外部应用列表。 */
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
    const triggerRect: ContextMenuTriggerRect = {
      top: event.clientY,
      bottom: event.clientY,
      right: event.clientX,
    };
    setFileContextMenu({
      rawTarget,
      path,
      sourceLeafId,
      openInApp,
      triggerRect,
    });
    setDefaultFileOpenTarget(null);
    setDefaultFileOpenTargetLoading(true);
    void ipc.getDefaultFileOpenTarget().then(
      (target) => setDefaultFileOpenTarget(target),
      () => setDefaultFileOpenTarget(null),
    ).finally(() => setDefaultFileOpenTargetLoading(false));
  };

  const activeFileContextMenu = fileContextMenu;
  const selectedApplication = defaultFileOpenTarget?.selectedEditorId
    ? defaultFileOpenTarget.applications.find(
        (application) => application.id === defaultFileOpenTarget.selectedEditorId,
      ) ?? null
    : null;
  const defaultOpenTargetName = selectedApplication?.name ?? t("fileContextMenu.systemDefaultApp");

  const openWithItems: ContextMenuItem[] = activeFileContextMenu
    ? [
        {
          id: "system-default-app",
          label: t("fileContextMenu.systemDefaultApp"),
          onSelect: () => {
            const { path } = activeFileContextMenu;
            closeFileContextMenu();
            void ipc.openPathWithTextEditor(path, null).catch(() => {
              toast.error(t("fileContextMenu.toasts.openFailed"));
            });
          },
        },
        ...(defaultFileOpenTargetLoading
          ? [{
              id: "loading-targets",
              label: t("fileContextMenu.loadingTargets"),
              disabled: true,
              loading: true,
            }]
          : []),
        ...(defaultFileOpenTarget?.applications.map((application) => ({
          id: `application-${application.id}`,
          label: application.name,
          onSelect: () => {
            const { path } = activeFileContextMenu;
            closeFileContextMenu();
            void ipc.openPathWithTextEditor(path, application.id).catch(() => {
              toast.error(t("fileContextMenu.toasts.openFailed"));
            });
          },
        })) ?? []),
      ]
    : [];

  const contextMenuConfig: ContextMenuConfig | null = activeFileContextMenu
    ? {
        onClose: closeFileContextMenu,
        x: activeFileContextMenu.triggerRect.right,
        y: activeFileContextMenu.triggerRect.bottom,
        triggerRect: activeFileContextMenu.triggerRect,
        minWidth: 228,
        estimatedHeight: 264,
        className: "chat-file-context-menu",
        itemClassName: "chat-file-context-menu-item",
        items: [
          {
            id: "open-file",
            label: t("fileContextMenu.openFile"),
            icon: <FolderOpen size={14} />,
            onSelect: () => {
              const { rawTarget, sourceLeafId, openInApp } = activeFileContextMenu;
              closeFileContextMenu();
              if (openInApp) {
                openInApp();
                return;
              }
              void navigateLinkTarget(rawTarget, {
                shiftKey: useChatComposerStore.getState().linkOpenGesture === "shift-click",
                sourceLeafId,
              }).catch(() => toast.error(t("fileContextMenu.toasts.openFailed")));
            },
          },
          {
            id: "open-in-default-app",
            label: t("fileContextMenu.openInTarget", { target: defaultOpenTargetName }),
            icon: <FolderOpen size={14} />,
            onSelect: () => {
              const { path } = activeFileContextMenu;
              closeFileContextMenu();
              void ipc.openPathWithDefaultApp(path).catch(() => {
                toast.error(t("fileContextMenu.toasts.openFailed"));
              });
            },
          },
          {
            id: "open-with",
            label: t("fileContextMenu.openWith"),
            children: openWithItems,
          },
          {
            id: "file-actions-divider",
            label: "",
            divider: true,
          },
          {
            id: "save-as",
            label: t("fileContextMenu.saveAs"),
            icon: <Save size={14} />,
            onSelect: () => {
              const { path } = activeFileContextMenu;
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
            },
          },
          {
            id: "copy-path",
            label: t("fileContextMenu.copyPath"),
            icon: <Copy size={14} />,
            onSelect: () => {
              const { path } = activeFileContextMenu;
              closeFileContextMenu();
              void copyTextToClipboard(path).then(
                () => toast.success(t("fileContextMenu.toasts.pathCopied")),
                () => toast.error(t("fileContextMenu.toasts.copyFailed")),
              );
            },
          },
          {
            id: "copy-file-contents",
            label: t("fileContextMenu.copyFileContents"),
            icon: <ClipboardCopy size={14} />,
            onSelect: () => {
              const { path } = activeFileContextMenu;
              closeFileContextMenu();
              void ipc.readTextFileForClipboard(path).then(async (content) => {
                if (content === null) {
                  return;
                }
                await copyTextToClipboard(content);
                toast.success(t("fileContextMenu.toasts.contentCopied"));
              }).catch(() => toast.error(t("fileContextMenu.toasts.copyFailed")));
            },
          },
          {
            id: "reveal-in-file-explorer",
            label: t("fileContextMenu.revealInFileExplorer"),
            icon: <FolderOpen size={14} />,
            onSelect: () => {
              const { path } = activeFileContextMenu;
              closeFileContextMenu();
              void ipc.openContainingDirectory(path).catch(() => {
                toast.error(t("fileContextMenu.toasts.revealFailed"));
              });
            },
          },
        ],
      }
    : null;

  const contextMenu = contextMenuConfig
    ? <ContextMenu config={contextMenuConfig} />
    : null;

  return { openLocalFileContextMenu, contextMenu };
}
