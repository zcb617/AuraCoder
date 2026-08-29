import { describe, expect, it } from "vitest";
import {
  isPermissionDefaultSelected,
  normalizePermissionComponent,
  updatePermissionComponentValue,
  type PermissionPickerProps,
} from "./PermissionPicker";
import {
  permissionSaveWaitAction,
  shouldUpdateWorkspaceTrustLevel,
} from "./ChatPanel";

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
    expect(updatePermissionComponentValue(values, "trust", "trusted").autonomyPreset).toEqual(["full"]);
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

  it("相同信任等级不更新，不同有效等级更新，automatic 和非法值不更新", () => {
    expect(shouldUpdateWorkspaceTrustLevel("standard", "standard")).toBe(false);
    expect(shouldUpdateWorkspaceTrustLevel("trusted", "standard")).toBe(true);
    expect(shouldUpdateWorkspaceTrustLevel("restricted", "standard")).toBe(true);
    expect(shouldUpdateWorkspaceTrustLevel("automatic", "standard")).toBe(false);
    expect(shouldUpdateWorkspaceTrustLevel(null, "standard")).toBe(false);
  });

  it("权限保存失败且旧请求已清理时阻止发送", () => {
    const pending = Promise.resolve(false);
    expect(permissionSaveWaitAction(false, pending, undefined)).toBe("fail");
  });

  it("权限保存期间登记新请求时继续等待新请求", () => {
    const pending = Promise.resolve(false);
    const latest = Promise.resolve(true);
    expect(permissionSaveWaitAction(false, pending, latest)).toBe("continue");
    expect(permissionSaveWaitAction(true, latest, latest)).toBe("complete");
  });
});
