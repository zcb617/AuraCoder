import { describe, expect, it } from "vitest";
import { formatRefreshAge, getRefreshAge, nextRefreshAgeUpdateDelay } from "./refreshAge";

const NOW = Date.UTC(2026, 6, 18, 2, 0, 0);
const timestampBefore = (elapsedMs: number) => new Date(NOW - elapsedMs).toISOString();

describe("extension refresh age", () => {
  it("uses seconds, minutes, hours, then days at the documented thresholds", () => {
    expect(getRefreshAge(timestampBefore(59_000), NOW)).toEqual({ value: 59, unit: "second" });
    expect(getRefreshAge(timestampBefore(60_000), NOW)).toEqual({ value: 1, unit: "minute" });
    expect(getRefreshAge(timestampBefore(3_599_000), NOW)).toEqual({ value: 59, unit: "minute" });
    expect(getRefreshAge(timestampBefore(3_600_000), NOW)).toEqual({ value: 1, unit: "hour" });
    expect(getRefreshAge(timestampBefore(86_400_000), NOW)).toEqual({ value: 1, unit: "day" });
  });

  it("updates at the next visible-unit boundary", () => {
    expect(nextRefreshAgeUpdateDelay(timestampBefore(59_500), NOW)).toBe(500);
    expect(nextRefreshAgeUpdateDelay(timestampBefore(61_000), NOW)).toBe(59_000);
  });

  it("formats the age in the current locale", () => {
    expect(formatRefreshAge(timestampBefore(20_000), "zh-CN", NOW)).toMatch(/^20秒/);
  });
});
