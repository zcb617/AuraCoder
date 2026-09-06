import { describe, expect, it } from "vitest";
import type { TFunction } from "i18next";

import type { ExtensionItem } from "../types";
import { buildSlashCommandsFromExtensions } from "./build-slash-commands";

// 斜杠菜单测试上下文只保留动作解析所需的运行时能力状态。
const testContext = {
  // 测试经典输入模式，使 Skill 条目参与菜单解析。
  inputMode: "classic" as const,
  // 测试翻译函数直接返回键名，避免访问真实国际化资源。
  t: ((key: string) => key) as unknown as TFunction,
  // 测试当前会话允许 Codex 历史操作。
  canManageActiveCodexThread: true,
  // 测试当前会话允许原生 Codex 历史工具。
  canUseNativeCodexHistoryTools: true,
};

// Skill 夹具只声明动作解析实际读取的 ExtensionItem 字段。
const skillWithInsertText = {
  // 测试 Skill 的菜单唯一标识。
  id: "skill-with-insert",
  // 测试 Claude 提供方的 Skill 条目。
  providerId: "claude",
  // 测试 Skill 类型优先于文本插入字段。
  kind: "skill",
  // 测试 Skill 在菜单中展示的名称。
  name: "review",
  // 测试 Skill 引用的实际路径。
  path: "/skills/review",
  // 测试旧快照可能携带的文本插入字段。
  insertText: "/review ",
} as ExtensionItem;

// Command 夹具用于确认文本命令仍然保持插入输入框的行为。
const commandWithInsertText = {
  // 测试 Command 的菜单唯一标识。
  id: "command-with-insert",
  // 测试 OpenCode 提供方的 Command 条目。
  providerId: "opencode",
  // 测试 Command 类型仍使用文本插入动作。
  kind: "command",
  // 测试 Command 在菜单中展示的名称。
  name: "review",
  // 测试 Command 的文本插入内容。
  insertText: "/review ",
} as ExtensionItem;

// 普通 Skill 夹具用于确认引用名称和路径按目录数据传递。
const skillReference = {
  // 测试普通 Skill 的菜单唯一标识。
  id: "skill-reference",
  // 测试 Claude 提供方的 Skill 条目。
  providerId: "claude",
  // 测试 Skill 类型生成引用动作。
  kind: "skill",
  // 测试引用动作使用的 Skill 名称。
  name: "deploy",
  // 测试引用动作使用的 Skill 路径。
  path: "/workspace/.claude/skills/deploy",
} as ExtensionItem;

describe("buildSlashCommandsFromExtensions", () => {
  it("keeps a Skill with stale insertText as a reference action", () => {
    const [command] = buildSlashCommandsFromExtensions([skillWithInsertText], testContext);

    expect(command?.action).toEqual({
      type: "reference",
      reference: {
        type: "skill",
        name: "review",
        path: "/skills/review",
      },
    });
  });

  it("keeps a Command with insertText as an insert action", () => {
    const [command] = buildSlashCommandsFromExtensions([commandWithInsertText], testContext);

    expect(command?.action).toEqual({ type: "insert", text: "/review " });
  });

  it("preserves a Skill reference name and path", () => {
    const [command] = buildSlashCommandsFromExtensions([skillReference], testContext);

    expect(command?.action).toEqual({
      type: "reference",
      reference: {
        type: "skill",
        name: "deploy",
        path: "/workspace/.claude/skills/deploy",
      },
    });
  });
});
