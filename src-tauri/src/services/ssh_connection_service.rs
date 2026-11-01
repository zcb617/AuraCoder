use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result};
use tauri::AppHandle;
use uuid::Uuid;

use crate::{
    cli_tools::{factory::CliToolFactory, CliExecutionContext, CliLocationKind},
    db::{self, ssh_connections::SshConnectionRecord, Database},
    models::{
        SshConfigHostDto, SshConnectionDto, SshConnectionImportResultDto, SshConnectionInput,
        SshConnectionTestDto,
    },
    ssh::{config, gateway, known_hosts},
    state::AppState,
};

/// 集中承载 SSH 连接配置、检测和关联资源生命周期的业务服务。
pub struct SshConnectionService {
    /// 当前应用的数据库、监控器和工作区资源依赖。
    state: AppState,
}

impl SshConnectionService {
    /// 创建 SSH 连接业务服务并绑定当前应用状态。
    pub fn new(state: AppState) -> Self {
        Self { state }
    }

    /// 查询所有未删除的 SSH 连接配置。
    pub async fn list_ssh_connections(&self) -> Result<Vec<SshConnectionDto>> {
        // 在线程池中读取连接列表，避免 SQLite 阻塞异步运行时。
        let result = run_db(self.state.db.clone(), |db| db::ssh_connections::list(db, false))
            .await
            .context("读取 SSH 连接列表失败");
        if let Err(error) = &result {
            log::error!("读取 SSH 连接列表失败: error={error:#}");
        }
        result
    }

    /// 查询所有已软删除的 SSH 连接配置。
    pub async fn list_deleted_ssh_connections(&self) -> Result<Vec<SshConnectionDto>> {
        // 在线程池中读取已删除连接，避免 SQLite 阻塞异步运行时。
        let result = run_db(self.state.db.clone(), |db| db::ssh_connections::list(db, true))
            .await
            .context("读取已删除的 SSH 连接列表失败");
        if let Err(error) = &result {
            log::error!("读取已删除的 SSH 连接列表失败: error={error:#}");
        }
        result
    }

    /// 扫描 SSH 配置并合并数据库中的已导入与已删除标记。
    pub async fn scan_ssh_config_hosts(&self) -> Result<Vec<SshConfigHostDto>> {
        let result = async {
            let hosts = config::scan().await.context("扫描 SSH 配置文件失败")?;

            // 一次性读取当前和已删除连接，再按 alias 映射扫描结果标记。
            run_db(self.state.db.clone(), move |db| {
                let active_aliases = db::ssh_connections::list_records(db, false)?
                    .into_iter()
                    .filter_map(|record| record.dto.config_alias)
                    .collect::<HashSet<_>>();
                let deleted_aliases = db::ssh_connections::list_records(db, true)?
                    .into_iter()
                    .filter_map(|record| record.dto.config_alias)
                    .collect::<HashSet<_>>();
                Ok(hosts
                    .into_iter()
                    .map(|host| {
                        let alias = host.alias.clone();
                        config::as_dto(
                            host,
                            active_aliases.contains(&alias),
                            deleted_aliases.contains(&alias),
                        )
                    })
                    .collect::<Vec<_>>())
            })
            .await
            .context("合并 SSH 配置扫描结果失败")
        }
        .await;
        if let Err(error) = &result {
            log::error!("扫描 SSH 配置主机失败: error={error:#}");
        }
        result
    }

