import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { normalizeDependencyReport } from "./dependencies";
import type { AppLocale } from "./locale";
import type { DisplayScale } from "./displayScale";
import type { ThemePreference } from "./theme";
import type {
  ApprovalResponse,
  ActionOutputPayload,
  AttachmentPreview,
  BrowserAnnotationAttachment,
  BrowserAnnotationSelection,
  BrowserBounds,
  ChatAttachment,
  ChatEngineId,
  ChatInputItem,
  ChatProviderUsage,
  ComputerControlStatus,
  ComputerControlApprovalRequest,
  CodexApprovalsReviewer,
  CodexReviewDelivery,
  CodexReviewTarget,
  CodexRemoteThreadPage,
  ContentBlock,
  CodexApp,
  CodexPlugin,
  CodexSkill,
  DependencyReport,
  EngineCheckResult,
  EngineRuntimeUpdatedEvent,
  GitBranchPage,
  GitBranchScope,
  GitCommitPage,
  GitInitRepoStatus,
  GitCompareSource,
  GitFileCompare,
  GitStash,
  GitRemote,
  GitWorktree,
  EngineHealth,
  EngineInfo,
  ExecutionTarget,
  ExtensionAction,
  ExtensionActionResult,
  ExtensionCatalog,
  ExtensionItem,
  ExtensionKind,
  ExtensionProviderId,
  DefaultFileOpenTarget,
  FileTreeEntry,
  FileTreePage,
  GitDiffPreview,
  GitStatus,
  HarnessReport,
  InstallProgressEvent,
  InstallResult,
  HelperStatus,
  KeepAwakeState,
  PowerSettings,
  PowerSettingsInput,
  RemoteAccessStatus,
  SshConnection,
  SshConfigHost,
  SshConnectionInput,
  SshConnectionImportResult,
  SshConnectionTest,
  Message,
  MessageWindow,
  MessageWindowCursor,
  OpenCodeRemoteSessionPage,
  OpenCodeRuntimeCatalog,
  ReadFileResult,
  WriteFileResult,
  ResolvedEditorFileReference,
  Repo,
  SearchResult,
  SteerReceipt,
  StreamEvent,
  TerminalNotificationClearedEvent,
  TerminalNotification,
  TerminalExitEvent,
  TerminalForegroundChangedEvent,
  TerminalNotificationIntegrationId,
  TerminalNotificationSettings,
  TerminalOutputReadyEvent,
  TerminalRendererDiagnostics,
  TerminalResumeSession,
  TerminalSession,
  WorkspaceStartupPreset,
  WorkspaceStartupPresetFormat,
  Thread,
  PermissionComponentJson,
  TrustLevel,
  WorkspaceGitSelectionStatus,
  Workspace,
  UpdateProcessState,
  UpdateInstallResult,
  SshRemoteDirectory,
} from "../types";
import type { ScheduledTask, ScheduledTaskInput } from "../types";

