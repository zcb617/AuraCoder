import { describe, expect, it, vi } from "vitest";
import type { GitStatus } from "../types";
import { isWorkspaceGitCommandAvailable, resolveCommandPaletteGitStatus } from "./commandPaletteGit";

const context = { kind: "repository" as const, workspaceId: "workspace-1", rootPath: "/workspace", name: "workspace", defaultBranch: "main" };

describe("command palette project Git context", () => {
  it("only enables Git commands for repository context", () => {
    expect(isWorkspaceGitCommandAvailable(context)).toBe(true);
    expect(isWorkspaceGitCommandAvailable({ kind: "not-repository", workspaceId: "workspace-1" })).toBe(false);
  });
  it("loads Git status with workspace identity", async () => {
    const status = {} as GitStatus;
    const loadStatus = vi.fn().mockResolvedValue(status);
    await expect(resolveCommandPaletteGitStatus({ workspaceId: "workspace-1", gitContext: context, loadStatus })).resolves.toBe(status);
    expect(loadStatus).toHaveBeenCalledWith("workspace-1");
  });
});