    /// 按 SSH 配置 alias 逐条导入连接并保留每条导入、检测和注册结果。
    pub async fn import_ssh_config_hosts(
        &self,
        aliases: Vec<String>,
    ) -> Result<Vec<SshConnectionImportResultDto>> {
        let result = async {
            let hosts = config::scan().await.context("扫描 SSH 配置文件失败")?;
            let selected = hosts
                .into_iter()
                .filter(|host| aliases.iter().any(|alias| alias == &host.alias))
                .collect::<Vec<_>>();

            // 单次数据库阶段查询 alias 状态，并按扫描顺序逐项写入结果。
            let mut results = run_db(self.state.db.clone(), move |db| {
                import_config_hosts(db, selected)
            })
            .await
            .context("导入 SSH 配置主机失败")?;

            // 批量读取已保存记录后逐条执行检测，避免在结果循环中查询数据库。
            let records = run_db(self.state.db.clone(), |db| {
                db::ssh_connections::list_records(db, false)
            })
            .await;
            match records {
                Ok(records) => {
                    let records_by_id = records
                        .into_iter()
                        .map(|record| (record.dto.id.clone(), record))
                        .collect::<HashMap<_, _>>();
                    for import_result in &mut results {
                        let Some(connection) = import_result.connection.as_ref() else {
                            continue;
                        };
                        let Some(record) = records_by_id.get(&connection.id) else {
                            append_import_error(
                                import_result,
                                "连接已保存，但读取配置进行检测时未找到记录".to_string(),
                            );
                            continue;
                        };
                        match self.test_and_register_cli_tunnels(record).await {
                            Ok(test) => {
                                if !test.ok {
                                    append_import_error(
                                        import_result,
                                        format!(
                                            "连接已保存，但连接检测失败：{}",
                                            test.error.unwrap_or_else(|| "未知检测错误".to_string())
                                        ),
                                    );
                                } else if let Some(error) = test.error {
                                    append_import_error(
                                        import_result,
                                        format!("连接已检测成功，但 CLI 服务注册未全部完成：{error}"),
                                    );
                                }
                            }
                            Err(error) => {
                                log::error!(
                                    "导入后检测 SSH 连接失败: connection_id={} alias={} error={error:#}",
                                    connection.id,
                                    import_result.alias
                                );
                                append_import_error(
                                    import_result,
                                    format!("连接已保存，但检测或 CLI 服务注册失败：{error}"),
                                );
                            }
                        }
                    }
                }
                Err(error) => {
                    log::error!("读取导入后的 SSH 连接记录失败: error={error:#}");
                    for import_result in &mut results {
                        if import_result.connection.is_some() {
                            append_import_error(
                                import_result,
                                format!("连接已保存，但读取配置进行检测失败：{error}"),
                            );
                        }
                    }
                }
            }

            // 导入结果已逐条生成，监控重建失败只附加到已保存记录，不丢弃结果。
            if let Err(error) = self.state.ssh_monitor.reconcile(self.state.db.clone()).await {
                log::error!("导入后同步 SSH 连接监控失败: error={error:#}");
                for import_result in &mut results {
                    if import_result.connection.is_some() {
                        append_import_error(
                            import_result,
                            format!("连接已保存，但监控同步失败：{error}"),
                        );
                    }
                }
            }
            Ok(results)
        }
        .await;
        if let Err(error) = &result {
            log::error!("导入 SSH 配置主机失败: aliases={aliases:?} error={error:#}");
        }
        result
    }

