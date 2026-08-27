import { useState } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { WorkspaceStartupSection } from "./WorkspaceStartupSection";
import type { TrustLevel, Workspace } from "../../types";

type Section = "general" | "repos" | "startup";

interface WorkspaceSettingsModalProps {
  workspace: Workspace;
  onClose: () => void;
}

/** 项目设置弹窗，负责项目级信任等级和启动预设。 */
export function WorkspaceSettingsModal({ workspace, onClose }: WorkspaceSettingsModalProps) {
  const { t } = useTranslation("workspace");
  const setWorkspaceTrustLevel = useWorkspaceStore((state) => state.setWorkspaceTrustLevel);
  const [section, setSection] = useState<Section>("general");
  const trustOptions: TrustLevel[] = ["trusted", "standard", "restricted"];
  return createPortal(
    <div className="workspace-settings-modal-backdrop" role="presentation" onClick={onClose}>
      <div className="workspace-settings-modal" role="dialog" aria-modal="true" onClick={(event) => event.stopPropagation()}>
        <header><h2>{workspace.name}</h2><button type="button" onClick={onClose}>×</button></header>
        <nav>
          <button type="button" onClick={() => setSection("general")}>{t("nav.general")}</button>
          <button type="button" onClick={() => setSection("startup")}>{t("nav.startup")}</button>
        </nav>
        {section === "startup" ? <WorkspaceStartupSection workspace={workspace} /> : (
          <section><p>{workspace.rootPath}</p><label>{t("trust.label")}<select value={workspace.trustLevel} onChange={(event) => void setWorkspaceTrustLevel(workspace.id, event.target.value as TrustLevel)}>{trustOptions.map((level) => <option key={level} value={level}>{t(`trust.${level}`)}</option>)}</select></label></section>
        )}
      </div>
    </div>,
    document.body,
  );
}
