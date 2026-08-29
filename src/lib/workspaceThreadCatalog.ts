import type { Workspace } from "../types";

/** 根据活动项目的结构身份生成会话目录稳定键，避免权限等非结构变化触发会话重载。 */
export function workspaceThreadCatalogKey(workspaces: readonly Workspace[]): string {
  const workspaceEntries = workspaces.map((workspace) => [
    workspace.id,
    workspace.locationKind ?? "local",
  ] as const);

  workspaceEntries.sort(([firstId, firstLocationKind], [secondId, secondLocationKind]) => {
    return firstId.localeCompare(secondId)
      || firstLocationKind.localeCompare(secondLocationKind);
  });

  return JSON.stringify(workspaceEntries);
}