export const ipc = {
  getUpdateState: () => invoke<UpdateProcessState>("get_update_state"),
  isUpdateDownloaded: () => invoke<boolean>("is_update_downloaded"),
  checkForUpdate: (source: "manual" | "automatic") =>
    invoke<UpdateProcessState>("check_for_update", { source }),
  downloadUpdate: (source: "manual" | "automatic") =>
    invoke<UpdateProcessState>("download_update", { source }),
  installDownloadedUpdate: () => invoke<UpdateInstallResult>("install_downloaded_update"),
  prepareLocalUpdateForDevelopment: (archivePath: string) =>
    invoke<UpdateProcessState>("prepare_local_update_for_development", { archivePath }),
  listSshConnections: () => invoke<SshConnection[]>("list_ssh_connections"),
  listDeletedSshConnections: () => invoke<SshConnection[]>("list_deleted_ssh_connections"),
  scanSshConfigHosts: () => invoke<SshConfigHost[]>("scan_ssh_config_hosts"),
  importSshConfigHosts: (aliases: string[]) =>
    invoke<SshConnectionImportResult[]>("import_ssh_config_hosts", { aliases }),
  createManualSshConnection: (input: SshConnectionInput) =>
    invoke<SshConnection>("create_manual_ssh_connection", { input }),
  updateSshConnection: (connectionId: string, input: SshConnectionInput) =>
    invoke<SshConnection>("update_ssh_connection", { connectionId, input }),
  testSshConnection: (connectionId: string) =>
    invoke<SshConnectionTest>("test_ssh_connection", { connectionId }),
  setSshConnectionEnabled: (connectionId: string, enabled: boolean) =>
    invoke<SshConnection>("set_ssh_connection_enabled", { connectionId, enabled }),
  deleteSshConnection: (connectionId: string) =>
    invoke<void>("delete_ssh_connection", { connectionId }),
  restoreSshConnection: (connectionId: string) =>
    invoke<SshConnection>("restore_ssh_connection", { connectionId }),
  getRemoteAccessStatus: () => invoke<RemoteAccessStatus>("get_remote_access_status"),
  setRemoteAccessEnabled: (enabled: boolean) =>
    invoke<RemoteAccessStatus>("set_remote_access_enabled", { enabled }),
  regenerateRemoteAccessIdentity: () =>
    invoke<RemoteAccessStatus>("regenerate_remote_access_identity"),
  refreshRemotePairingToken: () =>
    invoke<RemoteAccessStatus>("refresh_remote_pairing_token"),
  revokeRemoteDevice: (deviceId: string) =>
    invoke<RemoteAccessStatus>("revoke_remote_device", { deviceId }),
  listScheduledTasks: () => invoke<ScheduledTask[]>("list_scheduled_tasks"),
  createScheduledTask: (input: ScheduledTaskInput) =>
    invoke<ScheduledTask>("create_scheduled_task", { input }),
  updateScheduledTask: (taskId: string, input: ScheduledTaskInput) =>
    invoke<ScheduledTask>("update_scheduled_task", { taskId, input }),
  setScheduledTaskEnabled: (taskId: string, enabled: boolean) =>
    invoke<ScheduledTask>("set_scheduled_task_enabled", { taskId, enabled }),
  acknowledgeScheduledTask: (taskId: string) =>
    invoke<ScheduledTask>("acknowledge_scheduled_task", { taskId }),
  deleteScheduledTask: (taskId: string) =>
    invoke<void>("delete_scheduled_task", { taskId }),
  getAppLocale: () => invoke<AppLocale>("get_app_locale"),
  setAppLocale: (locale: AppLocale) => invoke<AppLocale>("set_app_locale", { locale }),
  getAppTheme: () => invoke<ThemePreference>("get_app_theme"),
  setAppTheme: (theme: ThemePreference) => invoke<ThemePreference>("set_app_theme", { theme }),
  getDisplayScale: () => invoke<DisplayScale>("get_display_scale"),
  setDisplayScale: (displayScale: DisplayScale) =>
    invoke<DisplayScale>("set_display_scale", { displayScale }),
  getComputerControlStatus: () =>
    invoke<ComputerControlStatus>("get_computer_control_settings_status"),
  setComputerControl: (enabled: boolean) =>
    invoke<ComputerControlStatus>("set_computer_control_enabled", { enabled }),
  installComputerControlWaylandHelper: () =>
    invoke<ComputerControlStatus["waylandHelper"]>("install_computer_control_wayland_helper"),
  revokeComputerControlAuthorization: (requestId: string) =>
    invoke<ComputerControlStatus>("revoke_computer_control_authorization", { requestId }),
  respondComputerControlApproval: (requestId: string, allowed: boolean) =>
    invoke<void>("respond_computer_control_approval", { requestId, allowed }),
  getKeepAwakeState: () => invoke<KeepAwakeState>("get_keep_awake_state"),
  setKeepAwakeEnabled: (enabled: boolean) =>
    invoke<KeepAwakeState>("set_keep_awake_enabled", { enabled }),
  getPowerSettings: () => invoke<PowerSettings>("get_power_settings"),
  setPowerSettings: (settings: PowerSettingsInput) =>
    invoke<KeepAwakeState>("set_power_settings", { settings }),
  getHelperStatus: () => invoke<HelperStatus>("get_helper_status"),
  registerKeepAwakeHelper: () => invoke<HelperStatus>("register_keep_awake_helper"),
  getTerminalAcceleratedRendering: () =>
    invoke<boolean>("get_terminal_accelerated_rendering"),
  setTerminalAcceleratedRendering: (enabled: boolean) =>
    invoke<boolean>("set_terminal_accelerated_rendering", { enabled }),
  getTerminalFontSize: () => invoke<number>("get_terminal_font_size"),
  setTerminalFontSize: (fontSize: number) =>
    invoke<number>("set_terminal_font_size", { fontSize }),
  getAgentNotificationSettings: () =>
    invoke<TerminalNotificationSettings>("get_agent_notification_settings"),
  setChatNotificationsEnabled: (enabled: boolean) =>
    invoke<boolean>("set_chat_notifications_enabled", { enabled }),
  setTerminalNotificationsEnabled: (enabled: boolean) =>
    invoke<boolean>("set_terminal_notifications_enabled", { enabled }),
  installTerminalNotificationIntegration: (integration: TerminalNotificationIntegrationId) =>
    invoke<TerminalNotificationSettings>("install_terminal_notification_integration_command", { integration }),
  setNotificationSound: (sound: string) =>
    invoke<string>("set_notification_sound", { sound }),
  previewNotificationSound: (sound: string) =>
    invoke<void>("preview_notification_sound", { sound }),
  showAgentNotification: (title: string, body: string) =>
    invoke<void>("show_agent_notification", { title, body }),
  listWorkspaces: () => invoke<Workspace[]>("list_workspaces"),
  listArchivedWorkspaces: () => invoke<Workspace[]>("list_archived_workspaces"),
  getSshConnectionHome: (connectionId: string) =>
    invoke<SshConnectionTest>("get_ssh_connection_home", { connectionId }),
  listSshDirectories: (connectionId: string, path: string) =>
    invoke<SshRemoteDirectory[]>("list_ssh_directories", { connectionId, path }),
  resolveSshDirectory: (connectionId: string, path: string, parent = false) =>
    invoke<SshRemoteDirectory>("resolve_ssh_directory", { connectionId, path, parent }),
  createSshWorkspace: (
    connectionId: string,
    name: string,
    rootPath: string,
    scanDepth?: number,
  ) => invoke<Workspace>("create_ssh_workspace", {
    connectionId,
    name,
    rootPath,
    scanDepth: scanDepth ?? null,
  }),
  openWorkspace: (path: string, scanDepth?: number) =>
    invoke<Workspace>("open_workspace", {
      path,
      scanDepth: scanDepth ?? null,
    }),
  archiveWorkspace: (workspaceId: string) => invoke<void>("archive_workspace", { workspaceId }),
  restoreWorkspace: (workspaceId: string) => invoke<Workspace>("restore_workspace", { workspaceId }),
  deleteWorkspace: (workspaceId: string) => invoke<void>("delete_workspace", { workspaceId }),
  getRepos: (workspaceId: string) => invoke<Repo[]>("get_repos", { workspaceId }),
  setRepoTrustLevel: (repoId: string, trustLevel: TrustLevel) =>
    invoke<void>("set_repo_trust_level", { repoId, trustLevel }),
  setRepoGitActive: (repoId: string, isActive: boolean) =>
    invoke<void>("set_repo_git_active", { repoId, isActive }),
  setWorkspaceGitActiveRepos: (workspaceId: string, repoIds: string[]) =>
    invoke<void>("set_workspace_git_active_repos", { workspaceId, repoIds }),
  hasWorkspaceGitSelection: (workspaceId: string) =>
    invoke<WorkspaceGitSelectionStatus>("has_workspace_git_selection", { workspaceId }),
  getWorkspaceStartupPreset: (workspaceId: string) =>
    invoke<WorkspaceStartupPreset | null>("get_workspace_startup_preset", { workspaceId }),
  normalizeWorkspaceStartupPreset: (workspaceId: string, preset: WorkspaceStartupPreset) =>
    invoke<WorkspaceStartupPreset>("normalize_workspace_startup_preset", { workspaceId, preset }),
  serializeWorkspaceStartupPreset: (
    workspaceId: string,
    preset: WorkspaceStartupPreset,
    format: WorkspaceStartupPresetFormat,
  ) =>
    invoke<string>("serialize_workspace_startup_preset", { workspaceId, preset, format }),
  normalizeWorkspaceStartupPresetRaw: (
    workspaceId: string,
    format: WorkspaceStartupPresetFormat,
    rawText: string,
  ) =>
    invoke<WorkspaceStartupPreset>("normalize_workspace_startup_preset_raw", {
      workspaceId,
      format,
      rawText,
    }),
  setWorkspaceStartupPreset: (workspaceId: string, preset: WorkspaceStartupPreset) =>
    invoke<WorkspaceStartupPreset>("set_workspace_startup_preset", { workspaceId, preset }),
  setWorkspaceStartupPresetRaw: (
    workspaceId: string,
    format: WorkspaceStartupPresetFormat,
    rawText: string,
  ) =>
    invoke<WorkspaceStartupPreset>("set_workspace_startup_preset_raw", {
      workspaceId,
      format,
      rawText,
    }),
  clearWorkspaceStartupPreset: (workspaceId: string) =>
    invoke<void>("clear_workspace_startup_preset", { workspaceId }),
  exportWorkspaceStartupPreset: (
    workspaceId: string,
    format: WorkspaceStartupPresetFormat,
  ) =>
    invoke<string>("export_workspace_startup_preset", { workspaceId, format }),
  listWorkspaceDirs: (workspaceId: string, dirPath?: string | null) =>
    invoke<FileTreeEntry[]>("list_workspace_dirs", {
      workspaceId,
      dirPath: dirPath ?? null,
    }),
  getWorkspaceFileTreePage: (
    workspaceId: string,
    offset?: number,
    limit?: number,
    refresh?: boolean,
  ) =>
    invoke<FileTreePage>("get_workspace_file_tree_page", {
      workspaceId,
      offset: offset ?? null,
      limit: limit ?? null,
      refresh: refresh ?? null,
    }),
  searchWorkspaceFiles: (
    workspaceId: string,
    query: string,
    offset?: number,
    limit?: number,
    refresh?: boolean,
  ) =>
    invoke<FileTreePage>("search_workspace_files", {
      workspaceId,
      query,
      offset: offset ?? null,
      limit: limit ?? null,
      refresh: refresh ?? null,
    }),
  listThreads: (workspaceId: string) => invoke<Thread[]>("list_threads", { workspaceId }),
  listArchivedThreads: (workspaceId: string) =>
    invoke<Thread[]>("list_archived_threads", { workspaceId }),
  /** 保存线程运行时字段的局部更新；权限必须通过 setThreadPermissions 保存。 */
  updateThread: (update: {
    id: string;
    engineId?: string | null;
    modelId?: string | null;
    planMode?: boolean | null;
    sendMethod?: string | null;
    reasoningEffort?: string | null;
  }) => invoke<Thread>("update_thread", { update }),
  updateThreadRuntimeSelection: (
    threadId: string,
    selection: {
      engineId: string;
      modelId: string;
      planMode?: boolean | null;
      sendMethod?: string | null;
      reasoningEffort?: string | null;
      permissionMode?: string | null;
    },
  ) =>
    invoke<Thread>("update_thread_runtime_selection", {
      threadId,
      engineId: selection.engineId,
      modelId: selection.modelId,
      planMode: selection.planMode ?? null,
      sendMethod: selection.sendMethod ?? null,
      reasoningEffort: selection.reasoningEffort ?? null,
      permissionMode: selection.permissionMode ?? null,
    }),
  listCodexRemoteThreads: (
    workspaceId: string,
    options?: {
      cursor?: string | null;
      limit?: number | null;
      searchTerm?: string | null;
      archived?: boolean | null;
    },
  ) =>
    invoke<CodexRemoteThreadPage>("list_codex_remote_threads", {
      workspaceId,
      cursor: options?.cursor ?? null,
      limit: options?.limit ?? null,
      searchTerm: options?.searchTerm ?? null,
      archived: options?.archived ?? null,
    }),
  // 旧接口由界面传入 modelId，无法代表被导入 Codex 会话的真实模型；禁止恢复。
  // attachCodexRemoteThread: (workspaceId: string, engineThreadId: string, modelId: string) =>
  //   invoke<Thread>("attach_codex_remote_thread", { workspaceId, engineThreadId, modelId }),
  attachCodexRemoteThread: (workspaceId: string, engineThreadId: string) =>
    invoke<Thread>("attach_codex_remote_thread", {
      workspaceId,
      engineThreadId,
    }),
  listOpenCodeRemoteSessions: (
    workspaceId: string,
    options?: {
      cursor?: string | null;
      limit?: number | null;
      searchTerm?: string | null;
      archived?: boolean | null;
    },
  ) =>
    invoke<OpenCodeRemoteSessionPage>("list_opencode_remote_sessions", {
      workspaceId,
      cursor: options?.cursor ?? null,
      limit: options?.limit ?? null,
      searchTerm: options?.searchTerm ?? null,
      archived: options?.archived ?? null,
    }),
  attachOpenCodeRemoteSession: (
    workspaceId: string,
    engineThreadId: string,
    cwd: string,
    modelId: string,
  ) =>
    invoke<Thread>("attach_opencode_remote_session", {
      workspaceId,
      engineThreadId,
      cwd,
      modelId,
    }),
  createThread: (
    workspaceId: string,
    repoId: string | null,
    engineId: string,
    modelId: string,
    title: string,
    reasoningEffort?: string | null,
    serviceTier?: string | null,
  ) =>
    invoke<Thread>("create_thread", {
      workspaceId,
      repoId,
      engineId,
      modelId,
      title,
      reasoningEffort: reasoningEffort ?? null,
      serviceTier: serviceTier ?? null,
    }),
  reconfigureUnstartedThreadRuntime: (
    threadId: string,
    engineId: string,
    modelId: string,
    reasoningEffort?: string | null,
    serviceTier?: string | null,
  ) =>
    invoke<Thread>("reconfigure_unstarted_thread_runtime", {
      threadId,
      engineId,
      modelId,
      reasoningEffort: reasoningEffort ?? null,
      serviceTier: serviceTier ?? null,
    }),
  renameThread: (threadId: string, title: string) =>
    invoke<Thread>("rename_thread", {
      threadId,
      title,
    }),
  confirmWorkspaceThread: (threadId: string, writableRoots: string[]) =>
    invoke<void>("confirm_workspace_thread", { threadId, writableRoots }),
  setThreadReasoningEffort: (
    threadId: string,
    reasoningEffort: string | null,
    modelId?: string | null,
  ) =>
    invoke<void>("set_thread_reasoning_effort", { threadId, reasoningEffort, modelId: modelId ?? null }),
  setSshRemoteThreadSelectedModel: (threadId: string, modelId: string) =>
    invoke<Thread>("set_ssh_remote_thread_selected_model", { threadId, modelId }),
  setThreadExecutionPolicy: (
    threadId: string,
    patch: {
      approvalPolicy?: unknown;
      sandboxMode?: string | null;
      allowNetwork?: boolean | null;
      permissionProfile?: Record<string, unknown> | null;
      approvalsReviewer?: CodexApprovalsReviewer | null;
    },
  ) =>
    invoke<Thread>("set_thread_execution_policy", {
      threadId,
      updateApprovalPolicy: Object.prototype.hasOwnProperty.call(patch, "approvalPolicy"),
      approvalPolicy: patch.approvalPolicy ?? null,
      updateSandboxMode: Object.prototype.hasOwnProperty.call(patch, "sandboxMode"),
      sandboxMode: patch.sandboxMode ?? null,
      updateAllowNetwork: Object.prototype.hasOwnProperty.call(patch, "allowNetwork"),
      allowNetwork: patch.allowNetwork ?? null,
      updatePermissionProfile: Object.prototype.hasOwnProperty.call(patch, "permissionProfile"),
      permissionProfile: patch.permissionProfile ?? null,
      updateApprovalsReviewer: Object.prototype.hasOwnProperty.call(patch, "approvalsReviewer"),
      approvalsReviewer: patch.approvalsReviewer ?? null,
    }),
  /** 读取当前线程由后端适配后的统一权限组件数据。 */
  getThreadPermissions: (threadId: string) =>
    invoke<PermissionComponentJson>("get_thread_permissions", { threadId }),
  /** 保存完整统一权限组件数据，由当前 CLI 实现负责转换为原始权限 JSON。 */
  setThreadPermissions: (threadId: string, values: PermissionComponentJson) =>
    invoke<PermissionComponentJson>("set_thread_permissions", { threadId, values }),
  setThreadCodexConfig: (
    threadId: string,
    patch: {
      personality?: string | null;
      serviceTier?: string | null;
      outputSchema?: unknown;
    },
  ) =>
    invoke<Thread>("set_thread_codex_config", {
      threadId,
      updatePersonality: Object.prototype.hasOwnProperty.call(patch, "personality"),
      personality: patch.personality ?? null,
      updateServiceTier: Object.prototype.hasOwnProperty.call(patch, "serviceTier"),
      serviceTier: patch.serviceTier ?? null,
      updateOutputSchema: Object.prototype.hasOwnProperty.call(patch, "outputSchema"),
      outputSchema: patch.outputSchema ?? null,
    }),
  setThreadOpenCodeConfig: (
    threadId: string,
    patch: {
      agent?: string | null;
    },
  ) =>
    invoke<Thread>("set_thread_opencode_config", {
      threadId,
      updateAgent: Object.prototype.hasOwnProperty.call(patch, "agent"),
      agent: patch.agent ?? null,
    }),
  archiveThread: (threadId: string) => invoke<void>("archive_thread", { threadId }),
  archiveThreadLocally: (threadId: string) =>
    invoke<void>("archive_thread_locally", { threadId }),
  restoreThread: (threadId: string) => invoke<Thread>("restore_thread", { threadId }),
  syncThreadFromEngine: (threadId: string) =>
    invoke<Thread>("sync_thread_from_engine", { threadId }),
  forkCodexThread: (threadId: string) =>
    invoke<Thread>("fork_codex_thread", { threadId }),
  rollbackCodexThread: (threadId: string, numTurns: number) =>
    invoke<Thread>("rollback_codex_thread", { threadId, numTurns }),
  compactCodexThread: (threadId: string) =>
    invoke<Thread>("compact_codex_thread", { threadId }),
  deleteThread: (threadId: string) => invoke<void>("delete_thread", { threadId }),
  getExecutionTarget: (workspaceId?: string | null) =>
    invoke<ExecutionTarget>("get_execution_target", {
      workspaceId: workspaceId ?? null,
    }),
  listActivedClis: (connectionId?: string | null) =>
    invoke<EngineInfo[]>("list_actived_clis", { connectionId: connectionId ?? null }),
  getEngineInfo: (engineId: string, workspaceId?: string | null) =>
    invoke<EngineInfo>("get_engine_info", {
      engineId,
      workspaceId: workspaceId ?? null,
    }),
  getChatProviderUsage: (
    workspaceId?: string | null,
    engineId?: string | null,
  ) =>
    invoke<ChatProviderUsage[]>("get_chat_provider_usage", {
      workspaceId: workspaceId ?? null,
      engineId: engineId ?? null,
    }),
  engineHealth: (engineId: string, workspaceId?: string | null) =>
    invoke<EngineHealth>("engine_health", { engineId, workspaceId: workspaceId ?? null }),
  prewarmEngine: (engineId: string, workspaceId?: string | null) =>
    invoke<void>("prewarm_engine", { engineId, workspaceId: workspaceId ?? null }),
  runEngineCheck: (engineId: string, command: string) =>
    invoke<EngineCheckResult>("run_engine_check", { engineId, command }),
  listCodexSkills: (cwd: string, workspaceId?: string | null) =>
    invoke<CodexSkill[]>("list_codex_skills", {
      cwd,
      workspaceId: workspaceId ?? null,
    }),
  listCodexApps: (workspaceId?: string | null) =>
    invoke<CodexApp[]>("list_codex_apps", {
      workspaceId: workspaceId ?? null,
    }),
  listCodexPlugins: (cwd: string, workspaceId?: string | null) =>
    invoke<CodexPlugin[]>("list_codex_plugins", {
      cwd,
      workspaceId: workspaceId ?? null,
    }),
  getOpenCodeRuntimeCatalog: (cwd: string, workspaceId?: string | null) =>
    invoke<OpenCodeRuntimeCatalog>("get_opencode_runtime_catalog", {
      cwd,
      workspaceId: workspaceId ?? null,
    }),
  getExtensionCatalog: (
    providerId: ExtensionProviderId,
    workspaceId?: string | null,
    cwd?: string | null,
  ) =>
    invoke<ExtensionCatalog>("get_extension_catalog", {
      providerId,
      workspaceId: workspaceId ?? null,
      cwd: cwd ?? null,
    }),
  getCliExtensions: (cliId: string, workspaceId?: string | null) =>
    invoke<ExtensionItem[]>("get_cli_extensions", {
      cliId,
      workspaceId: workspaceId ?? null,
    }),
  scheduleExtensionCatalogWorkspaceRefresh: (workspaceId: string) =>
    invoke<void>("schedule_extension_catalog_workspace_refresh", { workspaceId }),
  requestExtensionCatalogRefresh: (
    providerId: ExtensionProviderId,
    workspaceId?: string | null,
    cwd?: string | null,
    kinds?: ExtensionKind[],
  ) =>
    invoke<ExtensionCatalog>("request_extension_catalog_refresh", {
      providerId,
      workspaceId: workspaceId ?? null,
      cwd: cwd ?? null,
      kinds: kinds ?? null,
    }),
  getExtensionDetails: (
    providerId: ExtensionProviderId,
    workspaceId: string | null | undefined,
    kind: ExtensionKind,
    extensionId: string,
    cwd?: string | null,
  ) =>
    invoke<ExtensionItem>("get_extension_details", {
      providerId,
      workspaceId: workspaceId ?? null,
      kind,
      extensionId,
      cwd: cwd ?? null,
    }),
  performExtensionAction: (
    providerId: ExtensionProviderId,
    workspaceId: string | null | undefined,
    kind: ExtensionKind,
    extensionId: string,
    action: ExtensionAction,
    scope?: string | null,
    cwd?: string | null,
  ) =>
    invoke<ExtensionActionResult>("perform_extension_action", {
      providerId,
      workspaceId: workspaceId ?? null,
      kind,
      extensionId,
      action,
      scope: scope ?? null,
      cwd: cwd ?? null,
    }),
  savePastedImageAttachment: (fileName: string, mimeType: string, dataBase64: string) =>
    invoke<ChatAttachment>("save_pasted_image_attachment", {
      fileName,
      mimeType,
      dataBase64,
    }),
  browserShow: (scope: string, bounds: BrowserBounds, initialUrl?: string | null) =>
    invoke<void>("browser_show", {
      scope,
      bounds,
      initialUrl: initialUrl ?? null,
    }),
  browserSetBounds: (scope: string, bounds: BrowserBounds) =>
    invoke<void>("browser_set_bounds", { scope, bounds }),
  browserHide: (scope: string) => invoke<void>("browser_hide", { scope }),
  browserTransferScope: (fromScope: string, toScope: string) =>
    invoke<void>("browser_transfer_scope", { fromScope, toScope }),
  browserNavigate: (scope: string, url: string) =>
    invoke<string>("browser_navigate", { scope, url }),
  browserReload: (scope: string) => invoke<void>("browser_reload", { scope }),
  browserGoBack: (scope: string) => invoke<void>("browser_go_back", { scope }),
  browserGoForward: (scope: string) => invoke<void>("browser_go_forward", { scope }),
  browserSetAnnotationEnabled: (scope: string, enabled: boolean) =>
    invoke<void>("browser_set_annotation_enabled", { scope, enabled }),
  browserClearPendingAnnotation: (scope: string) =>
    invoke<void>("browser_clear_pending_annotation", { scope }),
  browserClearAllAnnotations: (scope: string) =>
    invoke<void>("browser_clear_all_annotations", { scope }),
  browserCaptureAnnotation: (
    scope: string,
    number: number,
    selection: BrowserAnnotationSelection,
  ) =>
    invoke<BrowserAnnotationAttachment>("browser_capture_annotation", { scope, number, selection }),
  readAttachmentPreview: (filePath: string, mimeType?: string | null) =>
    invoke<AttachmentPreview | null>("read_attachment_preview", {
      filePath,
      mimeType: mimeType ?? null,
    }),
  sendMessage: (
    threadId: string,
    message: string,
    modelId?: string | null,
    reasoningEffort?: string | null,
    attachments?: ChatAttachment[] | null,
    inputItems?: ChatInputItem[] | null,
    planMode?: boolean | null,
    clientTurnId?: string | null,
    referencedThreadId?: string | null,
  ) =>
    invoke<string>("send_message", {
      threadId,
      message,
      modelId: modelId ?? null,
      reasoningEffort: reasoningEffort ?? null,
      attachments: attachments ?? null,
      inputItems: inputItems ?? null,
      planMode: planMode ?? null,
      clientTurnId: clientTurnId ?? null,
      referencedThreadId: referencedThreadId ?? null,
    }),
  steerMessage: (
    threadId: string,
    message: string,
    attachments?: ChatAttachment[] | null,
    inputItems?: ChatInputItem[] | null,
    planMode?: boolean | null,
    clientSteerId?: string | null,
    referencedThreadId?: string | null,
  ) =>
    invoke<SteerReceipt>("steer_message", {
      threadId,
      message,
      attachments: attachments ?? null,
      inputItems: inputItems ?? null,
      planMode: planMode ?? null,
      clientSteerId: clientSteerId ?? null,
      referencedThreadId: referencedThreadId ?? null,
    }),
  startCodexReview: (
    threadId: string,
    target: CodexReviewTarget,
    delivery: CodexReviewDelivery,
  ) =>
    invoke<Thread>("start_codex_review", {
      threadId,
      target,
      delivery,
    }),
  cancelTurn: (threadId: string) => invoke<void>("cancel_turn", { threadId }),
  restartRemoteCliService: (threadId: string) =>
    invoke<void>("restart_remote_cli_service", { threadId }),
  respondApproval: (threadId: string, approvalId: string, response: ApprovalResponse) =>
    invoke<void>("respond_to_approval", { threadId, approvalId, response }),
  getThreadMessages: (threadId: string) =>
    invoke<Message[]>("get_thread_messages", { threadId }),
  getThreadMessagesWindow: (
    threadId: string,
    cursor?: MessageWindowCursor | null,
    limit?: number | null,
  ) =>
    invoke<MessageWindow>("get_thread_messages_window", {
      threadId,
      cursor: cursor ?? null,
      limit: limit ?? null,
    }),
  getMessageBlocks: (messageId: string) =>
    invoke<ContentBlock[] | null>("get_message_blocks", { messageId }),
  getActionOutput: (messageId: string, actionId: string) =>
    invoke<ActionOutputPayload>("get_action_output", { messageId, actionId }),
  searchMessages: (workspaceId: string, query: string) =>
    invoke<SearchResult[]>("search_messages", {
      workspaceId,
      query
    }),
  getGitStatus: (repoPath: string) => invoke<GitStatus>("get_git_status", { repoPath }),
  getFileDiff: (repoPath: string, filePath: string, staged: boolean) =>
    invoke<GitDiffPreview>("get_file_diff", { repoPath, filePath, staged }),
  getGitFileCompare: (
    repoPath: string,
    filePath: string,
    source: GitCompareSource,
  ) =>
    invoke<GitFileCompare>("get_git_file_compare", {
      repoPath,
      filePath,
      source,
    }),
  getFileTree: (repoPath: string) => invoke<FileTreeEntry[]>("get_file_tree", { repoPath }),
  getFileTreePage: (repoPath: string, offset?: number, limit?: number) =>
    invoke<FileTreePage>("get_file_tree_page", { repoPath, offset: offset ?? null, limit: limit ?? null }),
  listDir: (
    repoPath: string,
    dirPath: string,
    workspaceId?: string | null,
  ) =>
    invoke<FileTreeEntry[]>("list_dir", {
      repoPath,
      dirPath,
      workspaceId: workspaceId ?? null,
    }),
  createFile: (repoPath: string, filePath: string, workspaceId?: string | null) =>
    invoke<void>("create_file", { repoPath, filePath, workspaceId: workspaceId ?? null }),
  createDir: (repoPath: string, dirPath: string, workspaceId?: string | null) =>
    invoke<void>("create_dir", { repoPath, dirPath, workspaceId: workspaceId ?? null }),
  renamePath: (repoPath: string, oldPath: string, newName: string, workspaceId?: string | null) =>
    invoke<void>("rename_path", { repoPath, oldPath, newName, workspaceId: workspaceId ?? null }),
  deletePath: (repoPath: string, filePath: string, workspaceId?: string | null) =>
    invoke<void>("delete_path", { repoPath, filePath, workspaceId: workspaceId ?? null }),
  stageFiles: (repoPath: string, files: string[]) => invoke<void>("stage_files", { repoPath, files }),
  unstageFiles: (repoPath: string, files: string[]) =>
    invoke<void>("unstage_files", { repoPath, files }),
  revealPath: (path: string) => invoke<void>("reveal_path", { path }),
  openContainingDirectory: (path: string) =>
    invoke<void>("open_containing_directory", { path }),
  openPathWithDefaultApp: (path: string) =>
    invoke<void>("open_path_with_default_app", { path }),
  openPathWithTextEditor: (path: string, editorId: string | null) =>
    invoke<void>("open_path_with_text_editor", { path, editorId }),
  saveFileAs: (sourcePath: string, destinationPath: string) =>
    invoke<void>("save_file_as", { sourcePath, destinationPath }),
  readTextFileForClipboard: (path: string) =>
    invoke<string | null>("read_text_file_for_clipboard", { path }),
  getDefaultFileOpenTarget: () =>
    invoke<DefaultFileOpenTarget>("get_default_file_open_target"),
  setDefaultFileOpenTarget: (editorId: string | null) =>
    invoke<string | null>("set_default_file_open_target", { editorId }),
  discardFiles: (repoPath: string, files: string[]) =>
    invoke<void>("discard_files", { repoPath, files }),
  commit: (repoPath: string, message: string) => invoke<string>("commit", { repoPath, message }),
  softResetLastCommit: (repoPath: string) =>
    invoke<void>("soft_reset_last_commit", { repoPath }),
  fetchGit: (repoPath: string) => invoke<void>("fetch_git", { repoPath }),
  pullGit: (repoPath: string) => invoke<void>("pull_git", { repoPath }),
  pushGit: (repoPath: string) => invoke<void>("push_git", { repoPath }),
  listGitBranches: (repoPath: string, scope: GitBranchScope, offset?: number, limit?: number, search?: string) =>
    invoke<GitBranchPage>("list_git_branches", {
      repoPath,
      scope,
      offset: offset ?? null,
      limit: limit ?? null,
      search: search ?? null,
    }),
  checkoutGitBranch: (repoPath: string, branchName: string, isRemote: boolean) =>
    invoke<void>("checkout_git_branch", { repoPath, branchName, isRemote }),
  createGitBranch: (repoPath: string, branchName: string, fromRef?: string | null) =>
    invoke<void>("create_git_branch", { repoPath, branchName, fromRef: fromRef ?? null }),
  renameGitBranch: (repoPath: string, oldName: string, newName: string) =>
    invoke<void>("rename_git_branch", { repoPath, oldName, newName }),
  deleteGitBranch: (repoPath: string, branchName: string, force: boolean) =>
    invoke<void>("delete_git_branch", { repoPath, branchName, force }),
  listGitCommits: (repoPath: string, offset?: number, limit?: number) =>
    invoke<GitCommitPage>("list_git_commits", {
      repoPath,
      offset: offset ?? null,
      limit: limit ?? null,
    }),
  getCommitDiff: (repoPath: string, commitHash: string) =>
    invoke<GitDiffPreview>("get_commit_diff", { repoPath, commitHash }),
  listGitStashes: (repoPath: string) =>
    invoke<GitStash[]>("list_git_stashes", { repoPath }),
  pushGitStash: (repoPath: string, message?: string) =>
    invoke<void>("push_git_stash", { repoPath, message: message ?? null }),
  applyGitStash: (repoPath: string, stashIndex: number) =>
    invoke<void>("apply_git_stash", { repoPath, stashIndex }),
  popGitStash: (repoPath: string, stashIndex: number) =>
    invoke<void>("pop_git_stash", { repoPath, stashIndex }),
  readFile: (
    repoPath: string,
    filePath: string,
    workspaceId?: string | null,
  ) =>
    invoke<ReadFileResult>("read_file", {
      repoPath,
      filePath,
      workspaceId: workspaceId ?? null,
    }),
  getFileVersion: (
    repoPath: string,
    filePath: string,
    workspaceId?: string | null,
  ) => invoke<string>("get_file_version", {
    repoPath,
    filePath,
    workspaceId: workspaceId ?? null,
  }),
  getDirectoryFingerprint: (
    repoPath: string,
    dirPath: string,
    workspaceId?: string | null,
  ) => invoke<string>("get_directory_fingerprint", {
    repoPath,
    dirPath,
    workspaceId: workspaceId ?? null,
  }),
  resolveEditorFileReference: (
    workspaceId: string,
    rawReference: string,
    preferredRepoPath?: string | null,
    currentCwd?: string | null,
  ) =>
    invoke<ResolvedEditorFileReference | null>("resolve_editor_file_reference", {
      workspaceId,
      rawReference,
      preferredRepoPath: preferredRepoPath ?? null,
      currentCwd: currentCwd ?? null,
    }),
  writeFile: (
    repoPath: string,
    filePath: string,
    content: string,
    workspaceId?: string | null,
    expectedVersion?: string | null,
  ) => invoke<WriteFileResult>("write_file", {
    repoPath,
    filePath,
    content,
    workspaceId: workspaceId ?? null,
    expectedVersion: expectedVersion ?? null,
  }),
  watchGitRepo: (repoPath: string) => invoke<void>("watch_git_repo", { repoPath }),
  addGitWorktree: (repoPath: string, worktreePath: string, branchName: string, baseRef?: string | null) =>
    invoke<GitWorktree>("add_git_worktree", { repoPath, worktreePath, branchName, baseRef: baseRef ?? null }),
  listGitWorktrees: (repoPath: string) =>
    invoke<GitWorktree[]>("list_git_worktrees", { repoPath }),
  removeGitWorktree: (
    repoPath: string,
    worktreePath: string,
    force: boolean,
    branchName?: string | null,
    deleteBranch?: boolean,
  ) =>
    invoke<void>("remove_git_worktree", {
      repoPath,
      worktreePath,
      force,
      branchName: branchName ?? null,
      deleteBranch: deleteBranch ?? false,
    }),
  pruneGitWorktrees: (repoPath: string) =>
    invoke<void>("prune_git_worktrees", { repoPath }),
  initGitRepo: (repoPath: string, validateOnly?: boolean) =>
    invoke<GitInitRepoStatus>("init_git_repo", {
      repoPath,
      validateOnly: validateOnly ?? null,
    }),
  listGitRemotes: (repoPath: string) =>
    invoke<GitRemote[]>("list_git_remotes", { repoPath }),
  addGitRemote: (repoPath: string, name: string, url: string) =>
    invoke<void>("add_git_remote", { repoPath, name, url }),
  removeGitRemote: (repoPath: string, name: string) =>
    invoke<void>("remove_git_remote", { repoPath, name }),
  renameGitRemote: (repoPath: string, oldName: string, newName: string) =>
    invoke<void>("rename_git_remote", { repoPath, oldName, newName }),
  terminalCreateSession: (workspaceId: string, cols: number, rows: number, cwd?: string | null) =>
    invoke<TerminalSession>("terminal_create_session", { workspaceId, cols, rows, cwd: cwd ?? null }),
  terminalWrite: (workspaceId: string, sessionId: string, data: string) =>
    invoke<void>("terminal_write", { workspaceId, sessionId, data }),
  terminalWriteBytes: (workspaceId: string, sessionId: string, data: number[]) =>
    invoke<void>("terminal_write_bytes", { workspaceId, sessionId, data }),
  terminalResize: (
    workspaceId: string,
    sessionId: string,
    cols: number,
    rows: number,
    pixelWidth: number = 0,
    pixelHeight: number = 0,
  ) =>
    invoke<void>("terminal_resize", {
      workspaceId,
      sessionId,
      cols,
      rows,
      pixelWidth,
      pixelHeight,
    }),
  terminalCloseSession: (workspaceId: string, sessionId: string) =>
    invoke<void>("terminal_close_session", { workspaceId, sessionId }),
  terminalCloseWorkspaceSessions: (workspaceId: string) =>
    invoke<void>("terminal_close_workspace_sessions", { workspaceId }),
  terminalListSessions: (workspaceId: string) =>
    invoke<TerminalSession[]>("terminal_list_sessions", { workspaceId }),
  terminalGetRendererDiagnostics: (workspaceId: string, sessionId: string) =>
    invoke<TerminalRendererDiagnostics>("terminal_get_renderer_diagnostics", {
      workspaceId,
      sessionId,
    }),
  terminalResumeSession: (
    workspaceId: string,
    sessionId: string,
    fromSeq?: number | null,
  ) =>
    invoke<TerminalResumeSession>("terminal_resume_session", {
      workspaceId,
      sessionId,
      fromSeq: fromSeq ?? null,
    }),
  terminalDrainOutput: (
    workspaceId: string,
    sessionId: string,
    fromSeq: number | null,
    targetBytes: number,
  ) =>
    invoke<TerminalResumeSession>("terminal_drain_output", {
      workspaceId,
      sessionId,
      fromSeq,
      targetBytes,
    }),
  terminalListNotifications: (workspaceId: string) =>
    invoke<TerminalNotification[]>("terminal_list_notifications", { workspaceId }),
  terminalClearNotification: (workspaceId: string, sessionId?: string | null) =>
    invoke<void>("terminal_clear_notification", { workspaceId, sessionId: sessionId ?? null }),
  terminalSetNotificationFocus: (
    workspaceId: string | null,
    sessionId: string | null,
    windowFocused: boolean,
  ) =>
    invoke<void>("terminal_set_notification_focus", {
      workspaceId: workspaceId ?? null,
      sessionId: sessionId ?? null,
      windowFocused,
    }),
  checkDependencies: async () =>
    normalizeDependencyReport(
      await invoke<Partial<DependencyReport> | null>("check_dependencies"),
    ),
  installDependency: (dependency: string, method: string) =>
    invoke<InstallResult>("install_dependency", { dependency, method }),
  checkHarnesses: () => invoke<HarnessReport>("check_harnesses"),
  installHarness: (harnessId: string) =>
    invoke<InstallResult>("install_harness", { harnessId }),
  launchHarness: (harnessId: string) =>
    invoke<string>("launch_harness", { harnessId }),
  getHarnessLaunchArgs: () =>
    invoke<Record<string, string>>("get_harness_launch_args"),
  setHarnessLaunchArgs: (harnessId: string, args: string) =>
    invoke<string>("set_harness_launch_args", { harnessId, args }),
  // 旧契约只有 boolean，无法区分“正常无变化”和“异常导致未变化”：
  // refreshLocalCliHealth: () => invoke<boolean>("refresh_local_cli_health"),
  refreshLocalCliHealth: () =>
    invoke<CliHealthReconcileResult>("refresh_local_cli_health"),
  getDefaultAutonomyPreset: () =>
    invoke<string | null>("get_default_autonomy_preset"),
  setDefaultAutonomyPreset: (preset: string | null) =>
    invoke<string | null>("set_default_autonomy_preset", { preset }),
  codexUsesExternalSandbox: (workspaceId?: string | null) =>
    invoke<boolean>("codex_uses_external_sandbox", { workspaceId: workspaceId ?? null }),
};

