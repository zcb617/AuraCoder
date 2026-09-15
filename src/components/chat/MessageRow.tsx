import {
  memo,
  useCallback,
  useEffect,
  useMemo,
  useState,
} from "react";
import {
  AtSign,
  BookOpen,
  Brain,
  Check,
  Compass,
  Copy,
  DollarSign,
  Eye,
  Lightbulb,
  ListChecks,
  Loader2,
  Pencil,
  Search,
  Sparkles,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import type { ApprovalResponse, AttachmentBlock, Message } from "../../types";
import { getHarnessIcon } from "../shared/HarnessLogos";
import { formatMessageTimestamp, hasVisibleContent } from "./formatters";
import { MessageBlocks } from "./MessageBlocks";
import { AttachmentChip } from "./AttachmentChip";
import { formatWorkingDuration } from "./workingDuration";

/** 消息行组件的输入参数，负责描述消息展示及消息行内操作所需的业务数据。 */
export interface MessageRowProps {
  /** 当前需要展示的消息数据。 */
  message: Message;
  /** 当前消息在消息列表中的位置，用于计算入场动画延迟。 */
  index: number;
  /** 当前消息是否处于搜索或导航高亮状态。 */
  isHighlighted: boolean;
  /** 当前助手消息展示的助手标签。 */
  assistantLabel: string;
  /** 当前助手消息实际使用的引擎标识。 */
  assistantEngineId: string;
  /** 当前消息回合实际使用的 CLI 名称，用于运行状态文案。 */
  assistantEngineName: string;
  /** 允许当前尾部空流式助手显示首轮会话准备占位。 */
  allowInitialPreparation: boolean;
  /** 允许当前尾部空流式助手在会话就绪后显示已收到消息并思考状态。 */
  allowTurnStartedThinking: boolean;
  /** 当前会话准备阶段的文案。 */
  preparingLabel?: string;
  /** 用户提交审批响应时执行的业务回调。 */
  onApproval: (approvalId: string, response: ApprovalResponse) => void;
  /** 用户请求加载动作输出时执行的业务回调。 */
  onLoadActionOutput: (messageId: string, actionId: string) => Promise<void>;
  /** 用户编辑并重新发送消息时执行的业务回调。 */
  onEditResend?: (text: string) => void;
  /** 用户打开差异文件时执行的业务回调。 */
  onOpenDiffFile?: (filePath: string) => void;
  /** 用户打开图片附件时执行的业务回调。 */
  onOpenImageAttachment?: (attachment: AttachmentBlock) => void;
}

const THINKING_VARIANTS = [
  { icon: Brain, key: "thinkingVariants.thinking" },
  { icon: Lightbulb, key: "thinkingVariants.reasoning" },
  { icon: Eye, key: "thinkingVariants.analyzing" },
  { icon: Compass, key: "thinkingVariants.exploring" },
  { icon: Search, key: "thinkingVariants.researching" },
  { icon: Sparkles, key: "thinkingVariants.generating" },
  { icon: BookOpen, key: "thinkingVariants.reading" },
  { icon: Brain, key: "thinkingVariants.considering" },
] as const;

/** 在助手思考占位期间轮换展示思考状态图标和文案。 */
function useThinkingVariant(active: boolean) {
  const [index, setIndex] = useState(() => Math.floor(Math.random() * THINKING_VARIANTS.length));
  useEffect(() => {
    if (!active) return;
    const interval = setInterval(() => {
      setIndex((i) => (i + 1) % THINKING_VARIANTS.length);
    }, 4000);
    return () => clearInterval(interval);
  }, [active]);
  return THINKING_VARIANTS[index];
}

/** 从消息块中提取可复制的纯文本或代码内容。 */
function extractMessageCopyText(message: Message): string {
  if (message.role === "user") {
    if (message.content) return message.content;
    return (message.blocks ?? [])
      .filter((b) => b.type === "text")
      .map((b) => String(b.content ?? ""))
      .join("\n");
  }
  return (message.blocks ?? [])
    .filter((b) => b.type === "text" || b.type === "code")
    .map((b) => {
      if (b.type === "code") return `\`\`\`${b.language ?? ""}\n${b.content ?? ""}\n\`\`\``;
      return String(b.content ?? "");
    })
    .join("\n\n");
}

/** 提供消息文本复制操作并反馈复制成功状态。 */
function MessageCopyButton({ message }: { message: Message }) {
  const [copied, setCopied] = useState(false);
  const handleCopy = useCallback(() => {
    const text = extractMessageCopyText(message);
    if (!text) return;
    navigator.clipboard.writeText(text).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    });
  }, [message]);
  return (
    <button
      type="button"
      onClick={handleCopy}
      style={{
        cursor: "pointer",
        background: "none",
        border: "none",
        padding: "2px 4px",
        display: "inline-flex",
        alignItems: "center",
        color: copied ? "var(--success)" : "var(--text-3)",
      }}
      aria-label="Copy message"
    >
      {copied ? <Check size={11} /> : <Copy size={11} />}
    </button>
  );
}

