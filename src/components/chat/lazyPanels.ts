import { lazy } from "react";

export const LazyTerminalPanel = lazy(() =>
  import("../terminal/TerminalPanel").then((module) => ({
    default: module.TerminalPanel,
  })),
);
export const LazyEditorWithExplorer = lazy(() =>
  import("../editor/EditorWithExplorer").then((module) => ({
    default: module.EditorWithExplorer,
  })),
);
