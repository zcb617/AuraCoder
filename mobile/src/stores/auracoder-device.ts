import { computed, ref } from "vue";
import type { MobileAuraCoderSettings, PairedAuraCoder, PairingConfig } from "../types";

const SETTINGS_STORAGE_KEY = "auracoder-mobile:settings:v2";
const LEGACY_PAIRING_STORAGE_KEY = "auracoder-mobile:pairing:v1";
const devices = ref<PairedAuraCoder[]>([]);
const activeAuraCoderId = ref<string | null>(null);
let initialized = false;

/** 持久化设备列表、启用状态和当前设备选择。 */
function saveSettings() {
  const settings: MobileAuraCoderSettings = {
    devices: devices.value,
    activeAuraCoderId: activeAuraCoderId.value,
  };
  uni.setStorageSync(SETTINGS_STORAGE_KEY, settings);
}

/** 将桌面端配对配置转换为手机端设备记录，并兼容旧设备名称。 */
/*
历史实现保留：
function toPairedAuraCoder(config: PairingConfig, existing?: PairedAuraCoder): PairedAuraCoder {
  const now = new Date().toISOString();
  return {
    auracoderId: existing?.auracoderId || config.tunnel_id,
    name: existing?.name || `AuraCoder ${config.tunnel_id.slice(0, 8)}`,
    endpoint: config.endpoint,
    tunnelId: config.tunnel_id,
    relayCredential: config.relay_credential,
    deviceCredential: config.device_credential,
    deviceId: config.device_id || existing?.deviceId,
    pairingToken: config.pairing_token,
    expiresAt: config.expires_at,
    pairedAt: existing?.pairedAt || now,
    lastConnectedAt: existing?.lastConnectedAt,
  };
}
*/
function toPairedAuraCoder(config: PairingConfig, existing?: PairedAuraCoder): PairedAuraCoder {
  const now = new Date().toISOString();
  const incomingDesktopName = config.desktop_name?.trim() || undefined;
  if (!existing) {
    return {
      auracoderId: config.tunnel_id,
      name: incomingDesktopName || "AuraCoder PC",
      endpoint: config.endpoint,
      tunnelId: config.tunnel_id,
      relayCredential: config.relay_credential,
      deviceCredential: config.device_credential,
      deviceId: config.device_id,
      pairingToken: config.pairing_token,
      expiresAt: config.expires_at,
      pairedAt: now,
      lastConnectedAt: undefined,
      desktopName: incomingDesktopName,
      enabled: true,
      nameCustomized: false,
    };
  }
  const oldAutomaticName = existing.name === `AuraCoder ${existing.tunnelId.slice(0, 8)}`;
  const desktopName = incomingDesktopName || existing.desktopName;
  const nameCustomized = existing.nameCustomized === true
    ? true
    : oldAutomaticName || !existing.name
      ? false
      : true;
  return {
    auracoderId: existing.auracoderId || config.tunnel_id,
    name: nameCustomized ? existing.name : desktopName || "AuraCoder PC",
    endpoint: config.endpoint,
    tunnelId: config.tunnel_id,
    relayCredential: config.relay_credential,
    deviceCredential: config.device_credential,
    deviceId: config.device_id || existing.deviceId,
    pairingToken: config.pairing_token,
    expiresAt: config.expires_at,
    pairedAt: existing.pairedAt || now,
    lastConnectedAt: existing.lastConnectedAt,
    desktopName,
    enabled: existing.enabled !== false,
    nameCustomized,
  };
}

/** 校验扫码或历史存储中的配对配置是否具备连接所需字段。 */
function isPairingConfig(value: unknown): value is PairingConfig {
  if (!value || typeof value !== "object") return false;
  const config = value as Partial<PairingConfig>;
  return config.version === 1
    && typeof config.endpoint === "string"
    && typeof config.tunnel_id === "string"
    && typeof config.relay_credential === "string"
    && (typeof config.device_credential === "string" || typeof config.pairing_token === "string");
}

