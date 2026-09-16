import type {
  Dispatch,
  MouseEvent as ReactMouseEvent,
  RefObject,
  SetStateAction,
} from "react";
import { Loader2, MessageSquare } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { PendingFlexibleMessage } from "../../stores/chatComposerStore";
import type {
  ApprovalResponse,
  AttachmentBlock,
  ChatTextAnnotation,
  GitStatus,
  Message,
  Thread,
  Workspace,
} from "../../types";
import { FlexibleMessageGroup } from "./FlexibleMessageGroup";
import { MessageRow, WorkingDurationIndicator } from "./MessageRow";
import { MESSAGE_ROW_GAP } from "./chatPanelConstants";
import type { TextAnnotationPopover } from "./chatPanelTypes";

/** 消息区展示组件，负责渲染聊天消息、发送状态、拖拽提示和文本标注交互。 */
interface ChatMessageAreaProps {
  /** 当前工作区是否处于文件拖拽悬停状态。 */
  isFileDropOver: boolean;
  /** 当前聊天是否正在流式处理。 */
  streaming: boolean;
  /** 当前回合开始时间，用于展示消息处理时长。 */
  turnStartedAt: number | null;
  /** 消息滚动视口的 DOM 引用。 */
  viewportRef: RefObject<HTMLDivElement | null>;
  /** 当前激活的工作区标识。 */
  activeWorkspaceId: string | null;
  /** 当前激活的工作区数据。 */
  activeWorkspace: Workspace | null;
  /** 当前工作区展示名称。 */
  workspaceName: string;
  /** 当前工作区的 Git 状态。 */
  gitStatus: GitStatus | null | undefined;
  /** 当前会话的全部消息。 */
  messages: Message[];
  /** 当前滚动位置下需要展示的消息。 */
  visibleMessages: Message[];
  /** 尚未正式发送完成的待发送消息。 */
  pendingSubmission: Message | null;
  /** 按消息标识记录助手展示身份。 */
  assistantIdentityByMessageId: Map<
    string,
    {
      /** 助手在消息行中展示的名称。 */
      label: string;
      /** 助手实际使用的引擎标识。 */
      engineId: string;
      /** 助手实际使用的引擎名称。 */
      engineName: string;
    }
  >;
  /** 当前需要高亮展示的消息标识。 */
  highlightedMessageId: string | null;
  /** 当前激活的线程。 */
  activeThread: Thread | null;
  /** 当前聊天会话是否已准备完成。 */
  sessionReady: boolean;
  /** 当前是否正在准备附件。 */
  preparingAttachments: boolean;
  /** 当前正在准备的引擎标识。 */
  preparingEngineId: string | null;
  /** 当前是否存在等待确认的灵活消息。 */
  hasPendingFlexibleMessages: boolean;
  /** 等待确认的灵活消息列表。 */
  pendingFlexibleMessages: PendingFlexibleMessage[];
  /** 灵活消息确认操作是否禁用。 */
  flexibleMessageConfirmDisabled: boolean;
  /** 灵活消息确认按钮的提示标题。 */
  flexibleMessageConfirmTitle: string;
  /** 当前是否锁定自动滚动到最新消息。 */
  autoScrollLocked: boolean;
  /** 当前文本标注浮层状态。 */
  textAnnotationPopover: TextAnnotationPopover | null;
  /** 当前文本标注评论内容。 */
  textAnnotationComment: string;
  /** 文本标注浮层的 DOM 引用。 */
  textAnnotationPopoverRef: RefObject<HTMLDivElement | null>;
  /** 文本标注评论输入框的 DOM 引用。 */
  annotationCommentInputRef: RefObject<HTMLInputElement | null>;
  /** 聊天输入框的 DOM 引用。 */
  inputRef: RefObject<HTMLTextAreaElement | null>;
  /** 设置文本标注浮层状态。 */
  setTextAnnotationPopover: Dispatch<SetStateAction<TextAnnotationPopover | null>>;
  /** 设置文本标注评论内容。 */
  setTextAnnotationComment: Dispatch<SetStateAction<string>>;
  /** 更新当前工作区的文本标注列表。 */
  setTextAnnotations: (
    next: ChatTextAnnotation[] | ((cur: ChatTextAnnotation[]) => ChatTextAnnotation[]),
  ) => void;
  /** 设置是否锁定自动滚动。 */
  setAutoScrollLocked: Dispatch<SetStateAction<boolean>>;
  /** 将消息视口滚动到底部。 */
  scrollViewportToBottom: (behavior?: ScrollBehavior) => void;
  /** 使用指定文本重新编辑并发送消息。 */
  handleEditResend: (text: string) => void;
  /** 响应消息中的审批请求。 */
  handleApproval: (approvalId: string, response: ApprovalResponse) => void;
  /** 加载消息动作的输出内容。 */
  handleLoadActionOutput: (messageId: string, actionId: string) => Promise<void>;
  /** 打开指定文件的差异查看。 */
  handleOpenDiffFile: (filePath: string) => void;
  /** 打开消息中的图片附件。 */
  handleOpenImageAttachment: (attachment: AttachmentBlock) => void;
  /** 提交等待确认的灵活消息。 */
  submitFlexibleMessages: () => Promise<void>;
  /** 撤回指定的灵活消息。 */
  withdrawFlexibleMessage: (message: PendingFlexibleMessage) => void;
}

