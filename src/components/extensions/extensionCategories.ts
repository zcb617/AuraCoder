import type { ExtensionItem } from "../../types";

export interface ExtensionCategoryDescriptor {
  id: string;
  translationKey: string;
  defaultLabel: string;
}

const OFFICIAL_CATEGORY_KEYS: Record<string, string> = {
  "business-operations": "businessOperations",
  communication: "communication",
  creativity: "creativity",
  "data-analytics": "dataAnalytics",
  database: "database",
  deployment: "deployment",
  design: "design",
  development: "development",
  "developer-tools": "developerTools",
  "education-research": "educationResearch",
  engineering: "engineering",
  finance: "finance",
  learning: "learning",
  location: "location",
  math: "math",
  monitoring: "monitoring",
  other: "other",
  productivity: "productivity",
  security: "security",
  testing: "testing",
  travel: "travel",
};

function normalizeCategory(value: string): string {
  return value
    .trim()
    .toLocaleLowerCase()
    .replace(/&/g, " and ")
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .replace(/-and-/g, "-");
}

export function getExtensionCategory(item: ExtensionItem): ExtensionCategoryDescriptor {
  const officialCategory = item.category?.trim();
  if (officialCategory) {
    const id = normalizeCategory(officialCategory) || "uncategorized";
    const translationId = OFFICIAL_CATEGORY_KEYS[id] ?? id;
    return {
      id: `official:${id}`,
      translationKey: `categories.official.${translationId}`,
      defaultLabel: officialCategory,
    };
  }

  if (item.kind === "skill") {
    return {
      id: `scope:${item.scope}`,
      translationKey: `scope.${item.scope}`,
      defaultLabel: item.scope,
    };
  }

  if (item.kind === "mcp") {
    const pluginProvided = Boolean(item.parentPluginId) || item.scope === "plugin";
    return pluginProvided
      ? {
          id: "mcp:plugin",
          translationKey: "categories.mcp.plugin",
          defaultLabel: "Plugin provided",
        }
      : {
          id: "mcp:standalone",
          translationKey: "categories.mcp.standalone",
          defaultLabel: "Standalone MCP",
        };
  }

  return {
    id: "uncategorized",
    translationKey: "categories.uncategorized",
    defaultLabel: "Uncategorized",
  };
}

export function getExtensionCategoryOptions(
  items: ExtensionItem[],
): ExtensionCategoryDescriptor[] {
  const categories = new Map<string, ExtensionCategoryDescriptor>();
  for (const item of items) {
    const category = getExtensionCategory(item);
    if (!categories.has(category.id)) categories.set(category.id, category);
  }
  return [...categories.values()];
}

export function groupExtensionItemsByCategory(
  items: ExtensionItem[],
): Array<{ category: ExtensionCategoryDescriptor; items: ExtensionItem[] }> {
  const groups = new Map<
    string,
    { category: ExtensionCategoryDescriptor; items: ExtensionItem[] }
  >();
  for (const item of items) {
    const category = getExtensionCategory(item);
    const group = groups.get(category.id);
    if (group) {
      group.items.push(item);
    } else {
      groups.set(category.id, { category, items: [item] });
    }
  }
  return [...groups.values()];
}