export async function listenThreadEvents(
  threadId: string,
  onEvent: (event: StreamEvent) => void
): Promise<UnlistenFn> {
  return listen<StreamEvent>(`stream-event-${threadId}`, ({ payload }) => onEvent(payload));
}

export interface CliServiceRestartRequiredEvent {
  threadId: string;
  workspaceId: string;
  engineId: string;
  threadTitle: string;
  connectionId: string;
  reason: string;
}

export async function listenCliServiceRestartRequired(
  onEvent: (event: CliServiceRestartRequiredEvent) => void,
): Promise<UnlistenFn> {
  return listen<CliServiceRestartRequiredEvent>(
    "chat-cli-service-restart-required",
    ({ payload }) => onEvent(payload),
  );
}

export interface GitRepoChangedEvent {
  repoPath: string;
}

export async function listenGitRepoChanged(
  onEvent: (event: GitRepoChangedEvent) => void
): Promise<UnlistenFn> {
  return listen<GitRepoChangedEvent>("git-repo-changed", ({ payload }) => onEvent(payload));
}

export interface ThreadUpdatedEvent {
  threadId: string;
  workspaceId: string;
  thread?: Thread | null;
}

export const SSH_REMOTE_PROJECT_SESSIONS_REFRESHED_EVENT =
  "ssh-remote-project-sessions-refreshed";

