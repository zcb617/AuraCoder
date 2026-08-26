import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Panel, PanelGroup, PanelResizeHandle } from "react-resizable-panels";
import { PanelRightOpen } from "lucide-react";
import { Sidebar } from "../sidebar/Sidebar";
import { ActiveWorkspacePaneShell } from "../workspace/WorkspacePaneShell";
import { HarnessPanel } from "../onboarding/HarnessPanel";
import { SettingsPage } from "../settings/SettingsPage";
import { ExtensionManagerPage } from "../extensions/ExtensionManagerPage";
import { ScheduledTasksPage } from "../scheduled/ScheduledTasksPage";
import { RightToolPanel } from "./RightToolPanel";
import { usesCustomWindowFrame } from "../../lib/windowActions";
import { useUiStore } from "../../stores/uiStore";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { getGitPanelLayoutState } from "../../lib/gitPanelLayout";
import { handleDragDoubleClick, handleDragMouseDown } from "../../lib/windowDrag";

const SIDEBAR_WIDTH_KEY = "auracoder:sidebar-width";
const GIT_PANEL_SIZE_KEY = "auracoder:git-panel-size";
const MIN_SIDEBAR = 160;
const MAX_SIDEBAR = 380;
const DEFAULT_SIDEBAR = 220;
const MIN_GIT_PANEL_SIZE = 18;
const MAX_GIT_PANEL_SIZE = 40;
const DEFAULT_GIT_PANEL_SIZE = 26;
const RESIZE_HANDLE_CLICK_THRESHOLD = 4;

function loadSidebarWidth(): number {
  try {
    const stored = localStorage.getItem(SIDEBAR_WIDTH_KEY);
    if (stored) {
      const v = parseInt(stored, 10);
      if (v >= MIN_SIDEBAR && v <= MAX_SIDEBAR) return v;
    }
  } catch { /* ignore */ }
  return DEFAULT_SIDEBAR;
}

function loadGitPanelSize(): number {
  try {
    const stored = localStorage.getItem(GIT_PANEL_SIZE_KEY);
    if (stored) {
      const value = Number.parseFloat(stored);
      if (value >= MIN_GIT_PANEL_SIZE && value <= MAX_GIT_PANEL_SIZE) {
        return value;
      }
    }
  } catch {
    // Ignore storage failures in non-browser/test environments.
  }
  return DEFAULT_GIT_PANEL_SIZE;
}

