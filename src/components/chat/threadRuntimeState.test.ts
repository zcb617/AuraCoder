import { describe, expect, it } from "vitest";
import { canChangeUnstartedThreadEngine, collectThreadEnvironmentMismatches } from "./threadRuntimeState";
import type { EngineInfo, EngineModel } from "../../types";

describe("canChangeUnstartedThreadEngine", () => {
  it("returns false when the thread is null or undefined", () => {
    expect(canChangeUnstartedThreadEngine(null)).toBe(false);
    expect(canChangeUnstartedThreadEngine(undefined)).toBe(false);
  });

  it("returns true for an empty thread without an engine session", () => {
    expect(
      canChangeUnstartedThreadEngine({
        // 空会话没有真实 CLI 会话 ID。
        engineThreadId: null,
        // 空会话不包含任何消息。
        messageCount: 0,
      }),
    ).toBe(true);
  });

  it("returns false when the thread already has an engine session", () => {
    expect(
      canChangeUnstartedThreadEngine({
        // 已建立的 CLI 会话不能原地更换 CLI。
        engineThreadId: "engine-thread-1",
        // 即使消息数仍为零，也不能覆盖已建立的会话上下文。
        messageCount: 0,
      }),
    ).toBe(false);
  });

  it("returns false when the thread already contains messages", () => {
    expect(
      canChangeUnstartedThreadEngine({
        // 没有 CLI 会话 ID，但已有消息时仍不能更换 CLI。
        engineThreadId: null,
        // 消息数大于零表示会话已经开始。
        messageCount: 1,
      }),
    ).toBe(false);
  });
});

function buildModel(partial: Partial<EngineModel>): EngineModel {
  return {
    id: "model-1",
    displayName: "Model One",
    description: "",
    hidden: false,
    isDefault: false,
    inputModalities: [],
    attachmentModalities: [],
    supportsPersonality: false,
    defaultReasoningEffort: "medium",
    supportedReasoningEfforts: [
      { reasoningEffort: "low", description: "" },
      { reasoningEffort: "medium", description: "" },
      { reasoningEffort: "high", description: "" },
    ],
    ...partial,
  } as EngineModel;
}

function buildEngine(partial: Partial<EngineInfo>): EngineInfo {
  return {
    id: "claude",
    name: "Claude Code",
    models: [buildModel({})],
    capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
    ...partial,
  } as EngineInfo;
}

describe("collectThreadEnvironmentMismatches", () => {
  const engines = [
    buildEngine({
      id: "claude",
      models: [
        buildModel({ id: "claude-a", displayName: "Claude A", isDefault: true }),
        buildModel({ id: "claude-b", displayName: "Claude B" }),
      ],
    }),
    buildEngine({ id: "codex", name: "Codex", models: [buildModel({ id: "gpt-x", displayName: "GPT X", isDefault: true })] }),
  ];

  it("引擎、模型、思考强度都与环境一致时返回空数组", () => {
    expect(
      collectThreadEnvironmentMismatches({
        engineId: "claude",
        modelId: "claude-a",
        reasoningEffort: "high",
        engines,
      }),
    ).toEqual([]);
  });

  it("引擎为 unknown 占位或环境列表为空时不参与比对", () => {
    expect(
      collectThreadEnvironmentMismatches({ engineId: "unknown", modelId: "unknown", reasoningEffort: null, engines }),
    ).toEqual([]);
    expect(
      collectThreadEnvironmentMismatches({ engineId: "claude", modelId: "claude-a", reasoningEffort: "high", engines: [] }),
    ).toEqual([]);
  });

  it("引擎在环境中不存在时只报告 engine 一项，默认值为环境第一个引擎", () => {
    expect(
      collectThreadEnvironmentMismatches({ engineId: "removed-cli", modelId: "m", reasoningEffort: "high", engines }),
    ).toEqual([
      { kind: "engine", savedValue: "removed-cli", fallbackValue: "claude", fallbackLabel: "Claude Code" },
    ]);
  });

  it("模型在引擎下不存在时只报告 model 一项，默认值优先 isDefault 且非 hidden", () => {
    expect(
      collectThreadEnvironmentMismatches({ engineId: "claude", modelId: "removed-model", reasoningEffort: "high", engines }),
    ).toEqual([
      { kind: "model", savedValue: "removed-model", fallbackValue: "claude-a", fallbackLabel: "Claude A" },
    ]);
  });

  it("思考强度不被当前模型支持时报告 effort 一项，默认值为模型默认强度", () => {
    expect(
      collectThreadEnvironmentMismatches({
        engineId: "claude",
        modelId: "claude-b",
        reasoningEffort: "extreme",
        engines,
      }),
    ).toEqual([
      { kind: "effort", savedValue: "extreme", fallbackValue: "medium", fallbackLabel: "medium" },
    ]);
  });

  it("思考强度为空时不报告 effort", () => {
    expect(
      collectThreadEnvironmentMismatches({ engineId: "claude", modelId: "claude-b", reasoningEffort: null, engines }),
    ).toEqual([]);
  });

  it("引擎不一致时短路，不再比对模型和思考强度", () => {
    const result = collectThreadEnvironmentMismatches({
      engineId: "removed-cli",
      modelId: "removed-model",
      reasoningEffort: "extreme",
      engines,
    });
    expect(result).toHaveLength(1);
    expect(result[0]?.kind).toBe("engine");
  });
});
