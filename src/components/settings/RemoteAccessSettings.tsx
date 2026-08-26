import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { CheckCircle2, Clipboard, Plus, RefreshCw, ShieldCheck, Smartphone, Wifi, WifiOff } from "lucide-react";
import { useTranslation } from "react-i18next";
import { ipc } from "../../lib/ipc";
import { toast } from "../../stores/toastStore";
import type { RemoteAccessStatus } from "../../types";

export function RemoteAccessSettings() {
  const { t } = useTranslation(["app", "common"]);
  const [status, setStatus] = useState<RemoteAccessStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  // “重置全部身份”不属于普通设备配对流程，设备管理只撤销单台设备。
  // const [rotating, setRotating] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [refreshingDevices, setRefreshingDevices] = useState(false);
  const [pairingOpen, setPairingOpen] = useState(false);
  const [revokingDeviceId, setRevokingDeviceId] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    void ipc.getRemoteAccessStatus()
      .then((nextStatus) => {
        if (!disposed) setStatus(nextStatus);
      })
      .catch((error) => {
        if (!disposed) toast.error(t("app:settingsPage.remoteAccess.loadFailed", { error: String(error) }));
      })
      .finally(() => {
        if (!disposed) setLoading(false);
      });

    void listen<RemoteAccessStatus>("remote-access-updated", (event) => {
      if (!disposed) setStatus(event.payload);
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    });

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [t]);

  useEffect(() => {
    if (pairingOpen && !refreshing && status && !status.pairingPayload) {
      setPairingOpen(false);
    }
  }, [pairingOpen, refreshing, status]);

  const refreshPairing = (openPanel: boolean) => {
    setRefreshing(true);
    void ipc.refreshRemotePairingToken()
      .then((nextStatus) => {
        setStatus(nextStatus);
        if (openPanel && nextStatus.pairingPayload) setPairingOpen(true);
        toast.success(t("app:settingsPage.remoteAccess.refreshed"));
      })
      .catch((error) => toast.error(t("app:settingsPage.remoteAccess.refreshFailed", { error: String(error) })))
      .finally(() => setRefreshing(false));
  };

  return (
    <>
      <section className="usp-section usp-section-first">
        <div className="usp-section-header">
          <h2>{t("app:settingsPage.remoteAccess.serviceTitle")}</h2>
          <p>{t("app:settingsPage.remoteAccess.serviceDescription")}</p>
        </div>
        <div className="usp-group">
          <div className="usp-row">
            <span className="usp-row-icon">
              {status?.connected ? <Wifi size={17} /> : <WifiOff size={17} />}
            </span>
            <span className="usp-row-copy">
              <span className="usp-row-title">{t("app:settingsPage.remoteAccess.connectionTitle")}</span>
              <span className="usp-row-description">
                {loading
                  ? t("app:settingsPage.remoteAccess.loading")
                  : !status?.enabled
                    ? t("app:settingsPage.remoteAccess.disabled")
                    : status.connected
                      ? status.peerOnline
                        ? t("app:settingsPage.remoteAccess.phoneOnline")
                        : t("app:settingsPage.remoteAccess.waitingForPhone")
                      : t("app:settingsPage.remoteAccess.connecting")}
              </span>
              {status?.lastError ? <span className="remote-access-error">{status.lastError}</span> : null}
            </span>
            <div className="usp-row-control">
              <span className={`usp-status${status?.connected ? " usp-status-ready" : ""}`}>
                {status?.connected
                  ? t("app:settingsPage.remoteAccess.connected")
                  : t("app:settingsPage.remoteAccess.offline")}
              </span>
            </div>
          </div>

          <div className="usp-row">
            <span className="usp-row-icon"><ShieldCheck size={17} /></span>
            <span className="usp-row-copy">
              <span className="usp-row-title">{t("app:settingsPage.remoteAccess.relayTitle")}</span>
              <span className="usp-row-description remote-access-mono">
                {status?.endpoint || "wss://auracoder.jxrjkf.cn/ws/tunnel"}
              </span>
            </span>
          </div>
        </div>
      </section>

      <section className="usp-section">
          <div className="usp-section-header usp-section-header-action">
            <div>
              <h2>{t("app:settingsPage.remoteAccess.devicesTitle")}</h2>
              <p>{t("app:settingsPage.remoteAccess.devicesDescription")}</p>
            </div>
            <div className="remote-access-header-actions">
              <button
                type="button"
                className="usp-icon-button"
                title={t("app:settingsPage.remoteAccess.refreshDevices")}
                disabled={loading || refreshingDevices}
                onClick={() => {
                  setRefreshingDevices(true);
                  void ipc.getRemoteAccessStatus()
                    .then(setStatus)
                    .catch((error) => toast.error(t("app:settingsPage.remoteAccess.loadFailed", { error: String(error) })))
                    .finally(() => setRefreshingDevices(false));
                }}
              >
                <RefreshCw size={13} className={refreshingDevices ? "usp-spin" : undefined} />
              </button>
              <button
                type="button"
                className="usp-button"
                disabled={loading || refreshing || !status?.enabled}
                onClick={() => refreshPairing(true)}
              >
                {refreshing ? <RefreshCw size={13} className="usp-spin" /> : <Plus size={13} />}
                {t("app:settingsPage.remoteAccess.addDevice")}
              </button>
            </div>
          </div>
          <div className="usp-group">
            <div className="usp-row">
              <span className="usp-row-copy">
                <span className="usp-row-title">{t("app:settingsPage.remoteAccess.allowConnections")}</span>
              </span>
              <div className="usp-row-control">
                <label className="ws-toggle" title={t("app:settingsPage.remoteAccess.allowConnections")}>
                  <input
                    type="checkbox"
                    checked={status?.enabled ?? false}
                    disabled={loading || saving}
                    aria-label={t("app:settingsPage.remoteAccess.allowConnections")}
                    onChange={(event) => {
                      const enabled = event.target.checked;
                      setSaving(true);
                      void ipc.setRemoteAccessEnabled(enabled)
                        .then((nextStatus) => {
                          setStatus(nextStatus);
                          if (!enabled) setPairingOpen(false);
                          toast.success(t(enabled
                            ? "app:settingsPage.remoteAccess.enabledToast"
                            : "app:settingsPage.remoteAccess.disabledToast"));
                        })
                        .catch((error) => toast.error(t("app:settingsPage.remoteAccess.saveFailed", { error: String(error) })))
                        .finally(() => setSaving(false));
                    }}
                  />
                  <span className="ws-toggle-track" />
                  <span className="ws-toggle-thumb" />
                </label>
              </div>
            </div>
            {loading ? (
              <div className="usp-usage-empty">{t("app:settingsPage.remoteAccess.loading")}</div>
            ) : (status?.devices.length ?? 0) === 0 ? (
              <div className="usp-usage-empty">{t("app:settingsPage.remoteAccess.devicesEmpty")}</div>
            ) : status?.devices.map((device) => (
              <div className="usp-row" key={device.id}>
                <span className="usp-row-icon"><Smartphone size={17} /></span>
                <span className="usp-row-copy">
                  <span className="usp-row-title">{device.name}</span>
                  <span className="usp-row-description">
                    {device.lastConnectedAt
                      ? t("app:settingsPage.remoteAccess.lastConnected", {
                          time: new Intl.DateTimeFormat(undefined, {
                            dateStyle: "medium",
                            timeStyle: "short",
                          }).format(new Date(device.lastConnectedAt)),
                        })
                      : t("app:settingsPage.remoteAccess.connectionTimeUnknown")}
                  </span>
                </span>
                <div className="usp-row-control">
                  <button
                    type="button"
                    className="usp-button remote-access-revoke-button"
                    disabled={revokingDeviceId !== null}
                    onClick={() => {
                      if (!window.confirm(t("app:settingsPage.remoteAccess.revokeDeviceConfirm", { name: device.name }))) return;
                      setRevokingDeviceId(device.id);
                      void ipc.revokeRemoteDevice(device.id)
                        .then((nextStatus) => {
                          setStatus(nextStatus);
                          toast.success(t("app:settingsPage.remoteAccess.deviceRevoked", { name: device.name }));
                        })
                        .catch((error) => toast.error(t("app:settingsPage.remoteAccess.revokeDeviceFailed", { error: String(error) })))
                        .finally(() => setRevokingDeviceId(null));
                    }}
                  >
                    {revokingDeviceId === device.id ? <RefreshCw size={13} className="usp-spin" /> : null}
                    {t("app:settingsPage.remoteAccess.revokeDevice")}
                  </button>
                </div>
              </div>
            ))}
          </div>
        </section>

      {pairingOpen && status?.enabled && status.pairingQrSvg && status.pairingPayload ? (
        <section className="usp-section">
          <div className="usp-section-header">
            <h2>{t("app:settingsPage.remoteAccess.pairTitle")}</h2>
            <p>{t("app:settingsPage.remoteAccess.pairDescription")}</p>
          </div>
          <div className="remote-access-pairing-card">
            <div className="remote-access-qr-wrap">
              <img
                className="remote-access-qr"
                src={`data:image/svg+xml;charset=utf-8,${encodeURIComponent(status.pairingQrSvg)}`}
                alt={t("app:settingsPage.remoteAccess.qrAlt")}
              />
            </div>
            <div className="remote-access-pairing-copy">
              <div>
                <span className="remote-access-label">{t("app:settingsPage.remoteAccess.deviceId")}</span>
                <code>{status.tunnelId}</code>
              </div>
              <p>
                {status.pairingExpiresAt
                  ? t("app:settingsPage.remoteAccess.expiresAt", {
                      time: new Intl.DateTimeFormat(undefined, { timeStyle: "short" }).format(new Date(status.pairingExpiresAt)),
                    })
                  : t("app:settingsPage.remoteAccess.securityNote")}
              </p>
              <div className="remote-access-actions">
                <button
                  type="button"
                  className="usp-button"
                  onClick={() => {
                    void navigator.clipboard.writeText(status.pairingPayload ?? "")
                      .then(() => toast.success(t("app:settingsPage.remoteAccess.copied")))
                      .catch((error) => toast.error(t("app:settingsPage.remoteAccess.copyFailed", { error: String(error) })));
                  }}
                >
                  <Clipboard size={13} />
                  {t("app:settingsPage.remoteAccess.copyPairing")}
                </button>
                <button
                  type="button"
                  className="usp-button"
                  disabled={refreshing}
                  onClick={() => refreshPairing(false)}
                >
                  {refreshing ? <RefreshCw size={13} className="usp-spin" /> : <RefreshCw size={13} />}
                  {t("app:settingsPage.remoteAccess.refresh")}
                </button>
              </div>
              <span className="remote-access-secure">
                <CheckCircle2 size={14} />
                {t("app:settingsPage.remoteAccess.tlsProtected")}
              </span>
            </div>
          </div>
        </section>
      ) : null}
    </>
  );
}