export const APP_STARTUP_PROGRESS_EVENT = "app-startup-progress";

export type AppStartupPhase =
  | "loading-base-data"
  | "connecting-ssh"
  | "creating-cli-tunnels"
  | "starting-cli-services"
  | "syncing-remote-sessions"
  | "completed";

export interface AppStartupProgressEvent {
  phase: AppStartupPhase;
  message: string;
}

export async function listenAppStartupProgress(
  onEvent: (event: AppStartupProgressEvent) => void,
): Promise<UnlistenFn> {
  return listen<AppStartupProgressEvent>(
    APP_STARTUP_PROGRESS_EVENT,
    ({ payload }) => onEvent(payload),
  );
}

export interface SshRemoteProjectSessionsRefreshedEvent {
  /** 收到同步通知的 SSH workspace 标识。 */
  workspaceId: string;
  /** 已成功从远端 CLI 同步会话的 CLI 标识。 */
  succeededCliIds: string[];
  /** 同步失败的远端 CLI 标识。 */
  failedCliIds: string[];
}

export async function listenSshRemoteProjectSessionsRefreshed(
  onEvent: (event: SshRemoteProjectSessionsRefreshedEvent) => void,
): Promise<UnlistenFn> {
  return listen<SshRemoteProjectSessionsRefreshedEvent>(
    SSH_REMOTE_PROJECT_SESSIONS_REFRESHED_EVENT,
    ({ payload }) => onEvent(payload),
  );
}

