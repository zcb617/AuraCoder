export type RefreshAgeUnit = "second" | "minute" | "hour" | "day";

export interface RefreshAge {
  value: number;
  unit: RefreshAgeUnit;
}

const SECOND_MS = 1_000;
const MINUTE_MS = 60 * SECOND_MS;
const HOUR_MS = 60 * MINUTE_MS;
const DAY_MS = 24 * HOUR_MS;

export function getRefreshAge(
  timestamp: string | null | undefined,
  nowMs = Date.now(),
): RefreshAge | null {
  if (!timestamp) return null;
  const timestampMs = Date.parse(timestamp);
  if (!Number.isFinite(timestampMs)) return null;

  const elapsedMs = Math.max(0, nowMs - timestampMs);
  if (elapsedMs < MINUTE_MS) {
    return { value: Math.floor(elapsedMs / SECOND_MS), unit: "second" };
  }
  if (elapsedMs < HOUR_MS) {
    return { value: Math.floor(elapsedMs / MINUTE_MS), unit: "minute" };
  }
  if (elapsedMs < DAY_MS) {
    return { value: Math.floor(elapsedMs / HOUR_MS), unit: "hour" };
  }
  return { value: Math.floor(elapsedMs / DAY_MS), unit: "day" };
}

export function nextRefreshAgeUpdateDelay(
  timestamp: string | null | undefined,
  nowMs = Date.now(),
): number | null {
  if (!timestamp) return null;
  const timestampMs = Date.parse(timestamp);
  if (!Number.isFinite(timestampMs)) return null;

  const elapsedMs = Math.max(0, nowMs - timestampMs);
  const intervalMs =
    elapsedMs < MINUTE_MS
      ? SECOND_MS
      : elapsedMs < HOUR_MS
        ? MINUTE_MS
        : elapsedMs < DAY_MS
          ? HOUR_MS
          : DAY_MS;
  return Math.max(1, intervalMs - (elapsedMs % intervalMs));
}

export function formatRefreshAge(
  timestamp: string | null | undefined,
  locale: string,
  nowMs = Date.now(),
): string | null {
  const age = getRefreshAge(timestamp, nowMs);
  if (!age) return null;
  return new Intl.RelativeTimeFormat(locale, { numeric: "always" }).format(
    -age.value,
    age.unit,
  );
}
