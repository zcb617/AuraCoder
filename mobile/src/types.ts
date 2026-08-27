export interface PairingConfig {
  /** 配对协议版本。 */
  version: 1;
  /** Relay 服务端点。 */
  endpoint: string;
  /** 远程 PC 隧道标识，仅用于内部路由和鉴权。 */
  tunnel_id: string;
  /** Relay 连接凭据。 */
  relay_credential: string;
  /** 首次配对令牌。 */
  pairing_token?: string;
  /** 设备凭据，用于后续身份确认。 */
  device_credential?: string;
  /** 手机设备在桌面端登记的稳定标识。 */
  device_id?: string;
  /** 桌面端本机电脑名称，用于手机端显示远端 PC。 */
  desktop_name?: string;
  /** 配对令牌过期时间。 */
  expires_at?: string;
}

export interface PairedAuraCoder {
  /** 手机端设备记录 ID。 */
  auracoderId: string;
  /** 设备显示名称，可为电脑名或用户自定义名称。 */
  name: string;
  /** Relay 服务端点。 */
  endpoint: string;
  /** 远程 PC 隧道标识，仅用于内部路由和鉴权。 */
  tunnelId: string;
  /** Relay 连接凭据。 */
  relayCredential: string;
  /** 后续身份确认使用的设备凭据。 */
  deviceCredential?: string;
  /** deviceId 只在当前 auracoderId 范围内有效，用于设备级实时事件校验。 */
  deviceId?: string;
  /** 首次配对令牌。 */
  pairingToken?: string;
  /** 配对令牌过期时间。 */
  expiresAt?: string;
  /** 首次配对时间。 */
  pairedAt: string;
  /** 最近一次连接成功时间。 */
  lastConnectedAt?: string;
  /** 桌面端本机电脑名称，用于设备管理和项目首页显示。 */
  desktopName?: string;
  /** 设备是否启用；关闭时保留配对凭据但不建立连接。 */
  enabled: boolean;
  /** 名称是否由用户手动修改，手动名称不被桌面电脑名覆盖。 */
  nameCustomized?: boolean;
}

export interface MobileAuraCoderSettings {
  /** 按设备管理页顺序保存的远程 PC 设备列表。 */
  devices: PairedAuraCoder[];
  /** 项目首页当前选中的启用设备 ID。 */
  activeAuraCoderId: string | null;
}

export interface ConnectionState {
  relayConnected: boolean;
  peerOnline: boolean;
  lastError: string | null;
}

export interface Workspace {
  id: string;
  name: string;
  rootPath: string;
  lastOpenedAt: string;
}

export interface Thread {
  id: string;
  workspaceId: string;
  engineId: string;
  /** CLI 原生会话标识；空会话为 null/缺省，用于判断是否还能切换 CLI。 */
  engineThreadId?: string | null;
  modelId: string;
  engineMetadata?: Record<string, unknown> | null;
  title: string;
  status: "idle" | "streaming" | "awaiting_approval" | "error" | "completed";
  messageCount: number;
  lastActivityAt: string;
}

/** 手机端附件来源；必须按用户入口分类，不能由 MIME 类型推断。 */
export type ChatAttachmentSource = "image" | "file";

/** 会话编辑区附件；localPath/source 只用于手机端选择和批次上传。 */
export interface ChatAttachment {
  /** 手机端本地稳定标识。 */
  id: string;
  /** 文件名。 */
  fileName: string;
  /** 桌面端暂存文件路径；选择阶段为空字符串。 */
  filePath: string;
  /** HTTP 上传成功后返回的服务端附件键；仅参与 message.send 的附件引用。 */
  attachmentKey?: string;
  /** 手机端选择后保留的可读本地路径或 content URI。 */
  localPath?: string;
  /** 已由桌面 AuraCoder 读取并返回的历史图片 data URL；只用于消息展示，不参与发送。 */
  previewUrl?: string;
  /** 选择入口来源；不参与远端 message.send 序列化。 */
  source?: ChatAttachmentSource;
  /** 服务端历史附件在消息 blocks 中的原始位置；仅用于再次请求对应图片预览。 */
  remoteAttachmentIndex?: number;
  /** 文件字节数；选择阶段无法读取时为 0。 */
  sizeBytes: number;
  /** 文件 MIME 类型。 */
  mimeType?: string;
  /** 当前批次是否正在上传该文件。 */
  uploading?: boolean;
  /** 当前批次该文件是否上传失败。 */
  failed?: boolean;
  /** 当前批次失败原因。 */
  error?: string;
}

/** 发送批次中的附件快照，和编辑区后续变化完全隔离。 */
export interface AttachmentBatchItem extends ChatAttachment {
  /** 批次上传使用的本地路径必须存在。 */
  localPath: string;
  /** 批次归属来源必须明确。 */
  source: ChatAttachmentSource;
}

/** 手机端一次点击发送形成的不可拆分正文和附件快照。 */
export interface AttachmentBatchState {
  /** 手机生成的 UUID，长度不超过协议上限。 */
  batchId: string;
  /** 批次所属会话。 */
  threadId: string;
  /** 发送时冻结的正文。 */
  message: string;
  /** 发送时冻结的模型 ID。 */
  modelId: string;
  /** 发送时冻结的思考强度。 */
  reasoningEffort: string;
  /** 批次附件快照。 */
  attachments: AttachmentBatchItem[];
  /** 批次当前阶段。 */
  status: "uploading" | "sending" | "failed";
  /** 用户取消后让正在运行的上传尽快停止。 */
  cancelled?: boolean;
  /** 失败阶段的人类可读错误。 */
  error?: string;
}

export interface Message {
  id: string;
  threadId: string;
  role: "user" | "assistant";
  content?: string;
  blocks?: Array<Record<string, unknown>>;
  /** 手机端本地回显附件；远端协议字段不会把 localPath/source 发送出去。 */
  attachments?: ChatAttachment[];
  status: "completed" | "streaming" | "interrupted" | "error";
  createdAt: string;
  // 本地回显尚未与桌面端历史消息合并时为 true；失败后会移除该消息。
  localOnly?: boolean;
}

export interface MessageWindowCursor {
  createdAt: string;
  id: string;
  rowId?: number;
}

export interface MessageWindow {
  messages: Message[];
  nextCursor: MessageWindowCursor | null;
}

export interface DesktopStatus {
  version: string;
  online: boolean;
}

export interface ReasoningEffortOption {
  reasoningEffort: string;
  description: string;
}

export interface EngineModel {
  id: string;
  displayName: string;
  description: string;
  defaultReasoningEffort: string | null;
  supportedReasoningEfforts: ReasoningEffortOption[];
  isDefault: boolean;
  hidden?: boolean;
}

export interface EngineInfo {
  id: string;
  name: string;
  models: EngineModel[];
}

export interface RemoteEvent {
  version: number;
  kind: "event";
  event: string;
  // 设备级事件的目标手机 ID；消息已由连接来源携带 auracoderId。
  targetDeviceId?: string;
  payload: Record<string, unknown>;
}

export interface ThreadMessageCompletedPayload {
  // 完成消息所属的会话 ID。
  threadId: string;
  // 桌面端最终消息 ID，用于手机端去重。
  messageId: string;
  // 与 message.list 返回一致的最终助手消息。
  message: Message;
}