export const CLI_SERVICES_UPDATED_EVENT = "cli-services-updated";

export interface CliHealthReconcileResult {
  /** 本次检查是否已经成功改变 CLI 生命周期登记。 */
  changed: boolean;
  /** 阻止某项生命周期登记变化完成的异常；空数组表示没有异常。 */
  errors: string[];
}

export interface CliServicesUpdatedEvent {
  /** 发生变化的范围：local 表示本机，ssh 表示指定远端连接。 */
  scope: "local" | "ssh";
  /** scope 为 ssh 时的 SSH 连接配置标识。 */
  connectionId: string | null;
  /** 单调递增的事件序号，用于识别乱序事件。 */
  revision: number;
  /** 本次检查是否已经成功改变 CLI 生命周期登记。 */
  changed: boolean;
  /** 健康检查异常的明确业务信号。 */
  errors: string[];
}

export async function listenCliServicesUpdated(
  onEvent: (event: CliServicesUpdatedEvent) => void,
): Promise<UnlistenFn> {
  return listen<CliServicesUpdatedEvent>(
    CLI_SERVICES_UPDATED_EVENT,
    ({ payload }) => onEvent(payload),
  );
}

export interface CodexRemoteThreadRemovedEvent {
  thread: Thread;
  remoteAction: "archived" | "deleted";
}

