pub mod claude_code;
mod claude_code_session_lifecycle;
pub mod codex;
pub mod factory;
mod mcp;
pub mod opencode;

pub(crate) use mcp::{BaseCliMcp, CliMcpRuntime};

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    engines::{
        ApprovalRequestRoute, CodexRuntimeEvent, EngineCapabilities, EngineEvent,
        EngineSteerReceipt, EngineThread, ModelInfo, SandboxPolicy, ThreadScope,
        ThreadSyncSnapshot, TurnInput,
    },
    models::{
        CachedExtensionCatalogDto, ChatProviderUsageDto, CodexAppDto, CodexPluginDto,
        CodexSkillDto, EngineHealthDto, EngineInfoDto, ExtensionActionResultDto,
        ExtensionCatalogKindRefreshDto, ExtensionItemDto, OpenCodeRuntimeCatalogDto,
        PermissionComponentJson, ThreadDto, ThreadStatusDto, WorkspaceDto,
    },
};

/// 当前 CLI 执行业务时使用的项目位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CliLocationKind {
    /// 用户正在使用本机项目。
    Local,
    /// 用户正在使用 SSH 远端项目。
    Ssh,
}

/// Gateway 注入的可信 MCP 调用上下文。
///
/// 这些字段只能由 Gateway 从已登记的 token 上下文构造，CLI 和模型提交的
/// `_meta` 不得覆盖它们。
#[derive(Debug, Clone)]
pub struct McpInvocationContext {
    /// 当前引擎线程的稳定标识，用于查询所属 AuraCoder 项目。
    pub engine_thread_id: String,
    /// 当前助手轮次的稳定标识，用于电脑操作授权和取消关联。
    pub turn_id: String,
}

/// MCP 工具调用结果，统一承载成功内容或业务错误内容。
#[derive(Debug, Clone, serde::Serialize)]
pub struct McpToolResult {
    /// MCP 标准内容块列表。
    pub content: Vec<Value>,
    /// 是否为业务错误结果；字段名遵循 MCP JSON 契约。
    #[serde(rename = "isError")]
    pub is_error: bool,
}

impl McpToolResult {
    /// 将工具返回值包装为 MCP 成功内容；已有 content 数组保持原样。
    pub(crate) fn success(value: Value) -> Self {
        let content = value
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| {
                vec![serde_json::json!({
                    "type": "text",
                    "text": serde_json::to_string(&value).unwrap_or_else(|_| "null".to_string())
                })]
            });
        Self {
            content,
            is_error: false,
        }
    }

    /// 将业务错误转换为 MCP 错误内容，并保留对外可读的错误语义。
    pub(crate) fn error(code: &str, message: impl Into<String>) -> Self {
        Self {
            content: vec![serde_json::json!({
                "type": "text",
                "text": format!("{code}: {}", message.into())
            })],
            is_error: true,
        }
    }
}

/// 当前 CLI 操作所属的 workspace 和机器目标。
#[derive(Debug, Clone)]
pub struct CliExecutionContext {
    /// 当前 workspace 的稳定标识。
    pub workspace_id: String,
    /// 当前机器上的项目根目录。
    pub root_path: String,
    /// 当前项目位于本机还是 SSH 远端机器。
    pub location_kind: CliLocationKind,
    /// SSH 远端项目正式绑定的连接标识；本机项目没有该值。
    pub ssh_connection_id: Option<String>,
}

impl CliExecutionContext {
    /// 用户进入某个 workspace 后，根据该 workspace 的正式项目位置建立 CLI 操作目标，保证后续操作不会使用其他项目或其他 SSH 连接。
    pub fn from_workspace(workspace: &WorkspaceDto) -> Result<Self> {
        let location_kind = if workspace.location_kind == "ssh" {
            if workspace.ssh_connection_id.is_none() {
                anyhow::bail!("SSH 远端项目未绑定连接");
            }
            CliLocationKind::Ssh
        } else {
            CliLocationKind::Local
        };
        Ok(Self {
            workspace_id: workspace.id.clone(),
            root_path: workspace.root_path.clone(),
            location_kind,
            ssh_connection_id: workspace.ssh_connection_id.clone(),
        })
    }
}

