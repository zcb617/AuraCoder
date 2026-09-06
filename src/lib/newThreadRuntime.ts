import type { EngineInfo, Thread } from "../types";
import type { OnboardingPreferredChatSelection } from "./onboarding";

export type NewThreadServiceTier = "fast" | "flex";

export interface NewThreadRuntimeSelection {
  engineId: string;
  modelId: string;
  reasoningEffort: string | null;
  serviceTier: NewThreadServiceTier | null;
}

export type ComposerRuntimeSnapshot = NewThreadRuntimeSelection;

export const NEW_THREAD_FALLBACK_RUNTIME: NewThreadRuntimeSelection = {
  engineId: "codex",
  modelId: "gpt-5.4",
  reasoningEffort: "high",
  serviceTier: null,
};

interface ResolveNewThreadRuntimeInput {
  engines: ReadonlyArray<EngineInfo>;
  composerRuntime?: ComposerRuntimeSnapshot | null;
  activeThread?: Thread | null;
  onboardingSelection?: OnboardingPreferredChatSelection | null;
}

function normalizeString(value: string | null | undefined): string | null {
  if (typeof value !== "string") {
    return null;
  }

  const normalized = value.trim();
  return normalized.length > 0 ? normalized : null;
}

function normalizeServiceTier(
  value: string | null | undefined,
): NewThreadServiceTier | null {
  return value === "fast" || value === "flex" ? value : null;
}

function runtimeFromThread(thread: Thread): NewThreadRuntimeSelection {
  const modelId = normalizeString(thread.modelId);
  const reasoningEffort = normalizeString(thread.reasoningEffort);
  const serviceTier =
    thread.engineId === "codex"
      ? normalizeServiceTier(
          typeof thread.engineMetadata?.serviceTier === "string"
            ? thread.engineMetadata.serviceTier
            : null,
        )
      : null;

  return {
    engineId: thread.engineId,
    modelId: modelId ?? thread.modelId,
    reasoningEffort,
    serviceTier,
  };
}

function resolveRuntimeCandidate(
  engines: ReadonlyArray<EngineInfo>,
  candidate: NewThreadRuntimeSelection | null | undefined,
): NewThreadRuntimeSelection | null {
  if (!candidate) {
    return null;
  }

  const engineId = normalizeString(candidate.engineId);
  const modelId = normalizeString(candidate.modelId);
  if (!engineId || !modelId) {
    return null;
  }

  const engine = engines.find((item) => item.id === engineId);
  if (!engine) {
    // 候选引擎不在已登记/探测到的 engines 中时不再放行，避免为未安装的
    // CLI 创建线程；候选链全部不可用时由调用方提示未检测到可用 CLI。
    return null;
  }

  const model = engine.models.find((item) => item.id === modelId);
  if (!model) {
    return null;
  }

  const reasoningEffort = normalizeString(candidate.reasoningEffort);
  const supportedEffort =
    reasoningEffort &&
    model.supportedReasoningEfforts.some(
      (option) => option.reasoningEffort === reasoningEffort,
    )
      ? reasoningEffort
      : null;

  return {
    engineId: engine.id,
    modelId: model.id,
    reasoningEffort: supportedEffort,
    serviceTier: engine.id === "codex" ? normalizeServiceTier(candidate.serviceTier) : null,
  };
}

export function resolveNewThreadRuntime({
  engines,
  composerRuntime,
  activeThread,
  onboardingSelection,
}: ResolveNewThreadRuntimeInput): NewThreadRuntimeSelection | null {
  const candidates: Array<NewThreadRuntimeSelection | null> = [
    composerRuntime ?? null,
    activeThread ? runtimeFromThread(activeThread) : null,
    onboardingSelection
      ? {
          engineId: onboardingSelection.engineId,
          modelId: onboardingSelection.modelId,
          reasoningEffort: null,
          serviceTier: null,
        }
      : null,
    // 旧逻辑把 NEW_THREAD_FALLBACK_RUNTIME 作为最终候选，会在本机未装该 CLI
    // 时仍按 codex 兜底创建线程，进而触发"未登记"报错；禁止恢复。
    // NEW_THREAD_FALLBACK_RUNTIME,
  ];

  for (const candidate of candidates) {
    const resolved = resolveRuntimeCandidate(engines, candidate);
    if (resolved) {
      return resolved;
    }
  }

  return null;
}