export interface ChatTurnFinishedEvent {
  threadId: string;
  workspaceId: string;
  engineId: ChatEngineId;
  threadTitle: string;
  status: "completed" | "interrupted" | "error";
  preview?: string | null;
}

export interface ExtensionCatalogUpdatedEvent {
  providerId: ExtensionProviderId;
  cwd?: string | null;
}

export async function listenExtensionCatalogUpdated(
  onEvent: (event: ExtensionCatalogUpdatedEvent) => void,
): Promise<UnlistenFn> {
  return listen<ExtensionCatalogUpdatedEvent>(
    "extension-catalog-updated",
    ({ payload }) => onEvent(payload),
  );
}

export interface ScheduledTaskChangedEvent {
  taskId: string;
}

export async function listenScheduledTaskUpdated(
  onEvent: (event: ScheduledTaskChangedEvent) => void,
): Promise<UnlistenFn> {
  return listen<ScheduledTaskChangedEvent>(
    "scheduled-task-updated",
    ({ payload }) => onEvent(payload),
  );
}

export async function listenScheduledTaskDeleted(
  onEvent: (event: ScheduledTaskChangedEvent) => void,
): Promise<UnlistenFn> {
  return listen<ScheduledTaskChangedEvent>(
    "scheduled-task-deleted",
    ({ payload }) => onEvent(payload),
  );
}

