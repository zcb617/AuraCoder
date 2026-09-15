import type { Dispatch, RefObject, SetStateAction } from "react";
import { FilePen, MessageSquare, Monitor, SquareTerminal } from "lucide-react";
import { useTranslation } from "react-i18next";
import { handleDragDoubleClick, handleDragMouseDown } from "../../lib/windowDrag";
import type { Thread } from "../../types";
import type { LayoutMode } from "../../stores/terminalStore";

interface ChatTitlebarProps {
  /** 当前聊天面板是否以内嵌模式展示。 */
  embedded?: boolean;
  /** 当前是否处于 Focus 模式。 */
  focusMode: boolean;
  /** 当前是否显示侧边栏。 */
  showSidebar: boolean;
  /** 当前是否展示 Focus 模式标题栏。 */
  showFocusModeHeader: boolean;
  /** 当前窗口标题栏是否使用安全区域内边距。 */
  useTitlebarSafeInset: boolean;
  /** 当前窗口是否使用自定义窗口边框。 */
  customWindowFrame: boolean;
  /** 当前工作区名称，用于标题栏面包屑。 */
  workspaceName: string;
  /** 当前是否正在编辑线程标题。 */
  editingThreadTitle: boolean;
  /** 当前正在编辑的线程标题草稿。 */
  threadTitleDraft: string;
  /** 线程标题输入框引用，用于聚焦和选中文本。 */
  titleInputRef: RefObject<HTMLInputElement | null>;
  /** 当前活动线程，用于展示和编辑线程标题。 */
  activeThread: Thread | null;
  /** 当前工作区变更文件数量。 */
  totalAdded: number;
  /** 当前工作区布局模式。 */
  layoutMode: LayoutMode;
  /** 当前活动工作区标识，用于切换布局。 */
  activeWorkspaceId: string | null;
  /** 聊天布局是否处于激活状态。 */
  isChatLayoutActive: boolean;
  /** 分栏布局是否处于激活状态。 */
  isSplitLayoutActive: boolean;
  /** 终端布局是否处于激活状态。 */
  isTerminalLayoutActive: boolean;
  /** 文件编辑器布局是否处于激活状态。 */
  isEditorLayoutActive: boolean;
  /** 设置指定工作区布局模式的业务回调。 */
  setLayoutMode: (workspaceId: string, mode: LayoutMode) => Promise<void>;
  /** 开始编辑当前线程标题的业务回调。 */
  startThreadTitleEdit: () => void;
  /** 保存当前线程标题编辑结果的业务回调。 */
  saveThreadTitleEdit: () => Promise<void>;
  /** 取消当前线程标题编辑的业务回调。 */
  cancelThreadTitleEdit: () => void;
  /** 更新线程标题草稿的状态 setter。 */
  setThreadTitleDraft: Dispatch<SetStateAction<string>>;
}

