use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod chat;

/// SSH 数据库连接记录及其未向前端暴露的主机密钥信息。
#[derive(Debug, Clone)]
pub struct SshConnectionRecord {
    /// SSH 连接的业务展示与配置数据。
    pub dto: SshConnectionDto,
    /// SSH 主机密钥的 Base64 编码，用于连接时校验远端身份。
    pub host_key_base64: String,
}

/// SSH 连接尚未建立或未检测时的状态。
pub const STATUS_UNKNOWN: &str = "unknown";
/// SSH 连接正在建立时的状态。
pub const STATUS_CONNECTING: &str = "connecting";
/// SSH 连接可用时的状态。
pub const STATUS_OK: &str = "ok";
/// SSH 连接建立或检测失败时的状态。
pub const STATUS_FAILED: &str = "failed";
/// SSH 连接被用户禁用时的状态。
pub const STATUS_DISABLED: &str = "disabled";
/// SSH 连接被软删除时的状态。
pub const STATUS_DELETED: &str = "deleted";

/// 本机或 SSH 工作区当前解析出的文件访问目标。
#[derive(Debug, Clone)]
pub struct WorkspaceTarget {
    /// 工作区的持久化业务信息。
    pub workspace: WorkspaceDto,
    /// SSH 工作区对应的连接记录；本机工作区没有连接。
    pub connection: Option<SshConnectionRecord>,
}

impl WorkspaceTarget {
    /// 判断工作区文件访问是否需要走 SSH。
    pub fn is_remote(&self) -> bool {
        self.workspace.location_kind == "ssh"
    }

    /// 取得 SSH 工作区的连接记录，缺失时保留原业务错误。
    pub fn remote_connection(&self) -> anyhow::Result<&SshConnectionRecord> {
        self.connection
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("远端项目未绑定 SSH 连接"))
    }
}

/// 消息窗口及其数据库行号，供远程分页协议保持消息与 row id 配对。
#[derive(Debug, Clone)]
pub struct MessageWindowWithRowIds {
    /// 按窗口顺序返回的消息分页。
    pub window: MessageWindowDto,
    /// 与消息一一对应的数据库行号。
    pub row_ids: Vec<i64>,
}

/// 尚未开始的线程运行时选择结果。
#[derive(Debug, Clone)]
pub struct ThreadRuntimeSelection {
    /// 被选中的 CLI 引擎标识。
    pub engine_id: String,
    /// 被选中的 CLI 模型标识。
    pub model_id: String,
    /// 被选中的推理强度；未设置时为 None。
    pub reasoning_effort: Option<String>,
}

/// 扩展目录后台刷新的 provider、上下文和类型集合。
#[derive(Debug, Clone)]
pub struct ExtensionRefreshJob {
    /// 负责刷新的 CLI provider 标识。
    pub provider_id: String,
    /// 扩展缓存对应的上下文键。
    pub context_key: String,
    /// 本次刷新的扩展类型列表。
    pub kinds: Vec<String>,
}

/// 扩展目录刷新任务在数据库中的单类型记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionRefreshTarget {
    /// 持有该扩展目录的 CLI provider 标识。
    pub provider_id: String,
    /// 扩展目录对应的工作区或全局上下文键。
    pub context_key: String,
    /// 本条刷新任务对应的扩展类型。
    pub kind: String,
}

/// 扩展目录缓存的一类快照及刷新状态。
#[derive(Debug, Clone)]
pub struct ExtensionCatalogSnapshot {
    /// 当前快照对应的扩展类型。
    pub kind: String,
    /// 缓存中的扩展项目列表。
    pub items: Vec<ExtensionItemDto>,
    /// 最近一次成功获取扩展目录的时间。
    pub fetched_at: Option<String>,
    /// 最近一次尝试刷新扩展目录的时间。
    pub last_attempt_at: Option<String>,
    /// 下一次允许刷新的时间。
    pub next_refresh_at: Option<String>,
    /// 最近一次刷新失败的错误文本。
    pub last_error: Option<String>,
}

/// 当前支持的扩展目录类型。
pub const EXTENSION_KINDS: [&str; 3] = ["skill", "plugin", "mcp"];