    /// 校验并保存手动 SSH 连接，再进行检测和监控启动。
    pub async fn create_manual_ssh_connection(
        &self,
        mut input: SshConnectionInput,
    ) -> Result<SshConnectionDto> {
        input.config_alias = None;
        let log_alias = input.display_name.clone();
        let result = async {
            validate_input(&input, true)?;
            let (key_type, key_base64) = known_hosts::parse_host_key(&input.host_key)
                .context("手动连接必须提供有效的 Host Key")?;
            let id = Uuid::new_v4().to_string();

            // 先持久化连接，后续网络检测或监控异常不得使前端误以为保存未发生。
            let saved = run_db(self.state.db.clone(), move |db| {
                db::ssh_connections::insert(
                    db,
                    &id,
                    "manual",
                    &input,
                    &key_type,
                    &key_base64,
                )
            })
            .await
            .context("保存手动 SSH 连接失败")?;

            let record = run_db(self.state.db.clone(), {
                let id = saved.id.clone();
                move |db| db::ssh_connections::find(db, &id)
            })
            .await;
            match record {
                Ok(Some(record)) => {
                    if let Err(error) = self.test_and_register_cli_tunnels(&record).await {
                        log::error!(
                            "手动 SSH 连接已保存，但检测或 CLI 注册失败: connection_id={} error={error:#}",
                            saved.id
                        );
                    }
                }
                Ok(None) => {
                    let error = anyhow::anyhow!("保存后未找到 SSH 连接记录");
                    log::error!(
                        "手动 SSH 连接已保存，但无法读取配置: connection_id={} error={error:#}",
                        saved.id
                    );
                }
                Err(error) => {
                    log::error!(
                        "手动 SSH 连接已保存，但读取配置失败: connection_id={} error={error:#}",
                        saved.id
                    );
                }
            }

            if let Err(error) = self.state.ssh_monitor.start(self.state.db.clone(), &saved.id).await {
                log::error!(
                    "手动 SSH 连接已保存，但监控启动失败: connection_id={} error={error:#}",
                    saved.id
                );
                let id = saved.id.clone();
                let expected_updated_at = saved.updated_at.clone();
                let last_error = format!("SSH 连接已保存，但监控启动失败：{error}");
                match run_db(self.state.db.clone(), move |db| {
                    db::ssh_connections::set_status_if_current(
                        db,
                        &id,
                        &expected_updated_at,
                        db::ssh_connections::STATUS_FAILED,
                        Some(&last_error),
                    )
                })
                .await
                {
                    Ok(_) => {}
                    Err(update_error) => log::error!(
                        "保存监控启动失败状态时出错: connection_id={} error={update_error:#}",
                        saved.id
                    ),
                }
            }

            let latest = run_db(self.state.db.clone(), {
                let id = saved.id.clone();
                move |db| db::ssh_connections::find(db, &id)
            })
            .await;
            match latest {
                Ok(Some(record)) => Ok(record.dto),
                Ok(None) => Ok(saved),
                Err(error) => {
                    log::error!(
                        "读取手动 SSH 连接最新状态失败，返回已保存结果: connection_id={} error={error:#}",
                        saved.id
                    );
                    Ok(saved)
                }
            }
        }
        .await;
        if let Err(error) = &result {
            log::error!("创建手动 SSH 连接失败: alias={} error={error:#}", log_alias);
        }
        result
    }

