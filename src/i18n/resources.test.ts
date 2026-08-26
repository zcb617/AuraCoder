import { describe, expect, it } from "vitest";
import commonEn from "./resources/en/common.json";
import appEn from "./resources/en/app.json";
import chatEn from "./resources/en/chat.json";
import workspaceEn from "./resources/en/workspace.json";
import setupEn from "./resources/en/setup.json";
import gitEn from "./resources/en/git.json";
import nativeEn from "./resources/en/native.json";
import extensionsEn from "./resources/en/extensions.json";
import scheduledEn from "./resources/en/scheduled.json";
import commonPtBr from "./resources/pt-BR/common.json";
import appPtBr from "./resources/pt-BR/app.json";
import chatPtBr from "./resources/pt-BR/chat.json";
import workspacePtBr from "./resources/pt-BR/workspace.json";
import setupPtBr from "./resources/pt-BR/setup.json";
import gitPtBr from "./resources/pt-BR/git.json";
import nativePtBr from "./resources/pt-BR/native.json";
import extensionsPtBr from "./resources/pt-BR/extensions.json";
import scheduledPtBr from "./resources/pt-BR/scheduled.json";
import commonZhCn from "./resources/zh-CN/common.json";
import appZhCn from "./resources/zh-CN/app.json";
import chatZhCn from "./resources/zh-CN/chat.json";
import workspaceZhCn from "./resources/zh-CN/workspace.json";
import setupZhCn from "./resources/zh-CN/setup.json";
import gitZhCn from "./resources/zh-CN/git.json";
import nativeZhCn from "./resources/zh-CN/native.json";
import extensionsZhCn from "./resources/zh-CN/extensions.json";
import scheduledZhCn from "./resources/zh-CN/scheduled.json";

function flattenKeys(value: unknown, prefix = ""): string[] {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return prefix ? [prefix] : [];
  }

  return Object.entries(value as Record<string, unknown>).flatMap(([key, child]) =>
    flattenKeys(child, prefix ? `${prefix}.${key}` : key),
  );
}

function readNestedString(
  value: Record<string, unknown>,
  path: string,
): string | undefined {
  const segments = path.split(".");
  let current: unknown = value;

  for (const segment of segments) {
    if (!current || typeof current !== "object" || Array.isArray(current)) {
      return undefined;
    }
    current = (current as Record<string, unknown>)[segment];
  }

  return typeof current === "string" ? current : undefined;
}

describe("i18n resources", () => {
  it("keeps pt-BR keys aligned with en", () => {
    const enKeys = [
      ...flattenKeys(commonEn, "common"),
      ...flattenKeys(appEn, "app"),
      ...flattenKeys(chatEn, "chat"),
      ...flattenKeys(workspaceEn, "workspace"),
      ...flattenKeys(setupEn, "setup"),
      ...flattenKeys(gitEn, "git"),
      ...flattenKeys(nativeEn, "native"),
      ...flattenKeys(extensionsEn, "extensions"),
      ...flattenKeys(scheduledEn, "scheduled"),
    ].sort();
    const ptBrKeys = [
      ...flattenKeys(commonPtBr, "common"),
      ...flattenKeys(appPtBr, "app"),
      ...flattenKeys(chatPtBr, "chat"),
      ...flattenKeys(workspacePtBr, "workspace"),
      ...flattenKeys(setupPtBr, "setup"),
      ...flattenKeys(gitPtBr, "git"),
      ...flattenKeys(nativePtBr, "native"),
      ...flattenKeys(extensionsPtBr, "extensions"),
      ...flattenKeys(scheduledPtBr, "scheduled"),
    ].sort();

    expect(ptBrKeys).toEqual(enKeys);
  });

  it("keeps zh-CN keys aligned with en", () => {
    const enKeys = [
      ...flattenKeys(commonEn, "common"),
      ...flattenKeys(appEn, "app"),
      ...flattenKeys(chatEn, "chat"),
      ...flattenKeys(workspaceEn, "workspace"),
      ...flattenKeys(setupEn, "setup"),
      ...flattenKeys(gitEn, "git"),
      ...flattenKeys(nativeEn, "native"),
      ...flattenKeys(extensionsEn, "extensions"),
      ...flattenKeys(scheduledEn, "scheduled"),
    ].sort();
    const zhCnKeys = [
      ...flattenKeys(commonZhCn, "common"),
      ...flattenKeys(appZhCn, "app"),
      ...flattenKeys(chatZhCn, "chat"),
      ...flattenKeys(workspaceZhCn, "workspace"),
      ...flattenKeys(setupZhCn, "setup"),
      ...flattenKeys(gitZhCn, "git"),
      ...flattenKeys(nativeZhCn, "native"),
      ...flattenKeys(extensionsZhCn, "extensions"),
      ...flattenKeys(scheduledZhCn, "scheduled"),
    ].sort();

    expect(zhCnKeys).toEqual(enKeys);
  });

  it("defines fallback thread titles used by the chat panel", () => {
    expect(readNestedString(chatEn, "panel.workspaceChatTitle")).toBeTruthy();
    expect(readNestedString(chatEn, "panel.repoChatTitle")).toBeTruthy();
    expect(readNestedString(chatPtBr, "panel.workspaceChatTitle")).toBeTruthy();
    expect(readNestedString(chatPtBr, "panel.repoChatTitle")).toBeTruthy();
  });

  it("describes incomplete thread archiving without exposing internal errors", () => {
    expect(appZhCn.sidebar.archiveThreadFailed).toBe(
      "本次会话归档未完成，AuraCoder 中的会话列表未变化。请稍后重试。",
    );
    expect(appEn.sidebar.archiveThreadFailed).toBe(
      "The thread archive was not completed. The thread list in AuraCoder was not changed. Please try again later.",
    );
    expect(appPtBr.sidebar.archiveThreadFailed).toBe(
      "O arquivamento da conversa não foi concluído. A lista de conversas no AuraCoder não foi alterada. Tente novamente mais tarde.",
    );
  });
});
