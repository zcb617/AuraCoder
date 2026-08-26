import { reactive } from "vue";
import type { Thread } from "../types";
import { auracoderConnectionManager } from "./auracoder-connection";

const threadsByProject = reactive<Record<string, Thread[]>>({});
const loadingByProject = reactive<Record<string, boolean>>({});

// 会话状态变化通过同一条设备级连接进入，已加载项目中的线程保持最新标题和状态。
auracoderConnectionManager.subscribe((auracoderId, event) => {
  if (event.event !== "thread.updated") return;
  const thread = event.payload.thread as Thread | undefined;
  if (!thread?.id || !thread.workspaceId) return;
  const key = projectKey(auracoderId, thread.workspaceId);
  const threads = threadsByProject[key];
  if (!threads) return;
  const index = threads.findIndex((item) => item.id === thread.id);
  if (index >= 0) threads.splice(index, 1, thread);
  else threads.unshift(thread);
});

function projectKey(auracoderId: string, workspaceId: string) {
  return `${auracoderId}:${workspaceId}`;
}

export const projectStore = {
  threadsByProject,
  loadingByProject,
  getThreads(auracoderId: string, workspaceId: string) {
    return threadsByProject[projectKey(auracoderId, workspaceId)] || [];
  },
  async load(auracoderId: string, workspaceId: string, force = false) {
    const key = projectKey(auracoderId, workspaceId);
    if (!force && threadsByProject[key]) return threadsByProject[key];
    loadingByProject[key] = true;
    try {
      const threads = await auracoderConnectionManager.request<Thread[]>(auracoderId, "thread.list", { workspace_id: workspaceId });
      threadsByProject[key] = threads;
      return threads;
    } finally {
      loadingByProject[key] = false;
    }
  },
  async create(auracoderId: string, workspaceId: string) {
    const existing = this.getThreads(auracoderId, workspaceId)[0];
    const metadata = existing?.engineMetadata || {};
    const created = await auracoderConnectionManager.request<Thread>(auracoderId, "thread.create", {
      workspace_id: workspaceId,
      engine_id: existing?.engineId || "codex",
      model_id: existing?.modelId || "gpt-5.4",
      reasoning_effort: typeof metadata.reasoningEffort === "string" ? metadata.reasoningEffort : "high",
      service_tier: typeof metadata.serviceTier === "string" ? metadata.serviceTier : undefined,
    });
    const key = projectKey(auracoderId, workspaceId);
    const previous = threadsByProject[key] || [];
    threadsByProject[key] = [created, ...previous.filter((item) => item.id !== created.id)];
    return created;
  },
  upsert(auracoderId: string, thread: Thread) {
    const key = projectKey(auracoderId, thread.workspaceId);
    const threads = threadsByProject[key];
    if (!threads) return;
    const index = threads.findIndex((item) => item.id === thread.id);
    if (index >= 0) threads.splice(index, 1, thread);
    else threads.unshift(thread);
  },
  clear(auracoderId: string) {
    Object.keys(threadsByProject)
      .filter((key) => key.startsWith(`${auracoderId}:`))
      .forEach((key) => {
        delete threadsByProject[key];
        delete loadingByProject[key];
      });
  },
};
