import { useUiStore } from "../../stores/uiStore";
import { BrowserPanel } from "../browser/BrowserPanel";
import { GitPanel } from "../git/GitPanel";
import { ImageAttachmentPreviewPanel } from "../chat/ImageAttachmentPreviewPanel";

export function RightToolPanel() {
  const activeRightTool = useUiStore((state) => state.activeRightTool);
  const setActiveRightTool = useUiStore((state) => state.setActiveRightTool);

  return (
    <section className="right-tool-panel">
      <nav className="right-tool-tabs" aria-label="右侧工具">
        <button
          type="button"
          className={activeRightTool === "git" ? "right-tool-tab-active" : ""}
          onClick={() => setActiveRightTool("git")}
          aria-pressed={activeRightTool === "git"}
        >
          更改
        </button>
        <button
          type="button"
          className={activeRightTool === "browser" ? "right-tool-tab-active" : ""}
          onClick={() => setActiveRightTool("browser")}
          aria-pressed={activeRightTool === "browser"}
        >
          浏览器
        </button>
        <button
          type="button"
          className={activeRightTool === "attachments" ? "right-tool-tab-active" : ""}
          onClick={() => setActiveRightTool("attachments")}
          aria-pressed={activeRightTool === "attachments"}
        >
          用户附件
        </button>
      </nav>
      <div className="right-tool-content">
        {activeRightTool === "git" ? (
          <GitPanel />
        ) : activeRightTool === "browser" ? (
          <BrowserPanel />
        ) : (
          <ImageAttachmentPreviewPanel />
        )}
      </div>
    </section>
  );
}