/** 展示当前聊天消息区及其拖拽、发送状态和文本标注交互。 */
export function ChatMessageArea({
  isFileDropOver,
  streaming,
  turnStartedAt,
  viewportRef,
  activeWorkspaceId,
  activeWorkspace,
  workspaceName,
  gitStatus,
  messages,
  visibleMessages,
  pendingSubmission,
  assistantIdentityByMessageId,
  highlightedMessageId,
  activeThread,
  sessionReady,
  preparingAttachments,
  preparingEngineId,
  hasPendingFlexibleMessages,
  pendingFlexibleMessages,
  flexibleMessageConfirmDisabled,
  flexibleMessageConfirmTitle,
  autoScrollLocked,
  textAnnotationPopover,
  textAnnotationComment,
  textAnnotationPopoverRef,
  annotationCommentInputRef,
  inputRef,
  setTextAnnotationPopover,
  setTextAnnotationComment,
  setTextAnnotations,
  setAutoScrollLocked,
  scrollViewportToBottom,
  handleEditResend,
  handleApproval,
  handleLoadActionOutput,
  handleOpenDiffFile,
  handleOpenImageAttachment,
  submitFlexibleMessages,
  withdrawFlexibleMessage,
}: ChatMessageAreaProps) {
  const { t } = useTranslation("chat");

  return <>
            {isFileDropOver && (
              <div
                style={{
                  position: "absolute",
                  inset: 12,
                  borderRadius: "var(--radius-md)",
                  border: "1px solid var(--info-border)",
                  background: "var(--info-surface)",
                  color: "var(--text-1)",
                  fontSize: 13,
                  fontWeight: 600,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  pointerEvents: "none",
                  zIndex: 5,
                }}
              >
                {t("panel.dropFiles")}
              </div>
            )}
            {/* ── Messages ── */}
            {streaming && turnStartedAt !== null && (
              <WorkingDurationIndicator startedAt={turnStartedAt} />
            )}
            <div
              ref={viewportRef}
              className="chat-message-viewport"
              onMouseUp={(event: ReactMouseEvent<HTMLDivElement>) => {
                if (!activeWorkspaceId || event.button !== 0) {
                  return;
                }
                const selection = window.getSelection();
                const selectedText = selection?.toString().trim() ?? "";
                const viewport = viewportRef.current;
                const target = event.target instanceof Element ? event.target : null;
                if (
                  !selection ||
                  selection.rangeCount === 0 ||
                  !selectedText ||
                  !viewport?.contains(selection.getRangeAt(0).commonAncestorContainer) ||
                  !target?.closest(".msg-row")
                ) {
                  setTextAnnotationPopover(null);
                  setTextAnnotationComment("");
                  return;
                }
                setTextAnnotationComment("");
                setTextAnnotationPopover({
                  selectedText,
                  left: Math.max(
                    12,
                    Math.min(event.clientX + 12, window.innerWidth - 292),
                  ),
                  top: Math.max(
                    12,
                    Math.min(event.clientY + 12, window.innerHeight - 152),
                  ),
                  stage: "actions",
                });
              }}
              style={{
                position: "relative",
                flex: 1,
                overflow: "auto",
                padding: "20px 24px",
              }}
            >
        {messages.length === 0 && !pendingSubmission ? (
          <div
            className="animate-fade-in"
            style={{
              height: "100%",
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              justifyContent: "center",
              gap: 14,
              color: "var(--text-3)",
              textAlign: "center",
            }}
          >
            <div className="chat-empty-tile">
              <MessageSquare size={18} />
            </div>
            <div>
              <p style={{ margin: "0 0 4px", fontSize: 14, fontWeight: 500, color: "var(--text-2)" }}>
                {t("panel.startConversation")}
              </p>
              <p style={{ margin: 0, fontSize: 12.5 }}>
                {activeWorkspaceId && gitStatus?.branch
                  ? t("panel.emptyScopeRepoBranch", { repo: workspaceName, branch: gitStatus.branch })
                  : t("panel.emptyHint")}
              </p>
            </div>
            {activeWorkspaceId && (
              <div className="chat-empty-suggestions">
                <button
                  type="button"
                  className="chat-empty-suggestion"
                  onClick={() => handleEditResend(t("panel.emptySuggestionSummarize"))}
                >
                  {t("panel.emptySuggestionSummarize")}
                </button>
                <button
                  type="button"
                  className="chat-empty-suggestion"
                  onClick={() => handleEditResend(t("panel.emptySuggestionTests"))}
                >
                  {t("panel.emptySuggestionTests")}
                </button>
              </div>
            )}
            <p style={{ margin: 0, fontSize: 11 }}>
              <span className="chat-empty-kbd">⌘K</span> {t("panel.emptyHintCommands")}
              <span style={{ opacity: 0.5, padding: "0 5px" }}>·</span>
              <span className="chat-empty-kbd">/</span> {t("panel.emptyHintSlash")}
            </p>
          </div>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: MESSAGE_ROW_GAP }}>
            {visibleMessages.map((message, index) => {
              const assistantIdentity = assistantIdentityByMessageId.get(message.id);
              return (
                <MessageRow
                  key={message.id}
                  message={message}
                  index={index}
                  isHighlighted={message.id === highlightedMessageId}
                  assistantLabel={assistantIdentity?.label ?? ""}
                  assistantEngineId={assistantIdentity?.engineId ?? ""}
                  assistantEngineName={assistantIdentity?.engineName ?? ""}
                  threadEngineId={activeThread?.engineId}
                  allowInitialPreparation={
                    !sessionReady && message.id === messages[messages.length - 1]?.id
                  }
                  allowTurnStartedThinking={
                    sessionReady && message.id === messages[messages.length - 1]?.id
                  }
                  preparingLabel={
                    activeWorkspace?.locationKind === "ssh" &&
                    preparingAttachments &&
                    message.id === messages[messages.length - 1]?.id
                      ? t("panel.uploadingRemoteAttachments")
                      : activeWorkspace?.locationKind === "ssh" &&
                          !sessionReady &&
                          preparingEngineId === "claude" &&
                          message.id === messages[messages.length - 1]?.id
                        ? t("panel.preparingRemoteEngine", { engine: "Claude Code" })
                        : undefined
                  }
                  onApproval={handleApproval}
                  onLoadActionOutput={handleLoadActionOutput}
                  onEditResend={handleEditResend}
                  onOpenDiffFile={handleOpenDiffFile}
                  onOpenImageAttachment={handleOpenImageAttachment}
                />
              );
            })}
          </div>
        )}

        {hasPendingFlexibleMessages && !pendingSubmission && (
          <div
            style={{
              display: "flex",
              flexDirection: "column",
              marginTop: messages.length > 0 ? MESSAGE_ROW_GAP : 0,
            }}
          >
            <FlexibleMessageGroup
              messages={pendingFlexibleMessages}
              confirmDisabled={flexibleMessageConfirmDisabled}
              confirmTitle={flexibleMessageConfirmTitle}
              onConfirm={() => void submitFlexibleMessages()}
              onWithdraw={withdrawFlexibleMessage}
            />
          </div>
        )}

        {pendingSubmission && !streaming && (
          <div
            style={{
              display: "flex",
              flexDirection: "column",
              gap: 6,
              marginTop: messages.length > 0 ? MESSAGE_ROW_GAP : 0,
            }}
          >
            <MessageRow
              message={pendingSubmission}
              index={messages.length}
              isHighlighted={false}
              assistantLabel=""
              assistantEngineId=""
              assistantEngineName=""
              threadEngineId={activeThread?.engineId}
              allowInitialPreparation={false}
              allowTurnStartedThinking={false}
              onApproval={handleApproval}
              onLoadActionOutput={handleLoadActionOutput}
              onOpenImageAttachment={handleOpenImageAttachment}
            />
            <div
              role="status"
              aria-live="polite"
              style={{
                display: "inline-flex",
                alignItems: "center",
                alignSelf: "flex-start",
                gap: 7,
                padding: "4px 14px 8px",
                color: "var(--text-3)",
                fontSize: 12,
              }}
            >
              <Loader2
                size={12}
                className="chat-send-spinner"
                aria-hidden="true"
                style={{ color: "var(--info)" }}
              />
              <span>{t("panel.sendingMessage")}</span>
            </div>
          </div>
        )}
            </div>

            {textAnnotationPopover && (
              <div
                ref={textAnnotationPopoverRef}
                className="chat-text-annotation-popover"
                style={{
                  left: textAnnotationPopover.left,
                  top: textAnnotationPopover.top,
                }}
                role="dialog"
                aria-label={t("panel.textAnnotations.dialogLabel")}
              >
                {textAnnotationPopover.stage === "actions" ? (
                  <button
                    type="button"
                    className="chat-text-annotation-add-button"
                    onClick={() => {
                      setTextAnnotationPopover((current) =>
                        current ? { ...current, stage: "comment" } : null,
                      );
                    }}
                  >
                    {t("panel.textAnnotations.addToChat")}
                  </button>
                ) : (
                  <form
                    className="chat-text-annotation-comment-form"
                    onSubmit={(event) => {
                      event.preventDefault();
                      const comment = textAnnotationComment.trim();
                      if (!comment) {
                        return;
                      }
                      setTextAnnotations((current) => [
                        ...current,
                        {
                          id: crypto.randomUUID(),
                          selectedText: textAnnotationPopover.selectedText,
                          comment,
                        },
                      ]);
                      setTextAnnotationPopover(null);
                      setTextAnnotationComment("");
                      window.getSelection()?.removeAllRanges();
                      inputRef.current?.focus();
                    }}
                  >
                    <input
                      ref={annotationCommentInputRef}
                      value={textAnnotationComment}
                      onChange={(event) => setTextAnnotationComment(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key !== "Escape") {
                          return;
                        }
                        event.preventDefault();
                        setTextAnnotationPopover(null);
                        setTextAnnotationComment("");
                      }}
                      placeholder={t("panel.textAnnotations.placeholder")}
                      aria-label={t("panel.textAnnotations.placeholder")}
                    />
                    <div className="chat-text-annotation-comment-actions">
                      <button
                        type="submit"
                        className="chat-text-annotation-confirm-button"
                        disabled={!textAnnotationComment.trim()}
                      >
                        {t("panel.textAnnotations.confirm")}
                      </button>
                      <button
                        type="button"
                        className="chat-text-annotation-cancel-button"
                        onClick={() => {
                          setTextAnnotationPopover(null);
                          setTextAnnotationComment("");
                        }}
                      >
                        {t("panel.textAnnotations.cancel")}
                      </button>
                    </div>
                  </form>
                )}
              </div>
            )}

  </>;
}
