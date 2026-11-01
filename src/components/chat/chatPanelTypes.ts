import type { PermissionComponentJson } from "../../types";

/** 线程运行时选项及统一权限对象的局部保存字段。 */
export type ThreadRuntimeSelectionPatch = {
  /** 更新线程使用的 CLI 引擎。 */
  engineId?: string | null;
  /** 更新线程使用的模型。 */
  modelId?: string | null;
  /** 更新线程的计划模式。 */
  planMode?: boolean | null;
  /** 更新线程的消息发送方式。 */
  sendMethod?: string | null;
  /** 更新线程的推理强度。 */
  reasoningEffort?: string | null;
  /** 更新线程完整的 CLI 无关权限对象。 */
  permissionMode?: PermissionComponentJson | null;
};

interface TextAnnotationPopover {
  /** 当前批注对应的文本内容。 */
  selectedText: string;
  /** 批注弹层相对视口的水平位置。 */
  left: number;
  /** 批注弹层相对视口的垂直位置。 */
  top: number;
  /** 批注弹层当前显示阶段。 */
  stage: "actions" | "comment";
}

interface CodexReferenceCatalogState {
  /** Codex skills 目录是否已加载。 */
  skillsLoaded: boolean;
  /** Codex apps 目录是否已加载。 */
  appsLoaded: boolean;
}

export type { TextAnnotationPopover, CodexReferenceCatalogState };
