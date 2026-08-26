import { describe, expect, it } from "vitest";
import {
  availableScheduledEngines,
  defaultScheduledModel,
  firstTaskLine,
  getScheduledTaskColumn,
  scheduledAgentLabel,
  scheduledThreadsForAgent,
  selectableScheduledModels,
} from "./scheduledTasks";
import type { EngineInfo, EngineModel, ScheduledTask, Thread } from "../types";

function model(id: string, overrides: Partial<EngineModel> = {}): EngineModel {
  return {
    id,
    displayName: id,
    description: "",
    hidden: false,
    isDefault: false,
    inputModalities: ["text"],
    attachmentModalities: [],
    supportsPersonality: false,
    defaultReasoningEffort: "medium",
    supportedReasoningEfforts: [],
    ...overrides,
  };
}

function engine(id: EngineInfo["id"], models: EngineModel[]): EngineInfo {
  return {
    id,
    name: id,
    models,
    capabilities: { permissionModes: [], sandboxModes: [], approvalDecisions: [] },
  };
}

function task(overrides: Partial<ScheduledTask>): ScheduledTask {
  return {
    id: "task-1",
    description: "Check workspace",
    enabled: true,
    executionDeviceId: "local",
    targetType: "new_thread",
    workspaceId: "workspace-1",
    threadId: null,
    runtimeConfig: null,
    scheduleType: "daily",
    schedule: { time: "09:00" },
    timezone: "UTC",
    nextRunAt: null,
    lastRunAt: null,
    latestRun: null,
    needsConfirmation: false,
    targetValid: true,
    createdAt: "2026-08-09T00:00:00Z",
    updatedAt: "2026-08-09T00:00:00Z",
    ...overrides,
  };
}

describe("scheduled task presentation", () => {
  it("gives confirmation precedence over enabled state", () => {
    expect(getScheduledTaskColumn(task({ enabled: true, needsConfirmation: true }))).toBe(
      "confirmation",
    );
    expect(getScheduledTaskColumn(task({ enabled: false, needsConfirmation: true }))).toBe(
      "confirmation",
    );
  });

  it("separates enabled and disabled tasks", () => {
    expect(getScheduledTaskColumn(task({ enabled: true }))).toBe("enabled");
    expect(getScheduledTaskColumn(task({ enabled: false }))).toBe("disabled");
  });

  it("uses the first non-empty line as card title", () => {
    expect(firstTaskLine("\n  First line  \nSecond line")).toBe("First line");
  });
});

describe("scheduled task runtime choices", () => {
  const codex = engine("codex", [
    model("gpt-default", { isDefault: true }),
    model("gpt-active"),
    model("gpt-legacy", { hidden: true }),
  ]);
  const claude = engine("claude", [model("sonnet", { isDefault: true })]);
  const opencode = engine("opencode", [model("openai/gpt")]);

  it("reuses engines and models already loaded by the system", () => {
    expect(
      availableScheduledEngines([codex, claude, opencode]).map((item) => item.id),
    ).toEqual(["codex", "claude", "opencode"]);
  });

  it("uses product names for scheduled agents", () => {
    expect(scheduledAgentLabel(codex)).toBe("Codex");
    expect(scheduledAgentLabel(claude)).toBe("Claude Code");
    expect(scheduledAgentLabel(opencode)).toBe("OpenCode");
  });

  it("defaults to an active default model and preserves a selected legacy model", () => {
    expect(defaultScheduledModel(codex)?.id).toBe("gpt-default");
    expect(selectableScheduledModels(codex).map((item) => item.id)).toEqual([
      "gpt-default",
      "gpt-active",
    ]);
    expect(
      selectableScheduledModels(codex, "gpt-legacy").map((item) => item.id),
    ).toEqual(["gpt-default", "gpt-active", "gpt-legacy"]);
  });

  it("filters existing chats by project and selected agent", () => {
    const threads = [
      { id: "codex-a", workspaceId: "workspace-a", engineId: "codex" },
      { id: "claude-a", workspaceId: "workspace-a", engineId: "claude" },
      { id: "codex-b", workspaceId: "workspace-b", engineId: "codex" },
    ] as Thread[];
    expect(
      scheduledThreadsForAgent(threads, "workspace-a", "codex").map((item) => item.id),
    ).toEqual(["codex-a"]);
  });
});
