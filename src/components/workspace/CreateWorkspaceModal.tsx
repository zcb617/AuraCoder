import { useEffect, useId, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { ArrowLeft, Check, ChevronDown, ChevronRight, Folder, Globe2, Home, LoaderCircle, Monitor, RefreshCw, Server, X } from "lucide-react";
import { useTranslation } from "react-i18next";
import { ipc } from "../../lib/ipc";
import { useSshConnectionStore } from "../../stores/sshConnectionStore";
import type { SshConnection, SshRemoteDirectory, Workspace } from "../../types";
import "./CreateWorkspaceModal.css";

interface CreateWorkspaceModalProps {
  open: boolean;
  onClose: () => void;
  onSelectLocal: () => void;
  onCreateRemote: (connectionId: string, name: string, rootPath: string) => Promise<Workspace | null>;
  onCreated: (workspace: Workspace) => void;
  onOpenConnections: () => void;
}

interface RemoteWorkspaceFormProps {
  connections: SshConnection[];
  connectionId: string;
  setConnectionId: (value: string) => void;
  projectName: string;
  setProjectName: (value: string) => void;
  pathInput: string;
  setPathInput: (value: string) => void;
  currentPath: string;
  homePath: string;
  directories: SshRemoteDirectory[];
  selectedDirectory: string | null;
  setSelectedDirectory: (value: string | null) => void;
  loadingPath: boolean;
  error: string | null;
  // 历史属性（已停用）：projectNameEdited: boolean;
  loadPath: (path: string, parent?: boolean) => Promise<void>;
  onCreate: () => Promise<void>;
  canCreate: boolean;
  creating: boolean;
  onOpenConnections: () => void;
  t: (key: string, options?: Record<string, unknown>) => string;
}

function RemoteWorkspaceForm({
  connections,
  connectionId,
  setConnectionId,
  projectName,
  setProjectName,
  pathInput,
  setPathInput,
  currentPath,
  homePath,
  directories,
  selectedDirectory,
  setSelectedDirectory,
  loadingPath,
  error,
  // 历史解构（已停用）：projectNameEdited,
  loadPath,
  onCreate,
  canCreate,
  creating,
  onOpenConnections,
  t,
}: RemoteWorkspaceFormProps) {
  const selectedConnection = connections.find((connection) => connection.id === connectionId);
  const hostSelectId = useId();
  const hostSelectRef = useRef<HTMLDivElement>(null);
  const hostTriggerRef = useRef<HTMLButtonElement>(null);
  const [hostMenuOpen, setHostMenuOpen] = useState(false);
  const [activeHostIndex, setActiveHostIndex] = useState(0);
  const selectedHostIndex = Math.max(0, connections.findIndex((connection) => connection.id === connectionId));

  useEffect(() => {
    if (!hostMenuOpen) return;
    const handlePointerDown = (event: PointerEvent) => {
      if (!hostSelectRef.current?.contains(event.target as HTMLElement)) setHostMenuOpen(false);
    };
    document.addEventListener("pointerdown", handlePointerDown, true);
    return () => document.removeEventListener("pointerdown", handlePointerDown, true);
  }, [hostMenuOpen]);

  const openHostMenu = () => {
    if (!connections.length) return;
    setActiveHostIndex(selectedHostIndex);
    setHostMenuOpen(true);
  };

  const selectHost = (index: number) => {
    const connection = connections[index];
    if (!connection) return;
    setConnectionId(connection.id);
    setActiveHostIndex(index);
    setHostMenuOpen(false);
    hostTriggerRef.current?.focus();
  };

  const handleHostKeyDown = (event: React.KeyboardEvent<HTMLButtonElement>) => {
    if (event.key === "Escape" && hostMenuOpen) {
      event.preventDefault();
      event.stopPropagation();
      setHostMenuOpen(false);
      return;
    }
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (!hostMenuOpen) {
        openHostMenu();
        return;
      }
      const direction = event.key === "ArrowDown" ? 1 : -1;
      setActiveHostIndex((current) => (current + direction + connections.length) % connections.length);
      return;
    }
    if (event.key === "Home" && hostMenuOpen) {
      event.preventDefault();
      setActiveHostIndex(0);
      return;
    }
    if (event.key === "End" && hostMenuOpen) {
      event.preventDefault();
      setActiveHostIndex(connections.length - 1);
      return;
    }
    if ((event.key === "Enter" || event.key === " ") && hostMenuOpen) {
      event.preventDefault();
      selectHost(activeHostIndex);
      return;
    }
    if (event.key === "Enter" || event.key === " ") {
      event.preventDefault();
      openHostMenu();
    }
  };

  return (
    <>
      <div className="create-workspace-remote-form">
        <div className="create-workspace-field">
          <span id={`${hostSelectId}-label`}>{t("workspaceCreation.remoteHost")}</span>
          <div className="create-workspace-host-select" ref={hostSelectRef}>
            <button
              ref={hostTriggerRef}
              type="button"
              className={`create-workspace-host-trigger${hostMenuOpen ? " open" : ""}`}
              aria-labelledby={`${hostSelectId}-label`}
              aria-haspopup="listbox"
              aria-expanded={hostMenuOpen}
              aria-controls={`${hostSelectId}-listbox`}
              aria-activedescendant={hostMenuOpen ? `${hostSelectId}-option-${activeHostIndex}` : undefined}
              disabled={!connections.length}
              onClick={() => hostMenuOpen ? setHostMenuOpen(false) : openHostMenu()}
              onKeyDown={handleHostKeyDown}
            >
              <Server className="create-workspace-host-trigger-icon" size={16} />
              <span className="create-workspace-host-trigger-label">
                {selectedConnection?.displayName ?? t("workspaceCreation.noConnections")}
              </span>
              <ChevronDown className="create-workspace-host-chevron" size={16} />
            </button>
            {hostMenuOpen && (
              <div
                id={`${hostSelectId}-listbox`}
                className="create-workspace-host-menu"
                role="listbox"
                aria-labelledby={`${hostSelectId}-label`}
              >
                {connections.map((connection, index) => {
                  const selected = connection.id === connectionId;
                  const active = index === activeHostIndex;
                  return (
                    <button
                      id={`${hostSelectId}-option-${index}`}
                      key={connection.id}
                      type="button"
                      role="option"
                      aria-selected={selected}
                      className={`create-workspace-host-option${selected ? " selected" : ""}${active ? " active" : ""}`}
                      onPointerEnter={() => setActiveHostIndex(index)}
                      onClick={() => selectHost(index)}
                    >
                      <span className="create-workspace-host-option-icon"><Server size={14} /></span>
                      <span>{connection.displayName}</span>
                      {selected && <Check className="create-workspace-host-option-check" size={15} />}
                    </button>
                  );
                })}
              </div>
            )}
          </div>
        </div>
        <label className="create-workspace-field">
          <span>{t("workspaceCreation.projectName")}</span>
          <input
            value={projectName}
            placeholder={t("workspaceCreation.projectNamePlaceholder")}
            onChange={(event) => setProjectName(event.target.value)}
          />
        </label>
        <div className="create-workspace-field">
          <span>{t("workspaceCreation.remoteDirectory")}</span>
          <div className="create-workspace-path-row">
            <input
              value={pathInput}
              onChange={(event) => setPathInput(event.target.value)}
              onKeyDown={(event) => { if (event.key === "Enter") void loadPath(pathInput); }}
              placeholder="/home/user"
            />
            <button type="button" title={t("workspaceCreation.home")} onClick={() => homePath && void loadPath(homePath)} disabled={!homePath || loadingPath}><Home size={15} /></button>
            <button type="button" title={t("workspaceCreation.parent")} onClick={() => currentPath && void loadPath(currentPath, true)} disabled={!currentPath || currentPath === "/" || loadingPath}><ArrowLeft size={15} /></button>
            <button type="button" title={t("workspaceCreation.refresh")} onClick={() => currentPath && void loadPath(currentPath)} disabled={!currentPath || loadingPath}><RefreshCw size={15} /></button>
          </div>
        </div>
      </div>

      <div className="create-workspace-directory-panel">
        <div className="create-workspace-directory-header">
          <span><Folder size={15} /> {currentPath || t("workspaceCreation.directoryPlaceholder")}</span>
          {loadingPath && <LoaderCircle className="create-workspace-spin" size={15} />}
        </div>
        <div className="create-workspace-directory-list">
          {directories.length === 0 && !loadingPath ? (
            <div className="create-workspace-directory-empty">{error ? t("workspaceCreation.directoryUnavailable") : t("workspaceCreation.noDirectories")}</div>
          ) : directories.map((directory) => (
            <button
              type="button"
              key={directory.path}
              className={`create-workspace-directory-row${selectedDirectory === directory.path ? " selected" : ""}`}
              disabled={!directory.enterable}
              onClick={() => {
                setSelectedDirectory(directory.path);
                setPathInput(directory.path);
                // 历史逻辑（已停用）：if (!projectNameEdited) setProjectName(directory.name);
                // 选择目录后，目录末级名称覆盖此前手工输入的项目名称。
                setProjectName(projectNameFromRemoteDirectory(directory.path));
              }}
              onDoubleClick={() => directory.enterable && void loadPath(directory.path)}
            >
              <Folder size={15} />
              <span>{directory.name}</span>
              {!directory.enterable && <small>{t("workspaceCreation.notEnterable")}</small>}
              <ChevronRight size={14} />
            </button>
          ))}
        </div>
      </div>

      {error && <div className="create-workspace-error">{error}</div>}
      {!connections.length && (
        <button type="button" className="create-workspace-settings-link" onClick={onOpenConnections}>{t("workspaceCreation.openConnections")}</button>
      )}
      <footer className="create-workspace-footer">
        <span className="create-workspace-step">{selectedConnection?.displayName ?? t("workspaceCreation.noConnections")}</span>
        <button type="button" className="create-workspace-primary" onClick={() => void onCreate()} disabled={!canCreate || creating}>
          {creating ? <LoaderCircle className="create-workspace-spin" size={15} /> : <span className="create-workspace-plus">+</span>}
          {t("workspaceCreation.create")}
        </button>
      </footer>
    </>
  );
}