/// 前端选择文本编辑器时使用的应用信息。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextEditorApplicationDto {
    /// 已安装文本编辑器的稳定标识。
    pub id: String,
    /// 展示给用户的文本编辑器名称。
    pub name: String,
}

/// 前端展示默认文件打开目标与可选文本编辑器的结果。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultFileOpenTargetDto {
    /// 当前默认选择的文本编辑器标识。
    pub selected_editor_id: Option<String>,
    /// 当前系统可供选择的文本编辑器列表。
    pub applications: Vec<TextEditorApplicationDto>,
}

/// 定时任务创建和更新时用于兼容前端 JSON 契约的输入对象。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskInput {
    /// 定时任务执行时提供给 CLI 的任务描述。
    pub description: String,
    /// 用户是否启用该定时任务；省略时默认启用。
    #[serde(default = "scheduled_task_default_enabled")]
    pub enabled: bool,
    /// 定时任务执行设备标识；当前业务默认使用本机设备。
    #[serde(default = "scheduled_task_default_device_id")]
    pub execution_device_id: String,
    /// 定时任务目标类型，表示复用现有线程或创建新线程。
    pub target_type: String,
    /// 定时任务所属工作区标识。
    pub workspace_id: String,
    /// 复用现有线程时选中的线程标识。
    pub thread_id: Option<String>,
    /// 定时任务执行所需的 CLI、模型和推理运行配置。
    pub runtime_config: Option<Value>,
    /// 定时任务的周期类型。
    pub schedule_type: String,
    /// 定时任务周期规则的原始 JSON 配置。
    pub schedule: Value,
    /// 定时任务使用的时区名称。
    pub timezone: String,
}

/// 定时任务保存和调度执行共用的 CLI 运行配置快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledRuntimeConfig {
    /// 负责执行该定时任务的 CLI 标识。
    pub engine_id: String,
    /// 当前 CLI 任务使用的模型标识。
    pub model_id: String,
    /// 当前 CLI 任务使用的推理强度。
    pub reasoning_effort: Option<String>,
    /// Codex 任务使用的服务档位。
    pub service_tier: Option<String>,
}

/// 为省略 enabled 的定时任务输入提供启用默认值。
fn scheduled_task_default_enabled() -> bool {
    true
}

/// 为省略 executionDeviceId 的定时任务输入提供本机设备默认值。
fn scheduled_task_default_device_id() -> String {
    "local".to_string()
}

/// 表示前端展示、编辑并由数据库保存的统一线程权限对象，不包含 CLI 原生权限。
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionComponentJson {
    /// 前端套餐展示记录；空数组表示自定义，CLI 权限仅按细分项解析。。
    pub autonomy_preset: Vec<String>,
    /// 线程执行时采用的统一信任等级选择。
    pub trust: Vec<String>,
    /// 线程操作审批的统一策略选择。
    pub approval: Vec<String>,
    /// 线程文件系统访问的统一沙箱选择。
    pub sandbox: Vec<String>,
    /// 线程联网能力的统一选择。
    pub network: Vec<String>,
    /// 新线程默认统一权限预设；空数组表示未设置。
    pub default_for_new_threads: Vec<String>,
}

impl Default for PermissionComponentJson {
    /// 构造固定六字段的统一默认权限对象。
    fn default() -> Self {
        Self {
            autonomy_preset: vec!["automatic".to_owned()],
            trust: vec!["automatic".to_owned()],
            approval: vec!["automatic".to_owned()],
            sandbox: vec!["automatic".to_owned()],
            network: vec!["automatic".to_owned()],
            default_for_new_threads: Vec::new(),
        }
    }
}

