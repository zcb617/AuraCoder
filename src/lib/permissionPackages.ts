import type { PermissionComponentJson } from "../types";
import { ipc } from "./ipc";

/** 将前端权限套餐展开为完整的权限配置。 */
export function applyPermissionPackage(values: PermissionComponentJson, preset: string): PermissionComponentJson {
  // 根据套餐填写四项业务权限，保留新会话默认套餐设置。
  const packages: Record<string, [string, string, string, string]> = {
    automatic: ["automatic", "automatic", "automatic", "automatic"],
    "read-only": ["restricted", "restricted", "read-only", "restricted"],
    ask: ["standard", "ask", "workspace-write", "restricted"],
    auto: ["trusted", "automatic", "workspace-write", "enabled"],
    full: ["trusted", "autonomous", "full-access", "enabled"],
  };
  const selected = Object.hasOwn(packages, preset) ? packages[preset] : undefined;
  if (!selected) throw new Error(`未知权限套餐：${preset}`);
  return {
    autonomyPreset: [preset],
    trust: [selected[0]],
    approval: [selected[1]],
    sandbox: [selected[2]],
    network: [selected[3]],
    defaultForNewThreads: [...values.defaultForNewThreads],
  };
}

/** 读取新会话默认套餐并展开为完整权限配置。 */
export async function loadDefaultPermissionPackage(): Promise<PermissionComponentJson> {
  // 默认套餐未设置时使用自动权限，读取失败交由调用方处理。
  const preset = await ipc.getDefaultAutonomyPreset();
  return applyPermissionPackage({
    autonomyPreset: ["automatic"],
    trust: ["automatic"],
    approval: ["automatic"],
    sandbox: ["automatic"],
    network: ["automatic"],
    defaultForNewThreads: preset !== null ? [preset] : [],
  }, preset ?? "automatic");
}
