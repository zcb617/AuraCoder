use anyhow::{Context, Result};

use crate::{
    cli_tools::{factory::CliToolFactory, CliExecutionContext, CliLocationKind, CliUsageQueryScope},
    models::{ChatProviderUsageDto, EngineInfoDto},
    state::AppState,
};

/// 查询本机或指定 SSH 连接上已就绪的 CLI 信息目录。
pub async fn list_actived_clis(
    state: &AppState,
    connection_id: Option<String>,
) -> Result<Vec<EngineInfoDto>> {
    // 构造仅包含运行位置的上下文，不读取数据库或项目目录。
    let context = if let Some(connection_id) = connection_id.as_ref() {
        CliExecutionContext {
            workspace_id: String::new(),
            root_path: String::new(),
            location_kind: CliLocationKind::Ssh,
            ssh_connection_id: Some(connection_id.clone()),
        }
    } else {
        CliExecutionContext {
            workspace_id: String::new(),
            root_path: String::new(),
            location_kind: CliLocationKind::Local,
            ssh_connection_id: None,
        }
    };

    // 通过工厂遍历已支持 CLI；单个 CLI 失败时记录原始错误并保留其他结果。
    let factory = CliToolFactory::new(state.clone());
    let mut engines = Vec::new();
    for cli_id in CliToolFactory::supported_cli_ids() {
        let cli = match factory.create(cli_id) {
            Ok(cli) => cli,
            Err(error) => {
                log::warn!(
                    "创建 CLI 目录查询接口失败，保留其他结果: cli_id={cli_id} error={error:#}"
                );
                continue;
            }
        };
        match cli.get_active_engine_info(&context).await {
            Ok(Some(engine_info)) => engines.push(engine_info),
            Ok(None) => {}
            Err(error) => {
                log::warn!(
                    "查询 CLI 活动模型目录失败，保留其他结果: cli_id={cli_id} error={error:#}"
                );
            }
        }
    }

    // 保持原目录结果按 CLI 标识排序，并报告没有可用工具的远端目标。
    engines.sort_by(|left, right| left.id.cmp(&right.id));
    if connection_id.is_some() && engines.is_empty() {
        anyhow::bail!("SSH 远端机器没有已激活的 CLI 工具");
    }
    Ok(engines)
}

/// 查询本机可用 CLI 信息，并保留远程调用方使用的字符串错误类型。
pub async fn list_local_engine_infos(state: &AppState) -> std::result::Result<Vec<EngineInfoDto>, String> {
    list_actived_clis(state, None)
        .await
        .map_err(|error| format!("{error:#}"))
}

/// 按用户选择或页面汇总范围读取 CLI 额度信息。
pub async fn get_chat_provider_usage(
    state: &AppState,
    workspace_id: Option<String>,
    engine_id: Option<String>,
) -> Result<Vec<ChatProviderUsageDto>> {
    let result: Result<Vec<ChatProviderUsageDto>> = async {
        // 明确选择 CLI 时保留其不可用结果，并让错误沿当前 CLI 调用链返回。
        let factory = CliToolFactory::new(state.clone());
        let mut usage = Vec::new();
        if let Some(engine_id) = engine_id {
            let cli = factory
                .create(&engine_id)
                .with_context(|| format!("创建所选 CLI 接口失败: cli_id={engine_id}"))?;
            let context = cli
                .execution_context(workspace_id.as_deref())
                .await
                .with_context(|| format!("读取所选 CLI 执行目标失败: cli_id={engine_id}"))?;
            if let Some(item) = cli
                .get_chat_provider_usage(&context, CliUsageQueryScope::Explicit)
                .await
                .with_context(|| format!("读取所选 CLI 额度失败: cli_id={engine_id}"))?
            {
                usage.push(item);
            }
            return Ok(usage);
        }

        // 默认汇总所有支持的 CLI；无额度结果的实现不加入汇总列表。
        for cli_id in CliToolFactory::supported_cli_ids() {
            let cli = factory
                .create(cli_id)
                .with_context(|| format!("创建 CLI 汇总接口失败: cli_id={cli_id}"))?;
            let context = cli
                .execution_context(workspace_id.as_deref())
                .await
                .with_context(|| format!("读取 CLI 汇总执行目标失败: cli_id={cli_id}"))?;
            if let Some(item) = cli
                .get_chat_provider_usage(&context, CliUsageQueryScope::Summary)
                .await
                .with_context(|| format!("读取 CLI 额度汇总失败: cli_id={cli_id}"))?
            {
                usage.push(item);
            }
        }
        Ok(usage)
    }
    .await;
    if let Err(error) = &result {
        log::error!("读取 CLI 额度失败: {error:#}");
    }
    result
}
