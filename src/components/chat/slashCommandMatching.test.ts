import { describe, expect, it } from "vitest";
import {
  filterClassicSlashItems,
  findClassicSlashQuery,
  removeClassicSlashToken,
} from "./slashCommandMatching";

describe("classic slash command matching", () => {
  it("recognizes a slash token at the cursor", () => {
    expect(findClassicSlashQuery("/task-anchor", 12)).toBe("task-anchor");
    expect(findClassicSlashQuery("Use /skill", 10)).toBe("skill");
    expect(findClassicSlashQuery("Use /skill next", 10)).toBe("skill");
    expect(findClassicSlashQuery("Use /skill next", 15)).toBeNull();
  });

  it("removes only the active slash token when a resource is selected", () => {
    expect(removeClassicSlashToken("Use /task-anchor now", 16)).toEqual({
      value: "Use now",
      cursorPosition: 4,
    });
  });

  it("matches resource names with a fuzzy query and ranks by score", () => {
    const items = [
      {
        id: "command:review",
        name: "review",
        description: "Review current changes",
        group: "Commands",
      },
      {
        id: "skill:task-anchor",
        name: "task-anchor",
        description: "Anchor a long-running task",
        group: "Skills",
      },
      {
        id: "mcp:filesystem",
        name: "filesystem",
        description: "Workspace files",
        group: "MCP",
      },
    ];

    expect(filterClassicSlashItems(items, "tanchr").map((item) => item.id)).toEqual([
      "skill:task-anchor",
    ]);
    expect(filterClassicSlashItems(items, "r").map((item) => item.id)).toEqual([
      "command:review",
      "mcp:filesystem",
      "skill:task-anchor",
    ]);
  });

  it("ranks stronger matches first regardless of group order when a query is present", () => {
    const items = [
      {
        id: "plugin:brightsign",
        name: "brightsign",
        description: "make a choice",
        group: "插件",
      },
      {
        id: "skill:mac-release",
        name: "mac-release",
        description: "macOS 发布流程",
        group: "技能",
      },
    ];

    expect(filterClassicSlashItems(items, "mac").map((item) => item.id)).toEqual([
      "skill:mac-release",
      "plugin:brightsign",
    ]);
  });
});
