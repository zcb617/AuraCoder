import { reactive } from "vue";
import { RemoteClient } from "../remote";
import type { ConnectionState, PairingConfig, RemoteEvent } from "../types";
import { auracoderDeviceStore } from "./auracoder-device";

type RemoteEventListener = (auracoderId: string, event: RemoteEvent) => void;
type ConnectionStateListener = (auracoderId: string, state: ConnectionState, previous: ConnectionState) => void;

const clients = new Map<string, RemoteClient>();
const stateByAuraCoderId = reactive<Record<string, ConnectionState>>({});
const eventListeners = new Set<RemoteEventListener>();
const stateListeners = new Set<ConnectionStateListener>();
let initialized = false;

function defaultState(): ConnectionState {
  return { relayConnected: false, peerOnline: false, lastError: null };
}

function createClient(auracoderId: string) {
  const client = new RemoteClient();
  client.onState = (state) => {
    const previous = stateByAuraCoderId[auracoderId] || defaultState();
    stateByAuraCoderId[auracoderId] = state;
    if (!previous.peerOnline && state.peerOnline) auracoderDeviceStore.markConnected(auracoderId);
    stateListeners.forEach((listener) => listener(auracoderId, state, previous));
  };
  client.onPaired = (config) => auracoderDeviceStore.updatePairedCredential(auracoderId, config);
  client.onEvent = (event) => eventListeners.forEach((listener) => listener(auracoderId, event));
  clients.set(auracoderId, client);
  stateByAuraCoderId[auracoderId] = defaultState();
  return client;
}

export const auracoderConnectionManager = {
  stateByAuraCoderId,
  initialize() {
    if (initialized) return;
    initialized = true;
    auracoderDeviceStore.devices.value.forEach((device) => this.connect(device.auracoderId));
  },
  connect(auracoderId: string) {
    const config = auracoderDeviceStore.getRemoteConfig(auracoderId);
    if (!config) return;
    const client = clients.get(auracoderId) || createClient(auracoderId);
    client.connect(config);
  },
  reconnect(auracoderId: string) {
    this.disconnect(auracoderId);
    this.connect(auracoderId);
  },
  disconnect(auracoderId: string) {
    clients.get(auracoderId)?.disconnect();
  },
  remove(auracoderId: string) {
    clients.get(auracoderId)?.disconnect();
    clients.delete(auracoderId);
    delete stateByAuraCoderId[auracoderId];
  },
  resumeAll() {
    auracoderDeviceStore.devices.value.forEach((device) => {
      const client = clients.get(device.auracoderId);
      if (client) client.resume();
      else this.connect(device.auracoderId);
    });
  },
  keepAliveOnHide() {
    // 保持连接，避免相册、相机和系统文件选择器打开时中断正在进行的附件上传。
  },
  getState(auracoderId: string) {
    return stateByAuraCoderId[auracoderId] || defaultState();
  },
  request<T>(auracoderId: string, method: string, payload: Record<string, unknown> = {}) {
    const client = clients.get(auracoderId);
    if (!client) return Promise.reject(new Error("AuraCoder 连接尚未初始化")) as Promise<T>;
    return client.request<T>(method, payload);
  },
  subscribe(listener: RemoteEventListener) {
    eventListeners.add(listener);
    return () => eventListeners.delete(listener);
  },
  subscribeState(listener: ConnectionStateListener) {
    stateListeners.add(listener);
    return () => stateListeners.delete(listener);
  },
  applyRepairedConfig(auracoderId: string, config: PairingConfig) {
    auracoderDeviceStore.addOrReplace(config, auracoderId);
    this.reconnect(auracoderId);
  },
};
