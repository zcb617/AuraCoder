import { describe, expect, it } from "vitest";
import type { Workspace } from "../types";
import { workspaceThreadCatalogKey } from "./workspaceThreadCatalog";

/** 创建会话目录稳定键测试所需的最小项目夹具，并允许覆盖指定字段。 */
function createWorkspace(overrides: Partial<Workspace> = {}): Workspace {
  return {
    /** 项目的唯一身份，用于识别会话目录归属。 */
    id: "workspace-local",
    /** 项目展示名称，仅用于验证非结构字段不会影响稳定键。 */
    name: "本地项目",
    /** 本地项目根目录路径。 */
    rootPath: "/workspace/local",
    /** 项目连接类型，缺失时业务默认按 local 处理。 */
    locationKind: "local",
    /** SSH 连接身份，非会话目录结构字段。 */
    sshConnectionId: null,
    /** SSH 连接展示名称，非会话目录结构字段。 */
    connectionDisplayName: null,
    /** SSH 连接是否启用，非会话目录结构字段。 */
    connectionEnabled: true,
    /** SSH 连接是否已删除，非会话目录结构字段。 */
    connectionDeleted: false,
    /** SSH 连接当前状态，非会话目录结构字段。 */
    connectionStatus: "connected",
    /** 项目当前信任等级，非会话目录结构字段。 */
    trustLevel: "standard",
    /** 项目创建时间，非会话目录结构字段。 */
    createdAt: "2026-01-01T00:00:00.000Z",
    /** 项目最后打开时间，非会话目录结构字段。 */
    lastOpenedAt: "2026-01-01T00:00:00.000Z",
    /** 测试场景指定的项目字段覆盖值。 */
    ...overrides,
  };
}

describe("workspaceThreadCatalogKey", () => {
  it("本地和 SSH 项目的权限或信任变化不会触发两类项目会话重载", () => {
    const workspaces = [
      createWorkspace({ id: "workspace-local", locationKind: "local" }),
      createWorkspace({
        id: "workspace-ssh",
        name: "SSH 项目",
        locationKind: "ssh",
        sshConnectionId: "connection-1",
        connectionDisplayName: "开发机",
        connectionStatus: "connected",
      }),
    ];
    const changedWorkspaces = workspaces.map((workspace) => ({
      ...workspace,
      name: `${workspace.name}（已更新）`,
      trustLevel: (workspace.trustLevel === "standard" ? "trusted" : "standard") as Workspace["trustLevel"],
      connectionStatus: "reconnecting",
    }));

    expect(workspaceThreadCatalogKey(changedWorkspaces)).toBe(
      workspaceThreadCatalogKey(workspaces),
    );
  });

  it("相同项目仅调换数组顺序时保持稳定键不变", () => {
    const workspaces = [
      createWorkspace({ id: "workspace-local", locationKind: "local" }),
      createWorkspace({ id: "workspace-ssh", locationKind: "ssh" }),
    ];

    expect(workspaceThreadCatalogKey([...workspaces].reverse())).toBe(
      workspaceThreadCatalogKey(workspaces),
    );
  });

  it("新增、删除、修改项目 id 或 locationKind 时改变稳定键", () => {
    const workspaces = [
      createWorkspace({ id: "workspace-local", locationKind: "local" }),
      createWorkspace({ id: "workspace-ssh", locationKind: "ssh" }),
    ];
    const originalKey = workspaceThreadCatalogKey(workspaces);

    expect(
      workspaceThreadCatalogKey([
        ...workspaces,
        createWorkspace({ id: "workspace-new", locationKind: "local" }),
      ]),
    ).not.toBe(originalKey);
    expect(workspaceThreadCatalogKey(workspaces.slice(0, 1))).not.toBe(originalKey);
    expect(
      workspaceThreadCatalogKey(
        workspaces.map((workspace) =>
          workspace.id === "workspace-local"
            ? { ...workspace, id: "workspace-renamed" }
            : workspace,
        ),
      ),
    ).not.toBe(originalKey);
    expect(
      workspaceThreadCatalogKey(
        workspaces.map((workspace) =>
          workspace.id === "workspace-local"
            ? { ...workspace, locationKind: "ssh" }
            : workspace,
        ),
      ),
    ).not.toBe(originalKey);
  });
});