/// CLI 会话列表中的一条会话记录。
#[derive(Debug, Clone)]
pub struct CliSessionSnapshot {
    /// CLI 创建的会话标识。
    pub engine_thread_id: String,
    /// 用户在会话列表中看到的标题。
    pub title: String,
    /// 用户在会话列表中看到的内容预览。
    pub preview: Option<String>,
    /// 会话实际所属的项目目录。
    pub cwd: String,
    /// 会话最近使用的模型。
    pub model_id: String,
    /// 会话最近使用的思考强度。
    pub reasoning_effort: Option<String>,
    /// 会话创建时间。
    pub created_at: Option<String>,
    /// 会话最近一次活动时间。
    pub updated_at: Option<String>,
    /// CLI 返回的原始会话来源。
    pub source_kind: Option<String>,
    /// CLI 返回的原始会话状态。
    pub raw_status: Option<String>,
    /// 会话仍在进行的业务状态。
    pub active_flags: Vec<String>,
    /// 会话当前状态。
    pub status: ThreadStatusDto,
    /// 会话是否已经归档。
    pub archived: bool,
    /// 保持现有会话恢复和同步所需的信息。
    pub metadata: Value,
}

/// 当前 CLI 原始权限字段；来源仅限 threads.permission_mode。
#[derive(Debug, Clone, Default)]
pub struct CliRuntimePermissions {
    /// CLI 原生审批策略或策略对象。
    pub approval_policy: Option<Value>,
    /// CLI 原生沙箱模式。
    pub sandbox_mode: Option<String>,
    /// CLI 原生网络开关。
    pub allow_network: Option<bool>,
    /// CLI 原生权限配置对象。
    pub permission_profile: Option<Value>,
    /// CLI 原生审批审核人。
    pub approvals_reviewer: Option<String>,
}

/// 统一执行权限局部更新补丁；CLI 实现负责转换为各自原始 JSON。
#[derive(Debug, Clone, Default)]
pub struct CliRuntimePermissionPatch {
    /// 审批策略更新；外层 Some 表示更新，内层 None 表示清除。
    pub approval_policy: Option<Option<Value>>,
    /// 沙箱模式更新；外层 Some 表示更新，内层 None 表示清除。
    pub sandbox_mode: Option<Option<String>>,
    /// 网络开关更新；外层 Some 表示更新，内层 None 表示清除。
    pub allow_network: Option<Option<bool>>,
    /// 权限配置对象更新；外层 Some 表示更新，内层 None 表示清除。
    pub permission_profile: Option<Option<Value>>,
    /// 审批审核人更新；外层 Some 表示更新，内层 None 表示清除。
    pub approvals_reviewer: Option<Option<String>>,
}

/// CLI 明确报告会话不存在时使用的公共错误类型。
///
/// 网络失败、服务未就绪、解析失败和 workspace 不匹配都不得转换为该错误，
/// 这样恢复编排才能区分“会话已删除”和“当前 CLI 暂时不可用”。
#[derive(Debug, Clone)]
pub struct CliSessionNotFoundError {
    /// 报告不存在的 CLI 标识。
    pub engine_id: String,
    /// 报告不存在的 CLI 会话标识。
    pub engine_thread_id: String,
}

impl CliSessionNotFoundError {
    /// 创建带有 CLI 和会话标识的公共 NotFound 错误。
    pub fn new(engine_id: impl Into<String>, engine_thread_id: impl Into<String>) -> Self {
        Self {
            engine_id: engine_id.into(),
            engine_thread_id: engine_thread_id.into(),
        }
    }
}

impl std::fmt::Display for CliSessionNotFoundError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "CLI session not found: engine_id={} engine_thread_id={}",
            self.engine_id, self.engine_thread_id
        )
    }
}

impl std::error::Error for CliSessionNotFoundError {}

/// 用户从现有会话创建分支后得到的新会话。
#[derive(Debug, Clone)]
pub struct CliForkedThread {
    /// 新会话的 CLI 会话标识。
    pub engine_thread_id: String,
    /// 新会话继续使用的模型。
    pub model_id: String,
    /// 新会话在列表中显示的标题。
    pub title: Option<String>,
    /// 新会话在列表中显示的内容预览。
    pub preview: Option<String>,
    /// 新会话的当前状态。
    pub raw_status: Option<String>,
    /// 新会话仍在进行的业务状态。
    pub active_flags: Vec<String>,
}

