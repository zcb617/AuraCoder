import { describe, expect, it } from "vitest";
import { projectNameFromRemoteDirectory } from "./CreateWorkspaceModal";

describe("CreateWorkspaceModal 远端项目名称同步", () => {
  it("手工编辑名称后选择其他目录，项目名称改为新目录末级名称", () => {
    const manuallyEditedName = "my_codex";
    const selectedDirectory = "/var/work/llm_router";

    expect(projectNameFromRemoteDirectory(selectedDirectory)).toBe("llm_router");
    expect(projectNameFromRemoteDirectory(selectedDirectory)).not.toBe(manuallyEditedName);
  });

  it("未手工编辑名称时，仍按目录末级名称同步", () => {
    expect(projectNameFromRemoteDirectory("/home/user/example-project/")).toBe("example-project");
  });
});
