import type { GitStatus, WorkspaceGitContext } from "../types";

interface ResolveCommandPaletteGitStatusOptions {
  workspaceId: string | null;
  gitContext: WorkspaceGitContext | null;
  activeStatus?: GitStatus;
  loadStatus: (workspaceId: string) => Promise<GitStatus>;
}

/** 判断当前项目是否具备项目级 Git 操作上下文。 */
export function isWorkspaceGitCommandAvailable(gitContext: WorkspaceGitContext | null): boolean {
  return gitContext?.kind === "repository";
}

/** 读取命令面板所需的当前项目 Git 状态。 */
export async function resolveCommandPaletteGitStatus({
  workspaceId,
  gitContext,
  activeStatus,
  loadStatus,
}: ResolveCommandPaletteGitStatusOptions): Promise<GitStatus | undefined> {
  if (!workspaceId || gitContext?.kind !== "repository") return undefined;
  if (activeStatus) return activeStatus;
  return loadStatus(workspaceId);
}
