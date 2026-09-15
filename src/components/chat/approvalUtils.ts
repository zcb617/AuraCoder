import type {
  ApprovalBlock,
  ApprovalResponse,
  TrustLevel,
} from "../../types";
import { shouldShowClaudeUnsupportedApproval } from "./MessageBlocks";
import {
  buildPermissionsApprovalResponse,
  buildPermissionsDeclineResponse,
  isRequestUserInputApproval,
  isSupportedClaudeToolInputApproval,
  parseToolInputQuestions,
  requiresCustomApprovalPayload,
} from "./toolInputApproval";

/**
 * 判断权限保存等待结果；先处理更新中的新请求，再处理当前请求失败，
 * 保证旧请求清理 ref 后的失败不会被发送流程忽略。
 */
export function permissionSaveWaitAction(
  permissionSaved: boolean,
  pendingPermissionRequest: Promise<boolean>,
  latestPendingPermissionRequest: Promise<boolean> | undefined,
): "continue" | "fail" | "complete" {
  if (
    latestPendingPermissionRequest &&
    latestPendingPermissionRequest !== pendingPermissionRequest
  ) {
    return "continue";
  }
  if (!permissionSaved) {
    return "fail";
  }
  return "complete";
}

/**
 * 判断权限变更是否需要同步更新项目级信任等级，避免相同或非法等级触发无效写入。
 */
export function shouldUpdateWorkspaceTrustLevel(
  nextTrust: string | null,
  currentTrust: TrustLevel | null | undefined,
): nextTrust is TrustLevel {
  return (
    (nextTrust === "trusted" || nextTrust === "standard" || nextTrust === "restricted")
    && nextTrust !== currentTrust
  );
}

export function resolvePendingToolInputApproval(
  pendingApprovals: ApprovalBlock[],
  engineId?: string,
  preferredApprovalId?: string | null,
): ApprovalBlock | null {
  const eligibleApprovals = pendingApprovals.filter((approval) => {
    const details = approval.details ?? {};
    if (
      !isRequestUserInputApproval(details) ||
      parseToolInputQuestions(details).length === 0
    ) {
      return false;
    }

    return engineId !== "claude" || isSupportedClaudeToolInputApproval(details);
  });

  if (eligibleApprovals.length === 0) {
    return null;
  }

  if (preferredApprovalId) {
    const preferredApproval = eligibleApprovals.find(
      (approval) => approval.approvalId === preferredApprovalId,
    );
    if (preferredApproval) {
      return preferredApproval;
    }
  }

  return eligibleApprovals[eligibleApprovals.length - 1] ?? null;
}

export function filterPendingApprovalBannerRows(
  pendingApprovals: ApprovalBlock[],
  engineId?: string,
  activeToolInputApprovalId?: string | null,
): ApprovalBlock[] {
  return pendingApprovals.filter((approval) => {
    const details = approval.details ?? {};
    if (!isRequestUserInputApproval(details)) {
      return true;
    }

    if (parseToolInputQuestions(details).length === 0) {
      return true;
    }

    if (approval.approvalId === activeToolInputApprovalId) {
      return false;
    }

    return engineId === "claude";
  });
}

export function isOpenCodeQuestionApproval(details?: Record<string, unknown>): boolean {
  return details?._opencodeRequestKind === "question";
}

export function canUseApprovalDecisionActions(
  engineId?: string,
  details?: Record<string, unknown>,
): boolean {
  return engineId !== "opencode" || !isOpenCodeQuestionApproval(details);
}

export function canBatchApproveApproval(
  approval: ApprovalBlock,
  engineId?: string,
): boolean {
  const details = approval.details ?? {};
  return (
    canUseApprovalDecisionActions(engineId, details) &&
    !isRequestUserInputApproval(details) &&
    !requiresCustomApprovalPayload(details) &&
    !shouldShowClaudeUnsupportedApproval(details, true, engineId === "claude")
  );
}

export function buildPermissionApprovalResponseForEngine(
  engineId: string | undefined,
  details: Record<string, unknown> | undefined,
  decision: "accept" | "decline" | "accept_for_session",
): ApprovalResponse {
  if (engineId === "opencode") {
    return { decision };
  }

  if (decision === "decline") {
    return buildPermissionsDeclineResponse();
  }

  return buildPermissionsApprovalResponse(
    details,
    decision === "accept_for_session" ? "session" : "turn",
  );
}
