/** 线程运行时五项可局部保存字段；权限字段不属于本契约。 */
export type ThreadRuntimeSelectionPatch = {
  engineId?: string | null;
  modelId?: string | null;
  planMode?: boolean | null;
  sendMethod?: string | null;
  reasoningEffort?: string | null;
};

interface TextAnnotationPopover {
  selectedText: string;
  left: number;
  top: number;
  stage: "actions" | "comment";
}

interface CodexReferenceCatalogState {
  skillsLoaded: boolean;
  appsLoaded: boolean;
}

export type { TextAnnotationPopover, CodexReferenceCatalogState };
