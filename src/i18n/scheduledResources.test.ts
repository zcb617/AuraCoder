import { describe, expect, it } from "vitest";
import scheduledEn from "./resources/en/scheduled.json";
import scheduledPtBr from "./resources/pt-BR/scheduled.json";
import scheduledZhCn from "./resources/zh-CN/scheduled.json";

function keys(value: unknown, prefix = ""): string[] {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return prefix ? [prefix] : [];
  }
  return Object.entries(value as Record<string, unknown>).flatMap(([key, child]) =>
    keys(child, prefix ? `${prefix}.${key}` : key),
  );
}

describe("scheduled task translations", () => {
  it("keeps all locales aligned", () => {
    const expected = keys(scheduledEn).sort();
    expect(keys(scheduledZhCn).sort()).toEqual(expected);
    expect(keys(scheduledPtBr).sort()).toEqual(expected);
  });
});
