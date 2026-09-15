import { Clock, GitBranch, AlertTriangle } from "lucide-react";
import { useTranslation } from "react-i18next";
import type { ChatProviderUsageLimits, GitStatus } from "../../types";
import {
  formatResetTime,
  formatUsagePercent,
  usagePercentToWidth,
  usageProgressLevelClass,
} from "./formatters";
import { resolveUsageStatusKey } from "./usageStatus";

type SelectedClaudeWeeklyUsage = {
  /** 当前 Claude 模型族周额度的业务展示名称。 */
  label: string;
  /** 当前 Claude 模型族周额度的剩余百分比。 */
  percent: ChatProviderUsageLimits["windowFableWeeklyPercent"];
  /** 当前 Claude 模型族周额度的重置时间。 */
  resetsAt: ChatProviderUsageLimits["windowFableWeeklyResetsAt"];
};

interface ChatStatusBarProps {
  /** 当前选中引擎是否为 Codex，用于控制额度状态栏展示。 */
  isCodexEngine: boolean;
  /** 当前选中的引擎标识，用于识别 Claude 额度状态栏。 */
  selectedEngineId: string;
  /** 当前 CLI 账号额度窗口快照，用于展示五小时和周额度。 */
  usageLimits: ChatProviderUsageLimits | null;
  /** 当前 CLI 账号额度是否正在加载。 */
  usageLimitsLoading: boolean;
  /** 当前 Claude 模型族周额度展示数据。 */
  selectedClaudeWeeklyUsage: SelectedClaudeWeeklyUsage | null;
  /** 当前会话是否已经存在用户消息。 */
  hasUserMessage: boolean;
  /** 当前聊天会话是否正在流式响应。 */
  streaming: boolean;
  /** 当前工作区 Git 状态，用于展示当前分支。 */
  gitStatus: GitStatus | null | undefined;
  /** 当前聊天错误信息，为空时不展示错误提示。 */
  error: string | null | undefined;
  /** 打开账号额度详情弹窗的业务回调。 */
  onOpenUsageLimits: () => void;
}

/** 展示聊天输入区底部的额度状态、Git 分支与错误提示。 */
export function ChatStatusBar({
  isCodexEngine,
  selectedEngineId,
  usageLimits,
  usageLimitsLoading,
  selectedClaudeWeeklyUsage,
  hasUserMessage,
  streaming,
  gitStatus,
  error,
  onOpenUsageLimits,
}: ChatStatusBarProps) {
  const { t } = useTranslation("chat");

  return (
    <>
      {/* Bottom status bar with context usage */}
      <div className="chat-status-bar">
        {(isCodexEngine || selectedEngineId === "claude") && (
          usageLimits ? (
            <div className="chat-status-usage">
              <button
                type="button"
                className="chat-context-section"
                onClick={onOpenUsageLimits}
                title={t("status.openUsageLimits")}
              >
                <Clock size={10} />
                <span>{t("status.windowFiveHoursLeft")}</span>
                <div className="chat-context-progress">
                  <div
                    className={`chat-context-progress-fill${usageProgressLevelClass(usageLimits.windowFiveHourPercent)}`}
                    style={{ width: usagePercentToWidth(usageLimits.windowFiveHourPercent) }}
                  />
                </div>
                <span className="chat-context-percent">
                  {formatUsagePercent(usageLimits.windowFiveHourPercent)}
                </span>
                {usageLimits.windowFiveHourResetsAt && (
                  <span className="chat-context-reset">
                    {t("status.resets", {
                      time: formatResetTime(t, usageLimits.windowFiveHourResetsAt),
                    })}
                  </span>
                )}
              </button>

              <span className="chat-context-divider">&middot;</span>

              <button
                type="button"
                className="chat-context-section"
                onClick={onOpenUsageLimits}
                title={t("status.openUsageLimits")}
              >
                <Clock size={10} />
                <span>{t("status.windowWeeklyLeft")}</span>
                <div className="chat-context-progress">
                  <div
                    className={`chat-context-progress-fill${usageProgressLevelClass(usageLimits.windowWeeklyPercent)}`}
                    style={{ width: usagePercentToWidth(usageLimits.windowWeeklyPercent) }}
                  />
                </div>
                <span className="chat-context-percent">
                  {formatUsagePercent(usageLimits.windowWeeklyPercent)}
                </span>
                {usageLimits.windowWeeklyResetsAt && (
                  <span className="chat-context-reset">
                    {t("status.resets", {
                      time: formatResetTime(t, usageLimits.windowWeeklyResetsAt),
                    })}
                  </span>
                )}
              </button>

              {selectedClaudeWeeklyUsage && (
                <>
                  <span className="chat-context-divider">&middot;</span>

                  <button
                    type="button"
                    className="chat-context-section"
                    onClick={onOpenUsageLimits}
                    title={t("status.openUsageLimits")}
                  >
                    <Clock size={10} />
                    <span>{selectedClaudeWeeklyUsage.label}</span>
                    <div className="chat-context-progress">
                      <div
                        className={`chat-context-progress-fill${usageProgressLevelClass(selectedClaudeWeeklyUsage.percent)}`}
                        style={{ width: usagePercentToWidth(selectedClaudeWeeklyUsage.percent) }}
                      />
                    </div>
                    <span className="chat-context-percent">
                      {formatUsagePercent(selectedClaudeWeeklyUsage.percent)}
                    </span>
                    {selectedClaudeWeeklyUsage.resetsAt && (
                      <span className="chat-context-reset">
                        {t("status.resets", {
                          time: formatResetTime(t, selectedClaudeWeeklyUsage.resetsAt),
                        })}
                      </span>
                    )}
                  </button>
                </>
              )}
            </div>
          ) : (
            <div className="chat-context-section">
              <Clock size={10} />
              <span>
                {t(resolveUsageStatusKey(hasUserMessage, streaming || usageLimitsLoading))}
              </span>
            </div>
          )
        )}

        {/* Branch */}
        {gitStatus?.branch && (
          <span className="chat-status-branch">
            <GitBranch size={11} />
            {gitStatus.branch}
          </span>
        )}
      </div>

      {error && (
        <div className="msg-error-block" style={{ marginTop: 8, fontSize: 12 }}>
          <AlertTriangle size={14} style={{ flexShrink: 0, marginTop: 2 }} />
          {error}
        </div>
      )}
    </>
  );
}
