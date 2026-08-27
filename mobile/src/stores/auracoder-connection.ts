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

/** 返回远程设备未连接时使用的默认状态。 */
function defaultState(): ConnectionState {
  return { relayConnected: false, peerOnline: false, lastError: null };
}

/** 创建并登记单台远程设备客户端及其状态回调。 */
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
  /** 初始化所有已启用设备的远程连接。 */
  initialize() {
    if (initialized) return;
    initialized = true;
    // 历史实现保留：auracoderDeviceStore.devices.value.forEach((device) => this.connect(device.auracoderId));
    auracoderDeviceStore.enabledDevices.value.forEach((device) => this.connect(device.auracoderId));
  },
  /** 使用设备保存的凭据建立远程连接。 */
  connect(auracoderId: string) {
    const config = auracoderDeviceStore.getRemoteConfig(auracoderId);
    if (!config) return;
    const client = clients.get(auracoderId) || createClient(auracoderId);
    client.connect(config);
  },
  /** 断开当前连接后重新建立远程连接。 */
  reconnect(auracoderId: string) {
    this.disconnect(auracoderId);
    this.connect(auracoderId);
  },
  /** 断开连接但保留客户端实例和设备配置。 */
  disconnect(auracoderId: string) {
    clients.get(auracoderId)?.disconnect();
  },
  /** 解除设备客户端运行态并移除连接状态。 */
  remove(auracoderId: string) {
    clients.get(auracoderId)?.disconnect();
    clients.delete(auracoderId);
    delete stateByAuraCoderId[auracoderId];
  },
  /** 恢复启用设备连接，并断开已关闭设备但保留其客户端缓存。 */
  resumeAll() {
    auracoderDeviceStore.devices.value.forEach((device) => {
      const client = clients.get(device.auracoderId);
      if (!device.enabled) {
        if (client) client.disconnect();
        return;
      }
      if (client) client.resume();
      else this.connect(device.auracoderId);
    });
  },
  /** 页面进入后台时保持远程连接，支持系统选择器期间继续上传。 */
  keepAliveOnHide() {
    // 保持连接，避免相册、相机和系统文件选择器打开时中断正在进行的附件上传。
  },
  /** 读取设备当前连接状态，未登记设备返回默认离线状态。 */
  getState(auracoderId: string) {
    return stateByAuraCoderId[auracoderId] || defaultState();
  },
  /** 向指定远程设备发送业务请求。 */
  request<T>(auracoderId: string, method: string, payload: Record<string, unknown> = {}) {
    const client = clients.get(auracoderId);
    if (!client) return Promise.reject(new Error("AuraCoder 连接尚未初始化")) as Promise<T>;
    return client.request<T>(method, payload);
  },
  /** 订阅远程设备业务事件并返回取消订阅函数。 */
  subscribe(listener: RemoteEventListener) {
    eventListeners.add(listener);
    return () => eventListeners.delete(listener);
  },
  /** 订阅远程设备连接状态变化并返回取消订阅函数。 */
  subscribeState(listener: ConnectionStateListener) {
    stateListeners.add(listener);
    return () => stateListeners.delete(listener);
  },
  /** 应用修复后的配对配置并重新连接设备。 */
  applyRepairedConfig(auracoderId: string, config: PairingConfig) {
    auracoderDeviceStore.addOrReplace(config, auracoderId);
    this.reconnect(auracoderId);
  },
};