    /// 更新 SSH 连接参数并在结束路径尝试恢复对应监控。
    pub async fn update_ssh_connection(
        &self,
        connection_id: String,
        mut input: SshConnectionInput,
    ) -> Result<SshConnectionDto> {
        let result = async {
            validate_input(&input, false)?;
            let existing = run_db(self.state.db.clone(), {
                let id = connection_id.clone();
                move |db| db::ssh_connections::find(db, &id)
            })
            .await
            .context("读取待更新 SSH 连接失败")?
            .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在"))?;

            // 根据连接来源固定 alias 与 Host Key 数据，阻止手动连接改变来源类型。
            let (key_type, key_base64) = if existing.dto.source_kind == "ssh_config" {
                input.config_alias = existing.dto.config_alias.clone();
                ("config".to_string(), String::new())
            } else {
                input.config_alias = None;
                if input.host_key.trim().is_empty() {
                    (
                        existing.dto.host_key_type.clone(),
                        existing.host_key_base64.clone(),
                    )
                } else {
                    known_hosts::parse_host_key(&input.host_key)
                        .context("更新手动连接时 Host Key 无效")?
                }
            };

            self.state.ssh_monitor.stop(&connection_id).await;
            let update_result = run_db(self.state.db.clone(), {
                let id = connection_id.clone();
                move |db| db::ssh_connections::update(db, &id, &input, &key_type, &key_base64)
            })
            .await
            .context("更新 SSH 连接配置失败");
            let monitor_result = self
                .state
                .ssh_monitor
                .start(self.state.db.clone(), &connection_id)
                .await
                .context("重启 SSH 连接监控失败");

            match (update_result, monitor_result) {
                (Ok(dto), Ok(())) => Ok(dto),
                (Ok(_), Err(error)) => Err(error.context("SSH 连接配置已保存，但监控重启失败")),
                (Err(error), Ok(())) => Err(error),
                (Err(error), Err(monitor_error)) => Err(error.context(format!(
                    "更新 SSH 连接失败后监控恢复也失败: {monitor_error:#}"
                ))),
            }
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "更新 SSH 连接失败: connection_id={} error={error:#}",
                connection_id
            );
        }
        result
    }

    /// 检测指定 SSH 连接并仅保存当前配置版本对应的检测结果。
    pub async fn test_ssh_connection(&self, connection_id: String) -> Result<SshConnectionTestDto> {
        let result = async {
            let record = run_db(self.state.db.clone(), {
                let id = connection_id.clone();
                move |db| db::ssh_connections::find(db, &id)
            })
            .await
            .context("读取 SSH 连接配置失败")?
            .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在"))?;
            let test_result = gateway::test(&record).await;
            let id = record.dto.id.clone();
            let expected_updated_at = record.dto.updated_at.clone();
            let ok = test_result.ok;
            let error = test_result.error.clone();

            // 过期或已删除连接拒绝写入检测状态，避免旧探测覆盖新配置。
            let recorded = run_db(self.state.db.clone(), move |db| {
                db::ssh_connections::record_test_if_current(
                    db,
                    &id,
                    &expected_updated_at,
                    ok,
                    error.as_deref(),
                )
            })
            .await
            .context("保存 SSH 连接检测结果失败")?;
            if !recorded {
                anyhow::bail!("SSH 连接配置已变化或已删除，检测结果未保存");
            }
            Ok(test_result)
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "检测 SSH 连接失败: connection_id={} error={error:#}",
                connection_id
            );
        }
        result
    }

    /// 保存 SSH 连接启用状态并按数据库状态启动监控或关闭关联资源。
    pub async fn set_ssh_connection_enabled(
        &self,
        app: AppHandle,
        connection_id: String,
        enabled: bool,
    ) -> Result<SshConnectionDto> {
        let result = async {
            let dto = run_db(self.state.db.clone(), {
                let id = connection_id.clone();
                move |db| db::ssh_connections::set_enabled(db, &id, enabled)
            })
            .await
            .context("保存 SSH 连接启用状态失败")?;

            // 持久化状态优先，副作用失败时保留该状态并向用户说明后续动作未完成。
            if dto.enabled {
                self.state
                    .ssh_monitor
                    .start(self.state.db.clone(), &dto.id)
                    .await
                    .context("SSH 连接已启用，但监控启动失败")?;
            } else {
                self.state.ssh_monitor.stop(&dto.id).await;
                close_connection_resources(&self.state, &app, &dto.id)
                    .await
                    .context("SSH 连接已禁用，但关联资源未全部关闭")?;
            }
            Ok(dto)
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "设置 SSH 连接启用状态失败: connection_id={} enabled={} error={error:#}",
                connection_id,
                enabled
            );
        }
        result
    }

    /// 软删除 SSH 连接并关闭该连接关联的 CLI 与工作区资源。
    pub async fn delete_ssh_connection(
        &self,
        app: AppHandle,
        connection_id: String,
    ) -> Result<()> {
        let result = async {
            let record = run_db(self.state.db.clone(), {
                let id = connection_id.clone();
                move |db| db::ssh_connections::find(db, &id)
            })
            .await
            .context("读取待删除 SSH 连接失败")?
            .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在"))?;

            // 重复删除跳过数据库写入，但继续清理可能残留的运行资源。
            if record.dto.deleted_at.is_none() {
                run_db(self.state.db.clone(), {
                    let id = connection_id.clone();
                    move |db| db::ssh_connections::soft_delete(db, &id)
                })
                .await
                .context("删除 SSH 连接记录失败")?;
            }
            self.state.ssh_monitor.stop(&connection_id).await;
            close_connection_resources(&self.state, &app, &connection_id)
                .await
                .context("SSH 连接记录已删除，但关联资源未全部关闭")?;
            Ok(())
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "删除 SSH 连接失败: connection_id={} error={error:#}",
                connection_id
            );
        }
        result
    }

    /// 按原连接 ID 恢复 SSH 连接，并为启用连接重启监控。
    pub async fn restore_ssh_connection(&self, connection_id: String) -> Result<SshConnectionDto> {
        let result = async {
            let existing = run_db(self.state.db.clone(), {
                let id = connection_id.clone();
                move |db| db::ssh_connections::find(db, &id)
            })
            .await
            .context("读取待恢复 SSH 连接失败")?
            .ok_or_else(|| anyhow::anyhow!("SSH 连接不存在"))?;
            let dto = if existing.dto.deleted_at.is_some() {
                run_db(self.state.db.clone(), {
                    let id = connection_id.clone();
                    move |db| db::ssh_connections::restore(db, &id)
                })
                .await
                .context("恢复 SSH 连接失败")?
            } else {
                existing.dto
            };

            // 未删除的重复恢复复用当前记录，并仍可重试启用连接的监控启动。
            if dto.enabled {
                self.state
                    .ssh_monitor
                    .start(self.state.db.clone(), &dto.id)
                    .await
                    .context("SSH 连接已恢复，但监控启动失败")?;
            }
            Ok(dto)
        }
        .await;
        if let Err(error) = &result {
            log::error!(
                "恢复 SSH 连接失败: connection_id={} error={error:#}",
                connection_id
            );
        }
        result
    }

    /// 检测连接、条件保存检测状态，并为有效结果注册支持的远端 CLI 服务。
    async fn test_and_register_cli_tunnels(
        &self,
        record: &SshConnectionRecord,
    ) -> Result<SshConnectionTestDto> {
        let mut result = gateway::test(record).await;
        let id = record.dto.id.clone();
        let expected_updated_at = record.dto.updated_at.clone();
        let ok = result.ok;
        let error = result.error.clone();

        // 先按测试开始时的配置版本保存结果，过期结果不能继续注册 CLI。
        let recorded = run_db(self.state.db.clone(), move |db| {
            db::ssh_connections::record_test_if_current(
                db,
                &id,
                &expected_updated_at,
                ok,
                error.as_deref(),
            )
        })
        .await
        .with_context(|| format!("保存 SSH 连接检测结果失败: connection_id={}", record.dto.id))?;
        if !recorded {
            anyhow::bail!("SSH 连接配置已变化或已删除，检测结果未应用");
        }
        if !result.ok {
            return Ok(result);
        }

        let current = run_db(self.state.db.clone(), {
            let id = record.dto.id.clone();
            move |db| db::ssh_connections::find(db, &id)
        })
        .await
        .with_context(|| format!("读取 SSH 连接最新状态失败: connection_id={}", record.dto.id))?;
        let Some(current) = current else {
            anyhow::bail!("SSH 连接已删除，跳过远端 CLI 注册");
        };
        if !can_register_remote_cli(true, &record.dto.updated_at, &current) {
            return Ok(result);
        }

        let factory = CliToolFactory::new(self.state.clone());
        let supported_cli_ids = CliToolFactory::supported_cli_ids();
        let context = CliExecutionContext {
            workspace_id: String::new(),
            root_path: String::new(),
            location_kind: CliLocationKind::Ssh,
            ssh_connection_id: Some(record.dto.id.clone()),
        };
        let mut registration_errors = Vec::new();

        // 各 CLI 注册相互独立，单项错误写入检测结果并继续尝试其他工具。
        for cli_id in result.cli_versions.keys() {
            if !supported_cli_ids.contains(&cli_id.as_str()) {
                log::warn!(
                    "SSH 检测发现未支持的 CLI，跳过远端服务注册: connection_id={} cli_id={}",
                    record.dto.id,
                    cli_id
                );
                continue;
            }
            let cli = match factory.create(cli_id) {
                Ok(cli) => cli,
                Err(error) => {
                    log::error!(
                        "创建 SSH CLI 统一接口失败: connection_id={} cli_id={} error={error:#}",
                        record.dto.id,
                        cli_id
                    );
                    registration_errors.push(format!("{cli_id}: {error:#}"));
                    continue;
                }
            };
            if let Err(error) = cli.register_remote_service(&context).await {
                log::error!(
                    "注册 SSH 远端 CLI 服务失败: connection_id={} cli_id={} error={error:#}",
                    record.dto.id,
                    cli_id
                );
                registration_errors.push(format!("{cli_id}: {error:#}"));
            }
        }
        if !registration_errors.is_empty() {
            result.error = Some(format!(
                "SSH 检测成功，但部分远端 CLI 服务注册失败：{}",
                registration_errors.join("；")
            ));
        }
        Ok(result)
    }
}

