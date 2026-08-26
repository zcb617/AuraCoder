import type {
  EngineInfo,
  EngineModel,
  ScheduledTask,
  Thread,
} from "../types";

export type ScheduledTaskColumn = "disabled" | "enabled" | "confirmation";

export function getScheduledTaskColumn(task: ScheduledTask): ScheduledTaskColumn {
  if (task.needsConfirmation) return "confirmation";
  return task.enabled ? "enabled" : "disabled";
}

export function firstTaskLine(description: string): string {
  return description.split(/\r?\n/).find((line) => line.trim())?.trim() || "Scheduled task";
}

export function availableScheduledEngines(
  engines: ReadonlyArray<EngineInfo>,
): EngineInfo[] {
  return engines.filter((engine) => engine.models.length > 0);
}

export function scheduledAgentLabel(engine: Pick<EngineInfo, "id" | "name">): string {
  if (engine.id === "claude") return "Claude Code";
  if (engine.id === "opencode") return "OpenCode";
  if (engine.id === "codex") return "Codex";
  return engine.name;
}

export function selectableScheduledModels(
  engine: EngineInfo | null | undefined,
  selectedModelId?: string | null,
): EngineModel[] {
  if (!engine) return [];
  const active = engine.models.filter((model) => !model.hidden);
  const selectedLegacy = engine.models.find(
    (model) => model.hidden && model.id === selectedModelId,
  );
  if (active.length === 0) return [...engine.models];
  return selectedLegacy ? [...active, selectedLegacy] : active;
}

export function defaultScheduledModel(
  engine: EngineInfo | null | undefined,
): EngineModel | null {
  if (!engine) return null;
  return (
    engine.models.find((model) => !model.hidden && model.isDefault) ??
    engine.models.find((model) => !model.hidden) ??
    engine.models[0] ??
    null
  );
}

export function scheduledThreadsForAgent(
  threads: ReadonlyArray<Thread>,
  workspaceId: string,
  engineId: string,
): Thread[] {
  return threads.filter(
    (thread) => thread.workspaceId === workspaceId && thread.engineId === engineId,
  );
}