type CreationStep = "type" | "remote";
const LAST_REMOTE_CONNECTION_KEY = "auracoder.workspace.lastRemoteConnectionId";

function directoryName(path: string): string {
  const normalized = path.trim().replace(/\/+$/, "") || "/";
  if (normalized === "/") return "/";
  return normalized.slice(normalized.lastIndexOf("/") + 1) || "/";
}

/**
 * 远端目录是项目名称的业务来源；目录发生变化时，必须覆盖此前手工输入的名称。
 */
export function projectNameFromRemoteDirectory(path: string): string {
  return directoryName(path);
}

export function CreateWorkspaceModal({
  open,
  onClose,
  onSelectLocal,
  onCreateRemote,
  onCreated,
  onOpenConnections,
}: CreateWorkspaceModalProps) {
  const { t } = useTranslation("app");
  const connections = useSshConnectionStore((state) => state.connections);
  const refreshConnections = useSshConnectionStore((state) => state.refresh);
  const enabledConnections = useMemo(
    () => connections.filter(
      (connection) => connection.enabled && !connection.deletedAt && connection.connectionStatus === "ok",
    ),
    [connections],
  );
  const [step, setStep] = useState<CreationStep>("type");
  const [projectType, setProjectType] = useState<"local" | "ssh">("local");
  const [connectionId, setConnectionId] = useState("");
  const [homePath, setHomePath] = useState("");
  const [currentPath, setCurrentPath] = useState("");
  const [pathInput, setPathInput] = useState("");
  const [directories, setDirectories] = useState<SshRemoteDirectory[]>([]);
  const [selectedDirectory, setSelectedDirectory] = useState<string | null>(null);
  const [projectName, setProjectName] = useState("");
  const [nameEdited, setNameEdited] = useState(false);
  const [loadingPath, setLoadingPath] = useState(false);
  const [creating, setCreating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const requestSeq = useRef(0);

  useEffect(() => {
    if (!open) return;
    setStep("type");
    setProjectType("local");
    setConnectionId("");
    setHomePath("");
    setCurrentPath("");
    setPathInput("");
    setDirectories([]);
    setSelectedDirectory(null);
    setProjectName("");
    setNameEdited(false);
    setError(null);
    void refreshConnections();
  }, [open, refreshConnections]);

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [open, onClose]);

  useEffect(() => {
    if (step !== "remote" || !connectionId) return;
    const request = ++requestSeq.current;
    setLoadingPath(true);
    setError(null);
    setDirectories([]);
    setSelectedDirectory(null);
    void ipc.getSshConnectionHome(connectionId)
      .then((test) => {
        if (request !== requestSeq.current) return;
        const home = test.home?.trim();
        if (!home || !home.startsWith("/")) {
          throw new Error(t("workspaceCreation.remoteHomeMissing"));
        }
        setHomePath(home);
        return ipc.listSshDirectories(connectionId, home).then((items) => ({ home, items }));
      })
      .then((result) => {
        if (!result || request !== requestSeq.current) return;
        setCurrentPath(result.home);
        setPathInput(result.home);
        setDirectories(result.items);
        setSelectedDirectory(null);
        if (!nameEdited) setProjectName("");
      })
      .catch((loadError) => {
        if (request !== requestSeq.current) return;
        setError(String(loadError));
        setCurrentPath("");
        setPathInput("");
        setDirectories([]);
        setSelectedDirectory(null);
      })
      .finally(() => {
        if (request === requestSeq.current) setLoadingPath(false);
      });
  }, [connectionId, step, t]);

  useEffect(() => {
    if (step !== "remote" || connectionId || !enabledConnections.length) return;
    let savedConnectionId = "";
    try {
      savedConnectionId = window.localStorage.getItem(LAST_REMOTE_CONNECTION_KEY) ?? "";
    } catch {
      savedConnectionId = "";
    }
    const nextConnectionId = enabledConnections.some((connection) => connection.id === savedConnectionId)
      ? savedConnectionId
      : enabledConnections[0].id;
    setConnectionId(nextConnectionId);
    try {
      window.localStorage.setItem(LAST_REMOTE_CONNECTION_KEY, nextConnectionId);
    } catch {
      // Ignore storage failures; the current selection remains usable.
    }
  }, [connectionId, enabledConnections, step]);

  if (!open || typeof document === "undefined") return null;

  function selectType(type: "local" | "ssh") {
    setProjectType(type);
  }

  function continueFromType() {
    if (projectType === "local") {
      onClose();
      onSelectLocal();
      return;
    }
    if (!enabledConnections.length) {
      setConnectionId("");
      setStep("remote");
      setError(t("workspaceCreation.noConnections"));
      return;
    }
    let savedConnectionId = "";
    try {
      savedConnectionId = window.localStorage.getItem(LAST_REMOTE_CONNECTION_KEY) ?? "";
    } catch {
      savedConnectionId = "";
    }
    const preferredConnectionId = connectionId || savedConnectionId;
    const nextConnectionId = enabledConnections.some((connection) => connection.id === preferredConnectionId)
      ? preferredConnectionId
      : enabledConnections[0].id;
    try {
      window.localStorage.setItem(LAST_REMOTE_CONNECTION_KEY, nextConnectionId);
    } catch {
      // Ignore storage failures; the current selection remains usable.
    }
    setConnectionId(nextConnectionId);
    setStep("remote");
  }

  async function loadPath(nextPath: string, parent = false) {
    const normalizedPath = nextPath.trim();
    if (!normalizedPath || !connectionId) return;
    const request = ++requestSeq.current;
    setLoadingPath(true);
    setError(null);
    try {
      const resolved = await ipc.resolveSshDirectory(connectionId, normalizedPath, parent);
      if (request !== requestSeq.current) return;
      const items = await ipc.listSshDirectories(connectionId, resolved.path);
      if (request !== requestSeq.current) return;
      setCurrentPath(resolved.path);
      setPathInput(resolved.path);
      setDirectories(items);
      setSelectedDirectory(resolved.path);
      // 历史逻辑（已停用）：if (!nameEdited) setProjectName(resolved.name || directoryName(resolved.path));
      // 进入或切换目录后，目录末级名称覆盖此前手工输入的项目名称。
      setProjectName(projectNameFromRemoteDirectory(resolved.path));
    } catch (loadError) {
      if (request === requestSeq.current) setError(String(loadError));
    } finally {
      if (request === requestSeq.current) setLoadingPath(false);
    }
  }

  async function createRemoteProject() {
    const targetPath = selectedDirectory;
    if (!connectionId || !targetPath || !projectName.trim() || creating) return;
    setCreating(true);
    setError(null);
    try {
      const workspace = await onCreateRemote(
        connectionId,
        projectName.trim(),
        targetPath,
      );
      if (!workspace) throw new Error(t("workspaceCreation.createFailed"));
      onCreated(workspace);
      onClose();
    } catch (createError) {
      setError(String(createError));
    } finally {
      setCreating(false);
    }
  }

  const selectedConnection = enabledConnections.find((connection) => connection.id === connectionId);
  const canCreateRemote = Boolean(selectedConnection && selectedDirectory && projectName.trim() && !loadingPath);

  return createPortal(
    <div className="create-workspace-backdrop" onMouseDown={onClose}>
      <section
        className={`create-workspace-modal${step === "remote" ? " create-workspace-modal-remote" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby="create-workspace-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="create-workspace-header">
          <div>
            {step === "remote" && (
              <button type="button" className="create-workspace-back" onClick={() => setStep("type")}>
                <ArrowLeft size={15} />
                {t("workspaceCreation.back")}
              </button>
            )}
            <h2 id="create-workspace-title">
              {step === "remote" ? t("workspaceCreation.remoteTitle") : t("workspaceCreation.title")}
            </h2>
            <p>{step === "remote" ? t("workspaceCreation.remoteDescription") : t("workspaceCreation.description")}</p>
          </div>
          <button type="button" className="create-workspace-close" onClick={onClose} aria-label={t("common:actions.close")}>
            <X size={17} />
          </button>
        </header>

        {step === "type" ? (
          <>
            <div className="create-workspace-type-grid">
              <button
                type="button"
                className={`create-workspace-type-card${projectType === "local" ? " selected" : ""}`}
                onClick={() => selectType("local")}
                onDoubleClick={continueFromType}
              >
                <span className="create-workspace-type-icon"><Monitor size={21} /></span>
                <span className="create-workspace-type-content">
                  <strong>{t("workspaceCreation.localTitle")}</strong>
                  <span>{t("workspaceCreation.localDescription")}</span>
                </span>
                <span className="create-workspace-radio" aria-hidden="true" />
              </button>
              <button
                type="button"
                className={`create-workspace-type-card${projectType === "ssh" ? " selected" : ""}`}
                onClick={() => selectType("ssh")}
                onDoubleClick={continueFromType}
              >
                <span className="create-workspace-type-icon remote"><Globe2 size={21} /></span>
                <span className="create-workspace-type-content">
                  <strong>{t("workspaceCreation.remoteTypeTitle")}</strong>
                  <span>{t("workspaceCreation.remoteTypeDescription")}</span>
                </span>
                <span className="create-workspace-radio" aria-hidden="true" />
              </button>
            </div>
            {error && <div className="create-workspace-error">{error}</div>}
            <footer className="create-workspace-footer">
              <span className="create-workspace-step">{t("workspaceCreation.step", { current: 1, total: 2 })}</span>
              <button type="button" className="create-workspace-primary" onClick={continueFromType}>
                {t("workspaceCreation.next")}
                <ChevronRight size={15} />
              </button>
            </footer>
          </>
        ) : (
          <>
            {/* 历史传值（已停用）：projectNameEdited={nameEdited} */}
            <RemoteWorkspaceForm
              connections={enabledConnections}
              connectionId={connectionId}
              setConnectionId={(value) => {
                setNameEdited(false);
                setConnectionId(value);
                try {
                  window.localStorage.setItem(LAST_REMOTE_CONNECTION_KEY, value);
                } catch {
                  // Ignore storage failures; the current selection remains usable.
                }
              }}
              projectName={projectName}
              setProjectName={(value) => { setNameEdited(true); setProjectName(value); }}
              pathInput={pathInput}
              setPathInput={setPathInput}
              currentPath={currentPath}
              homePath={homePath}
              directories={directories}
              selectedDirectory={selectedDirectory}
              setSelectedDirectory={setSelectedDirectory}
              loadingPath={loadingPath}
              error={error}
              loadPath={loadPath}
              onCreate={createRemoteProject}
              canCreate={canCreateRemote}
              creating={creating}
              onOpenConnections={onOpenConnections}
              t={t}
            />
          </>
        )}
      </section>
    </div>,
    document.body,
  );
}
