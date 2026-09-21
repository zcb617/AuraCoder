import type { Dispatch, SetStateAction } from "react";
import { Shield } from "lucide-react";
import { useTranslation } from "react-i18next";
import type {
  ApprovalBlock,
  ApprovalResponse,
  EngineCapabilities,
  Thread,
  TrustLevel,
} from "../../types";
import { shouldShowClaudeUnsupportedApproval } from "./MessageBlocks";
import { approvalRowIcon } from "./approvalRowIcon";
import {
  buildPermissionApprovalResponseForEngine,
  canUseApprovalDecisionActions,
} from "./approvalUtils";
import {
  buildMcpElicitationApprovalResponse,
  isMcpElicitationApproval,
  isPermissionsRequestApproval,
  isRequestUserInputApproval,
  isSupportedClaudeToolInputApproval,
  parseApprovalCommand,
  parseApprovalReason,
  parseProposedExecpolicyAmendment,
  parseProposedNetworkPolicyAmendments,
  parseToolInputQuestions,
  requiresCustomApprovalPayload,
} from "./toolInputApproval";

/**
 * 审批横幅展示组件属性，承载审批列表、引擎能力及审批操作回调。
 */
interface ApprovalBannerProps {
  /** 当前需要在审批横幅中展示的审批记录。 */
  pendingApprovalBannerRows: ApprovalBlock[];
  /** 当前可批量批准的审批记录。 */
  batchApprovableRows: ApprovalBlock[];
  /** 是否展示审批横幅头部及批量操作。 */
  showApprovalBannerHeader: boolean;
  /** 当前活动会话，用于读取引擎和会话标识。 */
  activeThread: Thread | null;
  /** 当前活动引擎支持的审批决策类型。 */
  activeThreadApprovalDecisionCapabilities: EngineCapabilities["approvalDecisions"];
  /** 当前工作区信任等级。 */
  workspaceTrustLevel: TrustLevel;
  /** 向运行时发送审批响应。 */
  respondApproval: (
    approvalId: string,
    response: ApprovalResponse,
    threadIdOverride?: string,
  ) => Promise<boolean>;
  /** 执行全部待处理审批的批量批准操作。 */
  allowAllPendingApprovals: (stopAsking: boolean) => Promise<void>;
  /** 更新当前工作区信任等级。 */
  onWorkspaceTrustLevelChange: (nextTrustLevel: TrustLevel) => Promise<void>;
  /** 当前选中的工具输入审批记录。 */
  pendingToolInputApproval: ApprovalBlock | null | undefined;
  /** 更新当前选中的工具输入审批记录标识。 */
  setSelectedPendingToolInputApprovalId: Dispatch<SetStateAction<string | null>>;
}

/**
 * 展示待处理审批横幅，并按当前引擎能力渲染对应的审批动作。
 */
