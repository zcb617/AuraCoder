import { useEffect } from "react";
import { useTranslation } from "react-i18next";
import { GitBranch as GitBranchIcon } from "lucide-react";
import { useWorkspaceStore } from "../../stores/workspaceStore";
import { useGitStore /*, type GitPanelView */ } from "../../stores/gitStore";
import { GitChangesView } from "./GitChangesView";
import { GitBranchesView } from "./GitBranchesView";
import { GitCommitsView } from "./GitCommitsView";
import { GitStashView } from "./GitStashView";
import { GitWorktreesView } from "./GitWorktreesView";

interface Props { mode?: "docked" | "flyout"; visible?: boolean; onPin?: () => void; }

/** 项目级 Git 面板，依据当前项目根目录 Git 上下文展示单仓库操作。 */
export function GitPanel({ visible = true }: Props) {
  const { t } = useTranslation("git");
  const workspaceId = useWorkspaceStore((state) => state.activeWorkspaceId);
  const gitContext = useGitStore((state) => state.gitContext);
  const activeView = useGitStore((state) => state.activeView);
  // const setActiveView = useGitStore((state) => state.setActiveView);
  const loadWorkspaceContext = useGitStore((state) => state.loadWorkspaceContext);
  const error = useGitStore((state) => state.error);
  useEffect(() => { if (workspaceId) void loadWorkspaceContext(workspaceId); }, [workspaceId, loadWorkspaceContext]);
  if (!visible) return null;
  if (!workspaceId || gitContext?.kind !== "repository") {
    return <div className="git-empty"><GitBranchIcon size={20} /><span>{t("panel.notRepository", { defaultValue: "当前项目根目录不是 Git 仓库" })}</span></div>;
  }
  const onError = () => undefined;
  return <div className="git-panel">
    {/* <header className="git-panel-header"><span>{gitContext.name}</span><nav>{(["changes", "branches", "commits", "stash", "worktrees"] as GitPanelView[]).map((view) => <button type="button" key={view} onClick={() => setActiveView(view)}>{t(`views.${view}`, { defaultValue: view })}</button>)}</nav></header> */}
    {activeView === "changes" && <GitChangesView context={gitContext} showDiff onError={onError} />}
    {activeView === "branches" && <GitBranchesView context={gitContext} onError={onError} />}
    {activeView === "commits" && <GitCommitsView context={gitContext} />}
    {activeView === "stash" && <GitStashView context={gitContext} onError={onError} />}
    {activeView === "worktrees" && <GitWorktreesView context={gitContext} onError={onError} />}
    {error && <div className="git-error">{error}</div>}
  </div>;
}
