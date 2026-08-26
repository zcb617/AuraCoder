interface TriggerRect {
  top: number;
  bottom: number;
  right: number;
}

interface ActionMenuPositionOptions {
  triggerRect: TriggerRect;
  menuWidth: number;
  menuHeight: number;
  viewportWidth: number;
  viewportHeight: number;
  horizontalPlacement?: "before" | "after";
  verticalPlacement?: "flip" | "clamp";
  gap?: number;
  padding?: number;
}

interface ActionMenuPosition {
  top: number;
  left: number;
}

export function getActionMenuPosition({
  triggerRect,
  menuWidth,
  menuHeight,
  viewportWidth,
  viewportHeight,
  horizontalPlacement = "before",
  verticalPlacement = "flip",
  gap = 4,
  padding = 8,
}: ActionMenuPositionOptions): ActionMenuPosition {
  const unclampedLeft = horizontalPlacement === "after"
    ? triggerRect.right + gap
    : triggerRect.right - menuWidth;
  const maxLeft = Math.max(padding, viewportWidth - menuWidth - padding);
  const left = Math.min(Math.max(padding, unclampedLeft), maxLeft);

  const spaceBelow = viewportHeight - triggerRect.bottom - padding;
  const showAbove =
    spaceBelow < menuHeight + gap &&
    triggerRect.top - padding > spaceBelow;
  const preferredTop = verticalPlacement === "clamp"
    ? triggerRect.bottom + gap
    : showAbove
    ? triggerRect.top - menuHeight - gap
    : triggerRect.bottom + gap;
  const maxTop = Math.max(padding, viewportHeight - menuHeight - padding);
  const top = Math.min(Math.max(padding, preferredTop), maxTop);

  return { top, left };
}
