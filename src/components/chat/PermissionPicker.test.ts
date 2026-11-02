import { describe, expect, it } from "vitest";
import {
  isPermissionDefaultSelected,
  normalizePermissionComponent,
  updatePermissionComponentValue,
  type PermissionPickerProps,
} from "./PermissionPicker";

describe("PermissionPicker 统一 JSON 契约", () => {
  it("空权限数据补齐为六个数组参数并默认自动", () => {
    expect(normalizePermissionComponent({})).toEqual({
      autonomyPreset: ["automatic"],
      trust: ["automatic"],
      approval: ["automatic"],
      sandbox: ["automatic"],
      network: ["automatic"],
      defaultForNewThreads: [],
    });
  });

  it("保留四种后端来源都可承载的统一数组值", () => {
    const values = normalizePermissionComponent({
      autonomyPreset: ["full"],
      trust: ["trusted"],
      approval: ["autonomous"],
      sandbox: ["workspace-write"],
      network: ["enabled"],
      defaultForNewThreads: ["full"],
    });
    expect(Object.keys(values)).toHaveLength(6);
    expect(Object.values(values).every((value) => Array.isArray(value))).toBe(true);
  });

  it("预设空数组表示自定义，而不是自动", () => {
    const values = normalizePermissionComponent({ autonomyPreset: [] });
    expect(values.autonomyPreset).toEqual([]);
  });

  it("修改审批、沙箱或网络参数时清空预设", () => {
    const values = normalizePermissionComponent({ autonomyPreset: ["full"] });
    expect(updatePermissionComponentValue(values, "approval", "ask").autonomyPreset).toEqual([]);
    expect(updatePermissionComponentValue(values, "sandbox", "workspace-write").autonomyPreset).toEqual([]);
    expect(updatePermissionComponentValue(values, "network", "enabled").autonomyPreset).toEqual([]);
    expect(updatePermissionComponentValue(values, "trust", "trusted").autonomyPreset).toEqual([]);
  });

  it("各套餐填写完整权限，修改细分项保留其他权限和默认套餐", () => {
    const values = normalizePermissionComponent({ defaultForNewThreads: ["ask"] });
    const expected: Record<string, string[]> = {
      automatic: ["automatic", "automatic", "automatic", "automatic"],
      "read-only": ["restricted", "restricted", "read-only", "restricted"],
      ask: ["standard", "ask", "workspace-write", "restricted"],
      auto: ["trusted", "automatic", "workspace-write", "enabled"],
      full: ["trusted", "autonomous", "full-access", "enabled"],
    };
    for (const [preset, fields] of Object.entries(expected)) {
      const result = updatePermissionComponentValue(values, "autonomyPreset", preset);
      expect([result.trust[0], result.approval[0], result.sandbox[0], result.network[0]]).toEqual(fields);
      expect(result.defaultForNewThreads).toEqual(["ask"]);
    }
    const full = updatePermissionComponentValue(values, "autonomyPreset", "full");
    expect(updatePermissionComponentValue(full, "approval", "ask")).toEqual({
      autonomyPreset: [], trust: ["trusted"], approval: ["ask"],
      sandbox: ["full-access"], network: ["enabled"], defaultForNewThreads: ["ask"],
    });
  });

  it("默认预设必须与当前预设相同才显示选中", () => {
    expect(isPermissionDefaultSelected("automatic", ["automatic"])).toBe(true);
    expect(isPermissionDefaultSelected("full", ["automatic"])).toBe(false);
    expect(isPermissionDefaultSelected(null, ["full"])).toBe(false);
  });

  it("组件 props 只有统一值和交互回调，不接收 CLI 标识", () => {
    const props: PermissionPickerProps = {
      value: normalizePermissionComponent({}),
      onChange: () => undefined,
    };
    expect(Object.keys(props)).toEqual(["value", "onChange"]);

    // @ts-expect-error CLI 适配属于后端，组件 props 不允许出现 engineId。
    const invalidProps: PermissionPickerProps = { ...props, engineId: "codex" };
    void invalidProps;
  });

});