/** 展示当前助手回合已持续工作的时长。 */
export function WorkingDurationIndicator({ startedAt }: { startedAt: number }) {
  const { t } = useTranslation("chat");
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    setNow(Date.now());
    const intervalId = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(intervalId);
  }, [startedAt]);

  return (
    <div className="chat-working-duration" role="status" aria-live="polite">
      <span className="chat-working-duration-dot" aria-hidden="true" />
      <span>{t("panel.workingFor", { duration: formatWorkingDuration(now - startedAt) })}</span>
    </div>
  );
}

/** 展示单条用户或助手消息，并承载消息块及消息操作交互。 */
function MessageRowView({
  message,
  index,
  isHighlighted,
  assistantLabel,
  assistantEngineId,
  assistantEngineName,
  allowInitialPreparation,
  allowTurnStartedThinking,
  preparingLabel,
  onApproval,
  onLoadActionOutput,
  onEditResend,
  onOpenDiffFile,
  onOpenImageAttachment,
}: MessageRowProps) {
  const { t, i18n } = useTranslation("chat");
  const isUser = message.role === "user";
  const messageTimestamp = useMemo(
    () => formatMessageTimestamp(message.createdAt, i18n.language),
    [i18n.language, message.createdAt],
  );
  const userContent = useMemo(() => {
    if (message.content) {
      return message.content;
    }
    return (message.blocks ?? [])
      .filter((block) => block.type === "text")
      .map((block) => block.content)
      .join("\n");
  }, [message.blocks, message.content]);
  const userAuxiliaryBlocks = useMemo(
    () =>
      (message.blocks ?? []).filter(
        (block) =>
          block.type === "attachment" ||
          block.type === "skill" ||
          block.type === "mention",
      ),
    [message.blocks],
  );
  const userPlanMode = useMemo(
    () =>
      (message.blocks ?? []).some(
        (block) => block.type === "text" && Boolean(block.planMode),
      ),
    [message.blocks],
  );
  const hasAssistantContent = !isUser && hasVisibleContent(message.blocks);
  const showInitialPreparation =
    !isUser &&
    message.status === "streaming" &&
    !hasAssistantContent &&
    allowInitialPreparation;
  const showReceivedAndThinking =
    !isUser &&
    message.status === "streaming" &&
    !hasAssistantContent &&
    allowTurnStartedThinking;
  const showAssistantShell =
    !isUser &&
    (hasAssistantContent ||
      showInitialPreparation ||
      showReceivedAndThinking ||
      Boolean(preparingLabel));
  const showThinkingPlaceholder =
    !isUser &&
    !hasAssistantContent &&
    (showInitialPreparation || showReceivedAndThinking || Boolean(preparingLabel));
  const showTurnTail = hasAssistantContent && message.status === "streaming";
  const hasPendingApproval = (message.blocks ?? []).some(
    (block) => block.type === "approval" && block.status === "pending",
  );
  const runningAction = [...(message.blocks ?? [])].reverse().find(
    (block) => block.type === "action" && block.status === "running",
  );
  const hasAcceptedSteer = (message.blocks ?? []).some(
    (block) => block.type === "steer" && block.deliveryStatus === "accepted",
  );
  const thinkingVariant = useThinkingVariant(showThinkingPlaceholder);

  if (!isUser && !showAssistantShell) {
    return null;
  }

  return (
    <div
      data-message-id={message.id}
      className="animate-slide-up msg-row"
      style={{
        animationDelay: `${Math.min(index * 20, 200)}ms`,
        display: "flex",
        flexDirection: "column",
        alignItems: isUser ? "flex-end" : "flex-start",
        maxWidth: "100%",
        borderRadius: "var(--radius-md)",
        outline: isHighlighted ? "2px solid rgba(var(--accent-rgb), 0.35)" : "none",
        boxShadow: isHighlighted
          ? "0 10px 28px rgba(var(--accent-rgb), 0.12)"
          : "none",
        transition:
          "outline-color var(--duration-normal) var(--ease-out), box-shadow var(--duration-normal) var(--ease-out)",
      }}
    >
      {isUser ? (
        <>
          <div className="msg-user-bubble">
            {userAuxiliaryBlocks.length > 0 && (
              <div style={{ display: "flex", flexWrap: "wrap", gap: 4, marginBottom: 6 }}>
                {userAuxiliaryBlocks.map((block, i) => {
                  if (block.type === "attachment") {
                    return (
                      <AttachmentChip
                        key={i}
                        attachment={block}
                        compact
                        onOpen={onOpenImageAttachment
                          ? () => onOpenImageAttachment(block)
                          : undefined}
                      />
                    );
                  }

                  return (
                    <span
                      key={i}
                      className={`chat-attachment-chip ${block.type === "skill" ? "chat-attachment-chip--skill" : "chat-attachment-chip--mention"}`}
                    >
                      {block.type === "skill" ? (
                        <DollarSign size={10} />
                      ) : (
                        <AtSign size={10} />
                      )}
                      <span className="chat-attachment-chip-name" style={{ fontSize: 10 }}>
                        {block.name}
                      </span>
                    </span>
                  );
                })}
              </div>
            )}
            {userPlanMode && (
              <div style={{ display: "flex", alignItems: "center", gap: 4, marginBottom: 6, fontSize: 10, color: "var(--accent-2)" }}>
                <ListChecks size={10} />
                <span>{t("panel.planMode")}</span>
              </div>
            )}
            {userContent}
          </div>
          <div className="msg-row-timestamp" style={{ display: "flex", alignItems: "center", gap: 2, justifyContent: "flex-end", marginTop: 4, paddingRight: 4 }}>
            {onEditResend && (
              <button
                type="button"
                className="msg-row-action-btn"
                onClick={() => onEditResend(userContent)}
                title={t("panel.editResend")}
                aria-label={t("panel.editResend")}
              >
                <Pencil size={11} />
              </button>
            )}
            <MessageCopyButton message={message} />
            {messageTimestamp && <span>{messageTimestamp}</span>}
          </div>
        </>
      ) : showAssistantShell ? (
        <div
          style={{
            width: "100%",
            maxWidth: "100%",
            padding: "4px 0",
          }}
        >
          {assistantLabel && (
            <div className="msg-turn-header">
              {getHarnessIcon(assistantEngineId, 11)}
              <span className="msg-turn-header-label">{assistantLabel}</span>
              <span className="msg-turn-actions">
                {messageTimestamp && <span style={{ padding: "0 2px" }}>{messageTimestamp}</span>}
                <MessageCopyButton message={message} />
              </span>
            </div>
          )}
          {hasAssistantContent ? (
            <>
              <MessageBlocks
                messageId={message.id}
                blocks={message.blocks}
                status={message.status}
                engineId={assistantEngineId}
                onApproval={onApproval}
                onLoadActionOutput={(actionId) => onLoadActionOutput(message.id, actionId)}
                onOpenDiffFile={onOpenDiffFile}
                onOpenImageAttachment={onOpenImageAttachment}
              />
              {showTurnTail && (
                <div className="chat-turn-tail-status" role="status" aria-live="polite">
                  <Loader2 size={12} className="chat-send-spinner" aria-hidden="true" />
                  <span>
                    {t(
                      hasPendingApproval
                        ? "messageBlocks.turnProgress.waitingForApproval"
                        : runningAction?.type === "action"
                          ? "messageBlocks.turnProgress.runningAction"
                          : hasAcceptedSteer
                            ? "messageBlocks.turnProgress.runningWithSteer"
                            : "messageBlocks.turnProgress.running",
                      runningAction?.type === "action"
                        ? { summary: runningAction.summary }
                        : { engine: assistantEngineName },
                    )}
                  </span>
                  <span className="chat-streaming-dots" aria-hidden="true">
                    <span />
                    <span />
                    <span />
                  </span>
                </div>
              )}
            </>
          ) : showReceivedAndThinking && !preparingLabel ? (
            <div className="chat-turn-tail-status" role="status" aria-live="polite">
              <Loader2 size={12} className="chat-send-spinner" aria-hidden="true" />
              <span>
                {t("messageBlocks.turnProgress.receivedAndThinking", {
                  engine: assistantEngineName,
                })}
              </span>
              <span className="chat-streaming-dots" aria-hidden="true">
                <span />
                <span />
                <span />
              </span>
            </div>
          ) : (
            <div
              style={{
                padding: "4px 14px 8px",
                display: "inline-flex",
                alignItems: "center",
                gap: 8,
                color: "var(--text-3)",
                fontSize: 12,
              }}
            >
              {(() => {
                const ThinkIcon = thinkingVariant.icon;
                return <ThinkIcon size={12} className="thinking-icon-active" style={{ color: "var(--info)" }} />;
              })()}
              <span>{preparingLabel ?? t(thinkingVariant.key)}</span>
              <span className="chat-streaming-dots">
                <span />
                <span />
                <span />
              </span>
            </div>
          )}
        </div>
      ) : null}
    </div>
  );
}

/** 对消息行输入进行浅比较，避免无关状态变化触发消息行重复渲染。 */
export const MessageRow = memo(
  MessageRowView,
  (prev, next) =>
    prev.message === next.message &&
    prev.index === next.index &&
    prev.isHighlighted === next.isHighlighted &&
    prev.assistantLabel === next.assistantLabel &&
    prev.assistantEngineId === next.assistantEngineId &&
    prev.assistantEngineName === next.assistantEngineName &&
    prev.allowInitialPreparation === next.allowInitialPreparation &&
    prev.allowTurnStartedThinking === next.allowTurnStartedThinking &&
    prev.preparingLabel === next.preparingLabel &&
    prev.onApproval === next.onApproval &&
    prev.onLoadActionOutput === next.onLoadActionOutput &&
    prev.onEditResend === next.onEditResend &&
    prev.onOpenDiffFile === next.onOpenDiffFile &&
    prev.onOpenImageAttachment === next.onOpenImageAttachment,
);