export function ApprovalBanner({
  pendingApprovalBannerRows,
  batchApprovableRows,
  showApprovalBannerHeader,
  activeThread,
  activeThreadApprovalDecisionCapabilities,
  workspaceTrustLevel,
  respondApproval,
  allowAllPendingApprovals,
  onWorkspaceTrustLevelChange,
  pendingToolInputApproval,
  setSelectedPendingToolInputApprovalId,
}: ApprovalBannerProps) {
  const { t } = useTranslation("chat");

  return (
    <>
          {pendingApprovalBannerRows.length > 0 && (
            <div className="chat-approval-banner">
              {showApprovalBannerHeader && (
                <div className="approval-header">
                  <span className="approval-header-icon">
                    <Shield size={11} />
                  </span>
                  <span className="approval-header-title">
                    {t("panel.approvalBannerTitleCount", {
                      count: pendingApprovalBannerRows.length,
                    })}
                  </span>
                  <span className="approval-header-spacer" />
                  {batchApprovableRows.length > 1 &&
                    activeThreadApprovalDecisionCapabilities.includes("accept") && (
                      <>
                        <button
                          type="button"
                          className="approval-batch-btn"
                          onClick={() => void allowAllPendingApprovals(false)}
                        >
                          {t("autonomy.allowAll")}
                        </button>
                        {Boolean(activeThread?.id) && (
                          <button
                            type="button"
                            className="approval-batch-btn"
                            onClick={() => void allowAllPendingApprovals(true)}
                            title={t("autonomy.presets.full.description")}
                          >
                            {t("autonomy.allowAllStopAsking")}
                          </button>
                        )}
                      </>
                    )}
                  {workspaceTrustLevel !== "trusted" && (
                    <button
                      type="button"
                      className="approval-trust-btn"
                      onClick={() => void onWorkspaceTrustLevelChange("trusted")}
                      title={t("panel.setWorkspaceTrusted")}
                    >
                      {t("panel.trustWorkspace")}
                    </button>
                  )}
                </div>
              )}

              <div className="approval-rows">
                {pendingApprovalBannerRows.slice(-3).map((approval) => {
                  const details = approval.details ?? {};
                  const isPermissionsRequest = isPermissionsRequestApproval(details);
                  const isToolInputRequest = isRequestUserInputApproval(details);
                  const isMcpElicitationRequest = isMcpElicitationApproval(details);
                  const requiresCustomPayload = requiresCustomApprovalPayload(details);
                  const canUseDecisionActions = canUseApprovalDecisionActions(
                    activeThread?.engineId,
                    details,
                  );
                  const toolInputQuestionCount = isToolInputRequest
                    ? parseToolInputQuestions(details).length
                    : 0;
                  const isClaudeApproval = activeThread?.engineId === "claude";
                  const supportsDecline =
                    canUseDecisionActions &&
                    activeThreadApprovalDecisionCapabilities.includes("decline");
                  const supportsCancel =
                    canUseDecisionActions &&
                    activeThreadApprovalDecisionCapabilities.includes("cancel");
                  const supportsSession =
                    activeThreadApprovalDecisionCapabilities.includes("accept_for_session") &&
                    (!isClaudeApproval || details._claudeSessionPermissionAvailable === true);
                  const supportsAccept =
                    activeThreadApprovalDecisionCapabilities.includes("accept");
                  const proposedExecpolicyAmendment =
                    parseProposedExecpolicyAmendment(details);
                  const proposedNetworkPolicyAmendments =
                    parseProposedNetworkPolicyAmendments(details);
                  const hasUnsupportedClaudePayload = shouldShowClaudeUnsupportedApproval(
                    details,
                    true,
                    isClaudeApproval,
                  );
                  const canUseToolInputComposer =
                    isToolInputRequest &&
                    toolInputQuestionCount > 0 &&
                    (!isClaudeApproval || isSupportedClaudeToolInputApproval(details));
                  const showToolInputComposerHint = canUseToolInputComposer;
                  const canSelectToolInputComposer =
                    isClaudeApproval &&
                    canUseToolInputComposer &&
                    approval.approvalId !== pendingToolInputApproval?.approvalId;
                  const hidePositiveApprovalActions =
                    isToolInputRequest && toolInputQuestionCount === 0;
                  const command = parseApprovalCommand(details);
                  const reason = parseApprovalReason(details);

                  return (
                    <div
                      key={approval.approvalId}
                      className="chat-approval-row"
                    >
                      <div className="approval-row-info">
                        <div className="approval-row-head">
                          <span className="approval-row-icon">
                            {approvalRowIcon(approval.actionType)}
                          </span>
                          <div
                            className="approval-row-summary"
                            title={approval.summary}
                          >
                            {approval.summary}
                          </div>
                        </div>
                        {command && (
                          <div className="approval-row-command">{command}</div>
                        )}
                        {reason && (
                          <div className="approval-row-reason">{reason}</div>
                        )}
                      </div>

                      <div className="approval-actions">
                        {hasUnsupportedClaudePayload ? (
                          <>
                            <span className="approval-row-hint">
                              {t("panel.claudeApprovalUnsupported")}
                            </span>
                            {supportsDecline && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-deny"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    decision: "decline",
                                  })
                                }
                              >
                                {t("panel.approvalActions.deny")}
                              </button>
                            )}
                          </>
                        ) : showToolInputComposerHint || requiresCustomPayload ? (
                          <>
                            <span className="approval-row-hint">
                              {showToolInputComposerHint
                                ? t("panel.respondInCard")
                                : isMcpElicitationRequest
                                  ? t("panel.respondToMcpElicitation")
                                  : t("panel.respondInCustomCard")}
                            </span>
                            {canSelectToolInputComposer && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-allow"
                                onClick={() =>
                                  setSelectedPendingToolInputApprovalId(
                                    approval.approvalId,
                                  )
                                }
                              >
                                {t("panel.approvalActions.answerBelow")}
                              </button>
                            )}
                            {isMcpElicitationRequest && (
                              <>
                                <button
                                  type="button"
                                  className="approval-btn approval-btn-deny"
                                  onClick={() =>
                                    void respondApproval(approval.approvalId, {
                                      action: "decline",
                                    })
                                  }
                                >
                                  {t("panel.approvalActions.deny")}
                                </button>
                                <button
                                  type="button"
                                  className="approval-btn approval-btn-allow"
                                  onClick={() =>
                                    void respondApproval(
                                      approval.approvalId,
                                      buildMcpElicitationApprovalResponse(details, "accept"),
                                    )
                                  }
                                >
                                  {t("panel.approvalActions.approve")}
                                </button>
                              </>
                            )}
                          </>
                        ) : (
                          <>
                            {supportsDecline && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-deny"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    ...(isPermissionsRequest
                                      ? buildPermissionApprovalResponseForEngine(
                                          activeThread?.engineId,
                                          details,
                                          "decline",
                                        )
                                      : isToolInputRequest
                                        ? { action: "decline" }
                                        : { decision: "decline" }),
                                  })
                                }
                              >
                                {t("panel.approvalActions.deny")}
                              </button>
                            )}
                            {supportsCancel && !isPermissionsRequest && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-cancel"
                                onClick={() =>
                                  void respondApproval(
                                    approval.approvalId,
                                    isToolInputRequest
                                      ? { action: "cancel" }
                                      : { decision: "cancel" },
                                  )
                                }
                              >
                                {t("panel.approvalActions.cancel")}
                              </button>
                            )}
                            {!hidePositiveApprovalActions && (
                              <span className="approval-actions-gap" />
                            )}
                            {!hidePositiveApprovalActions && supportsSession && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-session"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    ...(isPermissionsRequest
                                      ? buildPermissionApprovalResponseForEngine(
                                          activeThread?.engineId,
                                          details,
                                          "accept_for_session",
                                        )
                                      : { decision: "accept_for_session" }),
                                  })
                                }
                              >
                                {t("panel.approvalActions.allowSession")}
                              </button>
                            )}
                            {!hidePositiveApprovalActions && !isClaudeApproval && !isPermissionsRequest && proposedExecpolicyAmendment.length > 0 && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-session"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    acceptWithExecpolicyAmendment: {
                                      execpolicy_amendment: proposedExecpolicyAmendment,
                                    },
                                  })
                                }
                              >
                                {t("panel.allowWithPolicy")}
                              </button>
                            )}
                            {!hidePositiveApprovalActions && !isClaudeApproval && !isPermissionsRequest && proposedNetworkPolicyAmendments.map((amendment) => (
                              <button
                                key={`${amendment.action}:${amendment.host}`}
                                type="button"
                                className="approval-btn approval-btn-session"
                                onClick={() =>
                                  void respondApproval(approval.approvalId, {
                                    applyNetworkPolicyAmendment: {
                                      network_policy_amendment: amendment,
                                    },
                                  })
                                }
                                title={t("panel.approvalActions.hostActionTitle", {
                                  action: amendment.action === "allow"
                                    ? t("panel.approvalActions.allow")
                                    : t("panel.approvalActions.block"),
                                  host: amendment.host,
                                })}
                              >
                                {amendment.action === "allow"
                                  ? t("panel.approvalActions.allowHost")
                                  : t("panel.approvalActions.blockHost")}
                              </button>
                            ))}
                            {!hidePositiveApprovalActions && supportsAccept && (
                              <button
                                type="button"
                                className="approval-btn approval-btn-allow"
                                onClick={() =>
                                  void respondApproval(
                                    approval.approvalId,
                                    isPermissionsRequest
                                      ? buildPermissionApprovalResponseForEngine(
                                          activeThread?.engineId,
                                          details,
                                          "accept",
                                        )
                                      : { decision: "accept" },
                                  )
                                }
                              >
                                {t("panel.approvalActions.allow")}
                              </button>
                            )}
                          </>
                        )}
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

    </>
  );
}
