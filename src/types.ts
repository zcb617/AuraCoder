export type TrustLevel = "trusted" | "standard" | "restricted";

export type UpdateProcessPhase =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "downloaded"
  | "installing"
  | "error";

/** 更新失败所属的业务阶段，用于区分自动重试下载和安装失败。 */
export type UpdateErrorStage = "download" | "install" | "check";

export interface UpdateProcessState {
  phase: UpdateProcessPhase;
  version: string | null;
  source: "manual" | "automatic" | null;
  downloadedBytes: number;
  totalBytes: number | null;
  error: string | null;
  /** 当前错误所属的业务阶段，非错误状态为空。 */
  errorStage: UpdateErrorStage | null;
}

export type UpdateInstallRestartMode = "tauriRelaunch" | "externalUpdater";

export interface UpdateInstallResult {
  /** 后端指定当前平台后续采用的重启方式。 */
  restartMode: UpdateInstallRestartMode;
}

export interface Workspace {
  /** 迁移期间允许旧持久化数据字段存在；项目业务不读取未知字段。 */
  [key: string]: any;
  id: string;
  name: string;
  rootPath: string;
  locationKind?: "local" | "ssh" | string;
  sshConnectionId?: string | null;
  connectionDisplayName?: string | null;
  connectionEnabled?: boolean | null;
  connectionDeleted?: boolean | null;
  connectionStatus?: string | null;
  trustLevel: TrustLevel;
  createdAt: string;
  lastOpenedAt: string;
}

export interface ExecutionTarget {
  targetKey: string;
  kind: "local" | "ssh";
  displayName: string;
  connectionId?: string | null;
  hostName?: string | null;
  user?: string | null;
  port?: number | null;
  projectPath?: string | null;
  connectionStatus?: string | null;
}

