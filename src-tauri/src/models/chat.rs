use serde::{Deserialize, Serialize};
use serde_json::{value::RawValue, Value};
use std::fmt;

use crate::{
    engines::{ActionResult, BrowserAnnotationMetadata},
    models::{MessageDto, PermissionComponentJson, ThreadDto},
};

/// 聚合单个引擎事件对消息、线程和动作状态的流式更新。
#[derive(Default)]
pub(crate) struct EventProgress {
    /// 事件需要写入的助手消息最终状态。
    pub(crate) message_status: Option<crate::models::MessageStatusDto>,
    /// 事件需要写入的本地线程最终状态。
    pub(crate) thread_status: Option<crate::models::ThreadStatusDto>,
    /// 事件中报告的输入、输出 token 使用量。
    pub(crate) token_usage: Option<(u64, u64)>,
    /// 事件中重新指定的本轮模型标识。
    pub(crate) turn_model_id: Option<String>,
    /// 事件结束时需补写完成状态的动作结果。
    pub(crate) finalized_actions: Vec<(String, ActionResult)>,
    /// 标记事件是否修改了消息块。
    pub(crate) blocks_changed: bool,
    /// 标记事件是否要求立即刷新所有流式状态。
    pub(crate) force_persist: bool,
}

/// 聊天消息块的持久化结构，供消息展示和流式更新共同使用。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ContentBlock {
    #[serde(rename = "text")]
    Text {
        /// 文本块实际展示的正文内容。
        content: String,
        /// 标记该正文是否处于计划模式。
        #[serde(rename = "planMode", skip_serializing_if = "Option::is_none")]
        plan_mode: Option<bool>,
        /// 标记该正文是否由追加发送产生。
        #[serde(rename = "isSteer", skip_serializing_if = "Option::is_none")]
        is_steer: Option<bool>,
        /// 关联前端追加发送幂等标识。
        #[serde(rename = "clientSteerId", skip_serializing_if = "Option::is_none")]
        client_steer_id: Option<String>,
    },
    #[serde(rename = "steer")]
    Steer {
        /// 追加消息块的业务标识。
        #[serde(rename = "steerId")]
        steer_id: String,
        /// 追加消息中用户输入的正文。
        content: String,
        /// 追加消息当前的投递状态。
        #[serde(rename = "deliveryStatus")]
        delivery_status: String,
        /// 追加消息是否按计划模式解释。
        #[serde(rename = "planMode", skip_serializing_if = "Option::is_none")]
        plan_mode: Option<bool>,
        /// 追加消息附带的文件内容块。
        #[serde(skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<ContentBlock>,
        /// 追加消息附带的技能引用块。
        #[serde(skip_serializing_if = "Vec::is_empty")]
        skills: Vec<ContentBlock>,
        /// 追加消息附带的提及引用块。
        #[serde(skip_serializing_if = "Vec::is_empty")]
        mentions: Vec<ContentBlock>,
    },
    #[serde(rename = "diff")]
    Diff {
        /// 消息中展示的差异文本。
        diff: String,
        /// 差异所属的业务范围。
        scope: String,
    },
    #[serde(rename = "action")]
    Action {
        /// 本地动作主键。
        #[serde(rename = "actionId")]
        action_id: String,
        /// CLI 原生动作标识。
        #[serde(rename = "engineActionId", skip_serializing_if = "Option::is_none")]
        engine_action_id: Option<String>,
        /// 动作的业务类型。
        #[serde(rename = "actionType")]
        action_type: String,
        /// 动作在消息中的简要说明。
        summary: String,
        /// 动作的结构化业务详情。
        details: Box<RawValue>,
        /// 后台动作的任务标识。
        #[serde(rename = "backgroundTaskId", skip_serializing_if = "Option::is_none")]
        background_task_id: Option<String>,
        /// 动作运行期间收集的输出分块。
        #[serde(rename = "outputChunks")]
        output_chunks: Vec<ActionOutputChunk>,
        /// 动作当前处理状态。
        status: String,
        /// 动作执行结束后的结果数据。
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<ActionBlockResult>,
    },
    #[serde(rename = "approval")]
    Approval {
        /// 本地审批记录主键。
        #[serde(rename = "approvalId")]
        approval_id: String,
        /// 审批所对应动作的业务类型。
        #[serde(rename = "actionType")]
        action_type: String,
        /// 审批内容的简要说明。
        summary: String,
        /// 审批所需的结构化业务详情。
        details: Box<RawValue>,
        /// 审批当前处理状态。
        status: String,
        /// 审批完成后记录的用户决定。
        #[serde(skip_serializing_if = "Option::is_none")]
        decision: Option<String>,
    },
    #[serde(rename = "thinking")]
    Thinking {
        /// 思考过程的可见内容。
        content: String,
        /// 思考过程开始时的时间戳。
        #[serde(rename = "startedAt", skip_serializing_if = "Option::is_none")]
        started_at: Option<f64>,
        /// 思考过程持续时间，单位为毫秒。
        #[serde(rename = "durationMs", skip_serializing_if = "Option::is_none")]
        duration_ms: Option<f64>,
    },
    #[serde(rename = "notice")]
    Notice {
        /// 提示事件的业务类型。
        kind: String,
        /// 提示事件的展示级别。
        level: String,
        /// 提示事件的展示标题。
        title: String,
        /// 提示事件的展示正文。
        message: String,
        /// 提示事件携带并持久化的可选结构化元数据。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        metadata: Option<Box<RawValue>>,
    },
    #[serde(rename = "error")]
    Error {
        /// 展示给用户的消息处理错误。
        message: String,
    },
    #[serde(rename = "attachment")]
    Attachment {
        /// 附件在消息中的文件名。
        #[serde(rename = "fileName")]
        file_name: String,
        /// 附件的本地或远程文件路径。
        #[serde(rename = "filePath")]
        file_path: String,
        /// 附件预览文件路径。
        #[serde(rename = "previewFilePath", default, skip_serializing_if = "Option::is_none")]
        preview_file_path: Option<String>,
        /// 附件原始文件字节数。
        #[serde(rename = "sizeBytes")]
        size_bytes: u64,
        /// 附件的媒体类型。
        #[serde(rename = "mimeType", skip_serializing_if = "Option::is_none")]
        mime_type: Option<String>,
        /// 附件图片标注所需的结构化元数据。
        #[serde(rename = "browserAnnotation", skip_serializing_if = "Option::is_none")]
        browser_annotation: Option<BrowserAnnotationMetadata>,
        /// 标记附件路径是否位于远程项目。
        #[serde(rename = "isRemote", default)]
        is_remote: bool,
    },
    #[serde(rename = "skill")]
    Skill {
        /// 被引用技能的展示名称。
        name: String,
        /// 被引用技能的路径。
        path: String,
    },
    #[serde(rename = "mention")]
    Mention {
        /// 被引用文件或资源的展示名称。
        name: String,
        /// 被引用文件或资源的路径。
        path: String,
    },
}

