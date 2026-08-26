import { describe, expect, it } from "vitest";
import { scaleBrowserBounds } from "./BrowserPanel";

describe("scaleBrowserBounds", () => {
  it.each([100, 110, 120, 130, 140, 150])(
    "按 %i%% 界面缩放比例换算浏览器位置和尺寸",
    (displayScale) => {
      const scale = displayScale / 100;

      expect(
        scaleBrowserBounds({ x: 874, y: 75, width: 320, height: 600 }, displayScale),
      ).toEqual({
        x: 874 * scale,
        y: 75 * scale,
        width: 320 * scale,
        height: 600 * scale,
      });
    },
  );
});
