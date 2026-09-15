import { useTranslation } from "react-i18next";
import type { PendingFlexibleMessage } from "../../stores/chatComposerStore";

/** 展示待确认的灵活消息集合，并提供确认与撤回操作。 */
export function FlexibleMessageGroup({
  messages,
  confirmDisabled,
  confirmTitle,
  onConfirm,
  onWithdraw,
}: {
  /** 当前待确认的灵活消息列表。 */
  messages: PendingFlexibleMessage[];
  /** 是否禁止确认当前灵活消息集合。 */
  confirmDisabled: boolean;
  /** 确认按钮的业务提示文案。 */
  confirmTitle: string;
  /** 用户确认发送灵活消息集合时执行的业务回调。 */
  onConfirm: () => void;
  /** 用户撤回单条灵活消息时执行的业务回调。 */
  onWithdraw: (message: PendingFlexibleMessage) => void;
}) {
  const { t } = useTranslation("chat");

  return (
    <div className="flexible-message-group" role="group" aria-label={t("panel.flexibleMessageGroup.label")}>
      <div className="flexible-message-group-content">
        {messages.map((message) => (
          <div key={message.id} className="flexible-message-group-item">
            <span className="flexible-message-group-item-text">{message.text}</span>
            <button
              type="button"
              className="flexible-message-group-withdraw"
              title={t("panel.flexibleMessageGroup.withdraw")}
              aria-label={t("panel.flexibleMessageGroup.withdraw")}
              onClick={() => onWithdraw(message)}
            >
              {t("panel.flexibleMessageGroup.withdraw")}
            </button>
          </div>
        ))}
      </div>
      <div className="flexible-message-group-actions">
        <button
          type="button"
          className="flexible-message-group-confirm"
          disabled={confirmDisabled}
          title={confirmTitle}
          onClick={onConfirm}
        >
          {t("panel.flexibleMessageGroup.confirm")}
        </button>
      </div>
    </div>
  );
}