/// 用户开始代码审查后得到的审查会话。
#[derive(Debug)]
pub struct CliReviewStarted {
    /// 审查结果所属的 CLI 会话标识。
    pub review_thread_id: String,
}

/// AuraCoder 中所有 CLI 共用的操作接口。
///
/// Codex、OpenCode 和 Claude Code 分别实现该接口。调用方根据用户当前选择的
/// CLI 取得对应实现，并且只通过本接口完成 CLI 业务操作。
#[async_trait]
pub trait CliTool: Send + Sync {
    /// 在模型选择器、会话和扩展页面中标识当前 CLI，保证用户选择的 CLI 不会被换成其他 CLI。
    fn id(&self) -> &str;

    /// 在页面中显示当前 CLI 的名称，让用户知道模型、会话和扩展属于哪个 CLI。
    fn name(&self) -> &str;

    /// 根据当前 CLI 已有能力决定页面显示哪些权限、沙箱和审批选项，避免用户选择该 CLI 不支持的操作。
    fn capabilities(&self) -> EngineCapabilities;

    /// 返回当前 CLI 在当前固定运行位置支持的 MCP 工具目录。
    fn list_mcp_tools(&self) -> Result<Vec<Value>, String>;

    /// 在当前 CLI 中执行一个 MCP 工具；公共业务判断统一由 BaseCliMcp 完成。
    async fn call_mcp_tool(
        &self,
        tool_name: &str,
        arguments: Value,
        context: McpInvocationContext,
        call_id: String,
        cancellation: CancellationToken,
    ) -> McpToolResult;

    /// 用户按当前 CLI 和 workspace 选择本机或 SSH 执行目标，调用方无需依赖具体 CLI 实现。
    async fn execution_context(&self, workspace_id: Option<&str>) -> Result<CliExecutionContext>;

    /// 用户按项目目录选择当前 CLI 的本机或 SSH 执行目标，调用方无需依赖具体 CLI 实现。
    async fn execution_context_for_cwd(&self, cwd: Option<&str>) -> Result<CliExecutionContext>;

    /// 用户打开模型选择器或切换 CLI 时，读取当前项目目标上的 CLI 和模型信息；页面只显示当前机器可用的模型。
    async fn get_engine_info(&self, context: &CliExecutionContext) -> Result<EngineInfoDto>;

    /// 用户选择模型或发送消息时，确认该模型属于当前 CLI 和当前机器；模型不可用时阻止发送并提示用户重新选择。
    async fn models_for_validation(
        &self,
        context: &CliExecutionContext,
        requested_model_id: &str,
    ) -> Result<Vec<ModelInfo>>;

    /// 用户查看用量时，读取当前 CLI 在当前机器上的可用额度；没有用量信息时页面保持现有的不可用状态。
    async fn get_chat_provider_usage(
        &self,
        context: &CliExecutionContext,
    ) -> Result<Option<ChatProviderUsageDto>>;

    /// 用户查看或重试运行目标时，检查当前 CLI 是否可以使用，并在页面显示版本、问题和处理建议。
    async fn engine_health(&self, context: &CliExecutionContext) -> Result<EngineHealthDto>;

    /// 用户使用 Codex 时，持续接收账户、模型、配置和运行状态变化，使页面及时显示最新状态和提示。
    fn subscribe_codex_runtime_events(&self) -> broadcast::Receiver<CodexRuntimeEvent>;

    /// 用户进入当前 CLI 的聊天环境时，提前确认该 CLI 可以接受后续操作；失败时保留当前目标并显示不可用结果。
    async fn prewarm_engine(&self, context: &CliExecutionContext) -> Result<()>;

    /// 用户使用 Codex 权限设置时，判断当前项目是否由外部沙箱管理，保证页面采用现有的权限选项和提示。
    async fn uses_external_sandbox(&self, context: &CliExecutionContext) -> Result<bool>;

    /// 用户进入 workspace 或刷新会话列表时，读取当前 CLI 在当前项目中的会话；其他目录和其他 CLI 的会话不得出现。
    async fn list_sessions(
        &self,
        context: &CliExecutionContext,
        search_term: Option<&str>,
        archived: Option<bool>,
    ) -> Result<Vec<CliSessionSnapshot>>;

    /// 用户选择一个已有 CLI 会话时，读取该会话的最新信息，并确认它仍属于当前项目。
    async fn read_session(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
    ) -> Result<CliSessionSnapshot>;

