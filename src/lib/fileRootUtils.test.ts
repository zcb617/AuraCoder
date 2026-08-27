import { describe, expect, it } from "vitest";
import { isWithinRoot, normalizeAbsolutePath, resolveAbsoluteFilePath, resolveRelativePathWithinRoot } from "./fileRootUtils";

describe("file root utilities", () => {
  it("normalizes separators and drive letters", () => expect(normalizeAbsolutePath("/c:\\Work\\App\\")).toBe("C:/Work/App"));
  it("checks root boundaries", () => {
    expect(isWithinRoot("/workspace/app/file.ts", "/workspace/app")).toBe(true);
    expect(isWithinRoot("/workspace/app2/file.ts", "/workspace/app")).toBe(false);
  });
  it("resolves project-relative paths", () => {
    expect(resolveAbsoluteFilePath("/workspace/app", "src/main.ts")).toBe("/workspace/app/src/main.ts");
    expect(resolveRelativePathWithinRoot("/workspace/app/src/main.ts", "/workspace/app")).toBe("src/main.ts");
    expect(resolveRelativePathWithinRoot("/outside/main.ts", "/workspace/app")).toBeNull();
  });
});
