import { describe, expect, it } from "vitest";
import {
  getLocaleDisplayName,
  normalizeAppLocale,
  SUPPORTED_APP_LOCALES,
} from "./locale";

describe("locale", () => {
  it("normalizes simplified Chinese aliases", () => {
    expect(normalizeAppLocale("zh")).toBe("zh-CN");
    expect(normalizeAppLocale("zh_CN")).toBe("zh-CN");
    expect(normalizeAppLocale("zh-Hans-SG")).toBe("zh-CN");
  });

  it("does not map traditional Chinese to simplified Chinese", () => {
    expect(normalizeAppLocale("zh-TW")).toBe("en");
  });

  it("exposes simplified Chinese as an application locale", () => {
    expect(SUPPORTED_APP_LOCALES).toContain("zh-CN");
    expect(getLocaleDisplayName("zh-CN")).toBe("简体中文");
  });
});
