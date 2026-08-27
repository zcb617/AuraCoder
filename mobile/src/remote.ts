import type { ConnectionState, PairingConfig, RemoteEvent } from "./types";

interface PendingRequest {
  resolve: (payload: unknown) => void;
  reject: (error: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

interface RemoteResponse {
  version: number;
  kind: "response";
  id: string;
  ok: boolean;
  payload?: unknown;
  error?: { code?: string; message?: string };
}

interface SocketFailure { errMsg?: string }
interface SocketCloseEvent { code?: number; reason?: string }
interface UniSocketTask {
  send(options: { data: string; fail?: (error: SocketFailure) => void }): void;
  close(options?: { code?: number; reason?: string }): void;
  onOpen(callback: () => void): void;
  onMessage(callback: (event: { data: string | ArrayBuffer }) => void): void;
  onClose(callback: (event: SocketCloseEvent) => void): void;
  onError(callback: (error: SocketFailure) => void): void;
}

export class RemoteClient {
  onState: (state: ConnectionState) => void = () => undefined;
  onEvent: (event: RemoteEvent) => void = () => undefined;
  onPaired: (config: PairingConfig) => void = () => undefined;

  private config: PairingConfig | null = null;
  private socket: UniSocketTask | null = null;
  private socketOpen = false;
  private pending = new Map<string, PendingRequest>();
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private retryIndex = 0;
  private requestSequence = 0;
  private generation = 0;
  private stopped = true;
  private peerOnline = false;
  private pairingInProgress = false;
  private deviceIdentifying = false;
  // 设备 ID 来自配对配置，重连时不能清空，否则会在身份确认前丢失目标事件。
  private deviceId: string | null = null;
  // 身份尚未确认时暂存带 targetDeviceId 的事件，确认后再按目标设备过滤并投递。
  private deferredEvents: RemoteEvent[] = [];
  private readonly retryDelays = [1000, 2000, 5000, 10000, 30000];

  connect(config: PairingConfig) {
    this.disconnect();
    this.config = config;
    this.deviceId = config.device_id || null;
    this.stopped = false;
    this.retryIndex = 0;
    this.generation += 1;
    this.open(this.generation);
  }

  disconnect() {
    this.stopped = true;
    this.generation += 1;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.socket?.close({ code: 1000, reason: "client disconnect" });
    this.config = null;
    this.socket = null;
    this.socketOpen = false;
    this.peerOnline = false;
    this.pairingInProgress = false;
    this.deviceIdentifying = false;
    this.deviceId = null;
    this.deferredEvents = [];
    this.rejectPending("连接已断开");
    this.onState({ relayConnected: false, peerOnline: false, lastError: null });
  }

  suspend() {
    if (this.stopped || !this.config) return;
    this.stopped = true;
    this.generation += 1;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.socket?.close({ code: 1000, reason: "app entered background" });
    this.socket = null;
    this.socketOpen = false;
    this.peerOnline = false;
    this.pairingInProgress = false;
    this.deviceIdentifying = false;
    this.rejectPending("应用已进入后台");
    this.onState({ relayConnected: false, peerOnline: false, lastError: null });
  }

  resume() {
    if (!this.stopped || !this.config) return;
    this.stopped = false;
    this.retryIndex = 0;
    this.generation += 1;
    this.open(this.generation);
  }

  request<T = unknown>(method: string, payload: Record<string, unknown> = {}): Promise<T> {
    if (!this.config || !this.socket || !this.socketOpen || !this.peerOnline) {
      return Promise.reject(new Error("桌面 AuraCoder 当前离线"));
    }
    this.requestSequence += 1;
    const id = `${Date.now().toString(36)}-${this.requestSequence.toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
    return new Promise<T>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error("请求桌面 AuraCoder 超时"));
      }, 15000);
      this.pending.set(id, { resolve: resolve as (value: unknown) => void, reject, timer });
      this.socket?.send({
        data: JSON.stringify({
          version: 1,
          kind: "request",
          id,
          method,
          auth: this.config?.device_credential || this.config?.pairing_token,
          payload,
        }),
        fail: (error) => {
          const request = this.pending.get(id);
          if (!request) return;
          this.pending.delete(id);
          clearTimeout(request.timer);
          request.reject(new Error(error.errMsg || "请求发送失败"));
        },
      });
    });
  }

  private open(generation: number) {
    if (this.stopped || !this.config || generation !== this.generation) return;
    this.retryTimer = null;
    this.onState({ relayConnected: false, peerOnline: false, lastError: null });
    let socket: UniSocketTask;
    try {
      socket = uni.connectSocket({
        url: this.config.endpoint,
        complete: () => undefined,
      }) as unknown as UniSocketTask;
    } catch (error) {
      this.scheduleReconnect(generation, String(error));
      return;
    }
    this.socket = socket;

    socket.onOpen(() => {
      if (!this.config || generation !== this.generation) return;
      this.socketOpen = true;
      socket.send({
        data: JSON.stringify({
          version: 1,
          type: "tunnel.hello",
          role: "mobile",
          tunnel_id: this.config.tunnel_id,
          credential: this.config.relay_credential,
        }),
      });
    });

    socket.onMessage((event) => {
      if (generation !== this.generation || typeof event.data !== "string") return;
      let message: Record<string, unknown>;
      try {
        message = JSON.parse(event.data) as Record<string, unknown>;
      } catch {
        return;
      }
      if (message.type === "tunnel.ready") {
        this.retryIndex = 0;
        if (message.peer_online === true) this.markPeerOnline();
        else this.onState({ relayConnected: true, peerOnline: false, lastError: null });
        return;
      }
      if (message.type === "tunnel.peer_online") {
        this.markPeerOnline();
        return;
      }
      if (message.type === "tunnel.peer_offline") {
        this.peerOnline = false;
        this.pairingInProgress = false;
        this.deviceIdentifying = false;
        this.onState({ relayConnected: true, peerOnline: false, lastError: null });
        return;
      }
      if (message.kind === "response" && typeof message.id === "string") {
        const response = message as unknown as RemoteResponse;
        const request = this.pending.get(response.id);
        if (!request) return;
        this.pending.delete(response.id);
        clearTimeout(request.timer);
        if (response.ok) request.resolve(response.payload);
        else request.reject(new Error(response.error?.message || "桌面 AuraCoder 拒绝了请求"));
        return;
      }
      if (message.kind === "event") {
        const targetDeviceId = typeof message.targetDeviceId === "string" ? message.targetDeviceId : "";
        const event = message as unknown as RemoteEvent;
        // 设备身份确认完成前先暂存事件，避免重连竞态导致完成消息丢失。
        if (targetDeviceId && !this.deviceId) {
          this.deferredEvents.push(event);
          // 连接长期等待身份响应时只保留有限窗口，避免异常服务端耗尽手机内存。
          if (this.deferredEvents.length > 1000) this.deferredEvents.splice(0, this.deferredEvents.length - 1000);
          return;
        }
        // 有目标设备标记的消息只允许对应设备处理；无目标标记的旧事件仍兼容透传。
        if (targetDeviceId && targetDeviceId !== this.deviceId) return;
        this.onEvent(event);
      }
    });

    socket.onClose((event) => {
      if (generation !== this.generation || this.socket !== socket) return;
      this.socket = null;
      this.socketOpen = false;
      this.peerOnline = false;
      this.deviceIdentifying = false;
      this.rejectPending("连接已断开，请稍后重试");
      if (!this.stopped) this.scheduleReconnect(generation, event.reason || `连接关闭（${event.code ?? "未知"}）`);
    });

    socket.onError((error) => {
      if (generation !== this.generation || this.socket !== socket) return;
      this.socket = null;
      this.socketOpen = false;
      this.peerOnline = false;
      this.pairingInProgress = false;
      this.deviceIdentifying = false;
      this.rejectPending("连接异常，请稍后重试");
      socket.close({ code: 1011, reason: "socket error" });
      if (!this.stopped) this.scheduleReconnect(generation, error.errMsg || "无法连接 Tunnel Relay");
    });
  }

  private scheduleReconnect(generation: number, error: string) {
    if (this.stopped || generation !== this.generation) return;
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.onState({ relayConnected: false, peerOnline: false, lastError: error });
    const delay = this.retryDelays[Math.min(this.retryIndex, this.retryDelays.length - 1)];
    this.retryIndex = Math.min(this.retryIndex + 1, this.retryDelays.length - 1);
    this.retryTimer = setTimeout(() => this.open(generation), delay);
  }

  private markPeerOnline() {
    this.peerOnline = true;
    const systemInfo = uni.getSystemInfoSync() as unknown as Record<string, unknown>;
    const rawOsName = typeof systemInfo.osName === "string"
      ? systemInfo.osName
      : typeof systemInfo.platform === "string"
        ? systemInfo.platform
        : "";
    const osName = rawOsName ? `${rawOsName.charAt(0).toUpperCase()}${rawOsName.slice(1)}` : "";
    const osVersion = typeof systemInfo.osVersion === "string" ? systemInfo.osVersion : "";
    const deviceModel = typeof systemInfo.deviceModel === "string"
      ? systemInfo.deviceModel
      : typeof systemInfo.model === "string"
        ? systemInfo.model
        : "";
    const deviceName = [osName, osVersion, deviceModel].filter(Boolean).join(" ") || "AuraCoder Mobile";
    if (!this.config) return;
    if (this.config.device_credential) {
      if (this.deviceIdentifying) return;
      this.deviceIdentifying = true;
      // request 需要 peerOnline 为 true；在身份确认完成前不对页面广播在线状态。
      this.peerOnline = true;
      /*
      历史实现保留：
      void this.request<{ device_id?: string }>("device.identify", {
        device_name: deviceName,
        device_id: this.deviceId || undefined,
      });
      */
      void this.request<{ device_id?: string; desktop_name?: string }>("device.identify", {
        device_name: deviceName,
        device_id: this.deviceId || undefined,
      })
        .then((result) => {
          const confirmedDeviceId = typeof result.device_id === "string" && result.device_id
            ? result.device_id
            : this.deviceId;
          if (!confirmedDeviceId) {
            throw new Error("桌面未返回设备身份");
          }
          this.deviceId = confirmedDeviceId;
          /* 历史实现保留：this.config = { ...this.config!, device_id: this.deviceId }; */
          const desktopName = typeof result.desktop_name === "string" ? result.desktop_name.trim() : "";
          this.config = {
            ...this.config!,
            device_id: this.deviceId,
            desktop_name: desktopName || this.config!.desktop_name,
          };
          this.onPaired(this.config);
          this.onState({ relayConnected: true, peerOnline: true, lastError: null });
          this.flushDeferredEvents();
        })
        .catch((error) => {
          this.peerOnline = false;
          this.onState({ relayConnected: true, peerOnline: false, lastError: `设备身份确认失败：${String(error)}` });
        })
        .finally(() => {
          this.deviceIdentifying = false;
        });
      return;
    }
    if (this.pairingInProgress) return;
    if (!this.config.pairing_token) {
      this.onState({ relayConnected: true, peerOnline: false, lastError: "配对凭据缺失，请重新扫码" });
      return;
    }
    this.pairingInProgress = true;
    this.onState({ relayConnected: true, peerOnline: false, lastError: null });
    /*
    历史实现保留：
    void this.request<{ device_credential: string; device_id?: string }>("device.pair", { device_name: deviceName });
    */
    void this.request<{ device_credential: string; device_id?: string; desktop_name?: string }>("device.pair", { device_name: deviceName })
      .then((result) => {
        if (!this.config) return;
        if (typeof result.device_id === "string" && result.device_id) this.deviceId = result.device_id;
        const desktopName = typeof result.desktop_name === "string" ? result.desktop_name.trim() : "";
        /*
        历史实现保留：
        this.config = {
          ...this.config,
          device_credential: result.device_credential,
          device_id: this.deviceId || undefined,
          pairing_token: undefined,
          expires_at: undefined,
        };
        */
        this.config = {
          ...this.config,
          device_credential: result.device_credential,
          device_id: this.deviceId || undefined,
          pairing_token: undefined,
          expires_at: undefined,
          desktop_name: desktopName || this.config.desktop_name,
        };
        this.onPaired(this.config);
        this.onState({ relayConnected: true, peerOnline: true, lastError: null });
        this.flushDeferredEvents();
      })
      .catch((error) => {
        this.peerOnline = false;
        this.onState({ relayConnected: true, peerOnline: false, lastError: `配对失败：${String(error)}` });
      })
      .finally(() => {
        this.pairingInProgress = false;
      });
  }

  private rejectPending(message: string) {
    for (const request of this.pending.values()) {
      clearTimeout(request.timer);
      request.reject(new Error(message));
    }
    this.pending.clear();
  }

  private flushDeferredEvents() {
    if (!this.deviceId || !this.deferredEvents.length) return;
    const events = this.deferredEvents.splice(0);
    events.forEach((event) => {
      const targetDeviceId = event.targetDeviceId;
      if (targetDeviceId && targetDeviceId !== this.deviceId) return;
      this.onEvent(event);
    });
  }
}
