import { describe, expect, it } from "vitest";
import type { Message } from "../../types";
import { isClaudeSystemInjectedUserMessage } from "./MessageRow";

/** 构造系统注入消息识别测试所需的最小消息对象。 */
function createMessage(overrides: Partial<Message> = {}): Message {
  return {
    id: "message-1",
    threadId: "thread-1",
    role: "user",
    status: "completed",
    schemaVersion: 1,
    createdAt: "2026-01-01T00:00:00.000Z",
    ...overrides,
  } as Message;
}

describe("isClaudeSystemInjectedUserMessage", () => {
  it("识别 Claude 子代理回传消息", () => {
    const message = createMessage({
      content: "Another Claude session sent a message: 子代理已完成。",
    });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(true);
  });

  it("识别 Claude 系统通知消息", () => {
    const message = createMessage({
      content: "[MESSAGE FROM NON-USER SOURCE\n系统通知内容",
    });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(true);
  });

  it("不识别 Claude 普通用户消息", () => {
    const message = createMessage({ content: "帮我改代码" });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(false);
  });

  it("不识别 Codex 会话中的带前缀用户消息", () => {
    const message = createMessage({
      content: "Another Claude session sent a message: 子代理已完成。",
    });

    expect(isClaudeSystemInjectedUserMessage("codex", message)).toBe(false);
  });

  it("不识别 Claude 助手角色中的带前缀消息", () => {
    const message = createMessage({
      role: "assistant",
      content: "Another Claude session sent a message: 子代理已完成。",
    });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(false);
  });

  it("识别 text block 中的 Claude 系统注入前缀", () => {
    const message = createMessage({
      content: undefined,
      blocks: [{ type: "text", content: "[MESSAGE FROM NON-USER SOURCE 系统通知" }],
    });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(true);
  });

  it("忽略前导空白后识别 Claude 子代理回传前缀", () => {
    const message = createMessage({
      content: "  \nAnother Claude session sent a message: 子代理已完成。",
    });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(true);
  });

  it("识别带 system-reminder 前缀的子代理回传消息", () => {
    const message = createMessage({
      content:
        "<system-reminder>\nAnother Claude session sent a message while you were working:\n<agent-message from=...>",
    });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(true);
  });

  it("识别上下文压缩总结消息", () => {
    const message = createMessage({
      content:
        "This session is being continued from a previous conversation that ran out of context. ...",
    });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(true);
  });

  it("识别 skill 重载通知消息", () => {
    const message = createMessage({
      content:
        "(Re-invocation of /zhang-dev-plugins:zhang-code-development — the skill instructions were previously loaded...)",
    });

    expect(isClaudeSystemInjectedUserMessage("claude", message)).toBe(true);
  });

  it("压缩总结前缀在 Codex 会话不识别", () => {
    const message = createMessage({
      content:
        "This session is being continued from a previous conversation that ran out of context. ...",
    });

    expect(isClaudeSystemInjectedUserMessage("codex", message)).toBe(false);
  });
});
