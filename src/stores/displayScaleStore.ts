import { create } from "zustand";
import { ipc } from "../lib/ipc";
import { normalizeDisplayScale, type DisplayScale } from "../lib/displayScale";

interface DisplayScaleStoreState {
  displayScale: DisplayScale;
  loaded: boolean;
  load: () => Promise<DisplayScale>;
  setDisplayScale: (displayScale: DisplayScale) => Promise<boolean>;
}

export const useDisplayScaleStore = create<DisplayScaleStoreState>((set /*, get */) => ({
  displayScale: 100,
  loaded: false,

  load: async () => {
    try {
      const saved = await ipc.getDisplayScale();
      const normalized = normalizeDisplayScale(saved);
      // applyDisplayScale(normalized);
      set({ displayScale: normalized, loaded: true });
      return normalized;
    } catch {
      // Frontend-only dev/test contexts won't have the Tauri invoke bridge.
      // applyDisplayScale(100);
      set({ loaded: true });
      return 100;
    }
  },

  setDisplayScale: async (displayScale) => {
    // const previous = get().displayScale;
    // set({ displayScale });
    // applyDisplayScale(displayScale);
    try {
      const saved = await ipc.setDisplayScale(displayScale);
      const normalized = normalizeDisplayScale(saved);
      set({ displayScale: normalized });
      // applyDisplayScale(normalized);
      return true;
    } catch {
      // set({ displayScale: previous });
      // applyDisplayScale(previous);
      return false;
    }
  },
}));