export interface SshConnection {
  id: string;
  displayName: string;
  sourceKind: "ssh_config" | "manual" | string;
  configAlias: string | null;
  hostName: string;
  user: string;
  port: number;
  identityFile: string | null;
  hostKeyType: string;
  enabled: boolean;
  connectionStatus: "unknown" | "connecting" | "ok" | "failed" | "disabled" | "deleted" | string;
  lastConnectedAt: string | null;
  lastError: string | null;
  deletedAt: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface SshConfigHost {
  alias: string;
  hostName: string;
  user: string;
  port: number;
  identityFile: string | null;
  imported: boolean;
  deleted: boolean;
}

export interface SshConnectionInput {
  displayName: string;
  hostName: string;
  user: string;
  port: number;
  identityFile: string | null;
  hostKey: string;
  configAlias: string | null;
}

export interface SshConnectionImportResult {
  alias: string;
  connection: SshConnection | null;
  error: string | null;
  restored: boolean;
}

export interface SshConnectionTest {
  connectionId: string;
  ok: boolean;
  os: string | null;
  home: string | null;
  shell: string | null;
  gitVersion: string | null;
  cliVersions: Record<string, string>;
  error: string | null;
}

export interface SshRemoteDirectory {
  path: string;
  name: string;
  readable: boolean;
  enterable: boolean;
}

export interface KeepAwakeState {
  supported: boolean;
  enabled: boolean;
  active: boolean;
  supportsClosedDisplay?: boolean | null;
  closedDisplayActive?: boolean | null;
  message?: string | null;
  displaySleepPrevented?: boolean;
  screenSaverPrevented?: boolean;
  onAcPower?: boolean | null;
  batteryPercent?: number | null;
  sessionRemainingSecs?: number | null;
  pausedDueToBattery?: boolean;
  closedDisplaySleepDisabled?: boolean;
}

export interface PowerSettings {
  keepAwakeEnabled: boolean;
  preventDisplaySleep: boolean;
  preventScreenSaver: boolean;
  acOnlyMode: boolean;
  batteryThreshold: number | null;
  sessionDurationSecs: number | null;
  preventClosedDisplaySleep: boolean;
}

export interface PowerSettingsInput {
  keepAwakeEnabled: boolean;
  preventDisplaySleep: boolean;
  preventScreenSaver: boolean;
  acOnlyMode: boolean;
  batteryThreshold: number | null;
  sessionDurationSecs: number | null;
  preventClosedDisplaySleep: boolean;
}

export interface HelperStatus {
  status: "registered" | "requiresApproval" | "notRegistered" | "notFound" | "notSupported" | "unknown";
  message?: string | null;
}

/** MCP 服务运行状态快照，供设置页只读展示服务是否正常。 */
export interface McpServiceStatus {
  /** 后端返回的 MCP 服务业务运行状态。 */
  state: "normal" | "abnormal";
}

export type TerminalNotificationIntegrationId = "claude" | "codex";

export interface TerminalNotificationIntegrationStatus {
  configured: boolean;
  configPath?: string | null;
  configExists: boolean;
  conflict: boolean;
  detail?: string | null;
}

export interface TerminalNotificationSettings {
  chatEnabled: boolean;
  terminalEnabled: boolean;
  terminalSetupComplete: boolean;
  notificationSound: string | null;
  notificationSoundOptions: string[];
  soundPreviewSupported: boolean;
  claude: TerminalNotificationIntegrationStatus;
  codex: TerminalNotificationIntegrationStatus;
}

export type WorkspaceGitContext =
  | { kind: "not-repository"; workspaceId: string }
  | {
      kind: "repository";
      workspaceId: string;
      rootPath: string;
      name: string;
      defaultBranch: string | null;
    };

export type WorkspaceStartupPresetFormat = "json" | "toml";
export type WorkspaceDefaultView = "chat" | "split" | "terminal" | "editor";
export type WorkspacePathBase = "workspace" | "worktree" | "absolute";
export type WorkspaceStartupApplyWhen = "no_live_sessions";
export type WorkspaceStartupSplitDirection = "horizontal" | "vertical";

export interface WorkspaceStartupPreset {
  version: 1;
  defaultView: WorkspaceDefaultView;
  splitPanelSize?: number | null;
  terminal?: WorkspaceTerminalStartupPreset | null;
}

export interface WorkspaceTerminalStartupPreset {
  applyWhen: WorkspaceStartupApplyWhen;
  groups: WorkspaceStartupGroup[];
  activeGroupId?: string | null;
  focusedSessionId?: string | null;
}

export interface WorkspaceStartupGroup {
  id: string;
  name: string;
  broadcastOnStart?: boolean;
  worktree?: WorkspaceStartupWorktreeConfig | null;
  sessions: WorkspaceStartupSession[];
  root: WorkspaceStartupSplitNode;
}

export interface WorkspaceStartupWorktreeConfig {
  /** 迁移期间忽略旧夹具字段，生产逻辑只读取下列项目级配置。 */
  [key: string]: any;
  enabled: boolean;
  baseBranch?: string | null;
  baseDir?: string | null;
  branchPrefix?: string | null;
}

export interface WorkspaceStartupSession {
  id: string;
  title?: string | null;
  cwd: string;
  cwdBase?: WorkspacePathBase | null;
  harnessId?: string | null;
  launchHarnessOnCreate?: boolean | null;
}

export type WorkspaceStartupSplitNode =
  | {
      type: "leaf";
      sessionId: string;
    }
  | {
      type: "split";
      direction: WorkspaceStartupSplitDirection;
      ratio: number;
      children: [WorkspaceStartupSplitNode, WorkspaceStartupSplitNode];
    };

export type ThreadStatus =
  | "idle"
  | "streaming"
  | "awaiting_approval"
  | "error"
  | "completed";

export type ChatEngineId = "codex" | "claude" | "opencode";

/** 权限组件参数值；所有固定参数均使用字符串数组。 */
export type PermissionComponentValue = string[];
/** 前端权限组件与后端之间的六键统一 JSON 契约。 */
export interface PermissionComponentJson {
  /** 当前统一权限预设；空数组表示自定义。 */
  autonomyPreset: string[];
  /** 当前项目或工作区信任级别。 */
  trust: string[];
  /** 当前 CLI 审批策略。 */
  approval: string[];
  /** 当前 CLI 沙箱策略。 */
  sandbox: string[];
  /** 当前 CLI 网络策略。 */
  network: string[];
  /** 是否将当前预设作为新会话默认值。 */
  defaultForNewThreads: string[];
}

export interface Thread {
  id: string;
  workspaceId: string;
  engineId: ChatEngineId;
  modelId: string;
  engineThreadId: string | null;
  engineMetadata?: Record<string, unknown>;
  /** 底部 6 项运行时选择的独立字段，唯一数据源。 */
  planMode?: boolean | null;
  sendMethod?: string | null;
  reasoningEffort?: string | null;
  /** 当前 CLI 原始权限 JSON 字符串；前端不得解析，统一权限组件由 IPC 读取。 */
  permissionMode?: string | null;
  title: string;
  status: ThreadStatus;
  messageCount: number;
  totalTokens: number;
  createdAt: string;
  lastActivityAt: string;
}

export type ComputerControlSdkState =
  | "disabled"
  | "uninitialized"
  | "ready"
  | "failed"
  | "unsupported";

export interface ComputerControlSdkStatus {
  state: ComputerControlSdkState;
  initialized: boolean;
  abiVersion?: string | null;
  driverVersion?: string | null;
  error?: string | null;
}

export interface ComputerControlAdapterStatus {
  id: "codex" | "claude" | "opencode";
  name: string;
  builtIn: boolean;
}

export interface ComputerControlWaylandHelperStatus {
  supported: boolean;
  wayland: boolean;
  installed: boolean;
  running: boolean;
}

export interface ComputerControlStatus {
  supported: boolean;
  enabled: boolean;
  sdk: ComputerControlSdkStatus;
  waylandHelper: ComputerControlWaylandHelperStatus;
  adapters: ComputerControlAdapterStatus[];
  currentAuthorizations: ComputerControlApprovalRequest[];
}

export interface TextEditorApplication {
  id: string;
  name: string;
}

export interface DefaultFileOpenTarget {
  selectedEditorId: string | null;
  applications: TextEditorApplication[];
}

export interface ComputerControlApprovalRequest {
  requestId: string;
  agent: "codex" | "claude" | "opencode" | string;
  tool: string;
  callId?: string;
  application: string;
  operation?: string;
  scope?: string;
  threadId?: string;
  turnId?: string;
}

export type ScheduledTaskTargetType = "existing_thread" | "new_thread";
export type ScheduledTaskScheduleType = "interval" | "daily" | "weekly";
export type ScheduledTaskRunStatus =
  | "queued"
  | "running"
  | "needs_confirmation"
  | "completed"
  | "error"
  | "interrupted"
  | "skipped";

export interface ScheduledRuntimeConfig {
  engineId: string;
  modelId: string;
  reasoningEffort?: string | null;
  serviceTier?: string | null;
}

export interface IntervalScheduleConfig {
  every: number;
  unit: "minutes" | "hours" | "days";
}

export interface DailyScheduleConfig {
  time: string;
}

export interface WeeklyScheduleConfig {
  everyWeeks: number;
  weekdays: number[];
  time: string;
  anchorDate: string;
}

export type ScheduledTaskSchedule =
  | IntervalScheduleConfig
  | DailyScheduleConfig
  | WeeklyScheduleConfig;

export interface ScheduledTaskRun {
  id: string;
  taskId: string;
  scheduledFor: string;
  startedAt: string | null;
  finishedAt: string | null;
  threadId: string | null;
  assistantMessageId: string | null;
  status: ScheduledTaskRunStatus;
  errorMessage: string | null;
  resultPreview: string | null;
  acknowledgedAt: string | null;
  createdAt: string;
}

export interface ScheduledTask {
  id: string;
  description: string;
  enabled: boolean;
  executionDeviceId: string;
  targetType: ScheduledTaskTargetType;
  workspaceId: string;
  threadId: string | null;
  runtimeConfig: ScheduledRuntimeConfig | null;
  scheduleType: ScheduledTaskScheduleType;
  schedule: ScheduledTaskSchedule;
  timezone: string;
  nextRunAt: string | null;
  lastRunAt: string | null;
  latestRun: ScheduledTaskRun | null;
  needsConfirmation: boolean;
  targetValid: boolean;
  createdAt: string;
  updatedAt: string;
}

export interface ScheduledTaskInput {
  description: string;
  enabled: boolean;
  executionDeviceId: "local";
  targetType: ScheduledTaskTargetType;
  workspaceId: string;
  threadId: string | null;
  runtimeConfig: ScheduledRuntimeConfig | null;
  scheduleType: ScheduledTaskScheduleType;
  schedule: ScheduledTaskSchedule;
  timezone: string;
}

export interface CodexRemoteThread {
  engineThreadId: string;
  title?: string | null;
  preview: string;
  cwd: string;
  createdAt: string;
  updatedAt: string;
  modelId: string;
  reasoningEffort?: string | null;
  modelProvider: string;
  sourceKind: string;
  statusType: string;
  activeFlags: string[];
  archived: boolean;
  localThreadId?: string | null;
}

export interface CodexRemoteThreadPage {
  threads: CodexRemoteThread[];
  nextCursor?: string | null;
}

export interface OpenCodeRemoteSession {
  engineThreadId: string;
  title?: string | null;
  cwd: string;
  createdAt: string;
  updatedAt: string;
  archived: boolean;
  localThreadId?: string | null;
}

export interface OpenCodeRemoteSessionPage {
  sessions: OpenCodeRemoteSession[];
  nextCursor?: string | null;
}

export type CodexReviewDelivery = "inline" | "detached";

export type CodexReviewTarget =
  | { type: "uncommittedChanges" }
  | { type: "baseBranch"; branch: string }
  | { type: "commit"; sha: string; title?: string | null }
  | { type: "custom"; instructions: string };

export type MessageStatus = "completed" | "streaming" | "interrupted" | "error";

export interface Message {
  id: string;
  threadId: string;
  role: "user" | "assistant";
  content?: string;
  blocks?: ContentBlock[];
  clientTurnId?: string | null;
  turnEngineId?: string | null;
  turnModelId?: string | null;
  turnReasoningEffort?: string | null;
  status: MessageStatus;
  schemaVersion: number;
  tokenUsage?: { input: number; output: number };
  createdAt: string;
  hydration?: "full" | "summary";
  hasDeferredContent?: boolean;
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

export type ActionType =
  | "file_read"
  | "file_write"
  | "file_edit"
  | "file_delete"
  | "command"
  | "git"
  | "search"
  | "other";

export interface TextBlock {
  type: "text";
  content: string;
  planMode?: boolean;
  isSteer?: boolean;
  clientSteerId?: string;
}

export interface CodeBlock {
  type: "code";
  language: string;
  content: string;
  filename?: string;
}

export interface DiffBlock {
  type: "diff";
  diff: string;
  scope: "turn" | "file" | "workspace";
}

export interface NoticeBlock {
  type: "notice";
  kind: string;
  level: "info" | "warning" | "error";
  title: string;
  message: string;
}

export interface ActionBlock {
  type: "action";
  actionId: string;
  engineActionId?: string;
  actionType: ActionType;
  summary: string;
  details: Record<string, unknown>;
  outputChunks: Array<{ stream: "stdout" | "stderr" | "stdin"; content: string }>;
  outputDeferred?: boolean;
  outputDeferredLoaded?: boolean;
  status: "pending" | "running" | "done" | "error";
  result?: {
    success: boolean;
    output?: string;
    error?: string;
    diff?: string;
    durationMs: number;
  };
}

export interface ActionOutputPayload {
  found: boolean;
  outputChunks: Array<{ stream: "stdout" | "stderr" | "stdin"; content: string }>;
  truncated: boolean;
}

export interface ApprovalBlock {
  type: "approval";
  approvalId: string;
  actionType: ActionType;
  summary: string;
  details: Record<string, unknown>;
  status: "pending" | "answered";
  decision?:
    | "accept"
    | "accept_for_session"
    | "decline"
    | "cancel"
    | "custom";
  responseData?: Record<string, unknown>;
}

export type ApprovalDecision =
  | "accept"
  | "accept_for_session"
  | "decline"
  | "cancel";

export interface AcceptWithExecpolicyAmendmentDecision {
  acceptWithExecpolicyAmendment: {
    execpolicy_amendment: string[];
  };
}

export interface NetworkPolicyAmendment {
  host: string;
  action: "allow" | "deny";
}

export interface ApplyNetworkPolicyAmendmentDecision {
  applyNetworkPolicyAmendment: {
    network_policy_amendment: NetworkPolicyAmendment;
  };
}

export interface PermissionsApprovalResponse {
  permissions: Record<string, unknown>;
  scope?: "turn" | "session";
}

export interface McpServerElicitationResponse {
  action: "accept" | "decline" | "cancel";
  content?: Record<string, unknown>;
  _meta?: Record<string, unknown>;
}

export interface DynamicToolCallOutputTextItem {
  type: "inputText";
  text: string;
}

export interface DynamicToolCallOutputImageItem {
  type: "inputImage";
  imageUrl: string;
}

export interface DynamicToolCallResponse {
  success: boolean;
  contentItems: Array<DynamicToolCallOutputTextItem | DynamicToolCallOutputImageItem>;
}

export interface ToolInputAnswer {
  answers: string[];
}

export type ApprovalResponse =
  | {
      decision: ApprovalDecision;
    }
  | AcceptWithExecpolicyAmendmentDecision
  | ApplyNetworkPolicyAmendmentDecision
  | PermissionsApprovalResponse
  | McpServerElicitationResponse
  | DynamicToolCallResponse
  | {
      answers: Record<string, ToolInputAnswer>;
    }
  | Record<string, unknown>;

export interface ThinkingBlock {
  type: "thinking";
  content: string;
  startedAt?: number;
  durationMs?: number;
}

export interface ErrorBlock {
  type: "error";
  message: string;
}

export interface AttachmentBlock {
  type: "attachment";
  fileName: string;
  filePath: string;
  previewFilePath?: string;
  sizeBytes: number;
  mimeType?: string;
  browserAnnotation?: BrowserAnnotationMetadata;
  isRemote?: boolean;
}

export interface SkillBlock {
  type: "skill";
  name: string;
  path: string;
}

export interface MentionBlock {
  type: "mention";
  name: string;
  path: string;
}

export type SteerDeliveryStatus = "sending" | "accepted" | "applied" | "failed" | "settled";

export interface SteerReceipt {
  clientSteerId: string;
  expectedTurnId: string;
  acceptedAt: string;
}

export interface SteerBlock {
  type: "steer";
  steerId: string;
  content: string;
  deliveryStatus?: SteerDeliveryStatus;
  expectedTurnId?: string;
  acceptedAt?: string;
  failureReason?: string;
  planMode?: boolean;
  attachments?: AttachmentBlock[];
  skills?: SkillBlock[];
  mentions?: MentionBlock[];
}

export type ContentBlock =
  | TextBlock
  | CodeBlock
  | DiffBlock
  | NoticeBlock
  | ActionBlock
  | ApprovalBlock
  | ThinkingBlock
  | ErrorBlock
  | AttachmentBlock
  | SkillBlock
  | MentionBlock
  | SteerBlock;

export interface EngineInfo {
  id: string;
  name: string;
  models: EngineModel[];
  capabilities: EngineCapabilities;
}

export interface ChatProviderUsage {
  engineId: string;
  name: string;
  available: boolean;
  windows: ChatProviderUsageWindow[];
}

export interface ChatProviderUsageWindow {
  kind: "five_hour" | "weekly" | "fable_weekly" | "opus_weekly" | "sonnet_weekly";
  usedPercent: number;
  resetsAt: number | null;
}

export interface EngineCapabilities {
  permissionModes: string[];
  sandboxModes: string[];
  approvalDecisions: string[];
}

export interface EngineModel {
  id: string;
  displayName: string;
  description: string;
  hidden: boolean;
  isDefault: boolean;
  upgrade?: string;
  availabilityNux?: EngineModelAvailabilityNux;
  upgradeInfo?: EngineModelUpgradeInfo;
  inputModalities: string[];
  attachmentModalities: string[];
  limits?: EngineModelLimits;
  supportsPersonality: boolean;
  defaultReasoningEffort: string;
  supportedReasoningEfforts: ReasoningEffortOption[];
}

export interface EngineModelLimits {
  contextTokens?: number | null;
  inputTokens?: number | null;
  outputTokens?: number | null;
}

export interface EngineModelAvailabilityNux {
  message: string;
}

export interface EngineModelUpgradeInfo {
  model: string;
  upgradeCopy?: string;
  modelLink?: string;
  migrationMarkdown?: string;
}

export interface ReasoningEffortOption {
  reasoningEffort: string;
  description: string;
}

export interface EngineHealth {
  id: string;
  available: boolean;
  version?: string;
  details?: string;
  warnings?: string[];
  checks?: string[];
  fixes?: string[];
  protocolDiagnostics?: CodexProtocolDiagnostics;
}

export interface CodexMethodAvailability {
  method: string;
  status: string;
  detail?: string;
}

export interface CodexExperimentalFeature {
  name: string;
  enabled: boolean;
  defaultEnabled: boolean;
  stage: string;
  displayName?: string;
  description?: string;
}

export interface CodexApp {
  id: string;
  name: string;
  description?: string;
  isEnabled: boolean;
  isAccessible: boolean;
}

export interface CodexSkill {
  name: string;
  path: string;
  description: string;
  enabled: boolean;
  scope: string;
}

export interface OpenCodeRuntimeCatalog {
  agents: OpenCodeAgent[];
  commands: OpenCodeCommand[];
  mcpServers: OpenCodeMcpServer[];
}

export interface OpenCodeAgent {
  name: string;
  description?: string | null;
  mode: string;
  native: boolean;
  hidden: boolean;
  modelProviderId?: string | null;
  modelId?: string | null;
  variant?: string | null;
  steps?: number | null;
}

export interface OpenCodeCommand {
  name: string;
  description?: string | null;
  agent?: string | null;
  model?: string | null;
  source?: string | null;
  subtask: boolean;
  hints: string[];
}

export interface OpenCodeMcpServer {
  name: string;
  status: string;
  detail?: string | null;
  raw: unknown;
}

export interface CodexPluginMarketplace {
  name: string;
  path: string;
  plugins: CodexPlugin[];
}

export interface CodexPlugin {
  id: string;
  name: string;
  enabled: boolean;
  installed: boolean;
  capabilities: string[];
  developerName?: string;
  description?: string;
}

export interface CodexMcpServer {
  name: string;
  authStatus: string;
  toolCount: number;
  resourceCount: number;
  resourceTemplateCount: number;
}

export type ExtensionProviderId = "codex" | "claude" | "opencode";
export type ExtensionKind = "skill" | "plugin" | "mcp" | "agent" | "command";
export type ExtensionScope = "builtin" | "user" | "project" | "plugin" | "managed" | "local";
export type ExtensionAction =
  | "install"
  | "uninstall"
  | "enable"
  | "disable"
  | "remove"
  | "configure"
  | "authenticate"
  | "logout"
  | "open_location"
  | "refresh";
export type ExtensionHealth =
  | "unknown"
  | "healthy"
  | "disconnected"
  | "auth_required"
  | "error";
export type ExtensionAuthState = "unknown" | "authenticated" | "required" | "failed";

export interface ExtensionProviderCapabilities {
  hasOfficialSkillCatalog: boolean;
  canToggleSkills: boolean;
  hasOfficialPluginCatalog: boolean;
  canInstallPlugins: boolean;
  canTogglePlugins: boolean;
  hasOfficialMcpCatalog: boolean;
  canManageMcp: boolean;
  canAuthenticateMcp: boolean;
}

export interface ExtensionSource {
  id: string;
  label: string;
  official: boolean;
}

export interface ExtensionItem {
  id: string;
  providerId: ExtensionProviderId;
  kind: ExtensionKind;
  name: string;
  description?: string | null;
  version?: string | null;
  scope: ExtensionScope;
  source?: string | null;
  marketplace?: string | null;
  path?: string | null;
  parentPluginId?: string | null;
  category?: string | null;
  officiallyAvailable: boolean;
  catalogAuthority?: "provider_official" | null;
  installed: boolean | null;
  configured: boolean | null;
  enabled: boolean | null;
  health: ExtensionHealth;
  authState?: ExtensionAuthState | null;
  availableActions: ExtensionAction[];
  requiresNewSession: boolean;
  readOnlyReason?: string | null;
  warning?: string | null;
  insertText?: string | null;
  panel?: string | null;
  group?: string | null;
  disabled?: boolean;
  searchTerms?: string[];
}

export interface ExtensionCatalog {
  providerId: ExtensionProviderId;
  cwd?: string | null;
  items: ExtensionItem[];
  sources: ExtensionSource[];
  capabilities: ExtensionProviderCapabilities;
  fetchedAt?: string | null;
  kindFetchedAt?: Partial<Record<ExtensionKind, string | null>>;
  lastAttemptAt?: string | null;
  nextRefreshAt?: string | null;
  refreshing?: boolean;
  refreshCompletedAt?: string | null;
  hasSnapshot?: boolean;
  refreshErrors?: Array<{
    kind: ExtensionKind;
    code: string;
  }>;
}

export interface ExtensionActionResult {
  providerId: ExtensionProviderId;
  kind: ExtensionKind;
  extensionId: string;
  action: ExtensionAction;
  requiresNewSession: boolean;
}

export interface CodexAccountState {
  provider: string;
  authMode?: string;
  email?: string;
  planType?: string;
  requiresOpenaiAuth: boolean;
}

export interface CodexConfigLayer {
  source: string;
  version: string;
}

export type CodexApprovalsReviewer = "user" | "auto_review" | "guardian_subagent";

export interface CodexConfigState {
  model?: string;
  modelProvider?: string;
  serviceTier?: string;
  approvalPolicy?: unknown;
  permissionProfile?: unknown;
  approvalsReviewer?: CodexApprovalsReviewer;
  sandboxMode?: string;
  webSearch?: string;
  profile?: string;
  layers: CodexConfigLayer[];
}

export interface CodexConfigWarning {
  summary: string;
  details?: string;
  path?: string;
  startLine?: number;
  startColumn?: number;
  endLine?: number;
  endColumn?: number;
}

export interface CodexAccountLoginCompleted {
  success: boolean;
  error?: string;
  loginId?: string;
}

export interface CodexMcpOauthCompleted {
  name: string;
  success: boolean;
  error?: string;
}

export interface CodexThreadRealtimeEvent {
  kind: string;
  threadId: string;
  sessionId?: string;
  reason?: string;
  message?: string;
  itemType?: string;
  sampleRate?: number;
  numChannels?: number;
  samplesPerChannel?: number;
}

export interface CodexWindowsSandboxSetup {
  mode: string;
  success: boolean;
  error?: string;
}

export interface CodexWindowsWorldWritableWarning {
  samplePaths: string[];
  extraCount: number;
  failedScan: boolean;
}

export interface CodexProtocolDiagnostics {
  methodAvailability: CodexMethodAvailability[];
  experimentalFeatures: CodexExperimentalFeature[];
  collaborationModes: string[];
  apps: CodexApp[];
  skills: CodexSkill[];
  pluginMarketplaces: CodexPluginMarketplace[];
  mcpServers: CodexMcpServer[];
  account?: CodexAccountState;
  config?: CodexConfigState;
  lastConfigWarning?: CodexConfigWarning;
  lastAccountLogin?: CodexAccountLoginCompleted;
  lastMcpOauth?: CodexMcpOauthCompleted;
  lastThreadRealtime?: CodexThreadRealtimeEvent;
  lastWindowsSandboxSetup?: CodexWindowsSandboxSetup;
  lastWindowsWorldWritableWarning?: CodexWindowsWorldWritableWarning;
  fetchedAt?: string;
  stale: boolean;
}

export interface RuntimeToast {
  variant: "success" | "error" | "warning" | "info";
  message: string;
}

export interface EngineRuntimeUpdatedEvent {
  engineId: string;
  protocolDiagnostics?: CodexProtocolDiagnostics;
  toast?: RuntimeToast;
}

export interface EngineCheckResult {
  command: string;
  success: boolean;
  exitCode: number | null;
  stdout: string;
  stderr: string;
  durationMs: number;
}

export interface SearchResult {
  threadId: string;
  threadTitle: string;
  workspaceName: string;
  messageId: string;
  snippet: string;
}

export interface GitFileStatus {
  path: string;
  indexStatus?: string;
  worktreeStatus?: string;
}

export interface GitStatus {
  branch: string;
  files: GitFileStatus[];
  ahead: number;
  behind: number;
}

export interface GitDiffPreview {
  content: string;
  truncated: boolean;
  originalBytes: number;
  returnedBytes: number;
}

export type GitCompareSource = "changes" | "staged";
export type GitChangeType =
  | "added"
  | "modified"
  | "deleted"
  | "renamed"
  | "untracked"
  | "conflicted";

export interface GitFileCompare {
  source: GitCompareSource;
  baseContent: string;
  modifiedContent: string;
  baseLabel: string;
  modifiedLabel: string;
  changeType: GitChangeType;
  hasStagedChanges: boolean;
  hasUnstagedChanges: boolean;
  isBinary: boolean;
  isEditable?: boolean;
  fallbackReason?: string | null;
}

export type GitBranchScope = "local" | "remote";

export interface GitBranch {
  name: string;
  fullName: string;
  isCurrent: boolean;
  isRemote: boolean;
  upstream?: string;
  ahead: number;
  behind: number;
  lastCommitAt?: string;
}

export interface GitBranchPage {
  entries: GitBranch[];
  offset: number;
  limit: number;
  total: number;
  hasMore: boolean;
}

export interface GitCommit {
  hash: string;
  shortHash: string;
  authorName: string;
  authorEmail: string;
  subject: string;
  body: string;
  authoredAt: string;
}

export interface GitCommitPage {
  entries: GitCommit[];
  offset: number;
  limit: number;
  total: number;
  hasMore: boolean;
}

export interface GitStash {
  index: number;
  name: string;
  branchHint?: string;
  createdAt?: string;
}

export interface GitWorktree {
  path: string;
  displayPath?: string | null;
  headSha: string | null;
  branch: string | null;
  isMain: boolean;
  isLocked: boolean;
  isPrunable: boolean;
}

export interface GitRemote {
  name: string;
  url: string;
}

export interface GitInitRepoStatus {
  canInitialize: boolean;
  blockingRepoPath: string | null;
}

export interface WorktreeSessionInfo {
  /** 迁移期间允许旧会话夹具字段存在，清理逻辑使用 workspaceId。 */
  [key: string]: any;
  workspaceId: string;
  worktreePath: string;
  branch: string;
}

export interface FileTreeEntry {
  path: string;
  isDir: boolean;
}

export interface FileTreePage {
  entries: FileTreeEntry[];
  offset: number;
  limit: number;
  total: number;
  hasMore: boolean;
  scanTruncated: boolean;
}

export interface ReadFileResult {
  content: string;
  sizeBytes: number;
  isBinary: boolean;
  version: string;
}

export interface WriteFileResult {
  version: string;
}

export interface ResolvedEditorFileReference {
  workspaceId: string;
  rootPath: string;
  filePath: string;
  line?: number | null;
  column?: number | null;
}

export type EditorRenderMode = "plain-editor" | "markdown-preview" | "git-diff-editor";

export interface GitEditorContext extends GitFileCompare {}

export interface EditorRevealLocation {
  line: number;
  column?: number | null;
}

export interface EditorRevealRequest extends EditorRevealLocation {
  nonce: string;
}

export interface EditorTab {
  id: string;
  workspaceId: string | null;
  rootPath: string;
  absolutePath: string;
  filePath: string;
  gitFilePath: string | null;
  fileName: string;
  content: string;
  savedContent: string;
  isDirty: boolean;
  isLoading: boolean;
  isBinary: boolean;
  version: string;
  externalVersion: string | null;
  renderMode: EditorRenderMode;
  gitContext: GitEditorContext | null;
  pendingReveal: EditorRevealRequest | null;
  loadError?: string;
}

export interface TerminalSession {
  id: string;
  workspaceId: string;
  shell: string;
  cwd: string;
  createdAt: string;
}

export interface TerminalNotification {
  id: string;
  workspaceId: string;
  sessionId: string;
  source: string;
  title: string;
  body: string;
  createdAt: string;
}

export interface TerminalNotificationClearedEvent {
  sessionId: string | null;
}

export interface TerminalOutputReadyEvent {
  sessionId: string;
  latestSeq: number;
  ts: string;
  bytes: number;
}

export interface TerminalReplayChunk {
  seq: number;
  ts: string;
  data: string;
}

export interface TerminalResumeSession {
  latestSeq: number;
  oldestAvailableSeq: number | null;
  gap: boolean;
  chunks: TerminalReplayChunk[];
}

export interface TerminalExitEvent {
  sessionId: string;
  code: number | null;
  signal: number | null;
}

export interface TerminalForegroundChangedEvent {
  sessionId: string;
  pid: number | null;
  name: string | null;
}

export interface TerminalEnvSnapshot {
  term: string | null;
  colorterm: string | null;
  termProgram: string | null;
  termProgramVersion: string | null;
  home: string | null;
  userProfile: string | null;
  appData: string | null;
  localAppData: string | null;
  xdgConfigHome: string | null;
  xdgDataHome: string | null;
  xdgCacheHome: string | null;
  xdgStateHome: string | null;
  tmpdir: string | null;
  temp: string | null;
  tmp: string | null;
  lang: string | null;
  lcAll: string | null;
  lcCtype: string | null;
  path: string | null;
}

export interface TerminalResizeSnapshot {
  cols: number;
  rows: number;
  pixelWidth: number;
  pixelHeight: number;
  recordedAt: string;
}

export interface TerminalIoCounters {
  stdinWrites: number;
  stdinBytes: number;
  stdinCtrlC: number;
  lastStdinWriteDurationMs: number | null;
  stdoutReads: number;
  stdoutBytes: number;
  stdoutEmits: number;
  stdoutEmitBytes: number;
  stdoutDroppedBytes: number;
  lastStdinWriteAt: string | null;
  lastStdoutReadAt: string | null;
  lastStdoutEmitAt: string | null;
}

export interface TerminalLatencySnapshot {
  stdinToStdoutReadMs: number | null;
  stdoutReadToEmitMs: number | null;
}

export interface TerminalOutputThrottleSnapshot {
  minEmitIntervalMs: number;
  maxEmitBytes: number;
  bufferBytes: number;
  bufferCapBytes: number;
  bufferPeakBytes: number;
  bufferTrimmedBytes: number;
}

export interface TerminalRendererDiagnostics {
  sessionId: string;
  shell: string;
  cwd: string;
  envSnapshot: TerminalEnvSnapshot;
  lastResize: TerminalResizeSnapshot | null;
  ioCounters: TerminalIoCounters;
  latency: TerminalLatencySnapshot;
  outputThrottle: TerminalOutputThrottleSnapshot;
}

// ── Terminal Split Layout ───────────────────────────────────────────

export type SplitDirection = "horizontal" | "vertical";

export interface SplitLeaf {
  type: "leaf";
  sessionId: string;
}

export interface SplitContainer {
  type: "split";
  id: string;
  direction: SplitDirection;
  ratio: number;
  children: [SplitNode, SplitNode];
}

export type SplitNode = SplitLeaf | SplitContainer;

export interface TerminalSessionRuntimeMeta {
  harnessId?: string | null;
  harnessName?: string | null;
  autoDetectedHarness?: boolean;
  launchHarnessOnCreate?: boolean;
  worktree?: WorktreeSessionInfo | null;
}

export interface TerminalGroup {
  id: string;
  root: SplitNode;
  name: string;
  sessionMeta?: Record<string, TerminalSessionRuntimeMeta>;
  worktreeConfig?: WorkspaceStartupWorktreeConfig | null;
}

// ── Setup / Onboarding ──────────────────────────────────────────────

export type OnboardingWorkflowPreference = "cli" | "chat";
export type OnboardingChatEngineId = ChatEngineId;
export type OnboardingStep =
  | "greeting"
  | "workflow"
  | "cliProviders"
  | "chatEngines"
  | "chatReadiness"
  | "workspace";

export interface DependencyReport {
  node: DepStatus;
  codex: DepStatus;
  git: DepStatus;
  platform: string;
  packageManagers: string[];
}

export interface DepStatus {
  found: boolean;
  version: string | null;
  path: string | null;
  canAutoInstall: boolean;
  installMethod: string | null;
}

export interface InstallResult {
  success: boolean;
  message: string;
}

export interface InstallProgressEvent {
  dependency: string;
  line: string;
  stream: string;
  finished: boolean;
}

// ── Harness Management ──────────────────────────────────────────────

export interface HarnessInfo {
  id: string;
  name: string;
  description: string;
  command: string;
  found: boolean;
  version: string | null;
  path: string | null;
  canAutoInstall: boolean;
  website: string;
  native: boolean;
}

export interface HarnessReport {
  harnesses: HarnessInfo[];
  npmAvailable: boolean;
  preferredInstallMethod: string | null;
}

// ── Stream Events ───────────────────────────────────────────────────

export type TurnCompletionStatus = "completed" | "interrupted" | "failed";

export interface StreamTokenUsage {
  input: number;
  output: number;
  reasoning?: number | null;
  cacheRead?: number | null;
  cacheWrite?: number | null;
  costUsd?: number | null;
}

export interface TurnStartedEvent {
  type: "TurnStarted";
  client_turn_id?: string | null;
  remote_turn_id?: string | null;
}

export interface TurnCompletedEvent {
  type: "TurnCompleted";
  token_usage?: StreamTokenUsage | null;
  status?: TurnCompletionStatus;
}

export interface TextDeltaEvent {
  type: "TextDelta";
  content: string;
}

export interface ThinkingDeltaEvent {
  type: "ThinkingDelta";
  content: string;
}

export interface ActionStartedEvent {
  type: "ActionStarted";
  action_id: string;
  engine_action_id?: string | null;
  action_type: ActionType;
  summary: string;
  details: Record<string, unknown>;
}

export interface ActionOutputDeltaEvent {
  type: "ActionOutputDelta";
  action_id: string;
  stream: "stdout" | "stderr" | "stdin";
  content: string;
}

export interface ActionProgressUpdatedEvent {
  type: "ActionProgressUpdated";
  action_id: string;
  message: string;
}

export interface ActionCompletedEvent {
  type: "ActionCompleted";
  action_id: string;
  result: {
    success: boolean;
    output?: string | null;
    error?: string | null;
    diff?: string | null;
    durationMs: number;
  };
}

export interface DiffUpdatedEvent {
  type: "DiffUpdated";
  diff: string;
  scope: "turn" | "file" | "workspace";
}

export interface ApprovalRequestedEvent {
  type: "ApprovalRequested";
  approval_id: string;
  action_type: ActionType;
  summary: string;
  details: Record<string, unknown>;
}

export interface ApprovalResolvedEvent {
  type: "ApprovalResolved";
  approval_id: string;
}

export interface ErrorEvent {
  type: "Error";
  message: string;
  recoverable: boolean;
}

export interface UsageLimitsUpdatedEvent {
  type: "UsageLimitsUpdated";
  usage: {
    current_tokens?: number | null;
    max_context_tokens?: number | null;
    context_window_percent?: number | null;
    five_hour_percent?: number | null;
    weekly_percent?: number | null;
    fable_weekly_percent?: number | null;
    opus_weekly_percent?: number | null;
    sonnet_weekly_percent?: number | null;
    five_hour_resets_at?: number | null;
    weekly_resets_at?: number | null;
    fable_weekly_resets_at?: number | null;
    opus_weekly_resets_at?: number | null;
    sonnet_weekly_resets_at?: number | null;
  };
}

export interface ModelReroutedEvent {
  type: "ModelRerouted";
  from_model: string;
  to_model: string;
  reason: string;
}

export interface NoticeEvent {
  type: "Notice";
  kind: string;
  level: "info" | "warning" | "error";
  title: string;
  message: string;
}

export interface SteerAppliedEvent {
  type: "SteerApplied";
  client_steer_id: string;
  content: string;
  plan_mode: boolean;
  attachments: ChatAttachment[];
  input_items: ChatInputItem[];
}

export type StreamEvent =
  | TurnStartedEvent
  | TurnCompletedEvent
  | TextDeltaEvent
  | ThinkingDeltaEvent
  | ActionStartedEvent
  | ActionOutputDeltaEvent
  | ActionProgressUpdatedEvent
  | ActionCompletedEvent
  | DiffUpdatedEvent
  | ApprovalRequestedEvent
  | ApprovalResolvedEvent
  | ModelReroutedEvent
  | NoticeEvent
  | SteerAppliedEvent
  | ErrorEvent
  | UsageLimitsUpdatedEvent;

// ── Attachments ─────────────────────────────────────────────────────

export interface BrowserAnnotationMetadata {
  comment: string;
  number?: number;
  sourceUrl?: string;
  targetLabel?: string;
}

export interface BrowserBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface BrowserAnnotationRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface BrowserAnnotationSelection {
  url: string;
  title: string;
  targetLabel: string;
  rect: BrowserAnnotationRect;
}

export interface BrowserAnnotationSubmission {
  selection: BrowserAnnotationSelection;
  comment: string;
}

export interface BrowserAnnotationAttachment {
  fileName: string;
  filePath: string;
  sizeBytes: number;
  mimeType: string;
}

export interface ChatTextAnnotation {
  id: string;
  selectedText: string;
  comment: string;
}

export interface ImageAttachmentAnnotation {
  id: string;
  number: number;
  xPercent: number;
  yPercent: number;
  comment: string;
}

export interface ChatAttachment {
  id: string;
  fileName: string;
  filePath: string;
  sizeBytes: number;
  mimeType?: string;
  browserAnnotation?: BrowserAnnotationMetadata;
  imageAnnotations?: ImageAttachmentAnnotation[];
}

export interface AttachmentPreview {
  mimeType: string;
  dataBase64: string;
}

export type ChatInputItem =
  | {
      type: "text";
      text: string;
    }
  | {
      type: "skill";
      name: string;
      path: string;
    }
  | {
      type: "mention";
      name: string;
      path: string;
    };

export type ChatInputReference = Exclude<ChatInputItem, { type: "text" }>;

// ── Context Usage ───────────────────────────────────────────────────

export interface ContextUsage {
  currentTokens: number | null;
  maxContextTokens: number | null;
  contextPercent: number | null;
  windowFiveHourPercent: number | null;
  windowWeeklyPercent: number | null;
  windowFableWeeklyPercent: number | null;
  windowOpusWeeklyPercent: number | null;
  windowSonnetWeeklyPercent: number | null;
  windowFiveHourResetsAt: string | null;
  windowWeeklyResetsAt: string | null;
  windowFableWeeklyResetsAt: string | null;
  windowOpusWeeklyResetsAt: string | null;
  windowSonnetWeeklyResetsAt: string | null;
}

export interface RemoteAccessDevice {
  id: string;
  name: string;
  pairedAt: string | null;
  lastConnectedAt: string | null;
}

export interface RemoteAccessStatus {
  enabled: boolean;
  endpoint: string;
  tunnelId: string;
  connected: boolean;
  peerOnline: boolean;
  lastError: string | null;
  pairingPayload: string | null;
  pairingQrSvg: string | null;
  pairingExpiresAt: string | null;
  paired: boolean;
  devices: RemoteAccessDevice[];
}