/// 在线程池运行同步 SQLite 操作，避免阻塞异步服务方法。
async fn run_db<T, F>(db: Database, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&Database) -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(move || operation(&db))
        .await
        .context("SSH 数据库任务执行失败")?
}

/// 校验手动创建和 SSH 配置导入共用的连接必填项与端口规则。
fn validate_input(input: &SshConnectionInput, require_key: bool) -> Result<()> {
    if input.display_name.trim().is_empty()
        || input.host_name.trim().is_empty()
        || input.user.trim().is_empty()
    {
        anyhow::bail!("显示名称、主机地址和用户名不能为空");
    }
    if input.port == 0 {
        anyhow::bail!("端口必须在 1-65535 范围内");
    }
    if require_key && input.host_key.trim().is_empty() {
        anyhow::bail!("手动添加必须填写完整的 Host Key");
    }
    Ok(())
}

/// 按数据库当前 alias 状态逐条处理扫描结果并生成可保留部分成功的导入 DTO。
fn import_config_hosts(
    db: &Database,
    hosts: Vec<config::ParsedHost>,
) -> Result<Vec<SshConnectionImportResultDto>> {
    let mut active_by_alias = db::ssh_connections::list_records(db, false)?
        .into_iter()
        .filter_map(|record| {
            let alias = record.dto.config_alias.clone()?;
            Some((alias, record.dto))
        })
        .collect::<HashMap<_, _>>();
    let mut deleted_by_alias = db::ssh_connections::list_records(db, true)?
        .into_iter()
        .filter_map(|record| {
            let alias = record.dto.config_alias.clone()?;
            Some((alias, record.dto))
        })
        .collect::<HashMap<_, _>>();
    let mut results = Vec::with_capacity(hosts.len());

    // alias 状态已预加载为 map，循环内只进行当前条目的校验和必要写入。
    for host in hosts {
        let alias = host.alias.clone();
        if let Some(existing) = active_by_alias.get(&alias) {
            results.push(SshConnectionImportResultDto {
                alias,
                connection: Some(existing.clone()),
                error: None,
                restored: false,
            });
            continue;
        }

        let deleted = deleted_by_alias.get(&alias).cloned();
        let input = SshConnectionInput {
            display_name: deleted
                .as_ref()
                .map(|connection| connection.display_name.clone())
                .unwrap_or_else(|| host.alias.clone()),
            host_name: host.host_name,
            user: host.user,
            port: host.port,
            identity_file: host.identity_file,
            host_key: String::new(),
            config_alias: Some(alias.clone()),
        };
        let imported = match validate_input(&input, false) {
            Ok(()) => {
                if let Some(existing) = deleted {
                    db::ssh_connections::restore_with_input(
                        db,
                        &existing.id,
                        &input,
                        "config",
                        "",
                    )
                    .map(|connection| (connection, true))
                    .with_context(|| format!("恢复已删除的 SSH 配置失败: alias={alias}"))
                } else {
                    db::ssh_connections::insert(
                        db,
                        &Uuid::new_v4().to_string(),
                        "ssh_config",
                        &input,
                        "config",
                        "",
                    )
                    .map(|connection| (connection, false))
                    .with_context(|| format!("保存 SSH 配置失败: alias={alias}"))
                }
            }
            Err(error) => Err(error).with_context(|| format!("SSH 配置参数无效: alias={alias}")),
        };

        match imported {
            Ok((connection, restored)) => {
                active_by_alias.insert(alias.clone(), connection.clone());
                deleted_by_alias.remove(&alias);
                results.push(SshConnectionImportResultDto {
                    alias,
                    connection: Some(connection),
                    error: None,
                    restored,
                });
            }
            Err(error) => {
                log::error!("导入单条 SSH 配置失败: alias={} error={error:#}", alias);
                results.push(SshConnectionImportResultDto {
                    alias,
                    connection: None,
                    error: Some(format!("导入连接失败：{error}")),
                    restored: false,
                });
            }
        }
    }
    Ok(results)
}