/// 动作输出中的单个标准输出或标准错误分块。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ActionOutputChunk {
    /// 输出所属的流类型。
    pub(crate) stream: String,
    /// 当前输出分块的文本内容。
    pub(crate) content: String,
}

/// 消息动作完成后的统一结果块。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActionBlockResult {
    /// 标记动作是否成功完成。
    pub(crate) success: bool,
    /// 动作成功时提供的结果文本。
    pub(crate) output: Option<String>,
    /// 动作失败时提供的错误说明。
    pub(crate) error: Option<String>,
    /// 动作修改文件产生的差异文本。
    pub(crate) diff: Option<String>,
    /// 动作执行所用时长，单位为毫秒。
    pub(crate) duration_ms: u64,
}

/// 向前端广播线程摘要更新的事件结构。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ThreadUpdatedEvent {
    /// 发生更新的本地线程主键。
    pub(crate) thread_id: String,
    /// 线程所属工作区主键。
    pub(crate) workspace_id: String,
    /// 更新后的完整线程数据。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) thread: Option<ThreadDto>,
}

/// 向前端广播一轮助手处理结束的事件结构。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatTurnFinishedEvent {
    /// 结束回合所属的本地线程主键。
    pub(crate) thread_id: String,
    /// 线程所属工作区主键。
    pub(crate) workspace_id: String,
    /// 当前回合使用的 CLI 标识。
    pub(crate) engine_id: String,
    /// 线程标题，用于前端通知展示。
    pub(crate) thread_title: String,
    /// 本轮消息最终状态。
    pub(crate) status: String,
    /// 助手最终回复的短预览文本。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) preview: Option<String>,
}

/// 向前端广播线程进入待审批状态的事件结构。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ChatApprovalRequestedEvent {
    /// 待审批回合所属的本地线程主键。
    pub(crate) thread_id: String,
    /// 线程所属工作区主键。
    pub(crate) workspace_id: String,
    /// 产生审批请求的 CLI 标识。
    pub(crate) engine_id: String,
    /// 线程标题，用于前端通知展示。
    pub(crate) thread_title: String,
    /// 审批请求的简要说明。
    pub(crate) summary: String,
}

/// 向前端广播远端 CLI 需要重启的事件结构。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CliServiceRestartRequiredEvent {
    /// 需要重启服务的本地线程主键。
    pub(crate) thread_id: String,
    /// 线程所属工作区主键。
    pub(crate) workspace_id: String,
    /// 当前 CLI 标识。
    pub(crate) engine_id: String,
    /// 线程标题，用于前端通知展示。
    pub(crate) thread_title: String,
    /// 远端 CLI 所属 SSH 连接主键。
    pub(crate) connection_id: String,
    /// 需要执行重启的业务原因。
    pub(crate) reason: String,
}

