import { afterEach, describe, expect, it, vi } from "vitest";
import { formatRelativeTime } from "./formatters";

describe("formatRelativeTime", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("formats simplified Chinese compact and suffixed relative times", () => {
    vi.useFakeTimers();
    vi.setSystemTime(new Date("2026-07-16T12:00:00.000Z"));

    const threeMinutesAgo = new Date("2026-07-16T11:57:00.000Z");
    const twoDaysAgo = new Date("2026-07-14T12:00:00.000Z");

    expect(formatRelativeTime(threeMinutesAgo, "zh-CN")).toBe("3分钟");
    expect(
      formatRelativeTime(threeMinutesAgo, "zh-CN", { style: "short-with-suffix" }),
    ).toBe("3分钟前");
    expect(
      formatRelativeTime(twoDaysAgo, "zh-CN", { style: "short-with-suffix" }),
    ).toBe("2天前");
  });
});
