import { describe, expect, it } from "vitest";
import { canChangeUnstartedThreadEngine } from "./threadRuntimeState";

describe("canChangeUnstartedThreadEngine", () => {
  it("returns false when the thread is null or undefined", () => {
    expect(canChangeUnstartedThreadEngine(null)).toBe(false);
    expect(canChangeUnstartedThreadEngine(undefined)).toBe(false);
  });

  it("returns true for an empty thread without an engine session", () => {
    expect(
      canChangeUnstartedThreadEngine({
        // 空会话没有真实 CLI 会话 ID。
        engineThreadId: null,
        // 空会话不包含任何消息。
        messageCount: 0,
      }),
    ).toBe(true);
  });

  it("returns false when the thread already has an engine session", () => {
    expect(
      canChangeUnstartedThreadEngine({
        // 已建立的 CLI 会话不能原地更换 CLI。
        engineThreadId: "engine-thread-1",
        // 即使消息数仍为零，也不能覆盖已建立的会话上下文。
        messageCount: 0,
      }),
    ).toBe(false);
  });

  it("returns false when the thread already contains messages", () => {
    expect(
      canChangeUnstartedThreadEngine({
        // 没有 CLI 会话 ID，但已有消息时仍不能更换 CLI。
        engineThreadId: null,
        // 消息数大于零表示会话已经开始。
        messageCount: 1,
      }),
    ).toBe(false);
  });
});