/// 向指定线程提交一轮普通聊天消息的统一请求。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    /// 需要继续对话的本地线程主键。
    pub(crate) thread_id: String,
    /// 本轮需要交给 CLI 处理的用户消息。
    pub(crate) message: String,
    /// 本轮指定的 CLI 标识；为空时使用线程当前 CLI。
    pub(crate) engine_id: Option<String>,
    /// 本轮指定的模型标识；为空时使用线程当前模型。
    pub(crate) model_id: Option<String>,
    /// 本轮是否启用计划模式。
    pub(crate) plan_mode: Option<bool>,
    /// 本轮消息发送方式及线程运行配置。
    pub(crate) send_method: Option<String>,
    /// 本轮指定的推理强度及线程运行配置。
    pub(crate) reasoning_effort: Option<String>,
    /// 本轮选中的统一权限对象 A。
    pub(crate) permission_values: Option<PermissionComponentJson>,
    /// 本轮需要交给 CLI 的文件附件。
    pub(crate) attachments: Option<Vec<ChatAttachmentPayload>>,
    /// 本轮需要交给 CLI 的结构化输入项。
    pub(crate) input_items: Option<Vec<ChatInputItemPayload>>,
    /// 前端为本轮生成的幂等标识。
    pub(crate) client_turn_id: Option<String>,
    /// 定时任务运行标识，用于绑定助手占位消息。
    pub(crate) scheduled_run_id: Option<String>,
    /// 本轮临时引用的 AuraCoder 线程主键。
    pub(crate) referenced_thread_id: Option<String>,
    /// 本次首次发送需要由后端创建的本地会话信息。
    pub(crate) new_thread: Option<NewChatThreadRequest>,
    /// 本次发送需要保存的 Codex 专属会话配置。
    pub(crate) codex_config: Option<ChatCodexSendConfig>,
    /// 本次发送需要保存的 OpenCode 专属会话配置。
    pub(crate) opencode_config: Option<ChatOpenCodeSendConfig>,
    /// 本次发送是否明确修改了权限中的新会话默认值。
    pub(crate) permission_defaults_changed: Option<bool>,
}

/// 首次发送时用于创建本地会话记录的工作区和标题。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewChatThreadRequest {
    /// 新会话所属的工作区标识。
    pub(crate) workspace_id: String,
    /// 新会话首次发送时采用的标题。
    pub(crate) title: String,
}

/// 本次发送需要应用到 Codex 会话的专属设置。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatCodexSendConfig {
    /// 本次发送选择的 Codex 个性配置。
    pub(crate) personality: Option<String>,
    /// 本次发送选择的 Codex 服务档位。
    pub(crate) service_tier: Option<String>,
    /// 本次发送选择的 Codex 输出结构约束。
    pub(crate) output_schema: Option<Value>,
}

/// 本次发送需要应用到 OpenCode 会话的专属设置。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatOpenCodeSendConfig {
    /// 本次发送选择的 OpenCode Agent。
    pub(crate) agent: Option<String>,
}

/// 向当前线程追加一条待处理消息的统一请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SteerMessageRequest {
    /// 需要追加消息的本地线程主键。
    pub thread_id: String,
    /// 本次追加的用户消息正文。
    pub message: String,
    /// 追加消息需要交给 CLI 的附件。
    pub attachments: Option<Vec<ChatAttachmentPayload>>,
    /// 追加消息需要交给 CLI 的结构化输入项。
    pub input_items: Option<Vec<ChatInputItemPayload>>,
    /// 指定追加消息采用的计划模式。
    pub plan_mode: Option<bool>,
    /// 前端为本次追加生成的幂等标识。
    pub client_steer_id: Option<String>,
    /// 追加消息临时引用的 AuraCoder 线程主键。
    pub referenced_thread_id: Option<String>,
    /// 本次追加消息使用的统一权限对象 A。
    pub permission_values: Option<PermissionComponentJson>,
}

/// 提交某个本地审批响应的统一请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResponseRequest {
    /// 审批所属的本地线程主键。
    pub thread_id: String,
    /// 本地审批记录主键。
    pub approval_id: String,
    /// 前端提交的统一审批选择，由对应 CLI 转换为原生答复。
    pub response: Value,
}

/// 发起一次代码审查的统一请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReviewRequest {
    /// 需要发起审查的本地线程主键。
    pub thread_id: String,
    /// 本次审查的目标及其参数。
    pub target: CodexReviewTargetPayload,
    /// 审查结果采用的投递方式。
    pub delivery: Option<CodexReviewDeliveryPayload>,
}

