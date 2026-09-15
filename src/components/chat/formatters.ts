import type { TFunction } from "i18next";
import type { ContentBlock, EngineModel } from "../../types";

const MODEL_TOKEN_LABELS: Record<string, string> = {
  gpt: "GPT",
  codex: "Codex",
  mini: "Mini",
  nano: "Nano",
};

export function formatModelName(modelName: string): string {
  return modelName
    .split("-")
    .filter(Boolean)
    .map((segment) => {
      const lowerSegment = segment.toLowerCase();
      const knownLabel = MODEL_TOKEN_LABELS[lowerSegment];
      if (knownLabel) {
        return knownLabel;
      }
      if (/^\d+(\.\d+)*$/.test(segment)) {
        return segment;
      }
      if (/^[a-z]?\d+(\.\d+)*$/i.test(segment)) {
        return segment.toUpperCase();
      }
      return segment.charAt(0).toUpperCase() + segment.slice(1);
    })
    .join("-");
}

export function formatReasoningEffortLabel(
  t: TFunction<"chat">,
  effort?: string,
): string {
  if (!effort) {
    return "";
  }
  switch (effort.toLowerCase()) {
    case "none":
      return t("modelPicker.effort.none");
    case "minimal":
      return t("modelPicker.effort.minimal");
    case "low":
      return t("modelPicker.effort.low");
    case "medium":
      return t("modelPicker.effort.medium");
    case "high":
      return t("modelPicker.effort.high");
    case "xhigh":
      return t("modelPicker.effort.xhigh");
    default:
      break;
  }
  return effort.charAt(0).toUpperCase() + effort.slice(1);
}

export function formatEngineModelLabel(
  t: TFunction<"chat">,
  engineName?: string,
  modelDisplayName?: string,
  reasoningEffort?: string,
): string {
  const modelLabel = modelDisplayName ? formatModelName(modelDisplayName) : "";
  const baseLabel = engineName && modelLabel
    ? `${engineName} - ${modelLabel}`
    : modelLabel || engineName || t("panel.assistantFallback");
  const effortLabel = formatReasoningEffortLabel(t, reasoningEffort);
  return effortLabel ? `${baseLabel} ${effortLabel}` : baseLabel;
}

export function serializePrettyJson(value: unknown): string {
  try {
    return JSON.stringify(value, null, 2);
  } catch {
    return "";
  }
}

export function encodeModelOptionValue(engineId: string, modelId: string): string {
  return JSON.stringify([engineId, modelId]);
}

export function decodeModelOptionValue(value: string): { engineId: string; modelId: string } | null {
  if (!value) {
    return null;
  }

  try {
    const parsed = JSON.parse(value);
    if (
      Array.isArray(parsed) &&
      parsed.length === 2 &&
      typeof parsed[0] === "string" &&
      typeof parsed[1] === "string"
    ) {
      return { engineId: parsed[0], modelId: parsed[1] };
    }
  } catch {
    // Ignore malformed legacy values.
  }

  return null;
}

export function hasVisibleContent(blocks?: ContentBlock[]): boolean {
  if (!blocks || blocks.length === 0) return false;
  return blocks.some((b) => {
    if (b.type === "text" || b.type === "thinking") return Boolean(b.content?.trim());
    return true;
  });
}

export function parseMessageDate(raw?: string): Date | null {
  if (!raw) {
    return null;
  }

  const sqliteUtcPattern = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}$/;
  const normalized = sqliteUtcPattern.test(raw) ? `${raw.replace(" ", "T")}Z` : raw;
  const date = new Date(normalized);
  if (Number.isNaN(date.getTime())) {
    return null;
  }

  return date;
}

export function formatMessageTimestamp(raw: string | undefined, locale: string): string {
  const date = parseMessageDate(raw);
  if (!date) {
    return "";
  }

  const now = new Date();
  const sameDay = now.toDateString() === date.toDateString();

  if (sameDay) {
    return date.toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit" });
  }

  return date.toLocaleString(locale, {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatResetTime(
  t: TFunction<"chat">,
  isoDate: string | null,
): string {
  if (!isoDate) return "";
  const date = new Date(isoDate);
  if (Number.isNaN(date.getTime())) return "";
  const now = new Date();
  const diffMs = date.getTime() - now.getTime();
  if (diffMs <= 0) return t("status.now");
  const diffMin = Math.floor(diffMs / 60000);
  if (diffMin < 60) return t("status.minutesShort", { count: diffMin });
  const diffHr = Math.floor(diffMin / 60);
  if (diffHr < 24) {
    return t("status.hoursMinutesShort", { hours: diffHr, minutes: diffMin % 60 });
  }
  const diffDays = Math.floor(diffHr / 24);
  return t("status.daysHoursShort", { days: diffDays, hours: diffHr % 24 });
}

export function formatUsagePercent(percent: number | null): string {
  if (typeof percent !== "number" || !Number.isFinite(percent)) {
    return "--";
  }
  return `${Math.max(0, Math.min(100, Math.round(percent)))}%`;
}

/** 按聊天输入区业务规则格式化上下文 token，支持原数值、K 和 M 单位。 */
export function formatContextTokens(tokens: number | null): string {
  if (typeof tokens !== "number" || !Number.isFinite(tokens)) {
    return "--";
  }
  const normalized = Math.max(0, Math.round(tokens));
  if (normalized < 1_000) {
    return String(normalized);
  }
  if (normalized < 1_000_000) {
    const value = normalized / 1_000;
    return `${value >= 100 ? value.toFixed(0) : value.toFixed(1).replace(/\.0$/, "")}K`;
  }
  const value = normalized / 1_000_000;
  return `${value >= 100 ? value.toFixed(0) : value.toFixed(2).replace(/0+$/, "").replace(/\.$/, "")}M`;
}

/** 将线程上下文快照格式化为输入区展示的已用/总量/剩余比例。 */
export function formatContextUsage(
  contextUsage: { currentTokens: number | null; maxContextTokens: number | null; contextPercent: number | null } | null,
): string {
  if (
    !contextUsage ||
    contextUsage.currentTokens === null ||
    contextUsage.maxContextTokens === null ||
    contextUsage.contextPercent === null
  ) {
    return "--";
  }
  return `${formatContextTokens(contextUsage.currentTokens)}/${formatContextTokens(contextUsage.maxContextTokens)}（${formatUsagePercent(contextUsage.contextPercent)}）`;
}

export function usagePercentToWidth(percent: number | null): string {
  if (typeof percent !== "number" || !Number.isFinite(percent)) {
    return "0%";
  }
  return `${Math.max(0, Math.min(100, Math.round(percent)))}%`;
}

export function usageProgressLevelClass(percent: number | null): string {
  if (typeof percent !== "number" || !Number.isFinite(percent)) {
    return "";
  }
  if (percent <= 10) return " chat-context-progress-fill-critical";
  if (percent <= 25) return " chat-context-progress-fill-warning";
  return "";
}

export function resolveClaudeModelFamily(model: EngineModel | null): "fable" | "opus" | "sonnet" | null {
  if (!model) {
    return null;
  }
  const identity = `${model.id} ${model.displayName} ${model.description}`.toLowerCase();
  if (identity.includes("fable")) return "fable";
  if (identity.includes("opus")) return "opus";
  if (identity.includes("sonnet")) return "sonnet";
  return null;
}