/** 展示普通模式和 Focus 模式聊天标题栏，并承载标题编辑与布局切换业务。 */
export function ChatTitlebar({
  embedded,
  focusMode,
  showSidebar,
  showFocusModeHeader,
  useTitlebarSafeInset,
  customWindowFrame,
  workspaceName,
  editingThreadTitle,
  threadTitleDraft,
  titleInputRef,
  activeThread,
  totalAdded,
  layoutMode,
  activeWorkspaceId,
  isChatLayoutActive,
  isSplitLayoutActive,
  isTerminalLayoutActive,
  isEditorLayoutActive,
  setLayoutMode,
  startThreadTitleEdit,
  saveThreadTitleEdit,
  cancelThreadTitleEdit,
  setThreadTitleDraft,
}: ChatTitlebarProps) {
  const { t } = useTranslation("chat");

  return (
    <>
      {!embedded && (!focusMode || showSidebar) && (
        <div
          onMouseDown={handleDragMouseDown}
          onDoubleClick={handleDragDoubleClick}
          style={{
            height: "var(--panel-header-height)",
            padding: "0 16px",
            paddingLeft: showSidebar ? 16 : (customWindowFrame ? 16 : 80),
            display: "flex",
            alignItems: "center",
            gap: 8,
            borderBottom: "1px solid var(--border)",
            flexShrink: 0,
          }}
        >
          {/* Breadcrumb: workspace / thread title / +N files */}
          <div className="no-drag" style={{ flex: 1, display: "flex", alignItems: "center", gap: 0, minWidth: 0 }}>
            {workspaceName && (
              <>
                <span
                  style={{
                    fontSize: 12,
                    color: "var(--text-3)",
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {workspaceName}
                </span>
                <span style={{ fontSize: 12, color: "var(--border)", margin: "0 6px", flexShrink: 0 }}>/</span>
              </>
            )}
            {editingThreadTitle && activeThread ? (
              <input
                ref={titleInputRef}
                value={threadTitleDraft}
                onChange={(event) => setThreadTitleDraft(event.target.value)}
                onBlur={cancelThreadTitleEdit}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void saveThreadTitleEdit();
                    return;
                  }
                  if (event.key === "Escape") {
                    event.preventDefault();
                    cancelThreadTitleEdit();
                  }
                }}
                style={{
                  minWidth: 120,
                  width: "100%",
                  fontSize: 13.5,
                  fontWeight: 600,
                  letterSpacing: "-0.01em",
                  color: "var(--text-1)",
                  background: "var(--bg-3)",
                  border: "1px solid var(--border-active)",
                  borderRadius: "var(--radius-sm)",
                  padding: "4px 8px",
                }}
              />
            ) : (
              <button
                type="button"
                onClick={startThreadTitleEdit}
                disabled={!activeThread}
                title={activeThread ? t("panel.renameThread") : ""}
                style={{
                  border: "none",
                  background: "transparent",
                  padding: "2px 6px",
                  margin: 0,
                  fontSize: 13.5,
                  fontWeight: 600,
                  letterSpacing: "-0.01em",
                  color: "var(--text-1)",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  cursor: activeThread ? "text" : "default",
                  textAlign: "left",
                  borderRadius: "var(--radius-sm)",
                  transition: "background var(--duration-fast) var(--ease-out)",
                }}
                onMouseEnter={(e) => {
                  if (activeThread) e.currentTarget.style.background = "var(--wash-04)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "transparent";
                }}
              >
                {activeThread?.title || (
                  layoutMode === "terminal" ? t("panel.threadTitle.terminal")
                  : layoutMode === "editor" ? t("panel.threadTitle.fileEditor")
                  : layoutMode === "split" ? t("panel.threadTitle.newChat")
                  : t("panel.threadTitle.newChat")
                )}
              </button>
            )}
            {totalAdded > 0 && (
              <>
                <span style={{ fontSize: 12, color: "var(--border)", margin: "0 6px", flexShrink: 0 }}>/</span>
                <span
                  style={{
                    fontSize: 11,
                    fontFamily: '"Geist Mono", ui-monospace, monospace',
                    color: "var(--warning)",
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {t("panel.changedFiles", { count: totalAdded })}
                </span>
              </>
            )}
          </div>

          {/* Right-side action buttons */}
          {!embedded && (
          <div className="no-drag" style={{ display: "flex", alignItems: "center", gap: 4 }}>
            <div className="layout-mode-switcher">
              <button
                type="button"
                title={t("panel.layout.chatOnly")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "chat")}
                className={`layout-mode-btn ${isChatLayoutActive ? "active" : ""}`}
              >
                <MessageSquare size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.splitView")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "split")}
                className={`layout-mode-btn ${isSplitLayoutActive ? "active" : ""}`}
              >
                <Monitor size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.terminalOnly")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "terminal")}
                className={`layout-mode-btn ${isTerminalLayoutActive ? "active" : ""}`}
              >
                <SquareTerminal size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.fileEditor")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "editor")}
                className={`layout-mode-btn ${isEditorLayoutActive ? "active" : ""}`}
              >
                <FilePen size={12} />
              </button>
            </div>
          </div>
          )}
        </div>
      )}

      {showFocusModeHeader && (
        <div
          className="chat-focus-header"
          onMouseDown={handleDragMouseDown}
          onDoubleClick={handleDragDoubleClick}
        >
          <div
            className="chat-focus-header-leading"
            style={{ width: useTitlebarSafeInset ? 74 : 16 }}
          />

          <div className="chat-focus-header-content no-drag" style={{ flex: 1, display: "flex", alignItems: "center", gap: 0, minWidth: 0 }}>
            {workspaceName && (
              <>
                <span
                  style={{
                    fontSize: 12,
                    color: "var(--text-3)",
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {workspaceName}
                </span>
                <span style={{ fontSize: 12, color: "var(--border)", margin: "0 6px", flexShrink: 0 }}>/</span>
              </>
            )}
            {editingThreadTitle && activeThread ? (
              <input
                ref={titleInputRef}
                value={threadTitleDraft}
                onChange={(event) => setThreadTitleDraft(event.target.value)}
                onBlur={cancelThreadTitleEdit}
                onKeyDown={(event) => {
                  if (event.key === "Enter") {
                    event.preventDefault();
                    void saveThreadTitleEdit();
                    return;
                  }
                  if (event.key === "Escape") {
                    event.preventDefault();
                    cancelThreadTitleEdit();
                  }
                }}
                style={{
                  minWidth: 120,
                  width: "100%",
                  fontSize: 13.5,
                  fontWeight: 600,
                  letterSpacing: "-0.01em",
                  color: "var(--text-1)",
                  background: "var(--bg-3)",
                  border: "1px solid var(--border-active)",
                  borderRadius: "var(--radius-sm)",
                  padding: "4px 8px",
                }}
              />
            ) : (
              <button
                type="button"
                onClick={startThreadTitleEdit}
                disabled={!activeThread}
                title={activeThread ? t("panel.renameThread") : ""}
                style={{
                  border: "none",
                  background: "transparent",
                  padding: "2px 6px",
                  margin: 0,
                  fontSize: 13.5,
                  fontWeight: 600,
                  letterSpacing: "-0.01em",
                  color: "var(--text-1)",
                  overflow: "hidden",
                  textOverflow: "ellipsis",
                  whiteSpace: "nowrap",
                  cursor: activeThread ? "text" : "default",
                  textAlign: "left",
                  borderRadius: "var(--radius-sm)",
                  transition: "background var(--duration-fast) var(--ease-out)",
                }}
                onMouseEnter={(e) => {
                  if (activeThread) e.currentTarget.style.background = "var(--wash-04)";
                }}
                onMouseLeave={(e) => {
                  e.currentTarget.style.background = "transparent";
                }}
              >
                {activeThread?.title || (
                  layoutMode === "split" ? t("panel.threadTitle.newChat")
                  : t("panel.threadTitle.newChat")
                )}
              </button>
            )}
            {totalAdded > 0 && (
              <>
                <span style={{ fontSize: 12, color: "var(--border)", margin: "0 6px", flexShrink: 0 }}>/</span>
                <span
                  style={{
                    fontSize: 11,
                    fontFamily: '"Geist Mono", ui-monospace, monospace',
                    color: "var(--warning)",
                    whiteSpace: "nowrap",
                    flexShrink: 0,
                  }}
                >
                  {t("panel.changedFiles", { count: totalAdded })}
                </span>
              </>
            )}
          </div>

          {!embedded && (
          <div className="no-drag" style={{ display: "flex", alignItems: "center", gap: 4 }}>
            <div className="layout-mode-switcher">
              <button
                type="button"
                title={t("panel.layout.chatOnly")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "chat")}
                className={`layout-mode-btn ${isChatLayoutActive ? "active" : ""}`}
              >
                <MessageSquare size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.splitView")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "split")}
                className={`layout-mode-btn ${isSplitLayoutActive ? "active" : ""}`}
              >
                <Monitor size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.terminalOnly")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "terminal")}
                className={`layout-mode-btn ${isTerminalLayoutActive ? "active" : ""}`}
              >
                <SquareTerminal size={12} />
              </button>
              <button
                type="button"
                title={t("panel.layout.fileEditor")}
                disabled={!activeWorkspaceId}
                onClick={() => activeWorkspaceId && void setLayoutMode(activeWorkspaceId, "editor")}
                className={`layout-mode-btn ${isEditorLayoutActive ? "active" : ""}`}
              >
                <FilePen size={12} />
              </button>
            </div>
          </div>
          )}
        </div>
      )}

    </>
  );
}