/** 读取并归一化设备配置，确保旧数据具有启用状态和正确的当前设备。 */
function readSettings(): MobileAuraCoderSettings | null {
  const saved = uni.getStorageSync(SETTINGS_STORAGE_KEY) as MobileAuraCoderSettings | string | null;
  if (!saved) return null;
  try {
    const settings = typeof saved === "string" ? JSON.parse(saved) as MobileAuraCoderSettings : saved;
    if (!Array.isArray(settings.devices)) return null;
    const validDevices = settings.devices.filter((item): item is PairedAuraCoder => Boolean(
      item && item.auracoderId && item.endpoint && item.tunnelId && item.relayCredential
        && (item.deviceCredential || item.pairingToken),
    ));
    /*
    历史实现保留：
    const validDevices = settings.devices.filter((item): item is PairedAuraCoder => Boolean(
      item && item.auracoderId && item.endpoint && item.tunnelId && item.relayCredential
        && (item.deviceCredential || item.pairingToken),
    ));
    return {
      devices: validDevices,
      activeAuraCoderId: validDevices.some((item) => item.auracoderId === settings.activeAuraCoderId)
        ? settings.activeAuraCoderId
        : validDevices[0]?.auracoderId ?? null,
    };
    */
    const normalizedDevices = validDevices.map((item) => {
      const oldAutomaticName = item.name === `AuraCoder ${item.tunnelId.slice(0, 8)}`;
      return {
        ...item,
        enabled: item.enabled !== false,
        desktopName: item.desktopName,
        nameCustomized: item.nameCustomized === undefined
          ? Boolean(item.name) && !oldAutomaticName
          : item.nameCustomized,
      };
    });
    const activeDevice = normalizedDevices.find((item) => item.auracoderId === settings.activeAuraCoderId && item.enabled);
    return {
      devices: normalizedDevices,
      activeAuraCoderId: activeDevice?.auracoderId
        ?? validDevices.find((item) => item.enabled)?.auracoderId
        ?? null,
    };
  } catch {
    return null;
  }
}

/** 将旧版单设备配对存储迁移到当前设备列表存储。 */
function migrateLegacyPairing() {
  const legacy = uni.getStorageSync(LEGACY_PAIRING_STORAGE_KEY) as PairingConfig | string | null;
  if (!legacy) return;
  try {
    const config = typeof legacy === "string" ? JSON.parse(legacy) as PairingConfig : legacy;
    if (!isPairingConfig(config)) return;
    const device = toPairedAuraCoder(config);
    devices.value = [device];
    activeAuraCoderId.value = device.auracoderId;
    saveSettings();
  } catch {
    // 损坏的旧配置不能阻止 uni-app 启动；其余有效配置会继续保存在 v2 数据中。
  } finally {
    uni.removeStorageSync(LEGACY_PAIRING_STORAGE_KEY);
  }
}

/** 将设备记录转换为重连所需配置，保留桌面电脑名但不参与鉴权。 */
function toRemoteConfig(device: PairedAuraCoder): PairingConfig {
  return {
    version: 1,
    endpoint: device.endpoint,
    tunnel_id: device.tunnelId,
    relay_credential: device.relayCredential,
    device_credential: device.deviceCredential,
    device_id: device.deviceId,
    pairing_token: device.pairingToken,
    expires_at: device.expiresAt,
    desktop_name: device.desktopName,
  };
}