/// 普通聊天输入中传递的单一结构化内容项。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ChatInputItemPayload {
    /// 可供 CLI 处理的文本输入项。
    Text { text: String },
    /// 供 CLI 使用的技能名称和路径。
    Skill { name: String, path: String },
    /// 供 CLI 使用的资源提及名称和路径。
    Mention { name: String, path: String },
}

/// 代码审查时用户选择的审查目标。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodexReviewTargetPayload {
    /// 检查当前工作区尚未提交的改动。
    #[serde(rename = "uncommittedChanges")]
    UncommittedChanges,
    /// 检查指定基础分支的改动。
    #[serde(rename = "baseBranch")]
    BaseBranch {
        /// 作为审查基准的分支名称。
        branch: String,
    },
    /// 检查指定提交引入的改动。
    #[serde(rename = "commit")]
    Commit {
        /// 需要检查的提交 SHA。
        sha: String,
        /// 提交标题，用于审查上下文展示。
        #[serde(default)]
        title: Option<String>,
    },
    /// 按用户提供的指令执行定制审查。
    #[serde(rename = "custom")]
    Custom {
        /// 定制审查的用户指令。
        instructions: String,
    },
}

/// 代码审查结果投递到当前线程的方式。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexReviewDeliveryPayload {
    /// 将结果写入当前审查线程。
    Inline,
    /// 将结果写入独立审查线程。
    Detached,
}

/// 附件在前端预览时返回的媒体类型和编码数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentPreviewPayload {
    /// 预览数据的媒体类型。
    pub mime_type: String,
    /// 预览文件的 base64 编码内容。
    pub data_base64: String,
}

/// 聊天消息携带的附件信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatAttachmentPayload {
    /// 附件在消息中的文件名。
    pub file_name: String,
    /// 附件在本机或远程项目中的路径。
    pub file_path: String,
    /// 附件原始文件字节数。
    #[serde(default)]
    pub size_bytes: u64,
    /// 附件媒体类型，用于选择处理方式。
    #[serde(default)]
    pub mime_type: Option<String>,
    /// 图片标注处理所需的可选元数据。
    #[serde(default)]
    pub browser_annotation: Option<BrowserAnnotationMetadata>,
}

/// 从系统剪贴板读取并返回的图片数据。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardImagePayload {
    /// 剪贴板图片在本地保存时使用的文件名。
    pub file_name: String,
    /// 剪贴板图片的媒体类型。
    pub mime_type: String,
    /// 剪贴板图片的 base64 编码内容。
    pub data_base64: String,
    /// uri-list 兜底解析出的源图片本地路径。
    #[serde(default)]
    pub source_path: Option<String>,
}

/// 在远程事件中推送最终助手消息的结构。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AssistantMessageCompletedEvent {
    /// 已完成消息所属的本地线程主键。
    pub(crate) thread_id: String,
    /// 刚完成持久化的助手消息主键。
    pub(crate) message_id: String,
    /// 与消息列表单条消息结构一致的最终助手消息。
    pub(crate) message: MessageDto,
}

/// 审批已保存但附带后续处理说明的业务告警。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatBusinessWarning {
    /// 用于调用方识别告警类别的业务编码。
    pub code: String,
    /// 向用户说明已保存结果及后续处理状态的中文消息。
    pub message: String,
}

/// 返回审批业务状态和本地持久化后的线程、消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResponseOutcomeDto {
    /// 本地审批记录主键。
    pub approval_id: String,
    /// 审批保存后的状态。
    pub status: String,
    /// 审批保存后的决定内容。
    pub decision: Option<String>,
    /// 审批处理后的线程数据。
    pub thread: ThreadDto,
    /// 更新后的审批消息数据。
    pub message: Option<MessageDto>,
    /// 审批保存成功但后续处理存在异常时的说明。
    pub warning: Option<ChatBusinessWarning>,
}

/// 聊天命令向调用方返回的结构化业务错误。
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatBusinessError {
    /// 用于调用方判断业务错误类别的固定编码。
    pub code: String,
    /// 向用户显示的中文错误说明。
    pub message: String,
    /// 错误发生前已经保存的审批结果。
    pub outcome: Option<Box<ApprovalResponseOutcomeDto>>,
}

impl ChatBusinessError {
    /// 创建带有固定业务编码和用户可读中文消息的错误。
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            outcome: None,
        }
    }

    /// 将已保存的审批结果附加到后续处理失败的错误中。
    pub fn with_outcome(mut self, outcome: ApprovalResponseOutcomeDto) -> Self {
        self.outcome = Some(Box::new(outcome));
        self
    }
}

impl fmt::Display for ChatBusinessError {
    /// 以用户可读消息格式展示聊天业务错误。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ChatBusinessError {}
