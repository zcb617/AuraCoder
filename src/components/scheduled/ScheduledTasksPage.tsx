import { useEffect, useMemo, useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import {
  AlertCircle,
  CalendarClock,
  CheckCircle2,
  Clock3,
  MessageSquare,
  Pencil,
  Plus,
  Power,
  Trash2,
  X,
} from "lucide-react";
import { listenScheduledTaskDeleted, listenScheduledTaskUpdated } from "../../lib/ipc";
import { NEW_THREAD_FALLBACK_RUNTIME } from "../../lib/newThreadRuntime";
import {
  availableScheduledEngines,
  defaultScheduledModel,
  firstTaskLine,
  getScheduledTaskColumn,
  scheduledAgentLabel,
  scheduledThreadsForAgent,
  selectableScheduledModels,
} from "../../lib/scheduledTasks";
import { useChatComposerStore } from "../../stores/chatComposerStore";
import { useChatStore } from "../../stores/chatStore";
import { useEngineStore } from "../../stores/engineStore";
import { useScheduledTaskStore } from "../../stores/scheduledTaskStore";
import { useThreadStore } from "../../stores/threadStore";
import { useUiStore } from "../../stores/uiStore";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import type {
  DailyScheduleConfig,
  IntervalScheduleConfig,
  ScheduledRuntimeConfig,
  ScheduledTask,
  ScheduledTaskInput,
  ScheduledTaskScheduleType,
  ScheduledTaskTargetType,
  Thread,
  WeeklyScheduleConfig,
} from "../../types";
import { resolveReasoningEffortForModel } from "../chat/reasoningEffort";
import { ConfirmDialog } from "../shared/ConfirmDialog";

type BoardColumn = "disabled" | "enabled" | "confirmation";

const COLUMN_ICONS = {
  disabled: Power,
  enabled: CheckCircle2,
  confirmation: AlertCircle,
} as const;

function localDateString(date = new Date()): string {
  const year = date.getFullYear();
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${year}-${month}-${day}`;
}

function localWeekday(date = new Date()): number {
  const weekday = date.getDay();
  return weekday === 0 ? 7 : weekday;
}

function formatDateTime(value: string | null, locale: string): string | null {
  if (!value) return null;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return null;
  return new Intl.DateTimeFormat(locale, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function runtimeFromThread(thread: Thread | undefined): ScheduledRuntimeConfig | null {
  if (!thread) return null;
  const metadata = thread.engineMetadata ?? {};
  return {
    engineId: thread.engineId,
    modelId: thread.modelId,
    repoId: thread.repoId,
    reasoningEffort: thread.reasoningEffort,
    serviceTier: typeof metadata.serviceTier === "string" ? metadata.serviceTier : null,
  };
}

function scheduleSummary(
  task: ScheduledTask,
  t: ReturnType<typeof useTranslation>["t"],
  weekdayLabels: string[],
): string {
  if (task.scheduleType === "interval") {
    const schedule = task.schedule as IntervalScheduleConfig;
    return `${t("every")} ${schedule.every} ${t(schedule.unit)}`;
  }
  if (task.scheduleType === "daily") {
    const schedule = task.schedule as DailyScheduleConfig;
    return `${t("daily")} · ${schedule.time}`;
  }
  const schedule = task.schedule as WeeklyScheduleConfig;
  const days = schedule.weekdays.map((day) => weekdayLabels[day - 1]).filter(Boolean).join("、");
  return `${t("every")} ${schedule.everyWeeks} ${t("weeks")} · ${days} · ${schedule.time}`;
}

function ScheduledTaskCard({
  task,
  onEdit,
  onDelete,
  onOpenChat,
}: {
  task: ScheduledTask;
  onEdit: () => void;
  onDelete: () => void;
  onOpenChat: () => void;
}) {
  const { t, i18n } = useTranslation("scheduled");
  const setEnabled = useScheduledTaskStore((state) => state.setEnabled);
  const acknowledge = useScheduledTaskStore((state) => state.acknowledge);
  const workspaces = useWorkspaceStore((state) => state.workspaces);
  const threads = useThreadStore((state) => state.threads);
  const weekdayLabels = t("weekdays", { returnObjects: true }) as unknown as string[];
  const workspace = workspaces.find((candidate) => candidate.id === task.workspaceId);
  const thread = threads.find((candidate) => candidate.id === task.threadId);
  const target = task.targetType === "new_thread"
    ? t("targetNew", { workspace: workspace?.name ?? task.workspaceId })
    : t("targetExisting", {
        workspace: workspace?.name ?? task.workspaceId,
        thread: thread?.title || task.threadId || t("selectThread"),
      });
  const next = formatDateTime(task.nextRunAt, i18n.language);
  const latest = task.latestRun;
  const canOpenChat = Boolean(latest?.threadId);
  const canAcknowledge = task.needsConfirmation && latest?.status !== "needs_confirmation";

  return (
    <article className={`scheduled-card scheduled-card-${getScheduledTaskColumn(task)}`}>
      <div className="scheduled-card-heading">
        <h3>{firstTaskLine(task.description)}</h3>
        <button
          type="button"
          className={`scheduled-power ${task.enabled ? "is-on" : ""}`}
          title={t(task.enabled ? "disable" : "enable")}
          onClick={() => void setEnabled(task.id, !task.enabled)}
        >
          <Power size={15} />
        </button>
      </div>
      <p className="scheduled-card-description">{task.description}</p>
      <div className="scheduled-card-target">
        <MessageSquare size={13} />
        <span>{target}</span>
      </div>
      <div className="scheduled-card-meta">
        <span><Clock3 size={12} />{scheduleSummary(task, t, weekdayLabels)}</span>
        <span><CalendarClock size={12} />{next ? t("nextRun", { time: next }) : t("noNextRun")}</span>
      </div>
      {latest ? (
        <div className={`scheduled-last-run scheduled-run-${latest.status}`}>
          {t("lastResult", { status: t(`statuses.${latest.status}`) })}
          {latest.errorMessage ? <span>{latest.errorMessage}</span> : null}
        </div>
      ) : null}
      <div className="scheduled-card-actions">
        {canOpenChat ? (
          <button type="button" onClick={onOpenChat}>
            <MessageSquare size={13} />{t("openChat")}
          </button>
        ) : null}
        {canAcknowledge ? (
          <button type="button" onClick={() => void acknowledge(task.id)}>
            <CheckCircle2 size={13} />{t("acknowledge")}
          </button>
        ) : null}
        <span className="scheduled-card-actions-spacer" />
        <button type="button" title={t("edit")} onClick={onEdit}><Pencil size={14} /></button>
        <button type="button" title={t("delete")} onClick={onDelete}><Trash2 size={14} /></button>
      </div>
    </article>
  );
}

function ScheduledTaskModal({
  task,
  onClose,
}: {
  task: ScheduledTask | null;
  onClose: () => void;
}) {
  const { t } = useTranslation("scheduled");
  const workspaces = useWorkspaceStore((state) => state.workspaces);
  const threads = useThreadStore((state) => state.threads);
  const runtimeByWorkspace = useChatComposerStore((state) => state.runtimeByWorkspace);
  const engines = useEngineStore((state) => state.engines);
  const createTask = useScheduledTaskStore((state) => state.createTask);
  const updateTask = useScheduledTaskStore((state) => state.updateTask);
  const saving = useScheduledTaskStore((state) => state.saving);
  const storeError = useScheduledTaskStore((state) => state.error);
  const initialTargetType = task?.targetType ?? "existing_thread";
  const initialWorkspaceId = task?.workspaceId ?? workspaces[0]?.id ?? "";
  const initialThread = threads.find((thread) => thread.id === task?.threadId)
    ?? threads.find((thread) => thread.workspaceId === initialWorkspaceId);
  const initialRuntimeSource = task?.runtimeConfig
    ?? (initialTargetType === "existing_thread" ? runtimeFromThread(initialThread) : null)
    ?? runtimeByWorkspace[initialWorkspaceId]
    ?? runtimeFromThread(initialThread)
    ?? { ...NEW_THREAD_FALLBACK_RUNTIME, repoId: null };
  const initialRuntime: ScheduledRuntimeConfig = {
    ...initialRuntimeSource,
    repoId: "repoId" in initialRuntimeSource ? initialRuntimeSource.repoId : null,
  };

  const [description, setDescription] = useState(task?.description ?? "");
  const [enabled] = useState(task?.enabled ?? true);
  const [targetType, setTargetType] = useState<ScheduledTaskTargetType>(
    initialTargetType,
  );
  const [workspaceId, setWorkspaceId] = useState(initialWorkspaceId);
  const [threadId, setThreadId] = useState(task?.threadId ?? initialThread?.id ?? "");
  const [engineId, setEngineId] = useState(initialRuntime.engineId);
  const [modelId, setModelId] = useState(initialRuntime.modelId);
  const [reasoningEffort, setReasoningEffort] = useState<string | null>(
    initialRuntime.reasoningEffort ?? null,
  );
  const [runtimeRepoId, setRuntimeRepoId] = useState(initialRuntime.repoId ?? null);
  const [serviceTier, setServiceTier] = useState(initialRuntime.serviceTier ?? null);
  const [scheduleType, setScheduleType] = useState<ScheduledTaskScheduleType>(
    task?.scheduleType ?? "daily",
  );
  const initialInterval = task?.scheduleType === "interval"
    ? task.schedule as IntervalScheduleConfig
    : { every: 30, unit: "minutes" as const };
  const initialDaily = task?.scheduleType === "daily"
    ? task.schedule as DailyScheduleConfig
    : { time: "09:00" };
  const initialWeekly = task?.scheduleType === "weekly"
    ? task.schedule as WeeklyScheduleConfig
    : { everyWeeks: 1, weekdays: [localWeekday()], time: "09:00", anchorDate: localDateString() };
  const [intervalEvery, setIntervalEvery] = useState(initialInterval.every);
  const [intervalUnit, setIntervalUnit] = useState<IntervalScheduleConfig["unit"]>(initialInterval.unit);
  const [dailyTime, setDailyTime] = useState(initialDaily.time);
  const [weeklyEvery, setWeeklyEvery] = useState(initialWeekly.everyWeeks);
  const [weeklyDays, setWeeklyDays] = useState(initialWeekly.weekdays);
  const [weeklyTime, setWeeklyTime] = useState(initialWeekly.time);
  const [validationError, setValidationError] = useState<string | null>(null);
  const weekdayLabels = t("weekdays", { returnObjects: true }) as unknown as string[];
  const availableEngines = useMemo(
    () => availableScheduledEngines(engines),
    [engines],
  );
  const selectedEngine = useMemo(
    () => availableEngines.find((engine) => engine.id === engineId) ?? null,
    [availableEngines, engineId],
  );
  const selectableModels = useMemo(
    () => selectableScheduledModels(selectedEngine, modelId),
    [modelId, selectedEngine],
  );
  const selectedModel = useMemo(
    () => selectableModels.find((model) => model.id === modelId) ?? null,
    [modelId, selectableModels],
  );
  const availableThreads = useMemo(
    () => scheduledThreadsForAgent(threads, workspaceId, engineId),
    [engineId, threads, workspaceId],
  );

  useEffect(() => {
    if (availableEngines.length === 0) return;
    if (availableEngines.some((engine) => engine.id === engineId)) return;
    const nextEngine = availableEngines[0];
    const nextModel = defaultScheduledModel(nextEngine);
    setEngineId(nextEngine.id);
    setModelId(nextModel?.id ?? "");
    setReasoningEffort(resolveReasoningEffortForModel(nextModel, null));
    setRuntimeRepoId(null);
    setServiceTier(null);
  }, [availableEngines, engineId]);

  useEffect(() => {
    if (!selectedEngine || selectedModel) return;
    const nextModel = defaultScheduledModel(selectedEngine);
    setModelId(nextModel?.id ?? "");
    setReasoningEffort(resolveReasoningEffortForModel(nextModel, null));
  }, [selectedEngine, selectedModel]);

  useEffect(() => {
    setReasoningEffort((current) => resolveReasoningEffortForModel(selectedModel, current));
  }, [selectedModel]);

  useEffect(() => {
    if (targetType === "existing_thread" && !availableThreads.some((thread) => thread.id === threadId)) {
      setThreadId(availableThreads[0]?.id ?? "");
    }
  }, [availableThreads, targetType, threadId]);

  const handleAgentChange = (nextEngineId: string) => {
    const nextEngine = availableEngines.find((engine) => engine.id === nextEngineId);
    if (!nextEngine) return;
    const nextModel = defaultScheduledModel(nextEngine);
    setEngineId(nextEngine.id);
    setModelId(nextModel?.id ?? "");
    setReasoningEffort(resolveReasoningEffortForModel(nextModel, null));
    setRuntimeRepoId(null);
    setServiceTier(null);
  };

  const handleModelChange = (nextModelId: string) => {
    const nextModel = selectableModels.find((model) => model.id === nextModelId) ?? null;
    setModelId(nextModelId);
    setReasoningEffort(resolveReasoningEffortForModel(nextModel, reasoningEffort));
  };

  const handleThreadChange = (nextThreadId: string) => {
    setThreadId(nextThreadId);
    const nextRuntime = runtimeFromThread(
      availableThreads.find((thread) => thread.id === nextThreadId),
    );
    if (!nextRuntime || nextRuntime.engineId !== engineId) return;
    const nextModel = selectableModels.find((model) => model.id === nextRuntime.modelId);
    if (!nextModel) return;
    setModelId(nextModel.id);
    setReasoningEffort(
      resolveReasoningEffortForModel(nextModel, nextRuntime.reasoningEffort),
    );
  };

  const submit = async () => {
    if (!description.trim()) return setValidationError(t("validation.description"));
    if (!selectedEngine) return setValidationError(t("validation.agent"));
    if (!selectedModel) return setValidationError(t("validation.model"));
    if (selectedModel.supportedReasoningEfforts.length > 0 && !reasoningEffort) {
      return setValidationError(t("validation.reasoning"));
    }
    if (!workspaceId) return setValidationError(t("validation.workspace"));
    if (targetType === "existing_thread" && !threadId) {
      return setValidationError(t("validation.thread"));
    }
    if (scheduleType === "interval" && intervalEvery <= 0) {
      return setValidationError(t("validation.positive"));
    }
    if (scheduleType === "weekly" && weeklyDays.length === 0) {
      return setValidationError(t("validation.weekdays"));
    }

    const schedule = scheduleType === "interval"
      ? { every: intervalEvery, unit: intervalUnit }
      : scheduleType === "daily"
        ? { time: dailyTime }
        : {
            everyWeeks: weeklyEvery,
            weekdays: weeklyDays,
            time: weeklyTime,
            anchorDate: initialWeekly.anchorDate || localDateString(),
          };
    const selectedThread = availableThreads.find((thread) => thread.id === threadId);
    const runtimeConfig: ScheduledRuntimeConfig = {
      engineId: selectedEngine.id,
      modelId: selectedModel.id,
      repoId: targetType === "existing_thread" ? selectedThread?.repoId ?? null : runtimeRepoId,
      reasoningEffort,
      serviceTier: selectedEngine.id === "codex" ? serviceTier : null,
    };
    const input: ScheduledTaskInput = {
      description: description.trim(),
      enabled,
      executionDeviceId: "local",
      targetType,
      workspaceId,
      threadId: targetType === "existing_thread" ? threadId : null,
      runtimeConfig,
      scheduleType,
      schedule,
      timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
    };
    const saved = task
      ? await updateTask(task.id, input)
      : await createTask(input);
    if (saved) onClose();
  };

  if (typeof document === "undefined") return null;
  return createPortal(
    <div className="scheduled-modal-backdrop" onMouseDown={onClose}>
      <div className="scheduled-modal" onMouseDown={(event) => event.stopPropagation()}>
        <header className="scheduled-modal-header">
          <div>
            <h2>{t(task ? "edit" : "create")}</h2>
            <p>{t("subtitle")}</p>
          </div>
          <button type="button" onClick={onClose}><X size={18} /></button>
        </header>
        <div className="scheduled-modal-body">
          <label className="scheduled-description-field">
            <span>{t("description")}</span>
            <textarea
              value={description}
              onChange={(event) => setDescription(event.target.value)}
              placeholder={t("descriptionPlaceholder")}
              autoFocus
            />
          </label>

          <section className="scheduled-form-section">
            <div className="scheduled-form-row">
              <label>{t("device")}</label>
              <select disabled value="local"><option value="local">{t("localDevice")}</option></select>
            </div>
            <div className="scheduled-form-row">
              <label>{t("agent")}</label>
              <select
                aria-label={t("agent")}
                disabled={availableEngines.length === 0}
                value={selectedEngine?.id ?? ""}
                onChange={(event) => handleAgentChange(event.target.value)}
              >
                {availableEngines.length === 0 ? (
                  <option value="">{t("noAvailableAgents")}</option>
                ) : null}
                {availableEngines.map((engine) => (
                  <option key={engine.id} value={engine.id}>{scheduledAgentLabel(engine)}</option>
                ))}
              </select>
            </div>
            <div className="scheduled-form-row">
              <label>{t("model")}</label>
              <select
                aria-label={t("model")}
                disabled={!selectedEngine || selectableModels.length === 0}
                value={selectedModel?.id ?? ""}
                onChange={(event) => handleModelChange(event.target.value)}
              >
                {selectableModels.length === 0 ? <option value="">{t("noModels")}</option> : null}
                {selectableModels.map((model) => (
                  <option key={model.id} value={model.id}>
                    {model.displayName || model.id}{model.hidden ? ` · ${t("legacyModel")}` : ""}
                  </option>
                ))}
              </select>
            </div>
            <div className="scheduled-form-row">
              <label>{t("reasoning")}</label>
              <select
                aria-label={t("reasoning")}
                disabled={!selectedModel || selectedModel.supportedReasoningEfforts.length === 0}
                value={reasoningEffort ?? ""}
                onChange={(event) => setReasoningEffort(event.target.value || null)}
              >
                {selectedModel?.supportedReasoningEfforts.length ? null : (
                  <option value="">{t("reasoningNotSupported")}</option>
                )}
                {selectedModel?.supportedReasoningEfforts.map((option) => (
                  <option key={option.reasoningEffort} value={option.reasoningEffort}>
                    {t(`reasoningLevels.${option.reasoningEffort}`, {
                      defaultValue: option.reasoningEffort,
                    })}
                  </option>
                ))}
              </select>
            </div>
            <div className="scheduled-form-row">
              <label>{t("runsIn")}</label>
              <select value={targetType} onChange={(event) => setTargetType(event.target.value as ScheduledTaskTargetType)}>
                <option value="existing_thread">{t("existingThread")}</option>
                <option value="new_thread">{t("newThread")}</option>
              </select>
            </div>
            <div className="scheduled-form-row">
              <label>{t("workspace")}</label>
              <select value={workspaceId} onChange={(event) => {
                setWorkspaceId(event.target.value);
                setRuntimeRepoId(null);
              }}>
                <option value="">{t("selectWorkspace")}</option>
                {workspaces.map((workspace) => <option key={workspace.id} value={workspace.id}>{workspace.name}</option>)}
              </select>
            </div>
            {targetType === "existing_thread" ? (
              <div className="scheduled-form-row">
                <label>{t("thread")}</label>
                <select value={threadId} onChange={(event) => handleThreadChange(event.target.value)}>
                  <option value="">{availableThreads.length === 0 ? t("noThreadsForAgent") : t("selectThread")}</option>
                  {availableThreads.map((thread) => <option key={thread.id} value={thread.id}>{thread.title || t("selectThread")}</option>)}
                </select>
              </div>
            ) : null}
          </section>

          <div className="scheduled-section-label">{t("frequency")}</div>
          <section className="scheduled-form-section">
            <div className="scheduled-form-row">
              <label>{t("repeat")}</label>
              <select value={scheduleType} onChange={(event) => setScheduleType(event.target.value as ScheduledTaskScheduleType)}>
                <option value="interval">{t("interval")}</option>
                <option value="daily">{t("daily")}</option>
                <option value="weekly">{t("custom")}</option>
              </select>
            </div>
            {scheduleType === "interval" ? (
              <div className="scheduled-form-row scheduled-inline-fields">
                <label>{t("every")}</label>
                <span>
                  <input type="number" min={1} value={intervalEvery} onChange={(event) => setIntervalEvery(Number(event.target.value))} />
                  <select value={intervalUnit} onChange={(event) => setIntervalUnit(event.target.value as IntervalScheduleConfig["unit"])}>
                    <option value="minutes">{t("minutes")}</option>
                    <option value="hours">{t("hours")}</option>
                    <option value="days">{t("days")}</option>
                  </select>
                </span>
              </div>
            ) : null}
            {scheduleType === "daily" ? (
              <div className="scheduled-form-row"><label>{t("at")}</label><input type="time" value={dailyTime} onChange={(event) => setDailyTime(event.target.value)} /></div>
            ) : null}
            {scheduleType === "weekly" ? (
              <>
                <div className="scheduled-form-row scheduled-inline-fields">
                  <label>{t("every")}</label>
                  <span><input type="number" min={1} value={weeklyEvery} onChange={(event) => setWeeklyEvery(Number(event.target.value))} /><em>{t("weeks")}</em></span>
                </div>
                <div className="scheduled-form-row scheduled-weekdays-row">
                  <label>{t("startsOn")}</label>
                  <div className="scheduled-weekdays">
                    {weekdayLabels.map((label, index) => {
                      const value = index + 1;
                      const selected = weeklyDays.includes(value);
                      return <button key={value} type="button" className={selected ? "is-selected" : ""} onClick={() => setWeeklyDays(selected ? weeklyDays.filter((day) => day !== value) : [...weeklyDays, value].sort())}>{label}</button>;
                    })}
                  </div>
                </div>
                <div className="scheduled-form-row"><label>{t("at")}</label><input type="time" value={weeklyTime} onChange={(event) => setWeeklyTime(event.target.value)} /></div>
              </>
            ) : null}
          </section>
          {validationError || storeError ? <div className="scheduled-form-error"><AlertCircle size={14} />{validationError || storeError}</div> : null}
        </div>
        <footer className="scheduled-modal-footer">
          <button type="button" className="btn btn-ghost" onClick={onClose}>{t("cancel")}</button>
          <button type="button" className="btn btn-primary" disabled={saving} onClick={() => void submit()}>{t("save")}</button>
        </footer>
      </div>
    </div>,
    document.body,
  );
}

export function ScheduledTasksPage() {
  const { t } = useTranslation("scheduled");
  const tasks = useScheduledTaskStore((state) => state.tasks);
  const loading = useScheduledTaskStore((state) => state.loading);
  const error = useScheduledTaskStore((state) => state.error);
  const load = useScheduledTaskStore((state) => state.load);
  const deleteTask = useScheduledTaskStore((state) => state.deleteTask);
  const [editing, setEditing] = useState<ScheduledTask | null | undefined>(undefined);
  const [deleting, setDeleting] = useState<ScheduledTask | null>(null);

  useEffect(() => {
    void load();
    let disposed = false;
    const unlisteners: Array<() => void> = [];
    void Promise.all([
      listenScheduledTaskUpdated(() => void load()),
      listenScheduledTaskDeleted(() => void load()),
    ]).then((listeners) => {
      if (disposed) listeners.forEach((unlisten) => unlisten());
      else unlisteners.push(...listeners);
    });
    return () => {
      disposed = true;
      unlisteners.forEach((unlisten) => unlisten());
    };
  }, [load]);

  const grouped = useMemo(() => {
    const columns: Record<BoardColumn, ScheduledTask[]> = {
      disabled: [],
      enabled: [],
      confirmation: [],
    };
    tasks.forEach((task) => columns[getScheduledTaskColumn(task)].push(task));
    return columns;
  }, [tasks]);

  const openTaskChat = async (task: ScheduledTask) => {
    const threadId = task.latestRun?.threadId;
    if (!threadId) return;
    useUiStore.getState().setActiveView("chat");
    await useWorkspaceStore.getState().setActiveWorkspace(task.workspaceId);
    await useThreadStore.getState().refreshThreads(task.workspaceId);
    const thread = useThreadStore.getState().threads.find((candidate) => candidate.id === threadId);
    useWorkspaceStore.getState().setActiveRepo(thread?.repoId ?? null, { remember: false });
    useThreadStore.getState().setActiveThread(threadId);
    await useChatStore.getState().setActiveThread(threadId);
  };

  return (
    <div className="scheduled-page">
      <header className="scheduled-page-header">
        <div><h1>{t("title")}</h1><p>{t("subtitle")}</p></div>
        <button type="button" className="scheduled-create-button" onClick={() => setEditing(null)}><Plus size={16} />{t("create")}</button>
      </header>
      {error ? <div className="scheduled-page-error"><AlertCircle size={15} />{error}</div> : null}
      {loading && tasks.length === 0 ? <div className="scheduled-loading">{t("loading")}</div> : (
        <div className="scheduled-board">
          {(["disabled", "enabled", "confirmation"] as BoardColumn[]).map((column) => {
            const Icon = COLUMN_ICONS[column];
            return (
              <section key={column} className={`scheduled-column scheduled-column-${column}`}>
                <header><span><Icon size={15} />{t(`columns.${column}`)}</span><b>{grouped[column].length}</b></header>
                <div className="scheduled-column-body">
                  {grouped[column].length === 0 ? <div className="scheduled-empty">{t("empty")}</div> : grouped[column].map((task) => (
                    <ScheduledTaskCard key={task.id} task={task} onEdit={() => setEditing(task)} onDelete={() => setDeleting(task)} onOpenChat={() => void openTaskChat(task)} />
                  ))}
                </div>
              </section>
            );
          })}
        </div>
      )}
      {editing !== undefined ? <ScheduledTaskModal task={editing} onClose={() => setEditing(undefined)} /> : null}
      <ConfirmDialog
        open={Boolean(deleting)}
        title={t("deleteTitle")}
        message={t("deleteMessage")}
        confirmLabel={t("delete")}
        onCancel={() => setDeleting(null)}
        onConfirm={() => {
          if (deleting) void deleteTask(deleting.id);
          setDeleting(null);
        }}
      />
    </div>
  );
}
