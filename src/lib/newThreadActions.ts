import { useChatStore } from "../stores/chatStore";
import { useTerminalStore } from "../stores/terminalStore";
import { useThreadStore } from "../stores/threadStore";
import { useChatComposerStore } from "../stores/chatComposerStore";
import { useUiStore } from "../stores/uiStore";
import { useWorkspaceStore } from "../stores/workspaceStore";
import { resolveNewThreadTargetLayoutMode } from "./newThreadLayout";
import { getWorkspacePaneLayoutMode } from "./workspacePaneNavigation";
import { useWorkspacePaneStore } from "../stores/workspacePaneStore";

/** 初始化指定工作区的前端聊天草稿，不创建持久化会话。 */
export async function initializeWorkspaceChat(
  workspaceId: string | null | undefined,
): Promise<boolean> {
  if (!workspaceId) {
    return false;
  }

  const workspaceStore = useWorkspaceStore.getState();
  const activeWorkspaceId = workspaceStore.activeWorkspaceId;
  const terminalStore = useTerminalStore.getState();
  const currentLayoutMode =
    (activeWorkspaceId ? getWorkspacePaneLayoutMode(activeWorkspaceId) : null) ??
    (activeWorkspaceId
      ? terminalStore.workspaces[activeWorkspaceId]?.layoutMode
      : terminalStore.workspaces[workspaceId]?.layoutMode) ?? null;
  const targetLayoutMode = resolveNewThreadTargetLayoutMode(currentLayoutMode);

  useUiStore.getState().setActiveView("chat");
  useThreadStore.getState().setActiveThread(null);
  await useChatStore.getState().setActiveThread(null);
  useChatComposerStore.getState().initializeWorkspaceDraft(workspaceId);
  workspaceStore.setActiveWorkspaceLocal(workspaceId);
  useWorkspacePaneStore.getState().applyLegacyLayoutMode(workspaceId, targetLayoutMode);
  return true;
}

/*
 * 旧的新建动作会立即创建数据库会话并激活其 CLI 上下文，现由纯前端草稿初始化替代：
 * import { t } from "../i18n";
 * import { toast } from "../stores/toastStore";
 * import { applyWorkspaceLayoutMode } from "./workspacePaneNavigation";
 * export async function createAndActivateWorkspaceThread(
 *   workspaceId: string | null | undefined,
 * ): Promise<string | null> {
 *   if (!workspaceId) return null;
 *   const workspaceStore = useWorkspaceStore.getState();
 *   const activeWorkspaceId = workspaceStore.activeWorkspaceId;
 *   const terminalStore = useTerminalStore.getState();
 *   const currentLayoutMode =
 *     (activeWorkspaceId ? getWorkspacePaneLayoutMode(activeWorkspaceId) : null) ??
 *     (activeWorkspaceId
 *       ? terminalStore.workspaces[activeWorkspaceId]?.layoutMode
 *       : terminalStore.workspaces[workspaceId]?.layoutMode) ?? null;
 *   const targetLayoutMode = resolveNewThreadTargetLayoutMode(currentLayoutMode);
 *   useUiStore.getState().setActiveView("chat");
 *   if (activeWorkspaceId !== workspaceId) {
 *     await workspaceStore.setActiveWorkspace(workspaceId);
 *   }
 *   applyWorkspaceLayoutMode(workspaceId, targetLayoutMode);
 *   let threadId: string | null;
 *   try {
 *     threadId = await useThreadStore.getState().createThread({
 *       workspaceId,
 *       title: t("app:sidebar.newThreadTitle"),
 *     });
 *   } catch {
 *     toast.error(t("app:sidebar.newThreadFailed"));
 *     return null;
 *   }
 *   if (!threadId) {
 *     toast.error(t("app:sidebar.newThreadNoCli"));
 *     return null;
 *   }
 *   await useChatStore.getState().setActiveThread(threadId);
 *   return threadId;
 * }
 */
