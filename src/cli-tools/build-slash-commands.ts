import type { TFunction } from "i18next";
import {
  FlaskConical,
  GitBranch,
  type LucideIcon,
  Puzzle,
  RotateCcw,
  Scissors,
  Search,
  Server,
  Sparkles,
  SquareCode,
  UserCircle,
  Zap,
} from "lucide-react";

import type { ChatInputMode } from "../lib/chatInputSettings";
import type { ChatInputReference, ExtensionItem } from "../types";
import type { CliSlashCommand, CliSlashCommandPanel } from "./contracts/slash-command";

/// 斜杠菜单统一构建上下文。前端只提供当前输入模式和运行时能力开关，
/// 不再按 CLI 工具分别提供数据源。
export interface BuildSlashCommandsContext {
  inputMode: ChatInputMode;
  t: TFunction;
  canManageActiveCodexThread: boolean;
  canUseNativeCodexHistoryTools: boolean;
}

const KIND_ICON: Record<string, LucideIcon> = {
  skill: Sparkles,
  plugin: Puzzle,
  mcp: Server,
  agent: UserCircle,
  command: SquareCode,
};

/// 内置命令的图标按 id 覆盖 kind 默认图标，保持各 CLI 原有视觉标识。
const BUILTIN_ICON: Record<string, LucideIcon> = {
  review: Search,
  fork: GitBranch,
  rollback: RotateCcw,
  compact: Scissors,
  fast: Zap,
  personality: UserCircle,
  experimental: FlaskConical,
};

/// 内置命令和面板入口的说明文案按 id 映射到 i18n，后端不负责本地化。
const BUILTIN_DESCRIPTION: Record<string, (t: TFunction) => string> = {
  review: (t) => t("reviewPicker.subtitle"),
  fork: (t) => t("threadPicker.forkDescription"),
  rollback: (t) => t("threadPicker.rollbackDescription"),
  compact: (t) => t("threadPicker.compactDescription"),
  fast: (t) => t("configPicker.serviceTierDescription"),
  personality: (t) => t("configPicker.personalityDescription"),
  experimental: (t) => t("slashCommands.panels.experimental.description"),
  agents: (t) => t("slashCommands.panels.openCodeAgents.description"),
  commands: (t) => t("slashCommands.panels.openCodeCommands.description"),
  sessions: (t) => t("slashCommands.panels.openCodeSessions.description"),
  skills: (t) => t("slashCommands.panels.skills.description"),
  plugins: (t) => t("slashCommands.panels.plugins.title"),
  mcp: (t) => t("slashCommands.panels.mcp.description"),
};

const PANEL_TYPES = new Set<CliSlashCommandPanel["type"]>([
  "review",
  "fork",
  "rollback",
  "compact",
  "personality",
  "experimental",
  "skills",
  "plugins",
  "agents",
  "commands",
  "sessions",
  "mcp",
]);

function groupLabel(group: string | null | undefined, t: TFunction): string | undefined {
  if (!group) return undefined;
  const map: Record<string, string> = {
    commands: t("slashCommands.groups.commands"),
    skills: t("slashCommands.groups.skills"),
    plugins: t("slashCommands.groups.plugins"),
    mcp: t("slashCommands.groups.mcp"),
    agents: t("slashCommands.groups.commands"),
  };
  return map[group] ?? group;
}

/// 把后端返回的统一 ExtensionItemDto 列表解析成斜杠菜单命令。
/// 解析规则：
/// - 图标：kind 映射，内置命令按 id 覆盖
/// - 选中行为：insertText 非空 → 插入文本；kind=skill → 插入引用；panel=fast → fast 开关；其他 panel → 打开面板
/// - 禁用：item.disabled 叠加运行时能力开关
/// - classic 模式显示全部；非 classic 模式只显示 kind=command 的面板入口与内置命令
export function buildSlashCommandsFromExtensions(
  items: ExtensionItem[],
  ctx: BuildSlashCommandsContext,
): CliSlashCommand[] {
  const result: CliSlashCommand[] = [];
  for (const item of items) {
    if (ctx.inputMode !== "classic" && item.kind !== "command") {
      continue;
    }

    const icon = BUILTIN_ICON[item.id] ?? KIND_ICON[item.kind] ?? SquareCode;
    const description =
      item.description ?? BUILTIN_DESCRIPTION[item.id]?.(ctx.t) ?? "";

    let disabled = item.disabled ?? false;
    if (item.panel === "review" || item.panel === "compact") {
      disabled = disabled || !ctx.canManageActiveCodexThread;
    }
    if (item.panel === "fork" || item.panel === "rollback") {
      disabled = disabled || !ctx.canUseNativeCodexHistoryTools;
    }

    let action: CliSlashCommand["action"];
    if (item.insertText) {
      action = { type: "insert", text: item.insertText };
    } else if (item.kind === "skill") {
      const reference: ChatInputReference = {
        type: "skill",
        name: item.name,
        path: item.path || item.id,
      };
      action = { type: "reference", reference };
    } else if (item.panel === "fast") {
      action = { type: "fast" };
    } else if (item.panel && PANEL_TYPES.has(item.panel as CliSlashCommandPanel["type"])) {
      action = {
        type: "panel",
        panel: { type: item.panel as CliSlashCommandPanel["type"] },
      };
    } else {
      action = undefined;
    }

    result.push({
      id: item.id,
      name: item.name,
      description,
      icon,
      group: groupLabel(item.group, ctx.t),
      searchTerms: item.searchTerms,
      disabled: disabled || undefined,
      action,
    });
  }
  return result;
}
