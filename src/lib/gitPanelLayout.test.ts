import { describe, expect, it } from "vitest";
import { getGitPanelLayoutState } from "./gitPanelLayout";

describe("getGitPanelLayoutState", () => {
  it("docks Git inside the shared workspace layout", () => {
    expect(
      getGitPanelLayoutState({
        activeView: "chat",
        activeWorkspaceId: "workspace-1",
        showGitPanel: true,
      }),
    ).toEqual({
      workspaceLayoutIntegrated: true,
      gitPanelDocked: true,
      gitPanelDockedInWorkspace: true,
      showWorkspaceHeaderToggle: true,
      showEdgeReveal: false,
    });
  });

  it("keeps the Git toggle in the workspace header when Git is hidden", () => {
    expect(
      getGitPanelLayoutState({
        activeView: "chat",
        activeWorkspaceId: "workspace-1",
        showGitPanel: false,
      }),
    ).toMatchObject({
      workspaceLayoutIntegrated: true,
      showWorkspaceHeaderToggle: true,
      showEdgeReveal: false,
    });
  });

  it("keeps the edge restore control for views without a workspace header", () => {
    expect(
      getGitPanelLayoutState({
        activeView: "chat",
        activeWorkspaceId: null,
        showGitPanel: false,
      }),
    ).toMatchObject({
      workspaceLayoutIntegrated: false,
      showWorkspaceHeaderToggle: false,
      showEdgeReveal: true,
    });
  });

  it("does not render Git controls on settings", () => {
    expect(
      getGitPanelLayoutState({
        activeView: "settings",
        activeWorkspaceId: "workspace-1",
        showGitPanel: false,
      }),
    ).toMatchObject({
      workspaceLayoutIntegrated: false,
      gitPanelDocked: false,
      showWorkspaceHeaderToggle: false,
      showEdgeReveal: false,
    });
  });

  it("does not render Git controls on scheduled tasks", () => {
    expect(
      getGitPanelLayoutState({
        activeView: "scheduled",
        activeWorkspaceId: "workspace-1",
        showGitPanel: true,
      }),
    ).toMatchObject({
      workspaceLayoutIntegrated: false,
      gitPanelDocked: false,
      showWorkspaceHeaderToggle: false,
      showEdgeReveal: false,
    });
  });

  it.each(["harnesses", "extensions"] as const)(
    "does not render conversation tools on %s",
    (activeView) => {
      expect(
        getGitPanelLayoutState({
          activeView,
          activeWorkspaceId: "workspace-1",
          showGitPanel: true,
        }),
      ).toEqual({
        workspaceLayoutIntegrated: false,
        gitPanelDocked: false,
        gitPanelDockedInWorkspace: false,
        showWorkspaceHeaderToggle: false,
        showEdgeReveal: false,
      });
    },
  );
});
