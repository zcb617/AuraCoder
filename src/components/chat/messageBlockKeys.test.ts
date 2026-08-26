import { describe, expect, it } from "vitest";
import type { ContentBlock } from "../../types";
import { getActionGroupId, getMessageBlockKey } from "./messageBlockKeys";

describe("getMessageBlockKey", () => {
  it("keeps approval keys stable when a reroute notice is prepended", () => {
    const approvalBlock: ContentBlock = {
      type: "approval",
      approvalId: "approval-1",
      actionType: "command",
      summary: "Run tests",
      details: {},
      status: "pending",
    };
    const baseBlocks: ContentBlock[] = [approvalBlock];
    const reroutedBlocks: ContentBlock[] = [
      {
        type: "notice",
        kind: "model_rerouted",
        level: "info",
        title: "Model rerouted",
        message: "Switched models.",
      },
      approvalBlock,
    ];

    expect(getMessageBlockKey(baseBlocks[0], 0, baseBlocks)).toBe(
      getMessageBlockKey(reroutedBlocks[1], 1, reroutedBlocks),
    );
  });

  it("keeps diff keys stable when a reroute notice is prepended", () => {
    const diffBlock: ContentBlock = {
      type: "diff",
      scope: "turn",
      diff: "diff --git a/file b/file",
    };
    const secondDiffBlock: ContentBlock = {
      type: "diff",
      scope: "file",
      diff: "diff --git a/other b/other",
    };
    const baseBlocks: ContentBlock[] = [diffBlock, secondDiffBlock];
    const reroutedBlocks: ContentBlock[] = [
      {
        type: "notice",
        kind: "model_rerouted",
        level: "info",
        title: "Model rerouted",
        message: "Switched models.",
      },
      diffBlock,
      secondDiffBlock,
    ];

    expect(getMessageBlockKey(baseBlocks[0], 0, baseBlocks)).toBe(
      getMessageBlockKey(reroutedBlocks[1], 1, reroutedBlocks),
    );
    expect(getMessageBlockKey(baseBlocks[1], 1, baseBlocks)).toBe(
      getMessageBlockKey(reroutedBlocks[2], 2, reroutedBlocks),
    );
  });
});

describe("getActionGroupId", () => {
  it("stays stable when more actions are appended to the group", () => {
    const actionsBeforeAppend = ["action-1", "action-2"];
    const actionsAfterAppend = [...actionsBeforeAppend, "action-3"];
    const groupIdBeforeAppend = getActionGroupId("message-1", actionsBeforeAppend[0]);
    const groupIdAfterAppend = getActionGroupId("message-1", actionsAfterAppend[0]);

    expect(groupIdAfterAppend).toBe(groupIdBeforeAppend);
  });

  it("keeps groups from different messages separate", () => {
    expect(getActionGroupId("message-1", "action-1")).not.toBe(
      getActionGroupId("message-2", "action-1"),
    );
  });
});
