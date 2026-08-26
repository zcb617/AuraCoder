import type { ActiveView } from "../stores/uiStore";

interface GitPanelLayoutInput {
  activeView: ActiveView;
  activeWorkspaceId: string | null;
  showGitPanel: boolean;
}

export interface GitPanelLayoutState {
  workspaceLayoutIntegrated: boolean;
  gitPanelDocked: boolean;
  gitPanelDockedInWorkspace: boolean;
  showWorkspaceHeaderToggle: boolean;
  showEdgeReveal: boolean;
}

export function getGitPanelLayoutState({
  activeView,
  activeWorkspaceId,
  showGitPanel,
}: GitPanelLayoutInput): GitPanelLayoutState {
  // The right-side Git/browser tools belong to the conversation workspace only.
  // Standalone pages such as agents and extensions must not inherit a panel that
  // was left open while viewing a conversation.
  const toolPanelSupported = activeView === "chat";
  const workspaceLayoutIntegrated = toolPanelSupported && activeWorkspaceId !== null;
  const gitPanelDocked = showGitPanel && toolPanelSupported;
  const gitPanelDockedInWorkspace = gitPanelDocked && workspaceLayoutIntegrated;

  return {
    workspaceLayoutIntegrated,
    gitPanelDocked,
    gitPanelDockedInWorkspace,
    showWorkspaceHeaderToggle: workspaceLayoutIntegrated,
    showEdgeReveal: !showGitPanel && toolPanelSupported && !workspaceLayoutIntegrated,
  };
}