export async function listenThreadUpdated(
  onEvent: (event: ThreadUpdatedEvent) => void
): Promise<UnlistenFn> {
  return listen<ThreadUpdatedEvent>("thread-updated", ({ payload }) => onEvent(payload));
}

export async function listenCodexRemoteThreadRemoved(
  onEvent: (event: CodexRemoteThreadRemovedEvent) => void,
): Promise<UnlistenFn> {
  return listen<CodexRemoteThreadRemovedEvent>("codex-remote-thread-removed", ({ payload }) => onEvent(payload));
}

export async function listenChatTurnFinished(
  onEvent: (event: ChatTurnFinishedEvent) => void
): Promise<UnlistenFn> {
  return listen<ChatTurnFinishedEvent>("chat-turn-finished", ({ payload }) => onEvent(payload));
}

export interface ChatApprovalRequestedEvent {
  threadId: string;
  workspaceId: string;
  engineId: ChatEngineId;
  threadTitle: string;
  summary: string;
}

export async function listenChatApprovalRequested(
  onEvent: (event: ChatApprovalRequestedEvent) => void
): Promise<UnlistenFn> {
  return listen<ChatApprovalRequestedEvent>(
    "chat-approval-requested",
    ({ payload }) => onEvent(payload)
  );
}

export async function listenComputerControlApprovalRequested(
  onEvent: (event: ComputerControlApprovalRequest) => void,
): Promise<UnlistenFn> {
  return listen<ComputerControlApprovalRequest>(
    "computer-control-approval-requested",
    ({ payload }) => onEvent(payload),
  );
}

