import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type { ScheduledTask, ScheduledTaskInput } from "../types";

interface ScheduledTaskState {
  tasks: ScheduledTask[];
  loading: boolean;
  saving: boolean;
  error?: string;
  load: () => Promise<void>;
  createTask: (input: ScheduledTaskInput) => Promise<ScheduledTask | null>;
  updateTask: (taskId: string, input: ScheduledTaskInput) => Promise<ScheduledTask | null>;
  setEnabled: (taskId: string, enabled: boolean) => Promise<void>;
  acknowledge: (taskId: string) => Promise<void>;
  deleteTask: (taskId: string) => Promise<void>;
}

function upsertTask(tasks: ScheduledTask[], task: ScheduledTask): ScheduledTask[] {
  return [task, ...tasks.filter((candidate) => candidate.id !== task.id)].sort(
    (left, right) =>
      new Date(right.updatedAt).getTime() - new Date(left.updatedAt).getTime(),
  );
}

export const useScheduledTaskStore = create<ScheduledTaskState>((set, get) => ({
  tasks: [],
  loading: false,
  saving: false,
  load: async () => {
    set({ loading: true, error: undefined });
    try {
      const tasks = await ipc.listScheduledTasks();
      set({ tasks, loading: false });
    } catch (error) {
      set({ loading: false, error: String(error) });
    }
  },
  createTask: async (input) => {
    set({ saving: true, error: undefined });
    try {
      const task = await ipc.createScheduledTask(input);
      set({ tasks: upsertTask(get().tasks, task), saving: false });
      return task;
    } catch (error) {
      set({ saving: false, error: String(error) });
      return null;
    }
  },
  updateTask: async (taskId, input) => {
    set({ saving: true, error: undefined });
    try {
      const task = await ipc.updateScheduledTask(taskId, input);
      set({ tasks: upsertTask(get().tasks, task), saving: false });
      return task;
    } catch (error) {
      set({ saving: false, error: String(error) });
      return null;
    }
  },
  setEnabled: async (taskId, enabled) => {
    set({ error: undefined });
    try {
      const task = await ipc.setScheduledTaskEnabled(taskId, enabled);
      set({ tasks: upsertTask(get().tasks, task) });
    } catch (error) {
      set({ error: String(error) });
    }
  },
  acknowledge: async (taskId) => {
    set({ error: undefined });
    try {
      const task = await ipc.acknowledgeScheduledTask(taskId);
      set({ tasks: upsertTask(get().tasks, task) });
    } catch (error) {
      set({ error: String(error) });
    }
  },
  deleteTask: async (taskId) => {
    set({ error: undefined });
    try {
      await ipc.deleteScheduledTask(taskId);
      set({ tasks: get().tasks.filter((task) => task.id !== taskId) });
    } catch (error) {
      set({ error: String(error) });
    }
  },
}));
