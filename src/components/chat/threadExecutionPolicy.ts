import { isAutonomyPresetId } from "../../lib/autonomyPresets";
import type { AutonomyPresetId } from "../../lib/autonomyPresets";
import type { CodexPersonalityValue, CodexServiceTierValue } from "./CodexConfigPicker";
import type { Thread } from "../../types";
import { serializePrettyJson } from "./formatters";

/** 统一权限组件使用 automatic 表示旧全局默认 inherit。 */
export function autonomyPresetToComponentValue(preset: AutonomyPresetId | null): string | null {
  if (preset === "inherit") return "automatic";
  return preset;
}

/** 将统一组件的 automatic 默认值还原为现有全局默认 inherit。 */
export function componentValueToAutonomyPreset(value: string | null): AutonomyPresetId | null {
  if (value === "automatic") return "inherit";
  return isAutonomyPresetId(value) ? value : null;
}

export type CodexThreadApprovalPolicyValue =
  | "inherit"
  | "untrusted"
  | "on-request"
  | "never"
  | "custom";
/*
 * 旧 PermissionPicker 的 CLI 权限适配类型已停用，保留迁移记录：
type ClaudeThreadPermissionModeValue = "inherit" | "restricted" | "standard" | "trusted";
type OpenCodeThreadPermissionModeValue = "inherit" | "ask" | "allow" | "deny";
type ThreadApprovalPolicyValue =
  | CodexThreadApprovalPolicyValue
  | ClaudeThreadPermissionModeValue
  | OpenCodeThreadPermissionModeValue;
type ThreadApprovalPolicyStateValue =
  | ThreadApprovalPolicyValue
  | Record<string, unknown>;
type ThreadSandboxModeValue =
  | "inherit"
  | "read-only"
  | "workspace-write"
  | "danger-full-access";
type ThreadNetworkPolicyValue = "inherit" | "enabled" | "restricted";
type ThreadExecutionPolicyPatch = Partial<{
  approvalPolicy: ThreadApprovalPolicyStateValue;
  sandboxMode: ThreadSandboxModeValue;
  networkPolicy: ThreadNetworkPolicyValue;
  permissionProfile: Record<string, unknown> | null;
  approvalsReviewer: CodexApprovalsReviewer | null;
}>;
 */
export type ThreadSandboxModeValue =
  | "inherit"
  | "read-only"
  | "workspace-write"
  | "danger-full-access";

/* 旧 PermissionPicker 逐 CLI 选项构造函数已停用，保留完整迁移记录。
function getTrustLevelOptions(
  t: TFunction<"chat">,
): Array<{ value: TrustLevel; label: string; description: string }> {
  return [
    {
      value: "trusted",
      label: t("policy.trusted"),
      description: t("policy.trustedDescription"),
    },
    {
      value: "standard",
      label: t("policy.standard"),
      description: t("policy.standardDescription"),
    },
    {
      value: "restricted",
      label: t("policy.restricted"),
      description: t("policy.restrictedDescription"),
    },
  ];
}

function getCodexThreadApprovalPolicyOptions(
  t: TFunction<"chat">,
): Array<{
  value: CodexThreadApprovalPolicyValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoRepoTrust"),
    },
    {
      value: "untrusted",
      label: t("policy.untrusted"),
      description: t("policy.untrustedDescription"),
    },
    {
      value: "on-request",
      label: t("policy.onRequest"),
      description: t("policy.onRequestDescription"),
    },
    {
      value: "never",
      label: t("policy.never"),
      description: t("policy.neverDescription"),
    },
  ];
}

function getClaudeThreadPermissionModeOptions(
  t: TFunction<"chat">,
): Array<{
  value: ClaudeThreadPermissionModeValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoClaude"),
    },
    {
      value: "restricted",
      label: t("policy.restricted"),
      description: t("policy.claudeRestrictedDescription"),
    },
    {
      value: "standard",
      label: t("policy.standard"),
      description: t("policy.claudeStandardDescription"),
    },
    {
      value: "trusted",
      label: t("policy.trusted"),
      description: t("policy.claudeTrustedDescription"),
    },
  ];
}

function getOpenCodeThreadPermissionModeOptions(
  t: TFunction<"chat">,
): Array<{
  value: OpenCodeThreadPermissionModeValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoOpenCode"),
    },
    {
      value: "ask",
      label: t("policy.openCodeAsk"),
      description: t("policy.openCodeAskDescription"),
    },
    {
      value: "allow",
      label: t("policy.openCodeAllow"),
      description: t("policy.openCodeAllowDescription"),
    },
    {
      value: "deny",
      label: t("policy.openCodeDeny"),
      description: t("policy.openCodeDenyDescription"),
    },
  ];
}

function getThreadSandboxModeOptions(
  t: TFunction<"chat">,
): Array<{
  value: ThreadSandboxModeValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoSandbox"),
    },
    {
      value: "read-only",
      label: t("policy.readOnly"),
      description: t("policy.readOnlyDescription"),
    },
    {
      value: "workspace-write",
      label: t("policy.workspaceWrite"),
      description: t("policy.workspaceWriteDescription"),
    },
    {
      value: "danger-full-access",
      label: t("policy.fullAccess"),
      description: t("policy.fullAccessDescription"),
    },
  ];
}

function getThreadNetworkPolicyOptions(
  t: TFunction<"chat">,
): Array<{
  value: ThreadNetworkPolicyValue;
  label: string;
  description: string;
}> {
  return [
    {
      value: "inherit",
      label: t("policy.auto"),
      description: t("policy.autoNetwork"),
    },
    {
      value: "enabled",
      label: t("policy.enabled"),
      description: t("policy.enabledDescription"),
    },
    {
      value: "restricted",
      label: t("policy.restricted"),
      description: t("policy.networkRestrictedDescription"),
    },
  ];
}
*/

