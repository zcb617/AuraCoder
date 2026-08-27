import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useUiStore } from "../../stores/uiStore";
import { WorkspaceStartupSection } from "./WorkspaceStartupSection";
import type { TrustLevel } from "../../types";

export type WorkspaceSettingsSection = "general" | "repos" | "startup";

interface Props {
  embedded?: boolean;
  section?: WorkspaceSettingsSection;
}

/** 项目设置页面，负责项目级信任等级和启动预设配置。 */
export function WorkspaceSettingsPage({ embedded = false, section: controlledSection }: Props = {}) {
  const { t } = useTranslation("workspace");
  const workspace = useWorkspaceStore((state) => state.workspaces.find((item) => item.id === state.activeWorkspaceId) ?? null);
  const setWorkspaceTrustLevel = useWorkspaceStore((state) => state.setWorkspaceTrustLevel);
  const setActiveView = useUiStore((state) => state.setActiveView);
  const [localSection, setLocalSection] = useState<WorkspaceSettingsSection>(controlledSection ?? "general");
  const section = controlledSection ?? localSection;

  if (!workspace) return null;
  const trustOptions: TrustLevel[] = ["trusted", "standard", "restricted"];
  return (
    <div className={embedded ? "workspace-settings embedded" : "workspace-settings"}>
      <nav className="workspace-settings-nav">
        <button type="button" onClick={() => setActiveView("chat")}>{t("nav.back")}</button>
        <button type="button" onClick={() => setLocalSection("general")}>{t("nav.general")}</button>
        <button type="button" onClick={() => setLocalSection("startup")}>{t("nav.startup")}</button>
      </nav>
      {section === "startup" ? <WorkspaceStartupSection workspace={workspace} /> : (
        <section className="workspace-settings-content">
          <h2>{workspace.name}</h2>
          <p>{workspace.rootPath}</p>
          <label>
            {t("trust.label")}
            <select value={workspace.trustLevel} onChange={(event) => void setWorkspaceTrustLevel(workspace.id, event.target.value as TrustLevel)}>
              {trustOptions.map((level) => <option key={level} value={level}>{t(`trust.${level}`)}</option>)}
            </select>
          </label>
        </section>
      )}
    </div>
  );
}
