import { normalizeAppLocale, type AppLocale } from "./locale";

type RelativeTimeStyle = "compact" | "short-with-suffix";

interface RelativeTimeOptions {
  style?: RelativeTimeStyle;
}

const COMPACT_LABELS: Record<AppLocale, {
  now: string;
  minute: string;
  hour: string;
  day: string;
  month: string;
}> = {
  en: {
    now: "now",
    minute: "m",
    hour: "h",
    day: "d",
    month: "mo",
  },
  "pt-BR": {
    now: "agora",
    minute: "min",
    hour: "h",
    day: "d",
    month: "mo",
  },
  "zh-CN": {
    now: "刚刚",
    minute: "分钟",
    hour: "小时",
    day: "天",
    month: "个月",
  },
};

function asLocale(locale?: string | null): AppLocale {
  return normalizeAppLocale(locale);
}

function toDate(value: string | number | Date): Date | null {
  const normalizedValue = typeof value === "string" ? value.trim() : value;
  // SQLite's datetime('now') is UTC but returns a timestamp without an offset.
  // JavaScript otherwise treats that representation as local time, producing an
  // offset-sized error in relative timestamps.
  const sqliteUtcTimestamp =
    typeof normalizedValue === "string" &&
    /^\d{4}-\d{2}-\d{2}[ T]\d{2}:\d{2}:\d{2}(?:\.\d+)?$/.test(normalizedValue)
      ? `${normalizedValue.replace(" ", "T")}Z`
      : normalizedValue;
  const date = value instanceof Date ? value : new Date(sqliteUtcTimestamp);
  return Number.isNaN(date.getTime()) ? null : date;
}

function formatCompactAmount(amount: number, unit: string, locale: AppLocale): string {
  if (locale === "en" || locale === "zh-CN") {
    return `${amount}${unit}`;
  }
  return `${amount} ${unit}`;
}

function formatRelativeTimeWithSuffix(
  amount: number,
  unit: string,
  compact: string,
  locale: AppLocale,
): string {
  if (locale === "pt-BR") {
    return `há ${amount} ${unit}`;
  }
  if (locale === "zh-CN") {
    return `${amount}${unit}前`;
  }
  return `${compact} ago`;
}

export function formatRelativeTime(
  value: string | number | Date,
  locale?: string | null,
  options: RelativeTimeOptions = {},
): string {
  const date = toDate(value);
  if (!date) {
    return "";
  }

  const resolvedLocale = asLocale(locale);
  const labels = COMPACT_LABELS[resolvedLocale];
  const diffMs = Date.now() - date.getTime();
  if (diffMs <= 45_000) {
    return labels.now;
  }

  const minutes = Math.max(1, Math.floor(diffMs / 60_000));
  if (minutes < 60) {
    const compact = formatCompactAmount(minutes, labels.minute, resolvedLocale);
    return options.style === "short-with-suffix"
      ? formatRelativeTimeWithSuffix(minutes, labels.minute, compact, resolvedLocale)
      : compact;
  }

  const hours = Math.floor(minutes / 60);
  if (hours < 24) {
    const compact = formatCompactAmount(hours, labels.hour, resolvedLocale);
    return options.style === "short-with-suffix"
      ? formatRelativeTimeWithSuffix(hours, labels.hour, compact, resolvedLocale)
      : compact;
  }

  const days = Math.floor(hours / 24);
  if (days < 30) {
    const compact = formatCompactAmount(days, labels.day, resolvedLocale);
    return options.style === "short-with-suffix"
      ? formatRelativeTimeWithSuffix(days, labels.day, compact, resolvedLocale)
      : compact;
  }

  const months = Math.floor(days / 30);
  const compact = formatCompactAmount(months, labels.month, resolvedLocale);
  return options.style === "short-with-suffix"
    ? formatRelativeTimeWithSuffix(months, labels.month, compact, resolvedLocale)
    : compact;
}

export function formatShortDate(value: string | number | Date, locale?: string | null): string {
  const date = toDate(value);
  if (!date) {
    return String(value);
  }

  return new Intl.DateTimeFormat(asLocale(locale), {
    month: "short",
    day: "numeric",
    year: "numeric",
  }).format(date);
}

export function formatDate(value: string | number | Date, locale?: string | null): string {
  const date = toDate(value);
  if (!date) {
    return String(value);
  }

  return new Intl.DateTimeFormat(asLocale(locale), {
    year: "numeric",
    month: "long",
    day: "numeric",
  }).format(date);
}

export function formatDateTime(value: string | number | Date, locale?: string | null): string {
  const date = toDate(value);
  if (!date) {
    return String(value);
  }

  return new Intl.DateTimeFormat(asLocale(locale), {
    day: "2-digit",
    month: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

export function formatTime(value: string | number | Date, locale?: string | null): string {
  const date = toDate(value);
  if (!date) {
    return "";
  }

  return new Intl.DateTimeFormat(asLocale(locale), {
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}