/// 将后续单项错误追加到导入结果，保留该 alias 已有的检测或存储错误。
fn append_import_error(result: &mut SshConnectionImportResultDto, error: String) {
    match result.error.as_mut() {
        Some(existing) => {
            existing.push_str("；");
            existing.push_str(&error);
        }
        None => result.error = Some(error),
    }
}

/// 判断已保存的检测结果是否仍对应启用且未删除的当前连接版本。
fn can_register_remote_cli(
    probe_was_saved: bool,
    expected_updated_at: &str,
    current: &SshConnectionRecord,
) -> bool {
    probe_was_saved
        && current.dto.updated_at == expected_updated_at
        && current.dto.enabled
        && current.dto.deleted_at.is_none()
}

/// 关闭指定 SSH 连接的 CLI 远端服务、终端和工作区通知，并汇总各项错误。
async fn close_connection_resources(
    state: &AppState,
    app: &AppHandle,
    connection_id: &str,
) -> Result<()> {
    let factory = CliToolFactory::new(state.clone());
    let context = CliExecutionContext {
        workspace_id: String::new(),
        root_path: String::new(),
        location_kind: CliLocationKind::Ssh,
        ssh_connection_id: Some(connection_id.to_string()),
    };
    let mut errors = Vec::new();

    // 先逐个通过工厂与统一 CLI 接口注销远端服务，单个 CLI 失败不阻止其他项。
    for cli_id in CliToolFactory::supported_cli_ids() {
        let cli = match factory.create(cli_id) {
            Ok(cli) => cli,
            Err(error) => {
                log::error!(
                    "创建 SSH CLI 统一接口失败，无法注销远端服务: connection_id={} cli_id={} error={error:#}",
                    connection_id,
                    cli_id
                );
                errors.push(format!("{cli_id}: {error:#}"));
                continue;
            }
        };
        if let Err(error) = cli.unregister_remote_service(&context).await {
            log::error!(
                "注销 SSH 远端 CLI 服务失败: connection_id={} cli_id={} error={error:#}",
                connection_id,
                cli_id
            );
            errors.push(format!("{cli_id}: {error:#}"));
        }
    }

    // 一次查询取得关联工作区，逐项关闭终端并始终尝试清理通知。
    let workspace_ids = run_db(state.db.clone(), {
        let connection_id = connection_id.to_string();
        move |db| db::workspaces::workspace_ids_for_ssh_connection(db, &connection_id)
    })
    .await;
    match workspace_ids {
        Ok(workspace_ids) => {
            for workspace_id in workspace_ids {
                if let Err(error) = state
                    .terminals
                    .close_workspace(app.clone(), &workspace_id)
                    .await
                {
                    log::error!(
                        "关闭 SSH 工作区终端失败: connection_id={} workspace_id={} error={error:#}",
                        connection_id,
                        workspace_id
                    );
                    errors.push(format!("工作区 {workspace_id} 终端: {error:#}"));
                }
                state
                    .notifications
                    .clear_for_workspace(app, &workspace_id)
                    .await;
            }
        }
        Err(error) => {
            log::error!(
                "读取 SSH 连接关联工作区失败: connection_id={} error={error:#}",
                connection_id
            );
            errors.push(format!("读取关联工作区: {error:#}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        anyhow::bail!("连接关联资源关闭失败：{}", errors.join("；"));
    }
}

#[cfg(test)]
#[path = "../../tests/unit/ssh_connection_service_tests.rs"]
mod tests;