export function isCustomCodexApprovalPolicyValue(
  value: unknown,
): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/*
// 旧实现从 threads.engine_metadata_json 读取 Codex 权限，已停用；权限统一由
// ipc.getThreadPermissions 返回的 PermissionComponentJson 提供。
function readCodexThreadApprovalPolicyValue(thread: Thread | null): CodexThreadApprovalPolicyValue {
  const value = thread?.engineMetadata?.sandboxApprovalPolicy;
  if (value === "on-failure") {
    return "on-request";
  }
  if (value === "untrusted" || value === "on-request" || value === "never") {
    return value;
  }
  if (isCustomCodexApprovalPolicyValue(value)) {
    return "custom";
  }
  return "inherit";
}

function readCodexThreadCustomApprovalPolicyText(thread: Thread | null): string {
  const value = thread?.engineMetadata?.sandboxApprovalPolicy;
  return isCustomCodexApprovalPolicyValue(value) ? serializePrettyJson(value) : "";
}
*/

export function readThreadPersonalityValue(thread: Thread | null): CodexPersonalityValue {
  const value = thread?.engineMetadata?.personality;
  if (value === "none" || value === "friendly" || value === "pragmatic") {
    return value;
  }
  return "inherit";
}

export function readThreadServiceTierValue(thread: Thread | null): CodexServiceTierValue {
  const value = thread?.engineMetadata?.serviceTier;
  if (value === "fast" || value === "flex") {
    return value;
  }
  return "inherit";
}

export function readThreadOutputSchemaText(thread: Thread | null): string {
  const value = thread?.engineMetadata?.outputSchema;
  if (value === undefined || value === null) {
    return "";
  }
  return serializePrettyJson(value);
}

/* 旧 PermissionPicker 的 CLI 权限读取适配已停用，保留完整迁移记录。
function readClaudeThreadPermissionModeValue(
  thread: Thread | null,
): ClaudeThreadPermissionModeValue {
  const value = thread?.engineMetadata?.claudePermissionMode;
  if (value === "restricted" || value === "standard" || value === "trusted") {
    return value;
  }
  return "inherit";
}

function readOpenCodeThreadPermissionModeValue(
  thread: Thread | null,
): OpenCodeThreadPermissionModeValue {
  const value = thread?.engineMetadata?.opencodePermissionMode;
  if (value === "ask" || value === "allow" || value === "deny") {
    return value;
  }
  return "inherit";
}


function readThreadApprovalPolicyValue(thread: Thread | null): ThreadApprovalPolicyValue {
  if (thread?.engineId === "claude") {
    return readClaudeThreadPermissionModeValue(thread);
  }
  if (thread?.engineId === "opencode") {
    return readOpenCodeThreadPermissionModeValue(thread);
  }

  return readCodexThreadApprovalPolicyValue(thread);
}

function readThreadSandboxModeValue(thread: Thread | null): ThreadSandboxModeValue {
  const value = thread?.engineMetadata?.sandboxMode;
  if (value === "read-only" || value === "workspace-write" || value === "danger-full-access") {
    return value;
  }
  return "inherit";
}

function readThreadStoredNetworkPolicyValue(thread: Thread | null): ThreadNetworkPolicyValue {
  const value = thread?.engineMetadata?.sandboxAllowNetwork;
  if (value === true) {
    return "enabled";
  }
  if (value === false) {
    return "restricted";
  }
  return "inherit";
}

function readThreadNetworkPolicyValue(thread: Thread | null): ThreadNetworkPolicyValue {
  if (readThreadSandboxModeValue(thread) === "danger-full-access") {
    return "enabled";
  }

  return readThreadStoredNetworkPolicyValue(thread);
}
*/

