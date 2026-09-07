import type { EngineInfo, Thread } from "../../types";
import { resolveReasoningEffortForModel } from "./reasoningEffort";

/**
 * 判断会话是否仍处于首次发送前的空状态。
 *
 * 空会话尚未建立 CLI 会话且没有消息，可以在首次发送前原地切换 CLI；
 * 具体工作区位置和后端消息表校验不属于本地状态预判职责。
 */
export function canChangeUnstartedThreadEngine(
  thread: Pick<Thread, "engineThreadId" | "messageCount"> | null | undefined,
): boolean {
  return Boolean(
    thread &&
      thread.engineThreadId === null &&
      thread.messageCount === 0,
  );
}

/** 第二条路比对发现的不一致字段类别。 */
export type ThreadEnvironmentMismatchKind = "engine" | "model" | "effort";

/** 一条不一致项：数据库保存的值、建议切换的默认值及其展示名称。 */
export interface ThreadEnvironmentMismatch {
  kind: ThreadEnvironmentMismatchKind;
  /** 数据库保存的原始值（引擎 id / 模型 id / 思考强度）。 */
  savedValue: string;
  /** 建议切换到的默认值（引擎 id / 模型 id / 思考强度）。 */
  fallbackValue: string;
  /** 默认值展示名称（引擎 name / 模型 displayName / 思考强度原值）。 */
  fallbackLabel: string;
}

/**
 * 第二条路：CLI 环境加载完成后，将环境可用数据与会话数据库保存值比对。
 * 纯函数，只输出不一致项列表，不修改任何状态；是否切换由用户决定。
 * 比对按 engine → model → effort 短路：上一级不一致时，下级无从比对，直接返回。
 * unknown 是新会话落库前的占位值，不参与比对。
 */
export function collectThreadEnvironmentMismatches(input: {
  engineId: string | null | undefined;
  modelId: string | null | undefined;
  reasoningEffort: string | null | undefined;
  engines: EngineInfo[];
}): ThreadEnvironmentMismatch[] {
  const { engineId, modelId, reasoningEffort, engines } = input;
  if (!engineId || engineId === "unknown" || engines.length === 0) {
    return [];
  }
  const engine = engines.find((item) => item.id === engineId);
  if (!engine) {
    return [
      {
        kind: "engine",
        savedValue: engineId,
        fallbackValue: engines[0].id,
        fallbackLabel: engines[0].name,
      },
    ];
  }
  if (!modelId || modelId === "unknown" || engine.models.length === 0) {
    return [];
  }
  const model = engine.models.find((item) => item.id === modelId);
  if (!model) {
    const fallbackModel =
      engine.models.find((item) => item.isDefault && !item.hidden) ??
      engine.models.find((item) => !item.hidden) ??
      engine.models[0];
    return [
      {
        kind: "model",
        savedValue: modelId,
        fallbackValue: fallbackModel.id,
        fallbackLabel: fallbackModel.displayName,
      },
    ];
  }
  const normalizedEffort = reasoningEffort?.trim().toLowerCase() || null;
  if (!normalizedEffort || model.supportedReasoningEfforts.length === 0) {
    return [];
  }
  const effortSupported = model.supportedReasoningEfforts.some(
    (option) => option.reasoningEffort.trim().toLowerCase() === normalizedEffort,
  );
  if (effortSupported) {
    return [];
  }
  const fallbackEffort = resolveReasoningEffortForModel(model, null);
  if (!fallbackEffort) {
    return [];
  }
  return [
    {
      kind: "effort",
      savedValue: normalizedEffort,
      fallbackValue: fallbackEffort,
      fallbackLabel: fallbackEffort,
    },
  ];
}
