export const DISPLAY_SCALE_PREFERENCES = [100, 110, 120, 130, 140, 150] as const;

export type DisplayScale = (typeof DISPLAY_SCALE_PREFERENCES)[number];

export function isDisplayScale(value?: number | null): value is DisplayScale {
  return DISPLAY_SCALE_PREFERENCES.includes(value as DisplayScale);
}

export function normalizeDisplayScale(value?: number | null): DisplayScale {
  return isDisplayScale(value) ? value : 100;
}

/*
const DISPLAY_SCALE_STORAGE_KEY = "auracoder:display-scale";

export function applyDisplayScale(displayScale: DisplayScale) {
  if (typeof document !== "undefined") {
    document.documentElement.style.setProperty("zoom", `${displayScale}%`);
  }

  if (typeof window !== "undefined") {
    try {
      window.localStorage.setItem(DISPLAY_SCALE_STORAGE_KEY, String(displayScale));
    } catch {
      // Storage is only a first-paint hint; the persisted app config remains authoritative.
    }
  }
}
*/
