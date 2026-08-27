import { t } from "../i18n";
import { useChatStore } from "../stores/chatStore";
import { useTerminalStore } from "../stores/terminalStore";
import { useThreadStore } from "../stores/threadStore";
import { toast } from "../stores/toastStore";
import { useUiStore } from "../stores/uiStore";
import { useWorkspaceStore } from "../stores/workspaceStore";
import { resolveNewThreadTargetLayoutMode } from "./newThreadLayout";
import {
  applyWorkspaceLayoutMode,
  getWorkspacePaneLayoutMode,
} from "./workspacePaneNavigation";

/** 创建并激活指定工作区的新会话，失败时仅向用户展示固定业务提示。 */
export async function createAndActivateWorkspaceThread(
  workspaceId: string | null | undefined,
): Promise<string | null> {
  if (!workspaceId) {
    return null;
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

  if (activeWorkspaceId !== workspaceId) {
    await workspaceStore.setActiveWorkspace(workspaceId);
  }

  applyWorkspaceLayoutMode(workspaceId, targetLayoutMode);
  let threadId: string | null;
  try {
    threadId = await useThreadStore.getState().createThread({
      workspaceId,
      title: t("app:sidebar.newThreadTitle"),
    });
  } catch {
    toast.error(t("app:sidebar.newThreadFailed"));
    return null;
  }

  if (!threadId) {
    return null;
  }

  await useChatStore.getState().setActiveThread(threadId);
  return threadId;
}