    /// 用户准备发送一轮消息时，在模型校验、附件处理和会话启动之前取得当前 CLI 的整轮使用权，保证同一轮业务始终使用同一个本机或 SSH 运行目标。`context` 用于锁定本机或 SSH 项目，`thread` 用于锁定会话及其持续占用标识；该操作以异步 Rust 方法返回 `Result<()>`，失败时必须终止本轮发送，不能回退到其他运行目标。
    async fn acquire_turn(&self, context: &CliExecutionContext, thread: &ThreadDto) -> Result<()>;

    /// 读取当前 CLI 原始权限并转换为前端统一权限组件 JSON。
    async fn get_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<PermissionComponentJson>;

    /// 将前端统一权限组件 JSON 转换为当前 CLI 原始权限并持久化。
    async fn set_permissions(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        values: PermissionComponentJson,
    ) -> Result<PermissionComponentJson>;

    /// 读取当前 CLI 原始权限字段，不读取 engine_metadata_json 权限镜像。
    async fn runtime_permissions(
        &self,
        _context: &CliExecutionContext,
        thread: &ThreadDto,
    ) -> Result<CliRuntimePermissions>;

    /// 将通用执行权限补丁交给当前 CLI 实现转换并持久化。
    async fn patch_runtime_permissions(
        &self,
        _context: &CliExecutionContext,
        _thread: &ThreadDto,
        _patch: CliRuntimePermissionPatch,
    ) -> Result<ThreadDto> {
        anyhow::bail!("当前 CLI 不支持局部执行权限更新")
    }

    /// 将当前已存在的 CLI session 执行权限同步为用户选择的模式。
    ///
    /// 该操作只更新已有 session，不负责创建 CLI 服务或 session；不支持该能力的
    /// CLI 保持默认错误，避免调用方绕过统一的 CLI 生命周期边界。
    async fn sync_thread_execution_policy(
        &self,
        _context: &CliExecutionContext,
        _thread: &ThreadDto,
        _approval_policy: &Value,
    ) -> Result<()> {
        // Codex 和 Claude Code 没有 OpenCode session 权限同步动作；其线程权限
        // 已由 patch_runtime_permissions 持久化，保持统一业务调用的无操作结果。
        Ok(())
    }

    /// 用户新建会话或继续已有会话时，在当前 CLI 和当前项目中建立正式会话；恢复失败时不得创建替代会话。
    async fn start_thread(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        scope: ThreadScope,
        resume_engine_thread_id: Option<&str>,
        model: &str,
        sandbox: SandboxPolicy,
    ) -> Result<EngineThread>;

    /// 用户发送消息后，让当前 CLI 会话处理文字、附件和输入项，并把回答过程持续显示在当前消息区。
    async fn send_message(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        input: TurnInput,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
    ) -> Result<()>;

    /// 用户在任务运行期间补充要求时，把补充内容交给当前会话继续处理，并保留在同一条对话中。
    async fn steer_message(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        client_steer_id: &str,
        content: &str,
        input: TurnInput,
    ) -> Result<EngineSteerReceipt>;

    /// 用户在审批卡片中允许、拒绝或回答问题后，把选择交给当前 CLI，并让当前消息继续执行或停止。
    async fn respond_to_approval(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        approval_id: &str,
        response: Value,
        route: Option<ApprovalRequestRoute>,
    ) -> Result<()>;

