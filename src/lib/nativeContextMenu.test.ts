import { describe, expect, it, vi } from "vitest";
import { preventNativeContextMenu } from "./nativeContextMenu";

describe("preventNativeContextMenu", () => {
  it("cancels the WebView default context menu", () => {
    const preventDefault = vi.fn();

    preventNativeContextMenu({ preventDefault } as unknown as Event);

    expect(preventDefault).toHaveBeenCalledOnce();
  });
});
