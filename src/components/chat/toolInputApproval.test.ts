import { describe, expect, it } from "vitest";
import {
  buildMcpElicitationApprovalResponse,
  defaultAdvancedApprovalPayload,
} from "./toolInputApproval";

describe("MCP elicitation approval responses", () => {
  const formDetails = {
    _serverMethod: "mcpserver/elicitation/request",
    mode: "form",
    requestedSchema: {
      type: "object",
      properties: {
        remember: {
          type: "boolean",
          default: true,
        },
      },
    },
  };

  it("builds the same default payload when the user approves", () => {
    expect(buildMcpElicitationApprovalResponse(formDetails, "accept")).toEqual({
      action: "accept",
      content: { remember: true },
    });
    expect(defaultAdvancedApprovalPayload(formDetails)).toEqual({
      action: "accept",
      content: { remember: true },
    });
  });

  it("returns a protocol-native decline without exposing JSON", () => {
    expect(buildMcpElicitationApprovalResponse(formDetails, "decline")).toEqual({
      action: "decline",
    });
  });
});
