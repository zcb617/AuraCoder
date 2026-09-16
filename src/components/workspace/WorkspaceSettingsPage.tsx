import { useState } from "react";
import { useTranslation } from "react-i18next";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useUiStore } from "../../stores/uiStore";
import { Dropdown } from "../shared/Dropdown";
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
      <nav className="workspace-settings-nav" style={{ display: "none" }}>
        <button type="button" onClick={() => setActiveView("chat")}>{t("actions.back")}</button>
        <button className="workspace-settings-section-nav" type="button" onClick={() => setLocalSection("general")}>{t("nav.general")}</button>
        <button className="workspace-settings-section-nav" type="button" onClick={() => setLocalSection("startup")}>{t("nav.startup")}</button>
      </nav>
      {section === "startup" ? <WorkspaceStartupSection workspace={workspace} /> : (
        <section className="workspace-settings-content">
        <button type="button" style={{ width: "100px" }} className="ws-prop-btn ws-prop-btn-accent" onClick={() => setActiveView("chat")}>
          <div style={{ width: "100%" }}>前往对话页</div>
        </button>
          <label>
            {t("trust.label")}
            <Dropdown
              value={workspace.trustLevel}
              options={trustOptions.map((level) => ({ value: level, label: t(`trust.${level}`) }))}
              onChange={(value) => void setWorkspaceTrustLevel(workspace.id, value as TrustLevel)}
              triggerStyle={{ width: "100%", justifyContent: "space-between" }}
            />
          </label>
        </section>
      )}

    </div>
  );
}
