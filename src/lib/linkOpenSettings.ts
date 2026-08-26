export const LINK_OPEN_GESTURES = ["shift-click", "click"] as const;

export type LinkOpenGesture = (typeof LINK_OPEN_GESTURES)[number];

export const DEFAULT_LINK_OPEN_GESTURE: LinkOpenGesture = "click";

export function isLinkOpenGesture(value: string): value is LinkOpenGesture {
  return (LINK_OPEN_GESTURES as readonly string[]).includes(value);
}

export function shouldOpenLink(shiftKey: boolean, gesture: LinkOpenGesture): boolean {
  return gesture === "click" || shiftKey;
}
