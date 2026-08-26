import { describe, expect, it } from "vitest";
import { formatWorkingDuration } from "./workingDuration";

describe("formatWorkingDuration", () => {
  it("formats seconds", () => {
    expect(formatWorkingDuration(11_999)).toBe("11s");
  });

  it("formats minutes and seconds", () => {
    expect(formatWorkingDuration(11 * 60_000 + 59_000)).toBe("11m 59s");
  });

  it("formats hours, minutes, and seconds", () => {
    expect(formatWorkingDuration(1 * 60 * 60_000 + 2 * 60_000 + 3_000)).toBe(
      "1h 2m 3s",
    );
  });

  it("does not produce a negative duration", () => {
    expect(formatWorkingDuration(-1)).toBe("0s");
  });
});