impl PermissionComponentJson {
    /// 校验统一权限对象每个字段只有一个受支持选择，或按契约为空。
    pub fn validate(&self) -> anyhow::Result<()> {
        let fields = [
            (
                "autonomyPreset",
                &self.autonomy_preset,
                &["automatic", "read-only", "ask", "auto", "full"][..],
            ),
            (
                "trust",
                &self.trust,
                &["automatic", "trusted", "standard", "restricted"][..],
            ),
            (
                "approval",
                &self.approval,
                &["automatic", "restricted", "ask", "autonomous"][..],
            ),
            (
                "sandbox",
                &self.sandbox,
                &["automatic", "read-only", "workspace-write", "full-access"][..],
            ),
            (
                "network",
                &self.network,
                &["automatic", "enabled", "restricted"][..],
            ),
        ];

        // 校验固定组件字段的数量和值域，避免保存 CLI 无关契约之外的数据。
        for (field_name, values, allowed) in fields {
            if values.len() > 1 || values.iter().any(|value| !allowed.contains(&value.as_str())) {
                anyhow::bail!("统一权限字段 {field_name} 值无效: {values:?}");
            }
        }

        // 新线程默认值只允许不设置或保存一个已知统一预设。
        if self.default_for_new_threads.len() > 1
            || self
                .default_for_new_threads
                .iter()
                .any(|value| !["automatic", "read-only", "ask", "auto", "full"].contains(&value.as_str()))
        {
            anyhow::bail!(
                "统一权限字段 defaultForNewThreads 值无效: {:?}",
                self.default_for_new_threads
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceDto {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub location_kind: String,
    pub ssh_connection_id: Option<String>,
    pub connection_display_name: Option<String>,
    pub connection_enabled: Option<bool>,
    pub connection_deleted: Option<bool>,
    pub connection_status: Option<String>,
    /// 项目级信任等级，决定项目内引擎的权限策略。
    pub trust_level: TrustLevelDto,
    pub created_at: String,
    pub last_opened_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshRemoteDirectoryDto {
    pub path: String,
    pub name: String,
    pub readable: bool,
    pub enterable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectionDto {
    pub id: String,
    pub display_name: String,
    pub source_kind: String,
    pub config_alias: Option<String>,
    pub host_name: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub host_key_type: String,
    pub enabled: bool,
    pub connection_status: String,
    pub last_connected_at: Option<String>,
    pub last_error: Option<String>,
    pub deleted_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectionInput {
    pub display_name: String,
    pub host_name: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub host_key: String,
    pub config_alias: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConfigHostDto {
    pub alias: String,
    pub host_name: String,
    pub user: String,
    pub port: u16,
    pub identity_file: Option<String>,
    pub imported: bool,
    pub deleted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectionImportResultDto {
    pub alias: String,
    pub connection: Option<SshConnectionDto>,
    pub error: Option<String>,
    pub restored: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshConnectionTestDto {
    pub connection_id: String,
    pub ok: bool,
    pub os: Option<String>,
    pub home: Option<String>,
    pub shell: Option<String>,
    pub git_version: Option<String>,
    pub cli_versions: std::collections::BTreeMap<String, String>,
    pub error: Option<String>,
}

/// 工作区根目录自身的 Git 上下文。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase"
)]
pub enum WorkspaceGitContextDto {
    /// 工作区根目录不是 Git 仓库。
    #[serde(rename = "not-repository")]
    NotRepository {
        /// 工作区主键。
        workspace_id: String,
    },
    /// 工作区根目录是 Git 仓库。
    Repository {
        /// 工作区主键。
        workspace_id: String,
        /// Git 仓库根目录。
        root_path: String,
        /// 仓库显示名称。
        name: String,
        /// 默认分支名称。
        default_branch: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TrustLevelDto {
    Trusted,
    Standard,
    Restricted,
}

impl TrustLevelDto {
    pub fn as_str(&self) -> &'static str {
        match self {
            TrustLevelDto::Trusted => "trusted",
            TrustLevelDto::Standard => "standard",
            TrustLevelDto::Restricted => "restricted",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "trusted" => Self::Trusted,
            "restricted" => Self::Restricted,
            _ => Self::Standard,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadDto {
    pub id: String,
    pub workspace_id: String,
    pub engine_id: String,
    pub model_id: String,
    pub engine_thread_id: Option<String>,
    pub engine_metadata: Option<Value>,
    pub plan_mode: Option<bool>,
    pub send_method: Option<String>,
    pub reasoning_effort: Option<String>,
    /// 数据库中统一权限对象 A 的结构化内容。
    pub permission_mode: Option<PermissionComponentJson>,
    pub title: String,
    pub status: ThreadStatusDto,
    pub message_count: i64,
    pub total_tokens: i64,
    /// 当前线程已经占用的上下文 token 数量快照。
    pub context_current_tokens: Option<i64>,
    /// 当前线程允许使用的最大上下文 token 数量快照。
    pub context_max_tokens: Option<i64>,
    /// 上下文 token 快照的 RFC3339 更新时间。
    pub context_usage_updated_at: Option<String>,
    pub created_at: String,
    pub last_activity_at: String,
}

/// threads 表的局部更新入参；id 必填，其余字段仅在 Some 时更新。
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ThreadUpdateDto {
    /// 待更新线程主键，仅用于 WHERE 条件。
    pub id: String,
    /// 工作区主键。
    pub workspace_id: Option<String>,
    /// CLI 工具标识。
    pub engine_id: Option<String>,
    /// 模型标识。
    pub model_id: Option<String>,
    /// CLI 原生线程标识。
    pub engine_thread_id: Option<String>,
    /// CLI 原始 metadata JSON 字符串。
    pub engine_metadata_json: Option<String>,
    /// CLI 能力 JSON 字符串。
    pub engine_capabilities_json: Option<String>,
    /// 线程标题。
    pub title: Option<String>,
    /// 线程状态原始字符串。
    pub status: Option<String>,
    /// 归档时间。
    pub archived_at: Option<String>,
    /// 消息数量。
    pub message_count: Option<i64>,
    /// token 总量。
    pub total_tokens: Option<i64>,
    /// 创建时间。
    pub created_at: Option<String>,
    /// 最近活动时间。
    pub last_activity_at: Option<String>,
    /// 是否启用计划模式。
    pub plan_mode: Option<bool>,
    /// 消息发送方式。
    pub send_method: Option<String>,
    /// 推理强度。
    pub reasoning_effort: Option<String>,
    /// 待更新的统一线程权限对象 A。
    pub permission_mode: Option<PermissionComponentJson>,
    /// 当前线程已经占用的上下文 token 数量快照。
    pub context_current_tokens: Option<i64>,
    /// 当前线程允许使用的最大上下文 token 数量快照。
    pub context_max_tokens: Option<i64>,
    /// 上下文 token 快照的 RFC3339 更新时间。
    pub context_usage_updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskRunDto {
    pub id: String,
    pub task_id: String,
    pub scheduled_for: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub thread_id: Option<String>,
    pub assistant_message_id: Option<String>,
    pub status: String,
    pub error_message: Option<String>,
    pub result_preview: Option<String>,
    pub acknowledged_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskDto {
    pub id: String,
    pub description: String,
    pub enabled: bool,
    pub execution_device_id: String,
    pub target_type: String,
    pub workspace_id: String,
    pub thread_id: Option<String>,
    pub runtime_config: Option<Value>,
    pub schedule_type: String,
    pub schedule: Value,
    pub timezone: String,
    pub next_run_at: Option<String>,
    pub last_run_at: Option<String>,
    pub latest_run: Option<ScheduledTaskRunDto>,
    pub needs_confirmation: bool,
    pub target_valid: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRemoteThreadDto {
    pub engine_thread_id: String,
    pub title: Option<String>,
    pub preview: String,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: String,
    pub model_id: String,
    pub reasoning_effort: Option<String>,
    pub model_provider: String,
    pub source_kind: String,
    pub status_type: String,
    #[serde(default)]
    pub active_flags: Vec<String>,
    pub archived: bool,
    pub local_thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexRemoteThreadPageDto {
    pub threads: Vec<CodexRemoteThreadDto>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeRemoteSessionDto {
    pub engine_thread_id: String,
    pub title: Option<String>,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: String,
    pub archived: bool,
    pub local_thread_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeRemoteSessionPageDto {
    pub sessions: Vec<OpenCodeRemoteSessionDto>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ThreadStatusDto {
    Idle,
    Streaming,
    AwaitingApproval,
    Error,
    Completed,
}

impl ThreadStatusDto {
    pub fn as_str(&self) -> &'static str {
        match self {
            ThreadStatusDto::Idle => "idle",
            ThreadStatusDto::Streaming => "streaming",
            ThreadStatusDto::AwaitingApproval => "awaiting_approval",
            ThreadStatusDto::Error => "error",
            ThreadStatusDto::Completed => "completed",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "streaming" => Self::Streaming,
            "awaiting_approval" => Self::AwaitingApproval,
            "error" => Self::Error,
            "completed" => Self::Completed,
            _ => Self::Idle,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageDto {
    pub id: String,
    pub thread_id: String,
    pub role: String,
    pub content: Option<String>,
    pub blocks: Option<Value>,
    pub turn_engine_id: Option<String>,
    pub turn_model_id: Option<String>,
    pub turn_reasoning_effort: Option<String>,
    pub schema_version: i64,
    pub status: MessageStatusDto,
    pub token_usage: Option<TokenUsageDto>,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageWindowCursorDto {
    pub created_at: String,
    pub id: String,
    pub row_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageWindowDto {
    pub messages: Vec<MessageDto>,
    pub next_cursor: Option<MessageWindowCursorDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerReceiptDto {
    pub client_steer_id: String,
    pub expected_turn_id: String,
    pub accepted_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionOutputChunkDto {
    pub stream: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionOutputDto {
    pub found: bool,
    pub output_chunks: Vec<ActionOutputChunkDto>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageStatusDto {
    Completed,
    Streaming,
    Interrupted,
    Error,
}

impl MessageStatusDto {
    pub fn as_str(&self) -> &'static str {
        match self {
            MessageStatusDto::Completed => "completed",
            MessageStatusDto::Streaming => "streaming",
            MessageStatusDto::Interrupted => "interrupted",
            MessageStatusDto::Error => "error",
        }
    }

    pub fn from_str(value: &str) -> Self {
        match value {
            "streaming" => Self::Streaming,
            "interrupted" => Self::Interrupted,
            "error" => Self::Error,
            _ => Self::Completed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsageDto {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResultDto {
    pub thread_id: String,
    pub thread_title: String,
    pub workspace_name: String,
    pub message_id: String,
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineInfoDto {
    pub id: String,
    pub name: String,
    pub models: Vec<EngineModelDto>,
    pub capabilities: EngineCapabilitiesDto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionTargetDto {
    pub target_key: String,
    pub kind: String,
    pub display_name: String,
    pub connection_id: Option<String>,
    pub host_name: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub project_path: Option<String>,
    pub connection_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatProviderUsageDto {
    pub engine_id: String,
    pub name: String,
    pub available: bool,
    pub windows: Vec<ChatProviderUsageWindowDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatProviderUsageWindowDto {
    pub kind: String,
    pub used_percent: u8,
    pub resets_at: Option<i64>,
}

/// 当前 CLI 线程上下文窗口的统一快照。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CliContextUsageDto {
    /// 当前线程已经占用的上下文 token 数量。
    pub current_tokens: Option<u64>,
    /// 当前模型支持的最大上下文 token 数量。
    pub max_context_tokens: Option<u64>,
    /// 当前上下文剩余百分比，按原始 current/max 比值计算。
    pub context_percent: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineCapabilitiesDto {
    #[serde(default)]
    pub permission_modes: Vec<String>,
    #[serde(default)]
    pub sandbox_modes: Vec<String>,
    #[serde(default)]
    pub approval_decisions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineModelDto {
    pub id: String,
    pub display_name: String,
    pub description: String,
    pub hidden: bool,
    pub is_default: bool,
    pub upgrade: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub availability_nux: Option<EngineModelAvailabilityNuxDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_info: Option<EngineModelUpgradeInfoDto>,
    #[serde(default)]
    pub input_modalities: Vec<String>,
    #[serde(default)]
    pub attachment_modalities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limits: Option<EngineModelLimitsDto>,
    #[serde(default)]
    pub supports_personality: bool,
    pub default_reasoning_effort: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffortOptionDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineModelLimitsDto {
    pub context_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineModelAvailabilityNuxDto {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineModelUpgradeInfoDto {
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upgrade_copy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_link: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub migration_markdown: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOptionDto {
    pub reasoning_effort: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineHealthDto {
    pub id: String,
    pub available: bool,
    pub version: Option<String>,
    pub details: Option<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub checks: Vec<String>,
    #[serde(default)]
    pub fixes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_diagnostics: Option<CodexProtocolDiagnosticsDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct CodexProtocolDiagnosticsDto {
    #[serde(default)]
    pub method_availability: Vec<CodexMethodAvailabilityDto>,
    #[serde(default)]
    pub experimental_features: Vec<CodexExperimentalFeatureDto>,
    #[serde(default)]
    pub collaboration_modes: Vec<String>,
    #[serde(default)]
    pub apps: Vec<CodexAppDto>,
    #[serde(default)]
    pub skills: Vec<CodexSkillDto>,
    #[serde(default)]
    pub plugin_marketplaces: Vec<CodexPluginMarketplaceDto>,
    #[serde(default)]
    pub mcp_servers: Vec<CodexMcpServerDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<CodexAccountStateDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<CodexConfigStateDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_config_warning: Option<CodexConfigWarningDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_account_login: Option<CodexAccountLoginCompletedDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_mcp_oauth: Option<CodexMcpOauthCompletedDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_thread_realtime: Option<CodexThreadRealtimeEventDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_windows_sandbox_setup: Option<CodexWindowsSandboxSetupDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_windows_world_writable_warning: Option<CodexWindowsWorldWritableWarningDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<String>,
    #[serde(default)]
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexMethodAvailabilityDto {
    pub method: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexExperimentalFeatureDto {
    pub name: String,
    pub enabled: bool,
    pub default_enabled: bool,
    pub stage: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAppDto {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_enabled: bool,
    pub is_accessible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexSkillDto {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub description: String,
    pub enabled: bool,
    #[serde(default)]
    pub scope: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeRuntimeCatalogDto {
    #[serde(default)]
    pub agents: Vec<OpenCodeAgentDto>,
    #[serde(default)]
    pub commands: Vec<OpenCodeCommandDto>,
    #[serde(default)]
    pub mcp_servers: Vec<OpenCodeMcpServerDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeAgentDto {
    pub name: String,
    pub description: Option<String>,
    pub mode: String,
    pub native: bool,
    pub hidden: bool,
    pub model_provider_id: Option<String>,
    pub model_id: Option<String>,
    pub variant: Option<String>,
    pub steps: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeCommandDto {
    pub name: String,
    pub description: Option<String>,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub source: Option<String>,
    pub subtask: bool,
    #[serde(default)]
    pub hints: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeMcpServerDto {
    pub name: String,
    pub status: String,
    pub detail: Option<String>,
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexPluginMarketplaceDto {
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub plugins: Vec<CodexPluginDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexPluginDto {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub installed: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub developer_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexMcpServerDto {
    pub name: String,
    pub auth_status: String,
    pub tool_count: usize,
    pub resource_count: usize,
    pub resource_template_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionProviderCapabilitiesDto {
    pub has_official_skill_catalog: bool,
    pub can_toggle_skills: bool,
    pub has_official_plugin_catalog: bool,
    pub can_install_plugins: bool,
    pub can_toggle_plugins: bool,
    pub has_official_mcp_catalog: bool,
    pub can_manage_mcp: bool,
    pub can_authenticate_mcp: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSourceDto {
    pub id: String,
    pub label: String,
    pub official: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionItemDto {
    pub id: String,
    pub provider_id: String,
    pub kind: String,
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub scope: String,
    pub source: Option<String>,
    pub marketplace: Option<String>,
    pub path: Option<String>,
    pub parent_plugin_id: Option<String>,
    pub category: Option<String>,
    pub officially_available: bool,
    pub catalog_authority: Option<String>,
    pub installed: Option<bool>,
    pub configured: Option<bool>,
    pub enabled: Option<bool>,
    pub health: String,
    pub auth_state: Option<String>,
    #[serde(default)]
    pub available_actions: Vec<String>,
    pub requires_new_session: bool,
    pub read_only_reason: Option<String>,
    pub warning: Option<String>,
    #[serde(default)]
    pub insert_text: Option<String>,
    #[serde(default)]
    pub panel: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub search_terms: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ExtensionCatalogKindRefreshDto {
    pub kind: String,
    pub items: Vec<ExtensionItemDto>,
    pub success: bool,
}

impl ExtensionCatalogKindRefreshDto {
    pub fn success(kind: &str, items: Vec<ExtensionItemDto>) -> Self {
        Self {
            kind: kind.to_string(),
            items,
            success: true,
        }
    }

    pub fn failure(kind: &str) -> Self {
        Self {
            kind: kind.to_string(),
            items: Vec::new(),
            success: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionCatalogRefreshErrorDto {
    pub kind: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachedExtensionCatalogDto {
    pub provider_id: String,
    pub cwd: Option<String>,
    #[serde(default)]
    pub items: Vec<ExtensionItemDto>,
    #[serde(default)]
    pub sources: Vec<ExtensionSourceDto>,
    pub capabilities: ExtensionProviderCapabilitiesDto,
    pub fetched_at: Option<String>,
    #[serde(default)]
    pub kind_fetched_at: std::collections::BTreeMap<String, Option<String>>,
    pub last_attempt_at: Option<String>,
    pub next_refresh_at: Option<String>,
    pub refreshing: bool,
    pub refresh_completed_at: Option<String>,
    pub has_snapshot: bool,
    #[serde(default)]
    pub refresh_errors: Vec<ExtensionCatalogRefreshErrorDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionActionResultDto {
    pub provider_id: String,
    pub kind: String,
    pub extension_id: String,
    pub action: String,
    pub requires_new_session: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountStateDto {
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan_type: Option<String>,
    pub requires_openai_auth: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexConfigStateDto {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_profile: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approvals_reviewer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub web_search: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub layers: Vec<CodexConfigLayerDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexConfigLayerDto {
    pub source: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexConfigWarningDto {
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_column: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexAccountLoginCompletedDto {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub login_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexMcpOauthCompletedDto {
    pub name: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexThreadRealtimeEventDto {
    pub kind: String,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub num_channels: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub samples_per_channel: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexWindowsSandboxSetupDto {
    pub mode: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexWindowsWorldWritableWarningDto {
    pub sample_paths: Vec<String>,
    pub extra_count: u64,
    pub failed_scan: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeToastDto {
    pub variant: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineRuntimeUpdatedDto {
    pub engine_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_diagnostics: Option<CodexProtocolDiagnosticsDto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub toast: Option<RuntimeToastDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EngineCheckResultDto {
    pub command: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusDto {
    pub branch: String,
    pub files: Vec<GitFileStatusDto>,
    pub ahead: usize,
    pub behind: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileStatusDto {
    pub path: String,
    pub index_status: Option<String>,
    pub worktree_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffPreviewDto {
    pub content: String,
    pub truncated: bool,
    pub original_bytes: usize,
    pub returned_bytes: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitCompareSourceDto {
    Changes,
    Staged,
}

impl GitCompareSourceDto {
    pub fn from_str(value: &str) -> Self {
        match value {
            "staged" => Self::Staged,
            _ => Self::Changes,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeTypeDto {
    Added,
    Modified,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitFileCompareDto {
    pub source: GitCompareSourceDto,
    pub base_content: String,
    pub modified_content: String,
    pub base_label: String,
    pub modified_label: String,
    pub change_type: GitChangeTypeDto,
    pub has_staged_changes: bool,
    pub has_unstaged_changes: bool,
    pub is_binary: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_editable: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitBranchScopeDto {
    Local,
    Remote,
}

impl GitBranchScopeDto {
    pub fn from_str(value: &str) -> Self {
        match value {
            "remote" => Self::Remote,
            _ => Self::Local,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchDto {
    pub name: String,
    pub full_name: String,
    pub is_current: bool,
    pub is_remote: bool,
    pub upstream: Option<String>,
    pub ahead: usize,
    pub behind: usize,
    pub last_commit_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchPageDto {
    pub entries: Vec<GitBranchDto>,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitDto {
    pub hash: String,
    pub short_hash: String,
    pub author_name: String,
    pub author_email: String,
    pub subject: String,
    pub body: String,
    pub authored_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitPageDto {
    pub entries: Vec<GitCommitDto>,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStashDto {
    pub index: usize,
    pub name: String,
    pub branch_hint: Option<String>,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeDto {
    pub path: String,
    pub display_path: Option<String>,
    pub head_sha: Option<String>,
    pub branch: Option<String>,
    pub is_main: bool,
    pub is_locked: bool,
    pub is_prunable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeEntryDto {
    pub path: String,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreePageDto {
    pub entries: Vec<FileTreeEntryDto>,
    pub offset: usize,
    pub limit: usize,
    pub total: usize,
    pub has_more: bool,
    pub scan_truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadFileResultDto {
    pub content: String,
    pub size_bytes: u64,
    pub is_binary: bool,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WriteFileResultDto {
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedEditorFileReferenceDto {
    pub root_path: String,
    pub file_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionDto {
    pub id: String,
    pub workspace_id: String,
    pub shell: String,
    pub cwd: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalNotificationDto {
    pub id: String,
    pub workspace_id: String,
    pub session_id: String,
    pub source: String,
    pub title: String,
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalReplayChunkDto {
    pub seq: u64,
    pub ts: String,
    pub data: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResumeSessionDto {
    pub latest_seq: u64,
    pub oldest_available_seq: Option<u64>,
    pub gap: bool,
    pub chunks: Vec<TerminalReplayChunkDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TerminalEnvSnapshotDto {
    pub term: Option<String>,
    pub colorterm: Option<String>,
    pub term_program: Option<String>,
    pub term_program_version: Option<String>,
    pub home: Option<String>,
    pub user_profile: Option<String>,
    pub app_data: Option<String>,
    pub local_app_data: Option<String>,
    pub xdg_config_home: Option<String>,
    pub xdg_data_home: Option<String>,
    pub xdg_cache_home: Option<String>,
    pub xdg_state_home: Option<String>,
    pub tmpdir: Option<String>,
    pub temp: Option<String>,
    pub tmp: Option<String>,
    pub lang: Option<String>,
    pub lc_all: Option<String>,
    pub lc_ctype: Option<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizeSnapshotDto {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
    pub recorded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalRendererDiagnosticsDto {
    pub session_id: String,
    pub shell: String,
    pub cwd: String,
    pub env_snapshot: TerminalEnvSnapshotDto,
    pub last_resize: Option<TerminalResizeSnapshotDto>,
    pub io_counters: TerminalIoCountersDto,
    pub latency: TerminalLatencySnapshotDto,
    pub output_throttle: TerminalOutputThrottleSnapshotDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TerminalIoCountersDto {
    pub stdin_writes: u64,
    pub stdin_bytes: u64,
    pub stdin_ctrl_c: u64,
    pub last_stdin_write_duration_ms: Option<u64>,
    pub stdout_reads: u64,
    pub stdout_bytes: u64,
    pub stdout_emits: u64,
    pub stdout_emit_bytes: u64,
    pub stdout_dropped_bytes: u64,
    pub last_stdin_write_at: Option<String>,
    pub last_stdout_read_at: Option<String>,
    pub last_stdout_emit_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputThrottleSnapshotDto {
    pub min_emit_interval_ms: u64,
    pub max_emit_bytes: u64,
    pub buffer_bytes: u64,
    pub buffer_cap_bytes: u64,
    pub buffer_peak_bytes: u64,
    pub buffer_trimmed_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TerminalLatencySnapshotDto {
    pub stdin_to_stdout_read_ms: Option<u64>,
    pub stdout_read_to_emit_ms: Option<u64>,
}

// ── Git Remotes ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRemoteDto {
    pub name: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitInitRepoStatusDto {
    pub can_initialize: bool,
    pub blocking_root_path: Option<String>,
}

// ── Setup / Onboarding ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DependencyReport {
    pub node: DepStatus,
    pub codex: DepStatus,
    pub git: DepStatus,
    pub platform: String,
    pub package_managers: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepStatus {
    pub found: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub can_auto_install: bool,
    pub install_method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallResult {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallProgressEvent {
    pub dependency: String,
    pub line: String,
    pub stream: String,
    pub finished: bool,
}

// ── Harness Management ──────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub command: String,
    pub found: bool,
    pub version: Option<String>,
    pub path: Option<String>,
    pub can_auto_install: bool,
    pub website: String,
    pub native: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessReport {
    pub harnesses: Vec<HarnessInfo>,
    pub npm_available: bool,
    /// "mise" or "npm", whichever the current environment should use to
    /// auto-install npm-packaged harnesses. `None` when neither is
    /// available. Set to "mise" only inside a Flatpak sandbox where a
    /// bundled `mise` is resolvable, since outside a sandbox npm-based
    /// global installs remain the default.
    pub preferred_install_method: Option<String>,
}
