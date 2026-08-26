/**
 * Prevent the WebView's built-in context menu while allowing AuraCoder' own
 * context-menu handlers to continue receiving the event.
 */
export function preventNativeContextMenu(event: Event): void {
  event.preventDefault();
}
