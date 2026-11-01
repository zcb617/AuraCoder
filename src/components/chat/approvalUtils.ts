import type { ApprovalBlock, ApprovalResponse } from "../../types";
import { shouldShowClaudeUnsupportedApproval } from "./MessageBlocks";
import {
  buildPermissionsApprovalResponse,
  buildPermissionsDeclineResponse,
  isRequestUserInputApproval,
  isSupportedClaudeToolInputApproval,
  parseToolInputQuestions,
  requiresCustomApprovalPayload,
} from "./toolInputApproval";

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
