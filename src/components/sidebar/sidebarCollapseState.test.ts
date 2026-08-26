import { describe, expect, it } from "vitest";
import { normalizeSidebarCollapsedState } from "./sidebarCollapseState";

describe("normalizeSidebarCollapsedState", () => {
  it("collapses every workspace during startup", () => {
    expect(
      normalizeSidebarCollapsedState(["ws-a", "ws-b", "ws-c"], null, {}, null, true),
    ).toEqual({
      "ws-a": true,
      "ws-b": true,
      "ws-c": true,
    });
  });

  it("collapses the previous workspace when the active workspace changes externally", () => {
    expect(
      normalizeSidebarCollapsedState(
        ["ws-a", "ws-b", "ws-c"],
        "ws-c",
        {
          "ws-a": false,
          "ws-b": true,
          "ws-c": true,
        },
        "ws-a",
      ),
    ).toEqual({
      "ws-a": true,
      "ws-b": true,
      "ws-c": false,
    });
  });

  it("prunes removed ids and collapses newly added inactive workspaces", () => {
    expect(
      normalizeSidebarCollapsedState(
        ["ws-a", "ws-b", "ws-c"],
        "ws-a",
        {
          "ws-a": false,
          "ws-b": true,
          "ws-removed": true,
        },
        "ws-a",
      ),
    ).toEqual({
      "ws-a": false,
      "ws-b": true,
      "ws-c": true,
    });
  });

  it("preserves manual collapse state when the active workspace is unchanged", () => {
    expect(
      normalizeSidebarCollapsedState(
        ["ws-a", "ws-b"],
        "ws-a",
        {
          "ws-a": true,
          "ws-b": true,
        },
        "ws-a",
      ),
    ).toEqual({
      "ws-a": true,
      "ws-b": true,
    });
  });
});