    /// 用户点击停止、删除会话或归档会话时，停止当前 CLI 会话正在执行的任务，不影响其他会话。
    async fn interrupt(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()>;

    /// 用户归档会话时，将当前 CLI 会话移出活动列表；归档结果仍可在归档列表中恢复。
    async fn archive_thread(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()>;

    /// 用户恢复归档会话时，让原会话重新出现在活动列表中，并继续使用原来的会话标识。
    async fn unarchive_thread(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()>;

    /// 用户删除会话后，清除当前 CLI 对该会话保留的临时使用记录，避免后续操作误用已经删除的会话。
    async fn forget_session(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<()>;

    /// 系统为新会话生成标题时，读取当前会话已有的内容预览，使会话列表显示可识别的标题而不是会话编号。
    async fn read_thread_preview(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<Option<String>>;

    /// 用户重新打开或主动同步会话时，读取当前 CLI 中的最新标题、状态和历史消息，使页面恢复到当前会话的真实状态。
    async fn read_thread_sync_snapshot(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
    ) -> Result<Option<ThreadSyncSnapshot>>;

    /// 用户修改会话标题，或者系统从首条消息生成标题时，将标题保存到当前 CLI 会话，使下次打开时继续显示该标题。
    async fn set_thread_name(
        &self,
        context: &CliExecutionContext,
        thread: &ThreadDto,
        engine_thread_id: &str,
        name: &str,
    ) -> Result<()>;

    /// 用户在 Codex 聊天输入框中查看可用 Skill 时，读取当前项目可以引用的 Skill，并只显示当前机器上的结果。
    async fn list_codex_skills(
        &self,
        context: &CliExecutionContext,
        cwd: &str,
    ) -> Result<Vec<CodexSkillDto>>;

    /// 用户查看 Codex Apps 时，读取当前机器上可用的 Apps；SSH 查询失败时不得读取本机 Apps。
    async fn list_codex_apps(&self, context: &CliExecutionContext) -> Result<Vec<CodexAppDto>>;

    /// 用户在 Codex 聊天输入框中查看可用 Plugin 时，读取当前项目可以使用的 Plugin，并只显示当前机器上的结果。
    async fn list_codex_plugins(
        &self,
        context: &CliExecutionContext,
        cwd: &str,
    ) -> Result<Vec<CodexPluginDto>>;

    /// 用户使用 OpenCode 时，获取当前项目中的 Agent、Command 和 MCP 参数，并只返回当前机器上的结果。
    async fn get_opencode_runtime_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: &str,
    ) -> Result<OpenCodeRuntimeCatalogDto>;

    /// 用户刷新扩展目录时，重新读取当前 CLI 的指定扩展类型，并将最新结果保存给扩展页面和斜杠菜单使用。
    async fn refresh_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
        requested_kinds: &[String],
    ) -> Result<Vec<ExtensionCatalogKindRefreshDto>>;

    /// 用户打开扩展页或在聊天输入框中输入斜杠时，读取当前 CLI 在当前项目中可用的扩展，并只显示当前 CLI 的内容。
    async fn get_extension_catalog(
        &self,
        context: &CliExecutionContext,
        cwd: Option<&str>,
    ) -> Result<CachedExtensionCatalogDto>;

    /// 用户在聊天输入框中按下斜杠时，读取当前 CLI 在当前项目中可用的扩展菜单项；
    /// 三个 CLI 返回同一种 ExtensionItemDto 结构，前端按统一规则解析展示。
    /// 远端读取失败时不得回退读取本机数据。
    async fn get_extensions(&self, context: &CliExecutionContext) -> Result<Vec<ExtensionItemDto>>;

    /// 用户安装、卸载、启用、停用或认证扩展时，只对当前 CLI 和当前项目执行所选操作，并返回页面需要显示的结果。
    async fn perform_extension_action(
        &self,
        context: &CliExecutionContext,
        item: ExtensionItemDto,
        action: &str,
        scope: Option<&str>,
    ) -> Result<ExtensionActionResultDto>;

    /// 用户在 Codex 会话中创建分支时，从当前会话建立一个独立的新会话，并在侧栏显示新会话。
    async fn fork_thread(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
        cwd: &str,
        model: &str,
        sandbox: SandboxPolicy,
    ) -> Result<CliForkedThread>;

    /// 用户在 Codex 会话中回退若干轮对话时，从回退位置建立新会话，原会话和原消息保持不变。
    async fn rollback_thread(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
        num_turns: u32,
    ) -> Result<ThreadSyncSnapshot>;

    /// 用户在 Codex 会话中执行压缩时，压缩当前会话的历史上下文，并继续保留当前会话。
    async fn compact_thread(
        &self,
        context: &CliExecutionContext,
        engine_thread_id: &str,
    ) -> Result<()>;

    /// 用户在 Codex 会话中开始代码审查时，创建审查任务，并将审查过程和结果显示在对应的审查会话中。
    async fn start_review(
        &self,
        context: &CliExecutionContext,
        source_engine_thread_id: &str,
        target: Value,
        delivery: Option<&str>,
        event_tx: mpsc::Sender<EngineEvent>,
        cancellation: CancellationToken,
        started_tx: oneshot::Sender<CliReviewStarted>,
    ) -> Result<()>;
}