export async function listenEngineRuntimeUpdated(
  onEvent: (event: EngineRuntimeUpdatedEvent) => void
): Promise<UnlistenFn> {
  return listen<EngineRuntimeUpdatedEvent>(
    "engine-runtime-updated",
    ({ payload }) => onEvent(payload)
  );
}

export async function listenMenuAction(
  onEvent: (action: string) => void
): Promise<UnlistenFn> {
  return listen<string>("menu-action", ({ payload }) => onEvent(payload));
}

export async function listenTerminalOutput(
  workspaceId: string,
  onEvent: (event: TerminalOutputReadyEvent) => void
): Promise<UnlistenFn> {
  return listen<TerminalOutputReadyEvent>(
    `terminal-output-${workspaceId}`,
    ({ payload }) => onEvent(payload)
  );
}

export async function listenInstallProgress(
  onEvent: (event: InstallProgressEvent) => void
): Promise<UnlistenFn> {
  return listen<InstallProgressEvent>("setup-install-progress", ({ payload }) => onEvent(payload));
}

export async function listenTerminalExit(
  workspaceId: string,
  onEvent: (event: TerminalExitEvent) => void
): Promise<UnlistenFn> {
  return listen<TerminalExitEvent>(
    `terminal-exit-${workspaceId}`,
    ({ payload }) => onEvent(payload)
  );
}

export async function listenTerminalForegroundChanged(
  workspaceId: string,
  onEvent: (event: TerminalForegroundChangedEvent) => void
): Promise<UnlistenFn> {
  return listen<TerminalForegroundChangedEvent>(
    `terminal-fg-changed-${workspaceId}`,
    ({ payload }) => onEvent(payload)
  );
}

export async function listenTerminalNotification(
  workspaceId: string,
  onEvent: (event: TerminalNotification) => void
): Promise<UnlistenFn> {
  return listen<TerminalNotification>(
    `terminal-notification-${workspaceId}`,
    ({ payload }) => onEvent(payload)
  );
}

export async function listenTerminalNotificationCleared(
  workspaceId: string,
  onEvent: (event: TerminalNotificationClearedEvent) => void
): Promise<UnlistenFn> {
  return listen<TerminalNotificationClearedEvent>(
    `terminal-notification-cleared-${workspaceId}`,
    ({ payload }) => onEvent(payload)
  );
}

/**
 * Write a command to a newly created terminal session once the shell is ready.
 * Waits for terminal output (indicating the shell prompt), then writes.
 * Falls back to writing after a timeout if no output is detected.
 */
export async function writeCommandToNewSession(
  workspaceId: string,
  sessionId: string,
  command: string,
): Promise<void> {
  const FALLBACK_TIMEOUT_MS = 3000;
  const POST_OUTPUT_DELAY_MS = 50;

  return new Promise<void>((resolve) => {
    let settled = false;
    let unlisten: (() => void) | undefined;

    const doWrite = () => {
      if (settled) return;
      settled = true;
      unlisten?.();
      invoke<void>("terminal_write", {
        workspaceId,
        sessionId,
        data: command + "\r",
      })
        .catch(() => {})
        .finally(resolve);
    };

    const fallbackTimer = setTimeout(doWrite, FALLBACK_TIMEOUT_MS);

    listen<TerminalOutputReadyEvent>(
      `terminal-output-${workspaceId}`,
      ({ payload }) => {
        if (settled || payload.sessionId !== sessionId) return;
        clearTimeout(fallbackTimer);
        setTimeout(doWrite, POST_OUTPUT_DELAY_MS);
      },
    ).then((fn) => {
      if (settled) {
        fn();
      } else {
        unlisten = fn;
      }
    });
  });
}
