import { describe, expect, it } from "vitest";
import type { ExtensionItem } from "../../types";
import {
  getExtensionCategory,
  getExtensionCategoryOptions,
  groupExtensionItemsByCategory,
} from "./extensionCategories";

function item(overrides: Partial<ExtensionItem>): ExtensionItem {
  return {
    id: "example",
    providerId: "codex",
    kind: "plugin",
    name: "Example",
    scope: "user",
    officiallyAvailable: false,
    catalogAuthority: null,
    installed: true,
    configured: null,
    enabled: true,
    health: "healthy",
    availableActions: [],
    requiresNewSession: false,
    ...overrides,
  };
}

describe("extension categories", () => {
  it("uses provider marketplace categories without inventing a replacement", () => {
    expect(getExtensionCategory(item({ category: "Data & Analytics" }))).toEqual({
      id: "official:data-analytics",
      translationKey: "categories.official.dataAnalytics",
      defaultLabel: "Data & Analytics",
    });
  });

  it("uses factual scope categories for skills", () => {
    expect(getExtensionCategory(item({ kind: "skill", scope: "project" })).id).toBe(
      "scope:project",
    );
  });

  it("separates plugin-provided and standalone MCP configurations", () => {
    expect(
      getExtensionCategory(item({ kind: "mcp", parentPluginId: "linear" })).id,
    ).toBe("mcp:plugin");
    expect(getExtensionCategory(item({ kind: "mcp", parentPluginId: null })).id).toBe(
      "mcp:standalone",
    );
  });

  it("deduplicates options and groups items by category", () => {
    const items = [
      item({ id: "one", category: "Productivity" }),
      item({ id: "two", category: "Productivity" }),
      item({ id: "three", category: null }),
    ];
    expect(getExtensionCategoryOptions(items).map((category) => category.id)).toEqual([
      "official:productivity",
      "uncategorized",
    ]);
    expect(groupExtensionItemsByCategory(items).map((group) => group.items.length)).toEqual([
      2,
      1,
    ]);
  });
});