export const auracoderDeviceStore = {
  devices,
  activeAuraCoderId,
  /** 当前启用且按设备管理页顺序排列的远程 PC。 */
  enabledDevices: computed(() => devices.value.filter((item) => item.enabled)),
  /** 当前项目页选中的设备记录。 */
  activeDevice: computed(() => devices.value.find((item) => item.auracoderId === activeAuraCoderId.value) ?? null),
  /** 初始化设备持久化状态，只执行一次。 */
  initialize() {
    if (initialized) return;
    initialized = true;
    const settings = readSettings();
    if (settings) {
      devices.value = settings.devices;
      activeAuraCoderId.value = settings.activeAuraCoderId;
      return;
    }
    migrateLegacyPairing();
  },
  /** 按内部设备标识读取设备记录。 */
  getDevice(auracoderId: string) {
    return devices.value.find((item) => item.auracoderId === auracoderId) ?? null;
  },
  /** 按内部设备标识读取远程连接配置。 */
  getRemoteConfig(auracoderId: string) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    return device ? toRemoteConfig(device) : null;
  },
  /** 保存新配对设备或更新既有设备，并保持关闭设备的启用状态。 */
  addOrReplace(config: PairingConfig, preferredAuraCoderId?: string) {
    const existingIndex = devices.value.findIndex((item) => item.auracoderId === (preferredAuraCoderId || config.tunnel_id));
    const existing = existingIndex >= 0 ? devices.value[existingIndex] : undefined;
    const device = toPairedAuraCoder(config, existing ? { ...existing, auracoderId: preferredAuraCoderId || existing.auracoderId } : undefined);
    if (existingIndex >= 0) devices.value.splice(existingIndex, 1, device);
    else devices.value.push(device);
    // 历史实现保留：activeAuraCoderId.value = device.auracoderId;
    if (!existing || device.enabled) activeAuraCoderId.value = device.auracoderId;
    saveSettings();
    return device;
  },
  /** 设置当前项目页使用的启用设备。 */
  setActive(auracoderId: string) {
    // 历史实现保留：if (!devices.value.some((item) => item.auracoderId === auracoderId)) return;
    if (!devices.value.some((item) => item.auracoderId === auracoderId && item.enabled)) return;
    activeAuraCoderId.value = auracoderId;
    saveSettings();
  },
  /** 保存用户手动设置的设备显示名称。 */
  rename(auracoderId: string, name: string) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    if (!device) return;
    device.name = name.trim() || device.name;
    device.nameCustomized = true;
    saveSettings();
  },
  /** 更新已配对设备的凭据、电脑名和最近连接信息。 */
  updatePairedCredential(auracoderId: string, config: PairingConfig) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    if (!device) return;
    Object.assign(device, toPairedAuraCoder(config, device));
    saveSettings();
  },
  /** 记录设备最近一次成功建立在线连接的时间。 */
  markConnected(auracoderId: string) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    if (!device) return;
    device.lastConnectedAt = new Date().toISOString();
    saveSettings();
  },
  /** 删除设备配对记录，并将当前设备回退到第一台启用设备。 */
  remove(auracoderId: string) {
    const index = devices.value.findIndex((item) => item.auracoderId === auracoderId);
    if (index < 0) return;
    devices.value.splice(index, 1);
    // 历史实现保留：if (activeAuraCoderId.value === auracoderId) activeAuraCoderId.value = devices.value[0]?.auracoderId ?? null;
    if (activeAuraCoderId.value === auracoderId) {
      activeAuraCoderId.value = devices.value.find((item) => item.enabled)?.auracoderId ?? null;
    }
    saveSettings();
  },
  /** 切换设备启用状态；关闭仅影响连接和展示，不清理配对凭据。 */
  setEnabled(auracoderId: string, enabled: boolean) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    if (!device) return;
    device.enabled = enabled;
    if (!enabled && activeAuraCoderId.value === auracoderId) {
      activeAuraCoderId.value = devices.value.find((item) => item.enabled && item.auracoderId !== auracoderId)?.auracoderId ?? null;
    } else if (enabled && !activeAuraCoderId.value) {
      activeAuraCoderId.value = auracoderId;
    }
    saveSettings();
  },
  /** 按设备管理页拖动结果调整设备顺序并持久化。 */
  reorder(fromIndex: number, toIndex: number) {
    if (!Number.isInteger(fromIndex) || !Number.isInteger(toIndex)) return;
    if (fromIndex < 0 || fromIndex >= devices.value.length || toIndex < 0 || toIndex >= devices.value.length || fromIndex === toIndex) return;
    const moved = devices.value.splice(fromIndex, 1)[0];
    if (!moved) return;
    devices.value.splice(toIndex, 0, moved);
    saveSettings();
  },
};