export function readThreadOpenCodeAgentValue(thread: Thread | null): string {
  const value = thread?.engineMetadata?.opencodeAgent;
  return typeof value === "string" && value.trim() ? value.trim() : "build";
}

/* 旧 PermissionPicker 的本地策略合并、回写和请求适配已停用，保留完整迁移记录。
function readThreadExecutionPolicyState(thread: Thread | null): {
  approvalPolicy: ThreadApprovalPolicyStateValue;
  sandboxMode: ThreadSandboxModeValue;
  networkPolicy: ThreadNetworkPolicyValue;
} {
  // 权限优先读独立字段 permissionMode 的整包 JSON；解析失败回退到 engineMetadata 逐键读取。
  if (typeof thread?.permissionMode === "string" && thread.permissionMode) {
    try {
      const parsed = JSON.parse(thread.permissionMode) as {
        approvalPolicy?: ThreadApprovalPolicyStateValue;
        sandboxMode?: ThreadSandboxModeValue;
        networkPolicy?: ThreadNetworkPolicyValue;
      };
      if (
        parsed &&
        typeof parsed === "object" &&
        parsed.approvalPolicy !== undefined &&
        parsed.sandboxMode !== undefined &&
        parsed.networkPolicy !== undefined
      ) {
        return {
          approvalPolicy: parsed.approvalPolicy,
          sandboxMode: parsed.sandboxMode,
          networkPolicy: parsed.networkPolicy,
        };
      }
    } catch {
      // 落入下面的回退读取。
    }
  }
  const rawApprovalPolicy = thread?.engineMetadata?.sandboxApprovalPolicy;
  return {
    approvalPolicy:
      thread?.engineId === "codex" && isCustomCodexApprovalPolicyValue(rawApprovalPolicy)
        ? rawApprovalPolicy
        : readThreadApprovalPolicyValue(thread),
    sandboxMode: readThreadSandboxModeValue(thread),
    networkPolicy: readThreadStoredNetworkPolicyValue(thread),
  };
}

function applyThreadExecutionPolicyPatch(
  thread: Thread,
  patch: ThreadExecutionPolicyPatch,
): Thread {
  const metadata = { ...(thread.engineMetadata ?? {}) };
  const currentCodexApprovalPolicy = thread.engineMetadata?.sandboxApprovalPolicy;
  const nextApprovalPolicy =
    Object.prototype.hasOwnProperty.call(patch, "approvalPolicy")
      ? patch.approvalPolicy
      : isCustomCodexApprovalPolicyValue(currentCodexApprovalPolicy)
        ? currentCodexApprovalPolicy
        : readThreadApprovalPolicyValue(thread);
  const nextState = {
    ...readThreadExecutionPolicyState(thread),
    ...patch,
    approvalPolicy: nextApprovalPolicy,
  };

  if (thread.engineId === "claude") {
    if (
      nextState.approvalPolicy === "restricted" ||
      nextState.approvalPolicy === "standard" ||
      nextState.approvalPolicy === "trusted"
    ) {
      metadata.claudePermissionMode = nextState.approvalPolicy;
    } else {
      delete metadata.claudePermissionMode;
    }
  } else if (thread.engineId === "opencode") {
    if (
      nextState.approvalPolicy === "ask" ||
      nextState.approvalPolicy === "allow" ||
      nextState.approvalPolicy === "deny"
    ) {
      metadata.opencodePermissionMode = nextState.approvalPolicy;
    } else {
      delete metadata.opencodePermissionMode;
    }
    delete metadata.sandboxMode;
    delete metadata.sandboxAllowNetwork;
  } else {
    if (isCustomCodexApprovalPolicyValue(nextState.approvalPolicy)) {
      metadata.sandboxApprovalPolicy = nextState.approvalPolicy;
    } else if (
      nextState.approvalPolicy === "untrusted" ||
      nextState.approvalPolicy === "on-request" ||
      nextState.approvalPolicy === "never"
    ) {
      metadata.sandboxApprovalPolicy = nextState.approvalPolicy;
    } else {
      delete metadata.sandboxApprovalPolicy;
    }
  }

  if (thread.engineId !== "opencode") {
    if (nextState.sandboxMode === "inherit") {
      delete metadata.sandboxMode;
    } else {
      metadata.sandboxMode = nextState.sandboxMode;
    }

    if (nextState.networkPolicy === "inherit") {
      delete metadata.sandboxAllowNetwork;
    } else {
      metadata.sandboxAllowNetwork = nextState.networkPolicy === "enabled";
    }

    if ("sandboxMode" in patch || "networkPolicy" in patch) {
      delete metadata.permissionProfile;
    }
  }

  if ("permissionProfile" in patch) {
    if (patch.permissionProfile === null || patch.permissionProfile === undefined) {
      delete metadata.permissionProfile;
    } else {
      metadata.permissionProfile = patch.permissionProfile;
      delete metadata.sandboxMode;
      delete metadata.sandboxAllowNetwork;
    }
  }

  if ("approvalsReviewer" in patch) {
    if (patch.approvalsReviewer) {
      metadata.approvalsReviewer = patch.approvalsReviewer;
    } else {
      delete metadata.approvalsReviewer;
    }
  }

  return {
    ...thread,
    engineMetadata: Object.keys(metadata).length > 0 ? metadata : undefined,
  };
}

function toThreadExecutionPolicyRequest(
  patch: ThreadExecutionPolicyPatch,
  clearPermissionProfileOnSandboxChange = false,
): {
  approvalPolicy?: unknown;
  sandboxMode?: string | null;
  allowNetwork?: boolean | null;
  permissionProfile?: Record<string, unknown> | null;
  approvalsReviewer?: CodexApprovalsReviewer | null;
} {
  const request: {
    approvalPolicy?: unknown;
    sandboxMode?: string | null;
    allowNetwork?: boolean | null;
    permissionProfile?: Record<string, unknown> | null;
    approvalsReviewer?: CodexApprovalsReviewer | null;
  } = {};

  if ("approvalPolicy" in patch) {
    request.approvalPolicy = patch.approvalPolicy === "inherit" ? null : patch.approvalPolicy;
  }

  if ("sandboxMode" in patch) {
    request.sandboxMode = patch.sandboxMode === "inherit" ? null : patch.sandboxMode;
  }

  if ("networkPolicy" in patch) {
    request.allowNetwork =
      patch.networkPolicy === "inherit"
        ? null
        : patch.networkPolicy === "enabled";
  }

  if ("permissionProfile" in patch) {
    request.permissionProfile = patch.permissionProfile ?? null;
  } else if (
    clearPermissionProfileOnSandboxChange &&
    ("sandboxMode" in patch || "networkPolicy" in patch)
  ) {
    request.permissionProfile = null;
  }

  if ("approvalsReviewer" in patch) {
    request.approvalsReviewer = patch.approvalsReviewer ?? null;
  }

  return request;
}
*/

export function parseStoredOutputSchema(
  text: string,
): Record<string, unknown> | boolean | null {
  const normalized = text.trim();
  if (!normalized) {
    return null;
  }

  const parsed = JSON.parse(normalized) as unknown;
  if (
    typeof parsed !== "boolean" &&
    (!parsed || typeof parsed !== "object" || Array.isArray(parsed))
  ) {
    throw new Error("output schema must be a JSON Schema object or boolean");
  }

  return parsed as Record<string, unknown> | boolean;
}

export function parseStoredApprovalPolicy(
  text: string,
): Record<string, unknown> | null {
  const normalized = text.trim();
  if (!normalized) {
    return null;
  }

  const parsed = JSON.parse(normalized) as unknown;
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    throw new Error("approval policy must be a JSON object");
  }

  return parsed as Record<string, unknown>;
}

export function readThreadLastModelId(thread: { modelId?: string | null }): string | null {
  const normalized = thread.modelId?.trim();
  return normalized && normalized.length > 0 ? normalized : null;
}
