import { useEffect, useState } from "react";
import {
  CircleAlert,
  Globe2,
  Laptop,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  RotateCcw,
  Trash2,
  Wifi,
  X,
} from "lucide-react";
import { useTranslation } from "react-i18next";
import { useSshConnectionStore } from "../../stores/sshConnectionStore";
import type { SshConnection, SshConnectionInput } from "../../types";
import "./SshConnectionsSettings.css";

const emptyForm: SshConnectionInput = {
  displayName: "",
  hostName: "",
  user: "",
  port: 22,
  identityFile: null,
  hostKey: "",
  configAlias: null,
};

export function SshConnectionsSettings() {
  const { t } = useTranslation(["app"]);
  const {
    connections,
    deletedConnections,
    scanResults,
    tests,
    loading,
    scanning,
    error,
    refresh,
    scan,
    importHosts,
    createManual,
    update,
    test,
    setEnabled,
    remove,
    restore,
  } = useSshConnectionStore();
  const [modal, setModal] = useState<"none" | "add" | "manual" | "edit">("none");
  const [form, setForm] = useState<SshConnectionInput>(emptyForm);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [selected, setSelected] = useState<string[]>([]);
  const [saving, setSaving] = useState(false);
  const [expandedDeleted, setExpandedDeleted] = useState(false);

  useEffect(() => {
    void refresh();
    const refreshTimer = window.setInterval(() => {
      void refresh(true);
    }, 1_000);
    return () => window.clearInterval(refreshTimer);
  }, [refresh]);

  const openAdd = async () => {
    setModal("add");
    setSelected([]);
    await scan();
  };

  const openManual = () => {
    setForm({ ...emptyForm });
    setEditingId(null);
    setModal("manual");
  };

  const openEdit = (connection: SshConnection) => {
    setEditingId(connection.id);
    setForm({
      displayName: connection.displayName,
      hostName: connection.hostName,
      user: connection.user,
      port: connection.port,
      identityFile: connection.identityFile,
      hostKey: "",
      configAlias: connection.configAlias,
    });
    setModal("edit");
  };

  const submit = async () => {
    setSaving(true);
    try {
      if (modal === "edit" && editingId) {
        await update(editingId, form);
      } else {
        await createManual({ ...form, configAlias: null });
      }
      setModal("none");
    } catch (submitError) {
      window.alert(String(submitError));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="ssh-settings-section">
      <div className="ssh-settings-header">
        <div>
          <h2>{t("app:settingsPage.sshConnections.listTitle")}</h2>
        </div>
        <button type="button" className="usp-button usp-button-primary ssh-add-button" onClick={() => void openAdd()}>
          <Plus size={14} />
          {t("app:settingsPage.sshConnections.add")}
        </button>
      </div>

      {error ? (
        <div className="ssh-inline-error">
          <CircleAlert size={15} />
          <span>{error}</span>
        </div>
      ) : null}

      <section className="ssh-connections-card" aria-label={t("app:settingsPage.sshConnections.title")}>
        {loading ? (
          <div className="ssh-empty-state">
            <Loader2 size={16} className="ssh-spin" />
            {t("app:settingsPage.sshConnections.loading")}
          </div>
        ) : connections.length === 0 ? (
          <div className="ssh-empty-state">{t("app:settingsPage.sshConnections.empty")}</div>
        ) : (
          connections.map((connection) => {
            const testResult = tests[connection.id];
            const connected = connection.enabled && connection.connectionStatus === "ok";
            const connecting = connection.enabled && connection.connectionStatus === "connecting";
            const operatingSystem = connected ? (testResult?.os ?? (connection.lastConnectedAt ? "Linux" : null)) : null;
            const ConnectionIcon = connection.sourceKind === "ssh_config" ? Globe2 : Laptop;
            return (
              <div className="ssh-connection-row" key={connection.id}>
                <button
                  type="button"
                  className={`ssh-toggle ${connection.enabled ? "is-on" : ""}`}
                  aria-label={t("app:settingsPage.sshConnections.toggle")}
                  aria-pressed={connection.enabled}
                  onClick={() => {
                    if (!connection.enabled || window.confirm(t("app:settingsPage.sshConnections.disableConfirm"))) {
                      void setEnabled(connection.id, !connection.enabled);
                    }
                  }}
                >
                  <span />
                </button>
                <div className="ssh-connection-icon" aria-hidden="true">
                  <ConnectionIcon size={17} />
                </div>
                <div className="ssh-connection-main">
                  <strong>{connection.displayName}</strong>
                  <small className={connection.lastError ? "ssh-status-error" : connected ? "ssh-status-ok" : "ssh-status-muted"}>
                    <i aria-hidden="true" />
                    {connection.lastError
                      ? connection.lastError
                      : connected
                        ? `${t("app:settingsPage.sshConnections.connected")}${operatingSystem ? ` · ${operatingSystem}` : ""}`
                        : connecting
                          ? t("app:settingsPage.sshConnections.connecting")
                        : t("app:settingsPage.sshConnections.notTested")}
                  </small>
                </div>
                <div className="ssh-connection-actions">
                  <button type="button" className="usp-icon-button" title={t("app:settingsPage.sshConnections.test")} onClick={() => void test(connection.id)}>
                    <Wifi size={15} />
                  </button>
                  <button type="button" className="usp-icon-button" title={t("app:settingsPage.sshConnections.edit")} onClick={() => openEdit(connection)}>
                    <Pencil size={15} />
                  </button>
                  <button
                    type="button"
                    className="usp-icon-button ssh-delete-button"
                    title={t("app:settingsPage.sshConnections.delete")}
                    onClick={() => {
                      if (window.confirm(t("app:settingsPage.sshConnections.deleteConfirm"))) {
                        void remove(connection.id);
                      }
                    }}
                  >
                    <Trash2 size={15} />
                  </button>
                </div>
              </div>
            );
          })
        )}
      </section>

      {deletedConnections.length ? (
        <section className="ssh-deleted-section">
          <button type="button" className="ssh-deleted-toggle" onClick={() => setExpandedDeleted((value) => !value)}>
            <RotateCcw size={14} />
            {t("app:settingsPage.sshConnections.deleted", { count: deletedConnections.length })}
          </button>
          {expandedDeleted ? (
            <div className="ssh-connections-card ssh-deleted-card">
              {deletedConnections.map((connection) => (
                <div className="ssh-connection-row" key={connection.id}>
                  <div className="ssh-connection-icon ssh-deleted-icon" aria-hidden="true">
                    <Trash2 size={16} />
                  </div>
                  <div className="ssh-connection-main">
                    <strong>{connection.displayName}</strong>
                    <small className="ssh-status-muted">{t("app:settingsPage.sshConnections.deletedNote")}</small>
                  </div>
                  <button type="button" className="usp-button" onClick={() => void restore(connection.id)}>
                    <RotateCcw size={13} />
                    {t("app:settingsPage.sshConnections.restore")}
                  </button>
                </div>
              ))}
            </div>
          ) : null}
        </section>
      ) : null}

      {modal === "add" ? (
        <div className="ssh-modal-backdrop" role="presentation">
          <div className="ssh-modal ssh-import-modal" role="dialog" aria-modal="true" aria-labelledby="ssh-import-title">
            <div className="ssh-modal-header">
              <div>
                <h3 id="ssh-import-title">{t("app:settingsPage.sshConnections.addTitle")}</h3>
              </div>
              <button type="button" className="ssh-modal-close" aria-label={t("app:settingsPage.sshConnections.close")} onClick={() => setModal("none")}>
                <X size={17} />
              </button>
            </div>
            <div className="ssh-scan-list">
              {scanning ? (
                <div className="ssh-empty-state">
                  <Loader2 size={16} className="ssh-spin" />
                  {t("app:settingsPage.sshConnections.scanning")}
                </div>
              ) : scanResults.length ? (
                scanResults.map((host) => (
                  <label className="ssh-scan-item" key={host.alias}>
                    <span className="ssh-scan-icon" aria-hidden="true">
                      <Laptop size={17} />
                    </span>
                    <span className="ssh-scan-copy">
                      <strong>{host.alias}</strong>
                      <small>
                        {host.hostName}
                        {host.user ? ` · ${host.user}` : ""}
                        {host.port !== 22 ? `:${host.port}` : ""}
                        {host.imported ? ` · ${t("app:settingsPage.sshConnections.imported")}` : host.deleted ? ` · ${t("app:settingsPage.sshConnections.deletedMark")}` : ""}
                      </small>
                    </span>
                    <input
                      type="checkbox"
                      checked={selected.includes(host.alias)}
                      disabled={host.imported}
                      onChange={(event) =>
                        setSelected((current) =>
                          event.target.checked ? [...current, host.alias] : current.filter((value) => value !== host.alias),
                        )
                      }
                    />
                  </label>
                ))
              ) : (
                <div className="ssh-empty-state">{t("app:settingsPage.sshConnections.noConfigHosts")}</div>
              )}
            </div>
            <div className="ssh-modal-footer">
              <button type="button" className="usp-button ssh-manual-button" onClick={openManual}>
                <Plus size={13} />
                {t("app:settingsPage.sshConnections.manual")}
              </button>
              <div className="ssh-modal-footer-actions">
                <button type="button" className="usp-button" onClick={() => void scan()}>
                  <RefreshCw size={13} />
                  {t("app:settingsPage.sshConnections.rescan")}
                </button>
                <button
                  type="button"
                  className="usp-button usp-button-primary"
                  disabled={!selected.length || scanning}
                  onClick={() => void importHosts(selected)}
                >
                  {t("app:settingsPage.sshConnections.import", { count: selected.length })}
                </button>
              </div>
            </div>
          </div>
        </div>
      ) : null}

      {modal === "manual" || modal === "edit" ? (
        <div className="ssh-modal-backdrop" role="presentation">
          <div className="ssh-modal ssh-form-modal" role="dialog" aria-modal="true" aria-labelledby="ssh-form-title">
            <div className="ssh-modal-header">
              <div>
                <h3 id="ssh-form-title">
                  {modal === "edit" ? t("app:settingsPage.sshConnections.editTitle") : t("app:settingsPage.sshConnections.manualTitle")}
                </h3>
                <p>{t("app:settingsPage.sshConnections.formDescription")}</p>
              </div>
              <button type="button" className="ssh-modal-close" aria-label={t("app:settingsPage.sshConnections.close")} onClick={() => setModal("none")}>
                <X size={17} />
              </button>
            </div>
            <div className="ssh-form-fields">
              <label>
                <span>{t("app:settingsPage.sshConnections.displayName")}</span>
                <input value={form.displayName} onChange={(event) => setForm({ ...form, displayName: event.target.value })} />
              </label>
              <label>
                <span>{t("app:settingsPage.sshConnections.host")}</span>
                <input value={form.hostName} onChange={(event) => setForm({ ...form, hostName: event.target.value })} />
              </label>
              <div className="ssh-form-grid">
                <label>
                  <span>{t("app:settingsPage.sshConnections.user")}</span>
                  <input value={form.user} onChange={(event) => setForm({ ...form, user: event.target.value })} />
                </label>
                <label>
                  <span>{t("app:settingsPage.sshConnections.port")}</span>
                  <input type="number" min={1} max={65535} value={form.port} onChange={(event) => setForm({ ...form, port: Number(event.target.value) })} />
                </label>
              </div>
              <label>
                <span>{t("app:settingsPage.sshConnections.identityFile")}</span>
                <input value={form.identityFile ?? ""} placeholder="~/.ssh/id_ed25519" onChange={(event) => setForm({ ...form, identityFile: event.target.value || null })} />
              </label>
              <label>
                <span>{t("app:settingsPage.sshConnections.hostKey")}</span>
                <textarea rows={3} value={form.hostKey} placeholder={modal === "edit" ? t("app:settingsPage.sshConnections.hostKeyKeep") : "ssh-ed25519 AAAA..."} onChange={(event) => setForm({ ...form, hostKey: event.target.value })} />
              </label>
            </div>
            <div className="ssh-modal-footer">
              <span>{t("app:settingsPage.sshConnections.hostKeyNote")}</span>
              <div className="ssh-modal-footer-actions">
                <button type="button" className="usp-button" onClick={() => setModal("none")}>{t("app:settingsPage.sshConnections.cancel")}</button>
                <button type="button" className="usp-button usp-button-primary" disabled={saving} onClick={() => void submit()}>
                  {saving ? t("app:settingsPage.sshConnections.saving") : t("app:settingsPage.sshConnections.save")}
                </button>
              </div>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}
