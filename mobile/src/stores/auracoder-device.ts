import { computed, ref } from "vue";
import type { MobileAuraCoderSettings, PairedAuraCoder, PairingConfig } from "../types";

const SETTINGS_STORAGE_KEY = "auracoder-mobile:settings:v2";
const LEGACY_PAIRING_STORAGE_KEY = "auracoder-mobile:pairing:v1";
const devices = ref<PairedAuraCoder[]>([]);
const activeAuraCoderId = ref<string | null>(null);
let initialized = false;

function saveSettings() {
  const settings: MobileAuraCoderSettings = {
    devices: devices.value,
    activeAuraCoderId: activeAuraCoderId.value,
  };
  uni.setStorageSync(SETTINGS_STORAGE_KEY, settings);
}

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

function isPairingConfig(value: unknown): value is PairingConfig {
  if (!value || typeof value !== "object") return false;
  const config = value as Partial<PairingConfig>;
  return config.version === 1
    && typeof config.endpoint === "string"
    && typeof config.tunnel_id === "string"
    && typeof config.relay_credential === "string"
    && (typeof config.device_credential === "string" || typeof config.pairing_token === "string");
}

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
    return {
      devices: validDevices,
      activeAuraCoderId: validDevices.some((item) => item.auracoderId === settings.activeAuraCoderId)
        ? settings.activeAuraCoderId
        : validDevices[0]?.auracoderId ?? null,
    };
  } catch {
    return null;
  }
}

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
  };
}

export const auracoderDeviceStore = {
  devices,
  activeAuraCoderId,
  activeDevice: computed(() => devices.value.find((item) => item.auracoderId === activeAuraCoderId.value) ?? null),
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
  getDevice(auracoderId: string) {
    return devices.value.find((item) => item.auracoderId === auracoderId) ?? null;
  },
  getRemoteConfig(auracoderId: string) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    return device ? toRemoteConfig(device) : null;
  },
  addOrReplace(config: PairingConfig, preferredAuraCoderId?: string) {
    const existingIndex = devices.value.findIndex((item) => item.auracoderId === (preferredAuraCoderId || config.tunnel_id));
    const existing = existingIndex >= 0 ? devices.value[existingIndex] : undefined;
    const device = toPairedAuraCoder(config, existing ? { ...existing, auracoderId: preferredAuraCoderId || existing.auracoderId } : undefined);
    if (existingIndex >= 0) devices.value.splice(existingIndex, 1, device);
    else devices.value.push(device);
    activeAuraCoderId.value = device.auracoderId;
    saveSettings();
    return device;
  },
  setActive(auracoderId: string) {
    if (!devices.value.some((item) => item.auracoderId === auracoderId)) return;
    activeAuraCoderId.value = auracoderId;
    saveSettings();
  },
  rename(auracoderId: string, name: string) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    if (!device) return;
    device.name = name.trim() || device.name;
    saveSettings();
  },
  updatePairedCredential(auracoderId: string, config: PairingConfig) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    if (!device) return;
    Object.assign(device, toPairedAuraCoder(config, device));
    saveSettings();
  },
  markConnected(auracoderId: string) {
    const device = devices.value.find((item) => item.auracoderId === auracoderId);
    if (!device) return;
    device.lastConnectedAt = new Date().toISOString();
    saveSettings();
  },
  remove(auracoderId: string) {
    const index = devices.value.findIndex((item) => item.auracoderId === auracoderId);
    if (index < 0) return;
    devices.value.splice(index, 1);
    if (activeAuraCoderId.value === auracoderId) activeAuraCoderId.value = devices.value[0]?.auracoderId ?? null;
    saveSettings();
  },
};