export function ThreeColumnLayout() {
  const { t } = useTranslation("git");
  const showSidebar = useUiStore((state) => state.showSidebar);
  const sidebarPinned = useUiStore((state) => state.sidebarPinned);
  const toggleSidebarPin = useUiStore((state) => state.toggleSidebarPin);
  const showGitPanel = useUiStore((state) => state.showGitPanel);
  const setGitPanelVisible = useUiStore((state) => state.setGitPanelVisible);
  const focusMode = useUiStore((state) => state.focusMode);
  const activeView = useUiStore((state) => state.activeView);
  const activeWorkspaceId = useWorkspaceStore((state) => state.activeWorkspaceId);
  const customWindowFrame = usesCustomWindowFrame();

  const settingsActive = activeView === "settings";
  const sidebarDocked = showSidebar && sidebarPinned && !settingsActive;
  const {
    gitPanelDocked,
    gitPanelDockedInWorkspace,
    showWorkspaceHeaderToggle,
    showEdgeReveal,
  } = getGitPanelLayoutState({
    activeView,
    activeWorkspaceId,
    showGitPanel,
  });
  const fullBleedContent = focusMode || !showSidebar || settingsActive;
  const showFocusDragStrip = focusMode && !showSidebar && !gitPanelDocked && !customWindowFrame;

  const [sidebarWidth, setSidebarWidth] = useState(loadSidebarWidth);
  const [gitPanelSize, setGitPanelSize] = useState(loadGitPanelSize);
  const sidebarHandleRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    try { localStorage.setItem(SIDEBAR_WIDTH_KEY, String(sidebarWidth)); } catch { /* ignore */ }
  }, [sidebarWidth]);

  useEffect(() => {
    try { localStorage.setItem(GIT_PANEL_SIZE_KEY, String(gitPanelSize)); } catch { /* ignore */ }
  }, [gitPanelSize]);

  const handleSidebarResizeMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = sidebarWidth;
    let isDragging = false;
    sidebarHandleRef.current?.classList.add("dragging");

    function onMove(ev: MouseEvent) {
      const delta = ev.clientX - startX;
      if (!isDragging && Math.abs(delta) < RESIZE_HANDLE_CLICK_THRESHOLD) {
        return;
      }
      isDragging = true;
      setSidebarWidth(Math.min(MAX_SIDEBAR, Math.max(MIN_SIDEBAR, startWidth + delta)));
    }

    function onUp(ev: MouseEvent) {
      sidebarHandleRef.current?.classList.remove("dragging");
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      if (!isDragging && Math.abs(ev.clientX - startX) < RESIZE_HANDLE_CLICK_THRESHOLD) {
        toggleSidebarPin();
      }
    }

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  }, [sidebarWidth, toggleSidebarPin]);

  const mainContent = (
    activeView === "harnesses" ? (
      <HarnessPanel />
    ) : activeView === "extensions" ? (
      <ExtensionManagerPage />
    ) : activeView === "scheduled" ? (
      <ScheduledTasksPage />
    ) : activeView === "settings" ? (
      <SettingsPage />
    ) : (
      <ActiveWorkspacePaneShell
        dockedGitPanel={
          gitPanelDockedInWorkspace ? (
            <RightToolPanel />
          ) : undefined
        }
        gitPanelSize={gitPanelSize}
        onGitPanelResize={setGitPanelSize}
        gitPanelVisible={showGitPanel}
        onToggleGitPanel={
          showWorkspaceHeaderToggle ? () => setGitPanelVisible(!showGitPanel) : undefined
        }
      />
    )
  );

  return (
    <div className="layout-root">
      {/* Unpinned sidebar — collapsed rail + hover flyout */}
      {showSidebar && !sidebarPinned && !settingsActive && <Sidebar />}

      {/* Pinned sidebar */}
      {sidebarDocked && (
        <div className="layout-sidebar" style={{ width: sidebarWidth }}>
          <Sidebar />
        </div>
      )}

      {/* Sidebar resize handle (pinned only) */}
      {sidebarDocked && (
        <div
          ref={sidebarHandleRef}
          className="sidebar-resize-handle"
          onMouseDown={handleSidebarResizeMouseDown}
        />
      )}

      {/* Floating content card */}
      <div
        className={`content-card ${fullBleedContent ? "content-card-full" : ""}`}
      >
        {showFocusDragStrip && (
          <div
            className="focus-drag-strip"
            onMouseDown={handleDragMouseDown}
            onDoubleClick={handleDragDoubleClick}
          />
        )}

        {gitPanelDocked && !gitPanelDockedInWorkspace ? (
          <PanelGroup
            key="main-layout-docked"
            id="main-layout-panels"
            autoSaveId="auracoder:main-layout-panels"
            direction="horizontal"
            style={{ height: "100%", flex: 1 }}
          >
            <Panel
              id="main-layout-content"
              order={1}
              defaultSize={100 - gitPanelSize}
              minSize={35}
            >
              <div className="content-panel" style={{ height: "100%" }}>
                {mainContent}
              </div>
            </Panel>

            <PanelResizeHandle
              id="main-layout-git-resize-handle"
              className="resize-handle"
              aria-label="调整右侧面板宽度"
            />

            <Panel
              id="main-layout-git-panel"
              order={2}
              defaultSize={gitPanelSize}
              minSize={MIN_GIT_PANEL_SIZE}
              maxSize={MAX_GIT_PANEL_SIZE}
              onResize={setGitPanelSize}
            >
              <div className="content-panel" style={{ height: "100%" }}>
                <RightToolPanel />
              </div>
            </Panel>
          </PanelGroup>
        ) : (
          <div className="content-panel" style={{ height: "100%", flex: 1 }}>
            {mainContent}
          </div>
        )}

        {showEdgeReveal ? (
          <button
            type="button"
            className="git-panel-reveal-button"
            title={t("panel.show")}
            aria-label={t("panel.show")}
            onClick={() => setGitPanelVisible(true)}
          >
            <PanelRightOpen size={15} />
          </button>
        ) : null}
      </div>
    </div>
  );
}
